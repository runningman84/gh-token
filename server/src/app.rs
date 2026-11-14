//! Server state, startup validation, background loops and serving.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use gh_token_common::mask_token;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::github::GithubClient;
use crate::http;
use crate::metrics::Metrics;
use crate::ratelimit::RateLimiter;
use crate::store::{TokenRecord, TokenStore};

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Thread-safe state of the last GitHub reachability probe.
#[derive(Debug, Default)]
pub struct ProbeState {
    last_success_nanos: AtomicU64,
    last_attempt_ok: AtomicU64, // 0 = unknown/false, 1 = last attempt succeeded
}

impl ProbeState {
    pub fn record_success(&self) {
        self.last_success_nanos.store(now_nanos(), Ordering::Relaxed);
        self.last_attempt_ok.store(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.last_attempt_ok.store(0, Ordering::Relaxed);
    }

    pub fn is_fresh(&self, max_age: Duration) -> bool {
        let last = self.last_success_nanos.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = now_nanos();
        now >= last && Duration::from_nanos(now - last) <= max_age
    }

    pub fn last_success_was_ok(&self) -> bool {
        self.last_attempt_ok.load(Ordering::Relaxed) == 1
    }

    pub fn last_success_age(&self) -> Option<Duration> {
        let last = self.last_success_nanos.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        let now = now_nanos();
        (last <= now).then(|| Duration::from_nanos(now - last))
    }

    pub fn last_success_rfc3339(&self) -> Option<String> {
        let last = self.last_success_nanos.load(Ordering::Relaxed);
        (last != 0).then(|| {
            DateTime::from_timestamp_nanos(last as i64).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
    }
}

/// Shared server state handed to all handlers and background tasks.
#[derive(Clone)]
pub struct ServerState {
    pub config: Config,
    pub github: GithubClient,
    pub store: Arc<TokenStore>,
    pub metrics: Arc<Metrics>,
    pub rate_limiter: Arc<RateLimiter>,
    pub probe: Arc<ProbeState>,
}

/// Result of the startup credential validation.
#[derive(Debug, Clone)]
pub struct StartupReport {
    pub app_id: u64,
    pub app_name: String,
    pub installation_id: u64,
    /// App-level permission declarations (from `GET /app`).
    pub app_permissions: serde_json::Value,
    /// Effective installation permissions (from `GET /app/installations/{id}`).
    pub installation_permissions: serde_json::Value,
    /// `"all"` or `"selected"`.
    pub repository_selection: String,
    /// The bot login, if it could be determined with a test token.
    pub bot_login: Option<String>,
}

/// A running gh-token server.
pub struct Server {
    _state: Arc<ServerState>,
    addr: SocketAddr,
    stop: CancellationToken,
}

impl Server {
    /// Builds the shared state (does not bind anything).
    pub fn new(config: Config) -> Arc<ServerState> {
        Arc::new(ServerState {
            config: config.clone(),
            github: GithubClient::new(
                config.app_id,
                config.installation_id,
                config.app_key,
                config.github_base_url.clone(),
            ),
            store: TokenStore::new(),
            metrics: Metrics::new(),
            rate_limiter: Arc::new(RateLimiter::new(config.rate_window, config.rate_max)),
            probe: Arc::new(ProbeState::default()),
        })
    }

    /// Validates the GitHub App credentials against the GitHub API.
    ///
    /// This proves the auth level the app has: app identity (`GET /app`),
    /// installation access (`GET /app/installations/{id}`, including
    /// effective permissions and repository selection) and token creation
    /// (`POST /app/installations/{id}/access_tokens`, followed by
    /// `GET /user` with the new token).
    pub async fn validate_startup(state: &Arc<ServerState>) -> Result<StartupReport, String> {
        let app = state
            .github
            .get_app()
            .await
            .map_err(|e| {
                state.metrics.github_api_error("startup_get_app");
                format!("GET /app failed (invalid app key?): {e}")
            })?;

        if app.id != state.config.app_id {
            return Err(format!(
                "GH_APP_ID is {} but the provided key authenticates as app id {} (app {:#?})",
                state.config.app_id, app.id, app.name
            ));
        }

        let installation = state
            .github
            .get_installation()
            .await
            .map_err(|e| {
                state.metrics.github_api_error("startup_get_installation");
                format!(
                    "GET /app/installations/{} failed (invalid installation id or missing access): {e}",
                    state.config.installation_id
                )
            })?;

        let mut report = StartupReport {
            app_id: app.id,
            app_name: app.name.clone(),
            installation_id: installation.id,
            app_permissions: app.permissions.clone(),
            installation_permissions: installation.permissions.clone(),
            repository_selection: installation.repository_selection.clone(),
            bot_login: None,
        };

        // Create a throwaway token to prove token creation works and to
        // discover the bot login. The token simply expires an hour later.
        let test_token = state.github.create_installation_token().await.map_err(|e| {
            state.metrics.github_api_error("startup_create_token");
            format!("creating an installation token failed: {e}")
        })?;
        match state.github.user_for_token(&test_token.token).await {
            Ok(login) => {
                report.bot_login = Some(login.clone());
                info!(bot_login = %login, "startup check: installation token works");
            }
            Err(e) => {
                warn!(%e, "startup check: GET /user with the new token failed (continuing)");
            }
        }

        Ok(report)
    }

    /// Binds the listener, starts the background loops and returns the server.
    pub async fn start(state: Arc<ServerState>) -> anyhow::Result<Server> {
        let stop = CancellationToken::new();
        let listener = tokio::net::TcpListener::bind(state.config.bind).await?;
        let addr = listener.local_addr()?;
        let router = router(state.clone());

        let probe_state = state.clone();
        let probe_stop = stop.clone();
        tokio::spawn(async move { probe_loop(probe_state, probe_stop).await });

        let watch_state = state.clone();
        let watch_stop = stop.clone();
        tokio::spawn(async move { watch_loop(watch_state, watch_stop).await });

        let serve_stop = stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = serve_stop.cancelled() => {}
                res = axum::serve(listener, router) => {
                    if let Err(err) = res {
                        warn!(%err, "axum serve loop ended");
                    }
                }
            }
        });

        Ok(Server {
            _state: state,
            addr,
            stop,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop_handle(&self) -> CancellationToken {
        self.stop.clone()
    }

    /// Blocks until the server is stopped.
    pub async fn wait(&self) {
        self.stop.cancelled().await;
    }
}

fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/", get(http::root))
        .route("/health", get(http::health))
        .route("/ready", get(http::ready))
        .route("/metrics", get(http::metrics_handler))
        .route("/token", get(http::token).post(http::token))
        .with_state(state)
}

/// Periodically probes the GitHub API for the readiness endpoint.
async fn probe_loop(state: Arc<ServerState>, stop: CancellationToken) {
    loop {
        match state.github.probe().await {
            Ok(()) => state.probe.record_success(),
            Err(e) => {
                state.metrics.github_api_error("probe");
                state.probe.record_failure();
                warn!(%e, "github reachability probe failed");
            }
        }
        state
            .metrics
            .set_github_reachable(state.probe.last_success_was_ok());
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(state.config.probe_interval) => {}
        }
    }
}

/// Watches issued tokens: renews expiring ones for active clients and drops
/// tokens that expired on GitHub's side.
async fn watch_loop(state: Arc<ServerState>, stop: CancellationToken) {
    let activity_window = chrono::Duration::from_std(state.config.activity_window)
        .unwrap_or_else(|_| chrono::Duration::seconds(0));
    loop {
        let mut changed = false;
        for (client, rec) in state.store.snapshot().await {
            let now = Utc::now();
            if rec.github_expired() {
                state.store.remove(&client).await;
                state.metrics.tokens_expired();
                state.metrics.token_lifetime((now - rec.issued_at).num_seconds() as f64);
                info!(%client, "token expired on GitHub's side; removed from cache");
                changed = true;
                continue;
            }

            let remaining = rec.remaining();
            let active = now - rec.last_used <= activity_window;
            if remaining < state.config.renew_before && active {
                match state.github.create_installation_token().await {
                    Ok(new) => {
                        let mut record =
                            TokenRecord::new(new.token.clone(), rec.ttl, new.expires_at);
                        record.last_used = rec.last_used;
                        state.store.put(&client, record).await;
                        state.metrics.tokens_renewed();
                        state.metrics.token_lifetime((now - rec.issued_at).num_seconds() as f64);
                        info!(
                            %client,
                            token = %mask_token(&new.token),
                            "watcher renewed expiring token"
                        );
                        changed = true;
                    }
                    Err(e) => {
                        state.metrics.github_api_error("watch_renew");
                        warn!(%client, %e, "watcher failed to renew expiring token");
                    }
                }
            }
        }
        if changed {
            state.metrics.set_active_tokens(state.store.len().await);
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(state.config.watch_interval) => {}
        }
    }
}
