//! Server configuration, loaded from environment variables.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use gh_token_common::validate_client_id;
use std::collections::HashMap;
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
pub const DEFAULT_BACKOFF_BASE_SECS: u64 = 2;
pub const DEFAULT_BACKOFF_MAX_SECS: u64 = 300;

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
    /// Optional per-client keys (from `GHTS_CLIENT_KEYS`, a JSON object of
    /// `client -> key`). Requests for clients present in this map must send
    /// the matching key via `X-GH-Token-Key` / `Authorization: Bearer`;
    /// clients absent from the map are unaffected.
    pub client_keys: HashMap<String, String>,
    /// When true, the token purpose is part of the cache key: a request
    /// whose purpose differs from the cached record's issues a fresh token
    /// (off by default: the purpose is advisory audit metadata only).
    pub purpose_key: bool,
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
    /// Base delay for the GitHub rate-limit exponential backoff.
    pub backoff_base: Duration,
    /// Maximum delay for the GitHub rate-limit backoff cooldown.
    pub backoff_max: Duration,
    /// Log format: "pretty" or "json".
    pub log_json: bool,
}

impl Config {
    /// Reads configuration from the environment, validating required values.
    pub fn from_env() -> anyhow::Result<Self> {
        let app_id = parse_u64_env("GH_APP_ID")?;
        let installation_id = parse_u64_env("GH_APP_INSTALLATION_ID")?;
        let raw_app_key = env::var("GH_APP_KEY_B64")
            .map_err(|_| anyhow::anyhow!("GH_APP_KEY_B64 is not set (GitHub App credential)"))?;
        let app_key = decode_app_key(&raw_app_key)?;

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
        let activity_window =
            parse_duration_env("GHTS_ACTIVITY_WINDOW", DEFAULT_ACTIVITY_WINDOW_SECS)?;
        let watch_interval =
            parse_duration_env("GHTS_WATCH_INTERVAL", DEFAULT_WATCH_INTERVAL_SECS)?;
        let probe_interval =
            parse_duration_env("GHTS_PROBE_INTERVAL", DEFAULT_PROBE_INTERVAL_SECS)?;
        let ready_max_age = parse_duration_env("GHTS_READY_MAX_AGE", DEFAULT_READY_MAX_AGE_SECS)?;

        let rate_max = env::var("GHTS_RATE_MAX")
            .ok()
            .map(|v| parse_u64(&v, "GHTS_RATE_MAX"))
            .transpose()?
            .unwrap_or(DEFAULT_RATE_MAX)
            .max(1);
        let rate_window = parse_duration_env("GHTS_RATE_WINDOW", DEFAULT_RATE_WINDOW_SECS)?;

        let backoff_base = parse_duration_env("GHTS_BACKOFF_BASE_SECS", DEFAULT_BACKOFF_BASE_SECS)?;
        let backoff_max = parse_duration_env("GHTS_BACKOFF_MAX_SECS", DEFAULT_BACKOFF_MAX_SECS)?;

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
            client_keys: match env::var("GHTS_CLIENT_KEYS")
                .ok()
                .filter(|k| !k.trim().is_empty())
            {
                Some(raw) => parse_client_keys(&raw)?,
                None => HashMap::new(),
            },
            purpose_key: env::var("GHTS_PURPOSE_KEY")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            token_ttl,
            min_ttl,
            renew_before,
            activity_window,
            watch_interval,
            probe_interval,
            ready_max_age,
            rate_max,
            rate_window,
            backoff_base,
            backoff_max,
            log_json: env::var("GHTS_LOG_JSON")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        })
    }
}

fn parse_u64(value: &str, name: &str) -> anyhow::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("{name}: invalid value {value:?}: {e}"))
}

/// Parses `GHTS_CLIENT_KEYS`: a JSON object mapping client ids to keys.
fn parse_client_keys(raw: &str) -> anyhow::Result<HashMap<String, String>> {
    let map: HashMap<String, String> = serde_json::from_str(raw).map_err(|e| {
        anyhow::anyhow!("GHTS_CLIENT_KEYS: expected a JSON object of client -> key: {e}")
    })?;
    for client in map.keys() {
        validate_client_id(client).map_err(|e| anyhow::anyhow!("GHTS_CLIENT_KEYS: {e}"))?;
    }
    Ok(map)
}

