//! axum HTTP handlers.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{SecondsFormat, Utc};
use gh_token_common::{TokenResponse, is_valid_client_id, mask_token, validate_purpose};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::app::{ServerState, record_gh_error};
use crate::store::TokenRecord;

/// Returns a JSON error response and records metrics for it.
fn finish(
    state: &Arc<ServerState>,
    client: &str,
    status: &str,
    started: Instant,
    resp: Response,
) -> Response {
    state.metrics.token_request(client, status);
    state
        .metrics
        .token_request_duration(client, started.elapsed().as_secs_f64());
    resp
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// `GET /` — service info.
pub async fn root(State(state): State<Arc<ServerState>>) -> Response {
    Json(serde_json::json!({
        "service": "gh-token",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "token": "GET/POST /token?client=<id>&refresh=<bool>&ttl=<secs>&purpose=<text>",
            "health": "GET /health",
            "ready": "GET /ready",
            "metrics": "GET /metrics",
        },
        "app_id": state.config.app_id,
        "installation_id": state.config.installation_id,
    }))
    .into_response()
}

/// `GET /health` — liveness (always 200 once the process is up).
pub async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

/// `GET /ready` — readiness.
///
/// 200 only if the startup credential validation passed AND the GitHub
/// reachability probe succeeded on its most recent attempt (within
/// `ready_max_age`); otherwise 503 with the failing condition.
pub async fn ready(State(state): State<Arc<ServerState>>) -> Response {
    let startup_ok = state.startup_ok();
    let startup_error = state.startup_error();
    let probe_ok =
        state.probe.is_fresh(state.config.ready_max_age) && state.probe.last_success_was_ok();
    let last = state.probe.last_success_age();
    let ready = startup_ok && probe_ok;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({
            "status": if ready { "ready" } else { "not_ready" },
            "startup_check": if startup_ok { "passed" } else { "failed" },
            "startup_error": startup_error,
            "github_reachable": state.probe.last_success_was_ok(),
            "last_probe_success": state.probe.last_success_rfc3339(),
            "last_probe_success_age_seconds": last.map(|d| d.as_secs()),
        })),
    )
        .into_response()
}

