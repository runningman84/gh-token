//! Integration tests for the gh-token server against a mock GitHub API.

use std::time::Duration;

use chrono::DateTime;
use gh_token_server::app::Server;
use gh_token_server::config::Config;
use gh_token_testutil::{MockGitHub, MOCK_APP_ID, MOCK_INSTALLATION_ID, TEST_PRIVATE_KEY_PEM};
use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;

struct TestEnv {
    _server: Server,
    mock: MockGitHub,
    base: String,
}

async fn start_server(
    mut config_overrides: impl FnMut(&mut Config),
) -> TestEnv {
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
        probe_interval: Duration::from_millis(50),
        ready_max_age: Duration::from_secs(90),
        rate_max: 30,
        rate_window: Duration::from_secs(60),
        log_json: false,
    };
    let mut config = config;
    config_overrides(&mut config);

    let state = Server::new(config);
    Server::validate_startup(&state).await.expect("startup validation should pass");
    let server = Server::start(state).await.expect("server should start");
    let base = format!("http://{}", server.local_addr());
    TestEnv {
        _server: server,
        mock,
        base,
    }
}

async fn get(client: &Client, url: &str) -> (u16, Value) {
    let resp = client.get(url).send().await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

fn token_of(body: &Value) -> String {
    body["token"].as_str().unwrap().to_string()
}

async fn wait_for(
    client: &Client,
    url: &str,
    mut check: impl FnMut(&Value) -> bool,
    timeout: Duration,
) -> Value {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let resp = client.get(url).send().await.unwrap();
        let body: Value = resp.json().await.unwrap();
        if check(&body) || std::time::Instant::now() > deadline {
            return body;
        }
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn token_flow_issues_and_caches() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    // First request: token is created.
    let (status, body) = get(
        &client,
        &format!("{}/token?client=hermes", env.base),
    )
    .await;
    assert_eq!(status, 200);
    let token = token_of(&body);
    assert!(token.starts_with("ghs_mock"), "token: {token}");
    assert_eq!(body["client"], "hermes");
    assert!(!body["cached"].as_bool().unwrap());
    assert!(!body["renewed"].as_bool().unwrap());
    assert!(
        body["expires_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .is_some(),
        "expires_at should be RFC3339: {}",
        body["expires_at"]
    );

    let count = env.mock.create_count();
    assert_eq!(count, 2, "one token for startup check + one for the request");

    // Second request: served from cache, same token.
    let (status, body2) = get(
        &client,
        &format!("{}/token?client=hermes", env.base),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(token_of(&body2), token, "cache should serve the same token");
    assert!(body2["cached"].as_bool().unwrap(), "second request should be cached");
    assert_eq!(env.mock.create_count(), count, "no new token created");

    // Another client gets its own token.
    let (_, body3) = get(&client, &format!("{}/token?client=pi", env.base)).await;
    assert_ne!(token_of(&body3), token, "different client, different token");
    assert_eq!(env.mock.create_count(), count + 1);
}

#[tokio::test]
async fn token_refresh_forces_new_token() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let (_, body) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    let token = token_of(&body);

    let (status, body2) = get(
        &client,
        &format!("{}/token?client=hermes&refresh=true", env.base),
    )
    .await;
    assert_eq!(status, 200);
    let token2 = token_of(&body2);
    assert_ne!(token2, token, "refresh should produce a new token");
    assert!(body2["renewed"].as_bool().unwrap(), "refresh should be marked renewed");
    assert!(!body2["cached"].as_bool().unwrap());
}

#[tokio::test]
async fn token_ttl_param_is_respected() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let (_, body) = get(&client, &format!("{}/token?client=hermes&ttl=900", env.base)).await;
    let ttl: u64 = body["ttl"].as_u64().unwrap();
    assert!(ttl <= 900, "ttl should respect the requested 900s, got {ttl}");
    assert!(ttl > 800, "ttl should be close to 900s, got {ttl}");

    // Clamped to the configured max (3600) and min (60).
    let (_, body) = get(&client, &format!("{}/token?client=pi&ttl=5", env.base)).await;
    let ttl: u64 = body["ttl"].as_u64().unwrap();
    assert!(ttl <= 60 && ttl >= 50, "ttl below min should clamp to 60, got {ttl}");
}

#[tokio::test]
async fn invalid_client_is_rejected() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let bad_clients: Vec<String> = vec![
        String::new(),
        "UpperCase".to_string(),
        "has space".to_string(),
        "a".repeat(65),
    ];
    for bad in &bad_clients {
        let (status, body) = get(
            &client,
            &format!("{}/token?client={}", env.base, urlencoding_of(bad)),
        )
        .await;
        assert_eq!(status, 400, "client {bad:?} should be rejected");
        assert!(body["error"].is_string());
    }
}