fn parse_u64_env(name: &str) -> anyhow::Result<u64> {
    let raw =
        env::var(name).map_err(|_| anyhow::anyhow!("{name} is not set (GitHub App credential)"))?;
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
    let secs: u64 = raw
        .parse()
        .map_err(|e| anyhow::anyhow!("{name}: expected a number of seconds, got {raw:?}: {e}"))?;
    Ok(Duration::from_secs(secs))
}

/// Initializes tracing based on `GHTS_LOG` / `RUST_LOG` and `GHTS_LOG_JSON`.
pub fn init_logging() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let log = std::env::var("GHTS_LOG").unwrap_or_else(|_| "info".to_string());
        tracing_subscriber::EnvFilter::new(log)
    });

    let json =
        std::env::var("GHTS_LOG_JSON").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

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

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::{Config, decode_app_key};

    #[test]
    fn decode_app_key_accepts_raw_pem_and_base64() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBfoo\n-----END RSA PRIVATE KEY-----";
        assert_eq!(decode_app_key(pem).unwrap(), pem);

        let b64 = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());
        assert_eq!(decode_app_key(&b64).unwrap(), pem);

        assert!(
            decode_app_key("!!!")
                .unwrap_err()
                .to_string()
                .contains("invalid base64")
        );
        assert!(
            decode_app_key("bm90IGEga2V5") // base64("not a key")
                .unwrap_err()
                .to_string()
                .contains("does not look like a PEM private key")
        );
    }

    #[test]
    fn from_env_validates_required_credentials() {
        let saved = |key: &str| std::env::var(key);
        let app_id = saved("GH_APP_ID");
        let installation_id = saved("GH_APP_INSTALLATION_ID");
        let app_key = saved("GH_APP_KEY_B64");
        unsafe {
            std::env::remove_var("GH_APP_ID");
            std::env::remove_var("GH_APP_INSTALLATION_ID");
            std::env::remove_var("GH_APP_KEY_B64");
        }

        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GH_APP_ID is not set"), "{err}");

        unsafe { std::env::set_var("GH_APP_ID", "123") };
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GH_APP_INSTALLATION_ID is not set"), "{err}");

        unsafe { std::env::set_var("GH_APP_INSTALLATION_ID", "456") };
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GH_APP_KEY_B64 is not set"), "{err}");

        // Ids must be numeric.
        unsafe { std::env::set_var("GH_APP_ID", "abc") };
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GH_APP_ID: invalid value"), "{err}");

        // Client keys: valid map, invalid JSON, invalid client name.
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBfoo\n-----END RSA PRIVATE KEY-----";
        let key_b64 = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());
        unsafe {
            std::env::set_var("GH_APP_ID", "123");
            std::env::set_var("GH_APP_KEY_B64", key_b64);
            std::env::set_var("GHTS_CLIENT_KEYS", r#"{"hermes":"hkey","pi":"pkey"}"#);
        }
        let cfg = Config::from_env().expect("valid config with client keys");
        assert_eq!(cfg.client_keys.len(), 2);
        assert_eq!(
            cfg.client_keys.get("hermes").map(String::as_str),
            Some("hkey")
        );

        unsafe { std::env::set_var("GHTS_CLIENT_KEYS", "{not json") };
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GHTS_CLIENT_KEYS"), "{err}");

        unsafe { std::env::set_var("GHTS_CLIENT_KEYS", r#"{"UPPER":"x"}"#) };
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("GHTS_CLIENT_KEYS"), "{err}");
        unsafe { std::env::remove_var("GHTS_CLIENT_KEYS") };

        restore("GH_APP_ID", app_id);
        restore("GH_APP_INSTALLATION_ID", installation_id);
        restore("GH_APP_KEY_B64", app_key);
    }

    fn restore(key: &str, value: Result<String, std::env::VarError>) {
        match value {
            Ok(v) => unsafe { std::env::set_var(key, v) },
            Err(_) => unsafe { std::env::remove_var(key) },
        }
    }
}
