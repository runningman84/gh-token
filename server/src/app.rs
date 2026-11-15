//! Server state, startup validation, background loops and serving.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::routing::get;
use chrono::{DateTime, Utc};
use gh_token_common::mask_token;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::github::{GithubClient, GithubError};
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
        self.last_success_nanos
            .store(now_nanos(), Ordering::Relaxed);
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
            DateTime::from_timestamp_nanos(last as i64)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
    }
}

/// The installation token the reachability probe authenticates with.
#[derive(Clone)]
struct ProbeToken {
    token: String,
    expires_at: DateTime<Utc>,
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
    /// Token used by the reachability probe (only touched by the probe
    /// loop). `GET /rate_limit` does not accept app JWTs, and
    /// unauthenticated calls would burn the shared 60/h per-IP budget.
    probe_token: Arc<tokio::sync::Mutex<Option<ProbeToken>>>,
    /// Whether the startup credential validation passed.
    startup_ok: Arc<AtomicBool>,
    /// The last startup-validation error, if any.
    startup_error: Arc<std::sync::Mutex<Option<String>>>,
}

impl ServerState {
    /// Refreshes the GitHub cooldown gauge from the client's backoff state.
    pub fn update_cooldown_metric(&self) {
        self.metrics
            .set_github_cooldown(self.github.cooldown_remaining().map(|d| d.as_secs()));
    }

    /// Runs the GitHub reachability probe.
    ///
    /// Uses a cached installation token, creating a new one when none is
    /// cached or the cached one is about to expire. If creating a fresh
    /// token fails but the cached one is still valid on GitHub's side, the
    /// probe falls back to it.
    pub async fn probe_github(&self) -> Result<(), GithubError> {
        let mut guard = self.probe_token.lock().await;
        let need_new = match &*guard {
            Some(pt) => {
                (pt.expires_at - Utc::now()).num_seconds()
                    <= self.config.renew_before.as_secs() as i64
            }
            None => true,
        };
        let token = if need_new {
            match self.github.create_installation_token().await {
                Ok(new) => {
                    *guard = Some(ProbeToken {
                        token: new.token.clone(),
                        expires_at: new.expires_at,
                    });
                    new.token
                }
                Err(e) => match guard.as_ref() {
                    Some(pt) if pt.expires_at > Utc::now() => pt.token.clone(),
                    _ => return Err(e),
                },
            }
        } else {
            guard.as_ref().expect("probe token exists").token.clone()
        };
        drop(guard);
        self.github.probe(&token).await
    }

    /// Whether the startup credential validation has passed.
    pub fn startup_ok(&self) -> bool {
        self.startup_ok.load(Ordering::Relaxed)
    }

    /// The last startup-validation error, if any.
    pub fn startup_error(&self) -> Option<String> {
        self.startup_error.lock().ok().and_then(|g| g.clone())
    }

    /// Marks the startup credential validation as passed.
    pub fn mark_startup_ok(&self) {
        self.startup_ok.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.startup_error.lock() {
            *g = None;
        }
        self.metrics.set_startup_check_ok(true);
    }

    /// Marks the startup credential validation as failed.
    pub fn mark_startup_failed(&self, err: &str) {
        self.startup_ok.store(false, Ordering::Relaxed);
        if let Ok(mut g) = self.startup_error.lock() {
            *g = Some(err.to_string());
        }
        self.metrics.set_startup_check_ok(false);
    }
}

