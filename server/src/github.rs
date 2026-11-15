//! Minimal GitHub App API client (app JWTs, app/installation info,
//! installation access tokens, rate-limit detection and backoff,
//! rate-limit probe).
//!
//! Every request is logged (method, path, auth kind, status, duration,
//! truncated body) with any GitHub tokens masked. When GitHub signals a
//! rate limit (429, or 403 with rate-limit markers), the client enters an
//! exponential-backoff cooldown during which authenticated requests are
//! suppressed instead of hammering the API.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use thiserror::Error;
use tracing::{info, warn};

use crate::metrics::Metrics;
use gh_token_common::mask_gh_tokens;

/// Errors from the GitHub API.
#[derive(Debug, Error)]
#[error("github api error: {0}")]
pub struct GithubError(String, Option<Duration>);

impl GithubError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into(), None)
    }

    /// Error caused by GitHub rate limiting; `wait` is the recommended pause.
    pub fn rate_limited(msg: impl Into<String>, wait: Duration) -> Self {
        Self(msg.into(), Some(wait))
    }

    /// `Some(wait)` when this error is due to GitHub rate limiting.
    pub fn retry_after(&self) -> Option<Duration> {
        self.1
    }
}

/// Global rate-limit state shared by all operations of one client.
///
/// While `cooldown_until` is in the future, authenticated GitHub requests
/// are suppressed so we do not keep hammering an API that told us to slow
/// down. `consecutive` counts consecutive rate-limit responses and drives
/// the exponential backoff.
#[derive(Debug, Default)]
pub struct RateLimitState {
    cooldown_until: AtomicU64,
    consecutive: AtomicU64,
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Information about the GitHub App itself (`GET /app`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// App slug; the bot account is named `{slug}[bot]`.
    #[serde(default)]
    pub slug: String,
    /// App-level permission declarations.
    #[serde(default)]
    pub permissions: serde_json::Value,
}

/// Information about an installation (`GET /app/installations/{id}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallationInfo {
    pub id: u64,
    /// Effective installation permissions, e.g. `{"contents":"write"}`.
    #[serde(default)]
    pub permissions: serde_json::Value,
    /// `"all"` or `"selected"`.
    #[serde(default)]
    pub repository_selection: String,
}

/// An installation access token (`POST /app/installations/{id}/access_tokens`).
///
/// GitHub tokens always live exactly one hour; the `expires_at` is what
/// GitHub reports.
#[derive(Debug, Clone)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Rate limit information for a single resource (from `GET /rate_limit`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RateLimitResource {
    limit: u64,
    used: u64,
    remaining: u64,
    reset: u64,
}

/// Full rate limit response from `GET /rate_limit`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RateLimitResponse {
    resources: std::collections::HashMap<String, RateLimitResource>,
}

/// How a request authenticates (only used for logging, never logged raw).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Auth {
    /// Unauthenticated.
    None,
    /// App JWT.
    Jwt,
    /// Installation token.
    Token,
}

impl Auth {
    fn label(self) -> &'static str {
        match self {
            Auth::None => "none",
            Auth::Jwt => "app-jwt",
            Auth::Token => "installation-token",
        }
    }
}

/// Raw response from [`GithubClient::exchange`].
struct GithubResponse {
    status: StatusCode,
    body: String,
    /// `Some(wait)` when GitHub signaled a rate limit.
    rate_limit: Option<Duration>,
}

/// Body markers that identify rate-limit responses (case-insensitive).
const RATE_LIMIT_BODY_MARKERS: [&str; 4] =
    ["rate limit", "rate_limit", "abuse detection", "spammy"];

/// A thin GitHub App API client.
#[derive(Debug, Clone)]
pub struct GithubClient {
    http: reqwest::Client,
    metrics: Arc<Metrics>,
    app_id: u64,
    installation_id: u64,
    app_key: String,
    base_url: String,
    backoff_base: Duration,
    backoff_max: Duration,
    rate: Arc<RateLimitState>,
}

/// Normalizes a request path for use as a low-cardinality metric label:
/// digit runs of 4+ characters (ids) are replaced with `{id}`.
fn path_label(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while let Some(c) = path[i..].chars().next() {
        let len = c.len_utf8();
        if c.is_ascii_digit() {
            let start = i;
            let mut next = path[i..].chars().next();
            while let Some(n) = next {
                if !n.is_ascii_digit() {
                    break;
                }
                i += n.len_utf8();
                next = path[i..].chars().next();
            }
            if i - start >= 4 {
                out.push_str("{id}");
            } else {
                out.push_str(&path[start..i]);
            }
        } else {
            out.push(c);
            i += len;
        }
    }
    out
}