fn urlencoding_of(s: &str) -> String {
    s.replace(' ', "%20")
}

#[tokio::test]
async fn per_client_rate_limiting() {
    let env = start_server(|c| {
        c.rate_max = 2;
        c.rate_window = Duration::from_secs(60);
    })
    .await;
    let client = Client::new();

    let url = format!("{}/token?client=hammer", env.base);
    let (s1, _) = get(&client, &url).await;
    let (s2, _) = get(&client, &url).await;
    assert_eq!(s1, 200);
    assert_eq!(s2, 200);

    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 429);
    assert!(
        resp.headers().get("retry-after").is_some(),
        "429 should carry Retry-After"
    );

    // Other clients are unaffected.
    let (s3, _) = get(&client, &format!("{}/token?client=other", env.base)).await;
    assert_eq!(s3, 200);
}

#[tokio::test]
async fn api_key_enforcement() {
    let env = start_server(|c| c.api_key = Some("s3cret-key".to_string())).await;
    let client = Client::new();
    let url = format!("{}/token?client=hermes", env.base);

    // Missing key.
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    // Wrong key.
    let resp = client
        .get(&url)
        .header("x-gh-token-key", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    // Right key via custom header.
    let resp = client
        .get(&url)
        .header("x-gh-token-key", "s3cret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    // Right key via bearer.
    let resp = client
        .get(&url)
        .bearer_auth("s3cret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn readiness_flaps_with_github_availability() {
    let env = start_server(|c| c.ready_max_age = Duration::from_secs(1)).await;
    let client = Client::new();

    // Should become ready quickly (probe interval is 50ms).
    let body = wait_for(
        &client,
        &format!("{}/ready", env.base),
        |b| b["status"] == "ready",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(body["status"], "ready");

    // GitHub goes away -> not ready after the max age passes.
    env.mock.set_fail_probe(true);
    let body = wait_for(
        &client,
        &format!("{}/ready", env.base),
        |b| b["status"] == "not_ready",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(body["status"], "not_ready");

    // GitHub returns -> ready again.
    env.mock.set_fail_probe(false);
    let body = wait_for(
        &client,
        &format!("{}/ready", env.base),
        |b| b["status"] == "ready",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(body["status"], "ready");

    // Health is always ok.
    let (status, body) = get(&client, &format!("{}/health", env.base)).await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn metrics_endpoint_exposes_prometheus_metrics() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let _ = client.get(&format!("{}/token?client=hermes", env.base)).send().await;
    let resp = client.get(&format!("{}/metrics", env.base)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let text = resp.text().await.unwrap();
    for metric in [
        "ghts_token_requests_total",
        "ghts_tokens_issued_total",
        "ghts_tokens_served_cached_total",
        "ghts_active_tokens",
        "ghts_github_reachable",
        "ghts_token_request_duration_seconds",
    ] {
        assert!(text.contains(metric), "metrics should contain {metric}");
    }
}

#[tokio::test]
async fn watcher_renews_expiring_tokens() {
    let env = start_server(|c| {
        c.token_ttl = 3;
        c.min_ttl = 1;
        c.renew_before = Duration::from_secs(2);
        c.watch_interval = Duration::from_millis(100);
    })
    .await;
    let client = Client::new();

    let (_, body) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    let token = token_of(&body);
    let count_before = env.mock.create_count();

    // The watcher should renew the token once less than 2s remain (i.e.
    // within ~1.5s of issuance).
    sleep(Duration::from_millis(2000)).await;
    assert!(
        env.mock.create_count() > count_before,
        "watcher should have renewed the expiring token (created {} tokens)",
        env.mock.create_count() - count_before
    );

    // The served token is now the renewed one (not the expired original).
    let (_, body2) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_ne!(token_of(&body2), token, "client should get the renewed token");
}

#[tokio::test]
async fn token_creation_failure_returns_502() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    env.mock.set_fail_create(true);
    let (status, body) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_eq!(status, 502);
    assert!(body["error"].is_string());
    env.mock.set_fail_create(false);
}
