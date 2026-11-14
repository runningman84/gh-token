//! Entry point for the gh-token server.

use gh_token_server::app::Server;
use gh_token_server::config::{self, Config};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::init_logging();

    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(%err, "invalid configuration; refusing to start");
            return Err(err);
        }
    };

    let state = Server::new(cfg);

    // Fail fast (and visibly) when the GitHub App credentials are invalid,
    // reporting the auth level we managed to establish.
    match Server::validate_startup(&state).await {
        Ok(report) => {
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
            tracing::error!(
                %err,
                "github app credentials are invalid or the installation is unreachable; exiting"
            );
            std::process::exit(1);
        }
    }

    let server = match Server::start(state).await {
        Ok(server) => server,
        Err(err) => {
            tracing::error!(%err, "failed to start server");
            return Err(err);
        }
    };
    info!(addr = %server.local_addr(), "gh-token server listening");

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