impl GithubClient {
    pub fn new(
        app_id: u64,
        installation_id: u64,
        app_key: String,
        base_url: String,
        backoff_base: Duration,
        backoff_max: Duration,
        metrics: Arc<Metrics>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            // GitHub's edge closes idle keep-alive connections after about
            // 30s (measured: alive at 25s idle, dead at 35s). The default
            // pool idle timeout is 90s, so pooled connections would be
            // reused after the peer already closed them ("connection
            // closed before message completed"). Expire pool entries well
            // before GitHub does.
            .pool_idle_timeout(Duration::from_secs(15))
            .user_agent("gh-token")
            .build()
            .expect("valid reqwest client");
        Self {
            http,
            metrics,
            app_id,
            installation_id,
            app_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            backoff_base,
            backoff_max,
            rate: Arc::new(RateLimitState::default()),
        }
    }

    /// Remaining time in the rate-limit cooldown, if any.
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let until = self.rate.cooldown_until.load(Ordering::Relaxed);
        let now = now_nanos();
        (until > now).then(|| Duration::from_nanos(until - now))
    }

    /// True while authenticated requests are suppressed by the cooldown.
    pub fn in_cooldown(&self) -> bool {
        self.cooldown_remaining().is_some()
    }

    /// Records a rate-limit response and returns the wait to apply.
    ///
    /// Exponential backoff (`backoff_base * 2^consecutive`, capped at
    /// `backoff_max`), honoring a longer wait suggested by GitHub.
    pub fn apply_rate_limit(&self, suggested: Option<Duration>) -> Duration {
        let n = self.rate.consecutive.fetch_add(1, Ordering::Relaxed);
        let exp = self
            .backoff_base
            .saturating_mul(1u32 << n.min(31))
            .min(self.backoff_max);
        let wait = suggested
            .map(|s| s.min(self.backoff_max))
            .unwrap_or_default()
            .max(exp);
        self.rate.cooldown_until.store(
            now_nanos().saturating_add(wait.as_nanos() as u64),
            Ordering::Relaxed,
        );
        wait
    }

    /// Records a successful response and resets the backoff state.
    pub fn record_success(&self) {
        self.rate.consecutive.store(0, Ordering::Relaxed);
        self.rate.cooldown_until.store(0, Ordering::Relaxed);
    }

    /// Creates an RS256 JWT authenticating this GitHub App.
    ///
    /// - `iss` = app id
    /// - `iat` = now - 60s (clock skew margin)
    /// - `exp` = iat + 9m (GitHub allows up to 10 minutes)
    pub fn app_jwt(&self) -> Result<String, GithubError> {
        let iat = Utc::now() - chrono::Duration::seconds(60);
        let exp = iat + chrono::Duration::minutes(9);
        let claims = serde_json::json!({
            "iss": self.app_id,
            "iat": iat.timestamp(),
            "exp": exp.timestamp(),
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.app_key.as_bytes())
            .map_err(|e| GithubError::new(format!("invalid RSA app key: {e}")))?;
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| GithubError::new(format!("failed to sign app JWT: {e}")))
    }

    /// Sends one request, logs it (with tokens masked) and detects
    /// rate-limit responses. Transport failures and rate limits are errors;
    /// HTTP error statuses are returned for the caller to interpret.
    async fn exchange(
        &self,
        method: &str,
        path: &str,
        auth: Auth,
        secret: Option<&str>,
    ) -> Result<GithubResponse, GithubError> {
        let url = format!("{}{path}", self.base_url);
        let started = Instant::now();
        let mut req = match method {
            "POST" => self.http.post(&url),
            _ => self.http.get(&url),
        };
        if let Some(secret) = secret {
            req = req.bearer_auth(secret);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // Log the whole error chain (source included) for diagnosis.
                let mut chain = String::new();
                let mut err: &dyn std::error::Error = &e;
                loop {
                    if !chain.is_empty() {
                        chain.push_str(": ");
                    }
                    chain.push_str(&err.to_string());
                    err = match err.source() {
                        Some(s) => s,
                        None => break,
                    };
                }
                warn!(
                    path,
                    auth = auth.label(),
                    error = %e,
                    source = %chain,
                    "github api request failed"
                );
                self.metrics.github_request(
                    &path_label(path),
                    "err",
                    started.elapsed().as_secs_f64(),
                );
                return Err(GithubError::new(format!(
                    "{method} {path} request failed: {chain}"
                )));
            }
        };
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        let rate_limit = detect_rate_limit(status, &headers, &body);
        let elapsed = started.elapsed().as_secs_f64();
        info!(
            path,
            auth = auth.label(),
            status = status.as_u16(),
            duration_ms = (elapsed * 1000.0) as u64,
            rate_limited = rate_limit.is_some(),
            body = %mask_gh_tokens(truncate(&body, 300)),
            "github api request"
        );
        self.metrics
            .github_request(&path_label(path), &status.as_u16().to_string(), elapsed);
        Ok(GithubResponse {
            status,
            body,
            rate_limit,
        })
    }

    /// Applies the global cooldown on rate-limit responses and resets the
    /// backoff state on success. Returns a rate-limit error when applicable.
    fn settle(&self, resp: &GithubResponse) -> Result<(), GithubError> {
        if let Some(suggested) = resp.rate_limit {
            let wait = self.apply_rate_limit(Some(suggested));
            return Err(GithubError::rate_limited(
                format!("github rate limited the request (wait ~{wait:?})"),
                wait,
            ));
        }
        if resp.status.is_success() {
            self.record_success();
        }
        Ok(())
    }

    fn parse_json<T: for<'de> Deserialize<'de>>(
        method: &str,
        path: &str,
        resp: &GithubResponse,
    ) -> Result<T, GithubError> {
        if !resp.status.is_success() {
            return Err(GithubError::new(format!(
                "{method} {path} returned {}: {}",
                resp.status,
                mask_gh_tokens(truncate(&resp.body, 300))
            )));
        }
        serde_json::from_str(&resp.body)
            .map_err(|e| GithubError::new(format!("invalid JSON from {method} {path}: {e}")))
    }

    /// `GET /app` — validates the app key and returns app metadata.
    pub async fn get_app(&self) -> Result<AppInfo, GithubError> {
        let jwt = self.app_jwt()?;
        let resp = self.exchange("GET", "/app", Auth::Jwt, Some(&jwt)).await?;
        self.settle(&resp)?;
        Self::parse_json::<AppInfo>("GET", "/app", &resp)
    }

    /// `GET /app/installations/{id}` — validates the installation id.
    pub async fn get_installation(&self) -> Result<InstallationInfo, GithubError> {
        let jwt = self.app_jwt()?;
        let path = format!("/app/installations/{}", self.installation_id);
        let resp = self.exchange("GET", &path, Auth::Jwt, Some(&jwt)).await?;
        self.settle(&resp)?;
        Self::parse_json::<InstallationInfo>("GET", &path, &resp)
    }

    /// `POST /app/installations/{id}/access_tokens` — creates a new
    /// installation token (valid for one hour, full installation permissions).
    ///
    /// While the rate-limit cooldown is active this returns an error
    /// immediately without sending a request.
    pub async fn create_installation_token(&self) -> Result<InstallationToken, GithubError> {
        if let Some(wait) = self.cooldown_remaining() {
            return Err(GithubError::rate_limited(
                format!(
                    "github rate-limit cooldown active (retry in ~{}s)",
                    wait.as_secs()
                ),
                wait,
            ));
        }
        let jwt = self.app_jwt()?;
        let path = format!("/app/installations/{}/access_tokens", self.installation_id);
        let resp = self.exchange("POST", &path, Auth::Jwt, Some(&jwt)).await?;
        self.settle(&resp)?;
        let body = Self::parse_json::<serde_json::Value>("POST", &path, &resp)?;
        let token = body["token"]
            .as_str()
            .ok_or_else(|| GithubError::new("access_tokens response missing `token`"))?
            .to_string();
        let expires_at_raw = body["expires_at"]
            .as_str()
            .ok_or_else(|| GithubError::new("access_tokens response missing `expires_at`"))?;
        let expires_at = DateTime::parse_from_rfc3339(expires_at_raw)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| {
                GithubError::new(format!(
                    "access_tokens: unparseable expires_at {expires_at_raw:?}: {e}"
                ))
            })?;
        Ok(InstallationToken { token, expires_at })
    }

    /// `GET /installation/repositories` with an installation token — proves
    /// the token can authenticate.
    ///
    /// (Installation tokens cannot call `GET /user`; GitHub answers 403
    /// "Resource not accessible by integration".)
    pub async fn verify_installation_token(&self, token: &str) -> Result<(), GithubError> {
        let path = "/installation/repositories";
        let resp = self.exchange("GET", path, Auth::Token, Some(token)).await?;
        self.settle(&resp)?;
        let body = Self::parse_json::<serde_json::Value>("GET", path, &resp)?;
        if !body
            .get("repositories")
            .map(serde_json::Value::is_array)
            .unwrap_or(false)
        {
            return Err(GithubError::new(
                "GET /installation/repositories response missing `repositories` array",
            ));
        }
        Ok(())
    }

    /// `GET /users/{slug}[bot]` — looks up the app's bot account via the
    /// public users API (GitHub app tokens cannot call `GET /user` at all:
    /// 401 for app JWTs, 403 for installation tokens).
    pub async fn bot_login(&self, slug: &str) -> Result<String, GithubError> {
        let path = format!("/users/{}%5Bbot%5D", slug);
        let resp = self.exchange("GET", &path, Auth::None, None).await?;
        self.settle(&resp)?;
        let user = Self::parse_json::<serde_json::Value>("GET", &path, &resp)?;
        user["login"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| GithubError::new(format!("GET {path} response missing `login`")))
    }

    /// `GET /rate_limit` with an installation token — reachability probe.
    ///
    /// The probe is authenticated so it uses the installation's rate-limit
    /// budget (thousands of requests/h) instead of the shared 60/h
    /// unauthenticated per-IP budget. (`GET /rate_limit` does not accept
    /// app JWTs — 401.) While the rate-limit cooldown is active the probe
    /// is suppressed, like other authenticated requests; a rate-limited
    /// response sets the global cooldown.
    pub async fn probe(&self, token: &str) -> Result<(), GithubError> {
        if let Some(wait) = self.cooldown_remaining() {
            return Err(GithubError::rate_limited(
                format!(
                    "probe suppressed: github rate-limit cooldown active (retry in ~{}s)",
                    wait.as_secs()
                ),
                wait,
            ));
        }
        let resp = self
            .exchange("GET", "/rate_limit", Auth::Token, Some(token))
            .await?;
        if let Some(suggested) = resp.rate_limit {
            let wait = self.apply_rate_limit(Some(suggested));
            return Err(GithubError::rate_limited(
                format!("probe returned {} (rate limited)", resp.status),
                wait,
            ));
        }
        if resp.status.is_success() {
            self.record_success();
            // Parse and expose rate limit metrics
            if let Ok(rate_limit) = serde_json::from_str::<RateLimitResponse>(&resp.body) {
                for (resource, rl) in rate_limit.resources {
                    self.metrics.github_rate_limit(
                        &resource,
                        rl.limit as i64,
                        rl.used as i64,
                        rl.remaining as i64,
                        rl.reset as i64,
                    );
                }
            }
            Ok(())
        } else {
            Err(GithubError::new(format!(
                "probe returned {}: {}",
                resp.status,
                mask_gh_tokens(truncate(&resp.body, 300))
            )))
        }
    }
}

