//! Server configuration, loaded from environment variables.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use tracing::info;

pub const DEFAULT_BIND: &str = "0.0.0.0:8080";
pub const DEFAULT_GITHUB_BASE_URL: &str = "https://api.github.com";

pub const DEFAULT_TOKEN_TTL_SECS: u64 = 3600;
pub const MAX_TOKEN_TTL_SECS: u64 = 3600; // GitHub installation tokens live exactly one hour
pub const DEFAULT_MIN_TTL_SECS: u64 = 60;
pub const DEFAULT_RENEW_BEFORE_SECS: u64 = 300;
pub const DEFAULT_ACTIVITY_WINDOW_SECS: u64 = 900;
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 30;
pub const DEFAULT_PROBE_INTERVAL_SECS: u64 = 30;
pub const DEFAULT_READY_MAX_AGE_SECS: u64 = 90;
pub const DEFAULT_RATE_MAX: u64 = 30;
pub const DEFAULT_RATE_WINDOW_SECS: u64 = 60;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// GitHub App ID.
    pub app_id: u64,
    /// GitHub App installation ID.
    pub installation_id: u64,
    /// GitHub App RSA private key (PEM).
    pub app_key: String,
    /// Base URL of the GitHub API (override for tests / GHE).
    pub github_base_url: String,
    /// Address to bind the HTTP listener to.
    pub bind: SocketAddr,
    /// Optional shared API key; when set, token requests must present it.
    pub api_key: Option<String>,
    /// Default effective (server-enforced) TTL for issued tokens in seconds.
    pub token_ttl: u64,
    /// Smallest TTL a client may request.
    pub min_ttl: u64,
    /// Renew tokens with less than this much effective lifetime remaining.
    pub renew_before: Duration,
    /// A client is "active" while it requested a token within this window.
    pub activity_window: Duration,
    /// How often the background watcher inspects issued tokens.
    pub watch_interval: Duration,
    /// How often the readiness probe polls the GitHub API.
    pub probe_interval: Duration,
    /// `/ready` reports ready only if the last probe succeeded within this age.
    pub ready_max_age: Duration,
    /// Max token requests per client per sliding window.
    pub rate_max: u64,
    /// Sliding window for per-client rate limiting.
    pub rate_window: Duration,
    /// Log format: "pretty" or "json".
    pub log_json: bool,
}

impl Config {
    /// Reads configuration from the environment, validating required values.
    pub fn from_env() -> anyhow::Result<Self> {
        let app_id = parse_u64_env("GH_APP_ID")?;
        let installation_id = parse_u64_env("GH_APP_INSTALLATION_ID")?;
        let app_key = decode_app_key(&env::var("GH_APP_KEY_B64")?)?;

        let bind = env::var("GHTS_BIND")
            .unwrap_or_else(|_| DEFAULT_BIND.to_string())
            .parse::<SocketAddr>()
            .map_err(|e| anyhow::anyhow!("GHTS_BIND: invalid address: {e}"))?;

        let token_ttl = env::var("GHTS_TOKEN_TTL")
            .ok()
            .map(|v| parse_u64(&v, "GHTS_TOKEN_TTL"))
            .transpose()?
            .unwrap_or(DEFAULT_TOKEN_TTL_SECS)
            .min(MAX_TOKEN_TTL_SECS);

        let min_ttl = env::var("GHTS_MIN_TTL")
            .ok()
            .map(|v| parse_u64(&v, "GHTS_MIN_TTL"))
            .transpose()?
            .unwrap_or(DEFAULT_MIN_TTL_SECS)
            .min(MAX_TOKEN_TTL_SECS);

        let renew_before = parse_duration_env("GHTS_RENEW_BEFORE", DEFAULT_RENEW_BEFORE_SECS)?;
        let activity_window = parse_duration_env("GHTS_ACTIVITY_WINDOW", DEFAULT_ACTIVITY_WINDOW_SECS)?;
        let watch_interval = parse_duration_env("GHTS_WATCH_INTERVAL", DEFAULT_WATCH_INTERVAL_SECS)?;
        let probe_interval = parse_duration_env("GHTS_PROBE_INTERVAL", DEFAULT_PROBE_INTERVAL_SECS)?;
        let ready_max_age = parse_duration_env("GHTS_READY_MAX_AGE", DEFAULT_READY_MAX_AGE_SECS)?;

        let rate_max = env::var("GHTS_RATE_MAX")
            .ok()
            .map(|v| parse_u64(&v, "GHTS_RATE_MAX"))
            .transpose()?
            .unwrap_or(DEFAULT_RATE_MAX)
            .max(1);
        let rate_window = parse_duration_env("GHTS_RATE_WINDOW", DEFAULT_RATE_WINDOW_SECS)?;

        if min_ttl > token_ttl {
            anyhow::bail!("GHTS_MIN_TTL ({min_ttl}) must not exceed GHTS_TOKEN_TTL ({token_ttl})");
        }

        Ok(Self {
            app_id,
            installation_id,
            app_key,
            github_base_url: env::var("GHTS_GITHUB_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_GITHUB_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            bind,
            api_key: env::var("GHTS_API_KEY").ok().filter(|k| !k.is_empty()),
            token_ttl,
            min_ttl,
            renew_before,
            activity_window,
            watch_interval,
            probe_interval,
            ready_max_age,
            rate_max,
            rate_window,
            log_json: env::var("GHTS_LOG_JSON").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        })
    }
}

fn parse_u64(value: &str, name: &str) -> anyhow::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("{name}: invalid value {value:?}: {e}"))
}

fn parse_u64_env(name: &str) -> anyhow::Result<u64> {
    let raw = env::var(name)
        .map_err(|_| anyhow::anyhow!("{name} is not set (GitHub App credential)"))?;
    parse_u64(&raw, name)
}

/// Decodes the app key: accepts base64 (optionally with PEM line breaks) or a
/// raw PEM string.
fn decode_app_key(value: &str) -> anyhow::Result<String> {
    let trimmed = value.trim();
    if trimmed.starts_with("-----BEGIN") {
        return Ok(trimmed.to_string());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(trimmed.replace(['\n', '\r', ' '], ""))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed))
        .map_err(|e| anyhow::anyhow!("GH_APP_KEY_B64: invalid base64: {e}"))?;
    let pem = String::from_utf8(decoded)
        .map_err(|e| anyhow::anyhow!("GH_APP_KEY_B64: key is not valid UTF-8: {e}"))?;
    if !pem.contains("PRIVATE KEY") {
        anyhow::bail!("GH_APP_KEY_B64: decoded key does not look like a PEM private key");
    }
    Ok(pem)
}

fn parse_duration_env(name: &str, default_secs: u64) -> anyhow::Result<Duration> {
    let raw = match env::var(name) {
        Ok(v) => v,
        Err(_) => return Ok(Duration::from_secs(default_secs)),
    };
    let secs: u64 = raw.parse().map_err(|e| {
        anyhow::anyhow!("{name}: expected a number of seconds, got {raw:?}: {e}")
    })?;
    Ok(Duration::from_secs(secs))
}

/// Initializes tracing based on `GHTS_LOG` / `RUST_LOG` and `GHTS_LOG_JSON`.
pub fn init_logging() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            let log = std::env::var("GHTS_LOG").unwrap_or_else(|_| "info".to_string());
            tracing_subscriber::EnvFilter::new(log)
        });

    let json = std::env::var("GHTS_LOG_JSON")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .init();
    }

    info!("logging initialized");
}
