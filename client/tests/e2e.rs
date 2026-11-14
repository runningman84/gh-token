//! End-to-end tests: the `gh-token` client binary against the real server
//! (which talks to a mock GitHub) and against a mock token service.

use std::process::Stdio;
use std::time::Duration;

use gh_token_server::app::Server;
use gh_token_server::config::Config;
use gh_token_testutil::{
    MockGitHub, MockTokenService, MOCK_APP_ID, MOCK_INSTALLATION_ID, TEST_PRIVATE_KEY_PEM,
    mock_token,
};
use tokio::time::sleep;

fn client_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gh-token")
}

struct FullStack {
    base: String,
    _server: Server,
    _mock: MockGitHub,
}

async fn start_full_stack() -> FullStack {
    let mock = MockGitHub::start().await;
    let config = Config {
        app_id: MOCK_APP_ID,
        installation_id: MOCK_INSTALLATION_ID,
        app_key: TEST_PRIVATE_KEY_PEM.to_string(),
        github_base_url: mock.base_url(),
        bind: "127.0.0.1:0".parse().unwrap(),
        api_key: None,
        token_ttl: 3600,
        min_ttl: 60,
        renew_before: Duration::from_secs(300),
        activity_window: Duration::from_secs(900),
        watch_interval: Duration::from_secs(30),
        probe_interval: Duration::from_secs(30),
        ready_max_age: Duration::from_secs(90),
        rate_max: 30,
        rate_window: Duration::from_secs(60),
        log_json: false,
    };
    let state = Server::new(config);
    Server::validate_startup(&state).await.expect("startup validation");
    let server = Server::start(state).await.expect("server start");
    FullStack {
        base: format!("http://{}", server.local_addr()),
        _server: server,
        _mock: mock,
    }
}

async fn run_client(args: &[&str]) -> (i32, String, String) {
    let out = tokio::process::Command::new(client_bin())
        .args(args)
        .env_remove("GHTS_URL")
        .env_remove("GHTS_CLIENT")
        .env_remove("GHTS_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn gh-token client");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[tokio::test]
async fn client_fetches_token_from_full_stack() {
    let stack = start_full_stack().await;

    let (code, stdout, stderr) = run_client(&[
        "--url",
        &stack.base,
        "--client",
        "hermes",
    ])
    .await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let token = stdout.trim();
    assert!(token.starts_with("ghs_mock"), "stdout should be the token, got {stdout:?}");
    assert_eq!(stdout.lines().count(), 1, "exactly one line on stdout");

    // The full token must never appear on stderr (only masked form).
    assert!(
        !stderr.contains(token),
        "stderr must not contain the full token:\n{stderr}"
    );
    assert!(
        stderr.contains("ghs_mo"),
        "stderr should log the masked token:\n{stderr}"
    );
}

#[tokio::test]
async fn client_gets_cached_token_on_second_call() {
    let stack = start_full_stack().await;

    let (code1, stdout1, _) = run_client(&["--url", &stack.base, "--client", "hermes"]).await;
    let (code2, stdout2, _) = run_client(&["--url", &stack.base, "--client", "hermes"]).await;
    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    assert_eq!(
        stdout1.trim(),
        stdout2.trim(),
        "the server cache should serve the same token"
    );
}

#[tokio::test]
async fn client_refresh_gets_new_token() {
    let stack = start_full_stack().await;

    let (_, stdout1, _) = run_client(&["--url", &stack.base, "--client", "hermes"]).await;
    let (code, stdout2, stderr) = run_client(&[
        "--url",
        &stack.base,
        "--client",
        "hermes",
        "--refresh",
    ])
    .await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_ne!(
        stdout1.trim(),
        stdout2.trim(),
        "--refresh should yield a new token"
    );
}

#[tokio::test]
async fn client_retries_transient_failures() {
    let token = mock_token();
    let mut service = MockTokenService::start(&token, 2, None).await;

    let (code, stdout, stderr) = run_client(&[
        "--url",
        &service.base_url(),
        "--client",
        "hermes",
        "--retries",
        "3",
        "--timeout",
        "5",
    ])
    .await;
    assert_eq!(code, 0, "client should retry past the 2 failures; stderr: {stderr}");
    assert_eq!(stdout.trim(), token);
    assert_eq!(service.hits(), 3, "two failures + one success");

    service.shutdown().await;
}

#[tokio::test]
async fn client_health_command() {
    let stack = start_full_stack().await;
    // Give the readiness probe a moment to succeed.
    sleep(Duration::from_millis(200)).await;

    let (code, _stdout, stderr) = run_client(&["--url", &stack.base, "health"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
}

#[tokio::test]
async fn client_sends_api_key() {
    let mut service = MockTokenService::start(&mock_token(), 0, Some("topsecret".to_string())).await;

    // Without the key: 401 -> non-zero exit.
    let (code, _stdout, stderr) = run_client(&["--url", &service.base_url()]).await;
    assert_ne!(code, 0, "should fail without API key; stderr: {stderr}");

    // With the key: success.
    let (code, stdout, stderr) = run_client(&[
        "--url",
        &service.base_url(),
        "--api-key",
        "topsecret",
    ])
    .await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.trim().starts_with("ghs_"));

    service.shutdown().await;
}