/// Detects GitHub rate-limit responses and derives the suggested wait.
///
/// GitHub signals limits via 429, or 403 with `X-RateLimit-Remaining: 0`
/// (primary limit) or an abuse/secondary-limit message. The suggested wait
/// comes from `Retry-After` (seconds or HTTP-date) or `X-RateLimit-Reset`
/// (unix seconds); without hints a 60s default is used.
fn detect_rate_limit(status: StatusCode, headers: &HeaderMap, body: &str) -> Option<Duration> {
    let lower = body.to_lowercase();
    let marker = RATE_LIMIT_BODY_MARKERS.iter().any(|m| lower.contains(m));
    let remaining_zero = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|v| matches!(v.to_str(), Ok("0")));
    let limited = match status {
        StatusCode::TOO_MANY_REQUESTS => true,
        StatusCode::FORBIDDEN => marker || remaining_zero,
        _ => false,
    };
    if !limited {
        return None;
    }
    if let Some(wait) = suggested_from_retry_after(headers) {
        return Some(wait);
    }
    suggested_from_reset_header(headers).or_else(|| Some(Duration::from_secs(60)))
}

fn suggested_from_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date format.
    if let Ok(dt) = DateTime::parse_from_rfc2822(value) {
        let now = Utc::now();
        let until = dt.with_timezone(&Utc);
        if until > now {
            return Some((until - now).to_std().unwrap_or(Duration::ZERO));
        }
    }
    None
}