/// `GET /metrics` — Prometheus metrics.
pub async fn metrics_handler(State(state): State<Arc<ServerState>>) -> Response {
    state.metrics.update_process_metrics();
    let body = state.metrics.encode();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct TokenQuery {
    pub client: Option<String>,
    /// Force a fresh token from GitHub even if a valid cached one exists.
    pub refresh: Option<bool>,
    /// Requested effective TTL in seconds (clamped to [GHTS_MIN_TTL, GHTS_TOKEN_TTL]).
    pub ttl: Option<u64>,
    /// Free-form purpose for this token (advisory metadata, max 256 chars).
    pub purpose: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TokenBody {
    pub client: Option<String>,
    pub refresh: Option<bool>,
    pub ttl: Option<u64>,
    /// Free-form purpose for this token (advisory metadata, max 256 chars).
    pub purpose: Option<String>,
}

/// `GET|POST /token` — issue (or return the cached) installation token.
pub async fn token(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    body: Option<Json<TokenBody>>,
) -> Response {
    let started = Instant::now();
    let body = body.map(|Json(b)| b).unwrap_or_default();

    let client = body
        .client
        .clone()
        .or(query.client.clone())
        .unwrap_or_default();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    if !is_valid_client_id(&client) {
        warn!(client = %client, client_ip = %addr, user_agent = %user_agent, "token request rejected: invalid client identifier");
        return finish(
            &state,
            &client,
            "400",
            started,
            json_error(
                StatusCode::BAD_REQUEST,
                format!("invalid client identifier {client:?} (expected 1-64 chars of [a-z0-9_-])"),
            ),
        );
    }

    // Optional free-form purpose (advisory metadata; returned in the
    // response and included in issue/renewal logs).
    let purpose = body
        .purpose
        .clone()
        .or(query.purpose.clone())
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    if let Some(p) = &purpose
        && let Err(e) = validate_purpose(p)
    {
        warn!(client, %e, "token request rejected: invalid purpose");
        return finish(
            &state,
            &client,
            "400",
            started,
            json_error(StatusCode::BAD_REQUEST, e),
        );
    }

    if let Some(expected) = &state.config.api_key {
        match extract_api_key(&headers) {
            Some(provided) if constant_time_eq(provided.as_bytes(), expected.as_bytes()) => {}
            Some(_) => {
                warn!(client, client_ip = %addr, user_agent = %user_agent, "token request rejected: invalid API key");
                return finish(
                    &state,
                    &client,
                    "401",
                    started,
                    json_error(StatusCode::UNAUTHORIZED, "invalid API key"),
                );
            }
            None => {
                warn!(client, client_ip = %addr, user_agent = %user_agent, "token request rejected: missing API key");
                return finish(
                    &state,
                    &client,
                    "401",
                    started,
                    json_error(
                        StatusCode::UNAUTHORIZED,
                        "missing API key (send X-GH-Token-Key or Authorization: Bearer <key>)",
                    ),
                );
            }
        }
    }

    // Per-client key, when one is configured for this client (the key is
    // sent via the same X-GH-Token-Key / Authorization: Bearer header).
    if let Some(expected) = state.config.client_keys.get(&client) {
        match extract_api_key(&headers) {
            Some(provided) if constant_time_eq(provided.as_bytes(), expected.as_bytes()) => {}
            Some(_) => {
                warn!(client, client_ip = %addr, user_agent = %user_agent, "token request rejected: invalid client key");
                return finish(
                    &state,
                    &client,
                    "401",
                    started,
                    json_error(StatusCode::UNAUTHORIZED, "invalid API key for this client"),
                );
            }
            None => {
                warn!(client, client_ip = %addr, user_agent = %user_agent, "token request rejected: missing client key");
                return finish(
                    &state,
                    &client,
                    "401",
                    started,
                    json_error(
                        StatusCode::UNAUTHORIZED,
                        "missing API key for this client (send X-GH-Token-Key or Authorization: Bearer <key>)",
                    ),
                );
            }
        }
    }

    if let Some(retry_after) = state.rate_limiter.allow(&client) {
        state.metrics.rate_limit_rejected();
        warn!(
            client,
            client_ip = %addr,
            user_agent = %user_agent,
            retry_after_secs = retry_after.as_secs(),
            "token request rejected: rate limit exceeded"
        );
        let mut resp = json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded; retry later",
        );
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            retry_after
                .as_secs()
                .to_string()
                .parse()
                .expect("valid header value"),
        );
        return finish(&state, &client, "429", started, resp);
    }

    let requested_ttl = body
        .ttl
        .or(query.ttl)
        .unwrap_or(state.config.token_ttl)
        .clamp(state.config.min_ttl, state.config.token_ttl);
    let refresh = body.refresh.unwrap_or(false) || query.refresh.unwrap_or(false);

    // Look up any previous token (used both for serving from cache and to
    // determine whether the new token is a renewal).
    let previous: Option<TokenRecord> = state.store.get(&client).await;

    // A cached token is served when it covers the requested TTL and is fresh
    // enough. By default the purpose is advisory audit metadata and does not
    // affect caching; with GHTS_PURPOSE_KEY=true the purpose becomes part of
    // the cache key, so a request whose purpose differs from the cached
    // record's issues a fresh token.
    let cache_hit = !refresh
        && previous.as_ref().is_some_and(|rec| {
            rec.remaining() > state.config.renew_before
                && rec.ttl >= requested_ttl
                && (!state.config.purpose_key || rec.purpose == purpose)
        });
    if let Some(rec) = previous.as_ref().filter(|_| cache_hit) {
        let mut served = rec.clone();
        served.last_used = Utc::now();
        // Keep the stored purpose current for auditing (advisory mode only;
        // with purpose_key a hit already implies equal purposes).
        if served.purpose != purpose {
            served.purpose = purpose.clone();
        }
        state.store.put(&client, served.clone()).await;
        state.metrics.tokens_served_cached();
        debug!(client, client_ip = %addr, user_agent = %user_agent, token = %mask_token(&served.token), "serving cached token");
        return finish(
            &state,
            &client,
            "200",
            started,
            Json(token_response(&served, &client, true, false)).into_response(),
        );
    }

    match state.github.create_installation_token().await {
        Ok(new) => {
            let renewed = previous.is_some();
            let mut record = TokenRecord::new(
                new.token.clone(),
                requested_ttl,
                new.expires_at,
                purpose.clone(),
            );
            if let Some(old) = &previous {
                record.last_used = old.last_used;
            }
            state.store.put(&client, record.clone()).await;
            state.metrics.tokens_issued();
            if renewed {
                state.metrics.tokens_renewed();
            }
            state.metrics.set_active_tokens(state.store.len().await);
            info!(
                client,
                client_ip = %addr,
                user_agent = %user_agent,
                token = %mask_token(&new.token),
                ttl_secs = requested_ttl,
                purpose = ?purpose,
                "issued new installation token"
            );
            finish(
                &state,
                &client,
                "200",
                started,
                Json(token_response(&record, &client, false, renewed)).into_response(),
            )
        }
        Err(e) => {
            record_gh_error(&state.metrics, "create_token", &e);
            state.update_cooldown_metric();
            if let Some(wait) = e.retry_after() {
                warn!(client, client_ip = %addr, user_agent = %user_agent, wait_secs = wait.as_secs(), "token creation deferred: github rate limited");
                let mut resp = json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "github is rate limiting; please retry later",
                );
                resp.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    wait.as_secs()
                        .to_string()
                        .parse()
                        .expect("valid header value"),
                );
                return finish(&state, &client, "503", started, resp);
            }
            warn!(client, client_ip = %addr, user_agent = %user_agent, %e, "failed to create installation token from GitHub");
            finish(
                &state,
                &client,
                "502",
                started,
                json_error(
                    StatusCode::BAD_GATEWAY,
                    format!("github token creation failed: {e}"),
                ),
            )
        }
    }
}

fn token_response(rec: &TokenRecord, client: &str, cached: bool, renewed: bool) -> TokenResponse {
    TokenResponse {
        token: rec.token.clone(),
        client: client.to_string(),
        expires_at: rec
            .soft_expires_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        ttl: rec.remaining().as_secs(),
        github_expires_at: rec
            .github_expires_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        cached,
        renewed,
        purpose: rec.purpose.clone(),
    }
}

/// Extracts the API key from `X-GH-Token-Key` or `Authorization: Bearer <key>`.
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-gh-token-key") {
        return v.to_str().ok().map(str::to_string);
    }
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        let v = v.to_str().ok()?;
        if let Some(rest) = v.strip_prefix("Bearer ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Constant-time string comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
