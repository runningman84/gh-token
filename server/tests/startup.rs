//! Startup validation tests (credential checks + binary behavior).

use std::time::Duration;

use gh_token_server::app::{Server, StartupReport};
use gh_token_server::config::Config;
use gh_token_testutil::{
    MOCK_APP_ID, MOCK_APP_NAME, MOCK_BOT_LOGIN, MOCK_INSTALLATION_ID, MockGitHub, TEST_APP_KEY_B64,
    TEST_PRIVATE_KEY_PEM,
};
use tokio::time::sleep;

fn base_config(github_base_url: String) -> Config {
    Config {
        app_id: MOCK_APP_ID,
        installation_id: MOCK_INSTALLATION_ID,
        app_key: TEST_PRIVATE_KEY_PEM.to_string(),
        github_base_url,
        bind: "127.0.0.1:0".parse().unwrap(),
        api_key: None,
        client_keys: std::collections::HashMap::new(),
        purpose_key: false,
        token_ttl: 3600,
        min_ttl: 60,
        renew_before: Duration::from_secs(300),
        activity_window: Duration::from_secs(900),
        watch_interval: Duration::from_secs(30),
        probe_interval: Duration::from_secs(30),
        ready_max_age: Duration::from_secs(90),
        rate_max: 30,
        rate_window: Duration::from_secs(60),
        backoff_base: Duration::from_secs(1),
        backoff_max: Duration::from_secs(8),
        log_json: false,
    }
}

async fn validate(github_base_url: String) -> Result<StartupReport, String> {
    let config = base_config(github_base_url);
    let state = Server::state(config);
    Server::validate_startup(&state).await
}

#[tokio::test]
async fn startup_report_contains_auth_level() {
    let mock = MockGitHub::start().await;
    let report = validate(mock.base_url())
        .await
        .expect("startup validation should pass");

    assert_eq!(report.app_id, MOCK_APP_ID);
    assert_eq!(report.app_name, MOCK_APP_NAME);
    assert_eq!(report.installation_id, MOCK_INSTALLATION_ID);
    assert_eq!(report.bot_login.as_deref(), Some(MOCK_BOT_LOGIN));
    assert_eq!(report.repository_selection, "all");
    // Effective installation permissions are reported.
    let perms = &report.installation_permissions;
    assert_eq!(perms["contents"], "write");
    assert_eq!(perms["pull_requests"], "write");
}

#[tokio::test]
async fn startup_fails_on_wrong_app_id() {
    let mock = MockGitHub::start().await;
    let err = {
        let config = base_config(mock.base_url());
        let config = Config {
            app_id: 9999999,
            ..config
        };
        let state = Server::state(config);
        Server::validate_startup(&state).await.unwrap_err()
    };
    assert!(
        err.contains("authenticates as app id"),
        "error should mention the app id mismatch: {err}"
    );
}

#[tokio::test]
async fn startup_fails_when_app_key_is_rejected() {
    let mock = MockGitHub::start().await;
    mock.set_fail_app(true);
    let err = validate(mock.base_url()).await.unwrap_err();
    assert!(err.contains("GET /app"), "error: {err}");
}

#[tokio::test]
async fn startup_fails_for_unknown_installation() {
    let mock = MockGitHub::start().await;
    mock.set_fail_installation(true);
    let err = validate(mock.base_url()).await.unwrap_err();
    assert!(
        err.contains("installations"),
        "error should mention the installation: {err}"
    );
}

#[tokio::test]
async fn startup_fails_when_token_creation_fails() {
    let mock = MockGitHub::start().await;
    mock.set_fail_create(true);
    let err = validate(mock.base_url()).await.unwrap_err();
    assert!(
        err.contains("installation token"),
        "error should mention token creation: {err}"
    );
}

/// Spawns the real `gh-token-server` binary with env pointing at a dead
/// GitHub endpoint; the server keeps serving, and `/ready` must report the
/// failed startup check (503) instead of the process exiting.
#[tokio::test]
async fn binary_reports_not_ready_when_credentials_are_invalid() {
    // Find a free port for the server to bind.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_gh-token-server"))
        .env_clear()
        .env("GH_APP_ID", MOCK_APP_ID.to_string())
        .env("GH_APP_INSTALLATION_ID", MOCK_INSTALLATION_ID.to_string())
        .env("GH_APP_KEY_B64", TEST_APP_KEY_B64)
        // Nothing listens on this port -> startup validation must fail.
        .env("GHTS_GITHUB_BASE_URL", "http://127.0.0.1:1")
        .env("GHTS_BIND", format!("127.0.0.1:{port}"))
        .env("GHTS_LOG", "error")
        .env("GHTS_PROBE_INTERVAL", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn gh-token-server");

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let mut ready_body = None;
    for _ in 0..100 {
        sleep(Duration::from_millis(100)).await;
        if let Ok(resp) = client.get(format!("{base}/ready")).send().await
            && resp.status().as_u16() == 503
        {
            let body = resp.json::<serde_json::Value>().await.unwrap();
            // The server starts serving before its initial GitHub credential
            // validation completes. During that window /ready is 503, but
            // startup_error is still null; wait for the validation failure
            // rather than mistaking the initial not-ready state for it.
            if body["startup_error"].is_string() {
                ready_body = Some(body);
                break;
            }
        }
    }
    let body = ready_body.expect("/ready should report the startup validation failure");
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["startup_check"], "failed");
    assert!(
        body["startup_error"].is_string(),
        "startup_error should explain the failure: {body}"
    );

    // The server keeps running (it retries the startup check in the
    // background), so /health still answers.
    let resp = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("server should still be running");
    assert_eq!(resp.status().as_u16(), 200);

    child.kill().await.expect("kill server");
    let _ = child.wait().await;
}

/// Spawns the real binary against the mock GitHub; it must validate and
/// serve /health and /ready.
#[tokio::test]
async fn binary_serves_health_after_startup_validation() {
    let mock = MockGitHub::start().await;

    // Find a free port for the server to bind.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_gh-token-server"))
        .env_clear()
        .env("GH_APP_ID", MOCK_APP_ID.to_string())
        .env("GH_APP_INSTALLATION_ID", MOCK_INSTALLATION_ID.to_string())
        .env("GH_APP_KEY_B64", TEST_APP_KEY_B64)
        .env("GHTS_GITHUB_BASE_URL", mock.base_url())
        .env("GHTS_BIND", format!("127.0.0.1:{port}"))
        .env("GHTS_LOG", "info")
        .env("GHTS_PROBE_INTERVAL", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn gh-token-server");

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // /health appears once the server starts serving.
    let mut health_ok = false;
    for _ in 0..100 {
        sleep(Duration::from_millis(100)).await;
        if let Ok(resp) = client.get(format!("{base}/health")).send().await
            && resp.status().is_success()
        {
            health_ok = true;
            break;
        }
    }
    assert!(
        health_ok,
        "server should serve /health after startup validation"
    );

    // /ready becomes ready once a probe succeeds (1s probe interval).
    let mut ready_ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(500)).await;
        if let Ok(resp) = client.get(format!("{base}/ready")).send().await
            && resp.status().as_u16() == 200
        {
            ready_ok = true;
            break;
        }
    }
    assert!(
        ready_ok,
        "server should report ready against the mock GitHub"
    );

    child.kill().await.expect("kill server");
    let _ = child.wait().await;
}