fn suggested_from_reset_header(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get("x-ratelimit-reset")?.to_str().ok()?;
    let ts: i64 = value.trim().parse().ok()?;
    let now = Utc::now().timestamp();
    (ts > now).then(|| Duration::from_secs((ts - now) as u64))
}

fn truncate(s: &str, max: usize) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() <= max {
        return s;
    }
    match std::str::from_utf8(&bytes[..max]) {
        Ok(t) => t,
        Err(e) => &s[..e.valid_up_to()],
    }
}

#[cfg(test)]
mod tests {
    use super::path_label;

    #[test]
    fn path_label_replaces_long_ids() {
        assert_eq!(path_label("/app"), "/app");
        assert_eq!(path_label("/rate_limit"), "/rate_limit");
        assert_eq!(
            path_label("/app/installations/181623044"),
            "/app/installations/{id}"
        );
        assert_eq!(
            path_label("/app/installations/181623044/access_tokens"),
            "/app/installations/{id}/access_tokens"
        );
        // Short digit runs (e.g. in a slug) are kept, ids are not.
        assert_eq!(
            path_label("/users/hermes-agent-runningman84%5Bbot%5D"),
            "/users/hermes-agent-runningman84%5Bbot%5D"
        );
        assert_eq!(
            path_label("/repos/{owner}/{repo}/contents/12345"),
            "/repos/{owner}/{repo}/contents/{id}"
        );
    }
}
