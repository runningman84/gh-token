//! Server configuration, loaded from environment variables.

use std::env;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
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
    /// Reject unregistered clients. Mandatory with file-based keys.
    pub require_known_client: bool,
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

        let api_key = env::var("GHTS_API_KEY").ok().filter(|k| !k.is_empty());
        let keys_dir = env::var("GHTS_CLIENT_KEYS_DIR")
            .ok()
            .filter(|path| !path.trim().is_empty());
        let keys_json = env::var("GHTS_CLIENT_KEYS")
            .ok()
            .filter(|k| !k.trim().is_empty());
        anyhow::ensure!(
            keys_dir.is_none() || keys_json.is_none(),
            "GHTS_CLIENT_KEYS_DIR and GHTS_CLIENT_KEYS are mutually exclusive"
        );
        let client_keys = match (&keys_dir, keys_json) {
            (Some(dir), _) => load_client_keys_dir(Path::new(dir))?,
            (_, Some(raw)) => parse_client_keys(&raw)?,
            _ => HashMap::new(),
        };
        let require_known_client = match env::var("GHTS_REQUIRE_KNOWN_CLIENT") {
            Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
            Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
            Ok(_) => anyhow::bail!("GHTS_REQUIRE_KNOWN_CLIENT: expected true or false"),
            Err(env::VarError::NotPresent) => keys_dir.is_some(),
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            keys_dir.is_none() || require_known_client,
            "GHTS_CLIENT_KEYS_DIR requires GHTS_REQUIRE_KNOWN_CLIENT=true"
        );
        validate_client_auth(&client_keys, api_key.as_deref(), require_known_client)?;

        Ok(Self {
            app_id,
            installation_id,
            app_key,
            github_base_url: env::var("GHTS_GITHUB_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_GITHUB_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            bind,
            api_key,
            client_keys,
            require_known_client,
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

/// Loads one API key per client-named file at startup. Restart after rotation.
/// Kubernetes projected volumes use a `..data` symlink to a complete snapshot;
/// resolve it once so a startup never combines two versions of the volume.
/// Ordinary directories are also supported for non-Kubernetes deployments.
pub fn load_client_keys_dir(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    let snapshot = path.join("..data");
    let root = if snapshot.symlink_metadata().is_ok() {
        snapshot
            .canonicalize()
            .context("resolving client key snapshot")?
    } else {
        path.to_path_buf()
    };
    let mut keys = HashMap::new();
    for entry in std::fs::read_dir(&root).context("reading GHTS_CLIENT_KEYS_DIR")? {
        let entry = entry.context("reading client key entry")?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("client key filename is not UTF-8"))?;
        validate_client_id(&name).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            entry.path().is_file(),
            "client key entry {name:?} is not a file"
        );
        let key = std::fs::read_to_string(entry.path())
            .with_context(|| format!("reading key for client {name:?}"))?;
        keys.insert(name, key);
    }
    validate_client_auth(&keys, None, false)?;
    Ok(keys)
}

fn validate_client_auth(
    keys: &HashMap<String, String>,
    shared: Option<&str>,
    strict: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !strict || !keys.is_empty(),
        "strict client authentication requires client keys"
    );
    anyhow::ensure!(
        !strict || shared.is_none(),
        "GHTS_API_KEY cannot be combined with strict per-client authentication"
    );
    let mut seen = std::collections::HashSet::new();
    for (client, key) in keys {
        // Keys are sent in HTTP headers. Reject whitespace rather than silently
        // trimming a credential differently on the agent and server.
        anyhow::ensure!(
            !key.is_empty() && key.bytes().all(|b| (33..=126).contains(&b)),
            "API key for client {client:?} must be nonempty printable ASCII without whitespace"
        );
        anyhow::ensure!(seen.insert(key), "API keys must be unique per client");
    }
    Ok(())
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
    fn client_key_files_validate_credentials() {
        use super::{load_client_keys_dir, validate_client_auth};
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("agent-a");
        let b = dir.path().join("agent-b");
        std::fs::write(&a, "secret-a").unwrap();
        std::fs::write(&b, "secret-b").unwrap();
        let keys = load_client_keys_dir(dir.path()).unwrap();
        assert_eq!(keys["agent-a"], "secret-a");
        assert_eq!(keys["agent-b"], "secret-b");
        assert!(validate_client_auth(&keys, None, true).is_ok());
        assert!(validate_client_auth(&keys, Some("shared"), true).is_err());
        assert!(validate_client_auth(&Default::default(), None, true).is_err());
        for invalid in [
            "",
            " ",
            "secret-b\n",
            "not\ta\tkey",
            "non-ascii-ä",
            "secret-a",
        ] {
            std::fs::write(&b, invalid).unwrap();
            assert!(load_client_keys_dir(dir.path()).is_err());
        }
        std::fs::remove_file(&b).unwrap();
        std::fs::write(dir.path().join("INVALID"), "key").unwrap();
        assert!(load_client_keys_dir(dir.path()).is_err());
        std::fs::remove_file(dir.path().join("INVALID")).unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        assert!(load_client_keys_dir(dir.path()).is_err());
        assert!(load_client_keys_dir(&dir.path().join("missing")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn client_key_files_support_kubernetes_projection() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("..2026_snapshot");
        std::fs::create_dir(&snapshot).unwrap();
        std::fs::write(snapshot.join("agent-a"), "secret-a").unwrap();
        symlink("..2026_snapshot", dir.path().join("..data")).unwrap();
        symlink("..data/agent-a", dir.path().join("agent-a")).unwrap();
        let keys = super::load_client_keys_dir(dir.path()).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys["agent-a"], "secret-a");
    }

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
        let auth_vars = [
            "GHTS_CLIENT_KEYS",
            "GHTS_CLIENT_KEYS_DIR",
            "GHTS_REQUIRE_KNOWN_CLIENT",
            "GHTS_API_KEY",
        ];
        let auth_saved: Vec<_> = auth_vars.iter().map(|key| (*key, saved(key))).collect();
        for key in auth_vars {
            unsafe { std::env::remove_var(key) };
        }
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

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent-a"), "secret-a").unwrap();
        unsafe { std::env::set_var("GHTS_CLIENT_KEYS_DIR", dir.path()) };
        assert!(
            Config::from_env()
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
        unsafe { std::env::remove_var("GHTS_CLIENT_KEYS") };
        let cfg = Config::from_env().unwrap();
        assert!(cfg.require_known_client);
        assert_eq!(cfg.client_keys["agent-a"], "secret-a");
        unsafe { std::env::set_var("GHTS_REQUIRE_KNOWN_CLIENT", "false") };
        assert!(
            Config::from_env()
                .unwrap_err()
                .to_string()
                .contains("requires GHTS_REQUIRE_KNOWN_CLIENT=true")
        );
        unsafe { std::env::remove_var("GHTS_REQUIRE_KNOWN_CLIENT") };
        unsafe { std::env::set_var("GHTS_API_KEY", "shared") };
        assert!(
            Config::from_env()
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
        );
        unsafe {
            std::env::remove_var("GHTS_API_KEY");
            std::env::set_var("GHTS_REQUIRE_KNOWN_CLIENT", "invalid");
        }
        assert!(
            Config::from_env()
                .unwrap_err()
                .to_string()
                .contains("expected true or false")
        );
        unsafe {
            std::env::remove_var("GHTS_REQUIRE_KNOWN_CLIENT");
            std::env::remove_var("GHTS_CLIENT_KEYS_DIR");
        }

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
        for (key, value) in auth_saved {
            restore(key, value);
        }
    }

    fn restore(key: &str, value: Result<String, std::env::VarError>) {
        match value {
            Ok(v) => unsafe { std::env::set_var(key, v) },
            Err(_) => unsafe { std::env::remove_var(key) },
        }
    }
}