/// Records a GitHub API error (and, when applicable, a rate-limit response)
/// for one operation.
pub(crate) fn record_gh_error(metrics: &Metrics, operation: &str, err: &GithubError) {
    metrics.github_api_error(operation);
    if err.retry_after().is_some() {
        metrics.github_rate_limited(operation);
    }
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
    pub fn state(config: Config) -> Arc<ServerState> {
        let metrics = Metrics::new();
        Arc::new(ServerState {
            config: config.clone(),
            github: GithubClient::new(
                config.app_id,
                config.installation_id,
                config.app_key,
                config.github_base_url.clone(),
                config.backoff_base,
                config.backoff_max,
                metrics.clone(),
            ),
            store: TokenStore::new(),
            metrics,
            rate_limiter: Arc::new(RateLimiter::new(config.rate_window, config.rate_max)),
            probe: Arc::new(ProbeState::default()),
            probe_token: Arc::new(tokio::sync::Mutex::new(None)),
            startup_ok: Arc::new(AtomicBool::new(false)),
            startup_error: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Validates the GitHub App credentials against the GitHub API.
    ///
    /// This proves the auth level the app has: app identity (`GET /app`),
    /// installation access (`GET /app/installations/{id}`, including
    /// effective permissions and repository selection), token creation
    /// (`POST /app/installations/{id}/access_tokens`, verified with
    /// `GET /installation/repositories`) and the bot account (`GET /users/
    /// {slug}[bot]` — app tokens cannot call `GET /user`).
    pub async fn validate_startup(state: &Arc<ServerState>) -> Result<StartupReport, String> {
        let app = state.github.get_app().await.map_err(|e| {
            record_gh_error(&state.metrics, "startup_get_app", &e);
            format!("GET /app failed (invalid app key?): {e}")
        })?;

        if app.id != state.config.app_id {
            return Err(format!(
                "GH_APP_ID is {} but the provided key authenticates as app id {} (app {:#?})",
                state.config.app_id, app.id, app.name
            ));
        }

        let installation = state.github.get_installation().await.map_err(|e| {
            record_gh_error(&state.metrics, "startup_get_installation", &e);
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

        // Create a throwaway token to prove token creation works.
        // The token simply expires an hour later.
        let test_token = state
            .github
            .create_installation_token()
            .await
            .map_err(|e| {
                record_gh_error(&state.metrics, "startup_create_token", &e);
                format!("creating an installation token failed: {e}")
            })?;
        // Verify the token against an installation-scoped endpoint;
        // `GET /user` is not callable with installation tokens (403).
        if let Err(e) = state
            .github
            .verify_installation_token(&test_token.token)
            .await
        {
            record_gh_error(&state.metrics, "startup_verify_token", &e);
            warn!(%e, "startup check: verifying the new installation token failed (continuing)");
        } else {
            info!("startup check: installation token verified");
        }
        match state.github.bot_login(&app.slug).await {
            Ok(login) => {
                report.bot_login = Some(login.clone());
                info!(bot_login = %login, "startup check: bot login discovered");
            }
            Err(e) => {
                record_gh_error(&state.metrics, "startup_bot_login", &e);
                warn!(%e, "startup check: discovering the bot login failed (continuing)");
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
///
/// Consecutive probe failures back off exponentially (doubled interval,
/// capped) so a failing or rate-limited API is not hammered.
async fn probe_loop(state: Arc<ServerState>, stop: CancellationToken) {
    let base = state.config.probe_interval;
    let max_backoff = (base * 10).min(Duration::from_secs(300));
    let mut failures = 0u32;
    loop {
        match state.probe_github().await {
            Ok(()) => {
                failures = 0;
                state.probe.record_success();
            }
            Err(e) => {
                record_gh_error(&state.metrics, "probe", &e);
                state.probe.record_failure();
                failures = failures.saturating_add(1);
                if let Some(wait) = e.retry_after() {
                    warn!(
                        %e,
                        retry_after_secs = wait.as_secs(),
                        "github rate limited the probe; backing off"
                    );
                } else {
                    warn!(%e, "github reachability probe failed");
                }
            }
        }
        state
            .metrics
            .set_github_reachable(state.probe.last_success_was_ok());
        state.update_cooldown_metric();
        let delay = if failures == 0 {
            base
        } else {
            (base * 2u32.pow(failures.min(6))).min(max_backoff)
        };
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Retries the startup credential validation after a failed initial check.
///
/// The server keeps serving (with `/ready` reporting not-ready) while this
/// runs, so a transient outage at boot — or fixed credentials — recover
/// without a pod restart. Backs off exponentially (30s .. 300s) and stops
/// after the first success.
pub async fn startup_retry_loop(state: Arc<ServerState>, stop: CancellationToken) {
    let base = Duration::from_secs(30);
    let max = Duration::from_secs(300);
    let mut delay = base;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        match Server::validate_startup(&state).await {
            Ok(report) => {
                state.mark_startup_ok();
                info!(
                    app_id = report.app_id,
                    app_name = %report.app_name,
                    installation_id = report.installation_id,
                    bot_login = report.bot_login.as_deref().unwrap_or("<unknown>"),
                    repository_selection = %report.repository_selection,
                    "startup check passed"
                );
                return;
            }
            Err(err) => {
                state.mark_startup_failed(&err);
                warn!(
                    %err,
                    next_retry_secs = delay.as_secs(),
                    "startup check still failing; retrying"
                );
                delay = (delay * 2).min(max);
            }
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
                state
                    .metrics
                    .token_lifetime((now - rec.issued_at).num_seconds() as f64);
                info!(%client, "token expired on GitHub's side; removed from cache");
                changed = true;
                continue;
            }

            let remaining = rec.remaining();
            let active = now - rec.last_used <= activity_window;
            if remaining < state.config.renew_before && active {
                match state.github.create_installation_token().await {
                    Ok(new) => {
                        let mut record = TokenRecord::new(
                            new.token.clone(),
                            rec.ttl,
                            new.expires_at,
                            rec.purpose.clone(),
                        );
                        record.last_used = rec.last_used;
                        state.store.put(&client, record).await;
                        state.metrics.tokens_renewed();
                        state
                            .metrics
                            .token_lifetime((now - rec.issued_at).num_seconds() as f64);
                        info!(
                            %client,
                            token = %mask_token(&new.token),
                            "watcher renewed expiring token"
                        );
                        changed = true;
                    }
                    Err(e) => {
                        record_gh_error(&state.metrics, "watch_renew", &e);
                        if e.retry_after().is_some() {
                            warn!(
                                %client,
                                wait_secs = e.retry_after().map(|w| w.as_secs()).unwrap_or(0),
                                "watcher skipped renewal: github rate limited"
                            );
                        } else {
                            warn!(%client, %e, "watcher failed to renew expiring token");
                        }
                    }
                }
            }
        }
        if changed {
            state.metrics.set_active_tokens(state.store.len().await);
        }
        state.update_cooldown_metric();
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(state.config.watch_interval) => {}
        }
    }
}
