//! Entry point for the gh-token server.

use clap::Parser;
use gh_token_server::app::{self, Server};
use gh_token_server::config::{self, Config};
use tracing::{error, info};

/// Service that issues short-lived GitHub App installation tokens to clients.
#[derive(Parser)]
#[command(name = "gh-token-server", version, about)]
struct Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse arguments first so `--version` and `--help` work without any
    // credentials being configured.
    let _cli = Cli::parse();

    config::init_logging();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        "gh-token server starting"
    );

    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(%err, "invalid configuration; refusing to start");
            return Err(err);
        }
    };

    let state = Server::state(cfg);

    let server = match Server::start(state.clone()).await {
        Ok(server) => server,
        Err(err) => {
            error!(%err, "failed to start server");
            return Err(err);
        }
    };
    info!(addr = %server.local_addr(), "gh-token server listening");

    // Validate the GitHub App credentials and report the auth level we
    // managed to establish. Until this passes, `/ready` returns 503.
    match Server::validate_startup(&state).await {
        Ok(report) => {
            state.set_startup_report(report.clone());
            state.mark_startup_ok();
            info!(
                app_id = report.app_id,
                app_name = %report.app_name,
                installation_id = report.installation_id,
                bot_login = report.bot_login.as_deref().unwrap_or("<unknown>"),
                app_permissions = %report.app_permissions,
                installation_permissions = %report.installation_permissions,
                repository_selection = %report.repository_selection,
                "github app credentials validated (startup check passed)"
            );
        }
        Err(err) => {
            state.mark_startup_failed(&err);
            error!(
                %err,
                "github app credentials are invalid or the installation is unreachable; \
                 /ready reports not-ready and the startup check is retried in the background"
            );
            let retry_state = state.clone();
            let retry_stop = server.stop_handle();
            tokio::spawn(app::startup_retry_loop(retry_state, retry_stop));
        }
    }

    let stop = server.stop_handle();
    tokio::select! {
        _ = server.wait() => {}
        _ = tokio::signal::ctrl_c() => info!("received SIGINT, shutting down"),
        _ = terminate() => info!("received SIGTERM, shutting down"),
    }
    stop.cancel();
    Ok(())
}

#[cfg(unix)]
async fn terminate() {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler")
        .recv()
        .await;
}

#[cfg(not(unix))]
async fn terminate() {
    std::future::pending().await;
}
