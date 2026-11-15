//! gh-token client: fetch short-lived GitHub installation tokens from a
//! gh-token server.
//!
//! The token is written to stdout (for scriptability); all diagnostics go
//! to stderr, and tokens are only ever logged in masked form.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use reqwest::Client;
use tokio::time::sleep;
use tracing::{debug, info};

use gh_token_common::TokenResponse;

const DEFAULT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_RETRIES: u32 = 3;

/// In-cluster default URL (namespace/service match the Helm chart).
const IN_CLUSTER_URL: &str = "http://gh-token.gh-token.svc.cluster.local:8080";
const LOCAL_URL: &str = "http://127.0.0.1:8080";

#[derive(Debug, Parser)]
#[command(
    name = "gh-token",
    version,
    about = "Fetch short-lived GitHub installation tokens from a gh-token server"
)]
pub struct Cli {
    /// Check server liveness/readiness and exit non-zero on failure.
    ///
    /// When omitted, a token is fetched (the default behavior).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Base URL of the gh-token server.
    #[arg(long, env = "GHTS_URL", global = true, value_name = "URL")]
    pub url: Option<String>,

    /// Client identifier used by the server (defaults to the GHTS_CLIENT
    /// environment variable or "default").
    #[arg(long, env = "GHTS_CLIENT", global = true, value_name = "NAME")]
    pub client: Option<String>,

    /// Shared API key, when the server enforces one.
    #[arg(long, env = "GHTS_API_KEY", global = true, value_name = "KEY")]
    pub api_key: Option<String>,

    /// Request timeout in seconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS, global = true)]
    pub timeout: u64,

    /// Number of retries for transient failures (429/5xx/network).
    #[arg(long, default_value_t = DEFAULT_RETRIES, global = true)]
    pub retries: u32,

    /// Force a fresh token from GitHub (ignores the server cache).
    #[arg(long, global = true)]
    pub refresh: bool,

    /// Requested effective TTL in seconds.
    #[arg(long, global = true, value_name = "SECS")]
    pub ttl: Option<u64>,

    /// Free-form purpose for this token (advisory audit metadata, max 256
    /// chars), e.g. "triage of open issues in project abc".
    #[arg(long, env = "GHTS_PURPOSE", global = true, value_name = "PURPOSE")]
    pub purpose: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check server liveness/readiness and exit non-zero on failure.
    Health,
}

impl Cli {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }

    pub fn base_url(&self) -> String {
        if let Some(url) = &self.url {
            return url.trim_end_matches('/').to_string();
        }
        if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
            IN_CLUSTER_URL.to_string()
        } else {
            LOCAL_URL.to_string()
        }
    }

    pub fn client_name(&self) -> String {
        self.client
            .clone()
            .unwrap_or_else(|| std::env::var("GHTS_CLIENT").unwrap_or_else(|_| "default".into()))
    }
}

/// Runs the client. The token (if any) is printed to stdout.
pub async fn run(cli: &Cli) -> Result<()> {
    init_logging();
    let base_url = cli.base_url();
    let user_agent = format!(
        "{}/{} ({}) / {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        cli.client_name(),
        env!("CARGO_PKG_REPOSITORY")
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(cli.timeout.max(1)))
        .user_agent(&user_agent)
        .build()
        .context("building HTTP client")?;

    match &cli.command {
        Some(Command::Health) => {
            let health = probe(&client, &format!("{base_url}/health"), &cli.api_key).await;
            let ready = probe(&client, &format!("{base_url}/ready"), &cli.api_key).await;
            info!(health = %health, ready = %ready, "server status");
            if ready != "200" {
                bail!("server is not ready (probe returned {ready})");
            }
            Ok(())
        }
        None => {
            let refresh = cli.refresh;
            let ttl = cli.ttl;
            let name = cli.client_name();
            let mut params: Vec<(&str, String)> = vec![("client", name.clone())];
            if refresh {
                params.push(("refresh", "true".into()));
            }
            if let Some(ttl) = ttl {
                params.push(("ttl", ttl.to_string()));
            }
            if let Some(ref purpose) = cli.purpose {
                params.push(("purpose", purpose.clone()));
            }

            let resp =
                get_token_with_retries(&client, &base_url, &params, &cli.api_key, cli.retries)
                    .await?;

            info!(
                client = %name,
                cached = resp.cached,
                renewed = resp.renewed,
                ttl_secs = resp.ttl,
                token = %gh_token_common::mask_token(&resp.token),
                "token fetched"
            );
            println!("{}", resp.token);
            Ok(())
        }
    }
}

async fn get_token_with_retries(
    client: &Client,
    base_url: &str,
    params: &[(&str, String)],
    api_key: &Option<String>,
    retries: u32,
) -> Result<TokenResponse> {
    let url = format!("{base_url}/token");
    let mut delay = Duration::from_millis(500);
    let mut last_err: Option<String> = None;

    for attempt in 0..=retries {
        if attempt > 0 {
            debug!(attempt, "retrying token request");
            sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(15));
        }

        let mut req = client
            .get(&url)
            .query(params)
            .header("accept", "application/json");
        if let Some(key) = api_key {
            req = req.header("x-gh-token-key", key);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("request to {url} failed: {e}"));
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            let body: TokenResponse = resp
                .json()
                .await
                .with_context(|| format!("invalid token response from {url}"))?;
            if body.token.is_empty() {
                bail!("server returned an empty token");
            }
            return Ok(body);
        }

        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|r| Duration::from_secs(r.min(120)));
        let text = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            429 => {
                // Honor Retry-After when present.
                if let Some(retry) = retry_after {
                    delay = retry;
                }
                last_err = Some(format!("rate limited by server: {text}"));
            }
            400 | 401 | 403 => {
                bail!("server rejected the token request ({status}): {text}");
            }
            500..=599 => {
                last_err = Some(format!("server error {status}: {text}"));
            }
            other => {
                bail!("unexpected status {other} from {url}: {text}");
            }
        }
    }

    bail!(
        "token request failed after {} attempts: {}",
        retries + 1,
        last_err.unwrap_or_else(|| "unknown error".into())
    )
}

/// GETs a URL and returns its status code as a string (for `health`).
async fn probe(client: &Client, url: &str, api_key: &Option<String>) -> String {
    let mut req = client.get(url);
    if let Some(key) = api_key {
        req = req.header("x-gh-token-key", key);
    }
    // Note: user_agent is already set on the Client builder
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let _ = resp.text().await;
            status.as_u16().to_string()
        }
        Err(e) => format!("error: {e}"),
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}
