//! Integration tests for the gh-token server against a mock GitHub API.

use std::time::Duration;

use chrono::DateTime;
use gh_token_server::app::Server;
use gh_token_server::config::Config;
use gh_token_testutil::{MOCK_APP_ID, MOCK_INSTALLATION_ID, MockGitHub, TEST_PRIVATE_KEY_PEM};
use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;

struct TestEnv {
    state: std::sync::Arc<gh_token_server::app::ServerState>,
    _server: Server,
    mock: MockGitHub,
    base: String,
}

async fn start_server(config_overrides: impl FnOnce(&mut Config)) -> TestEnv {
    let mock = MockGitHub::start().await;
    let config = Config {
        app_id: MOCK_APP_ID,
        installation_id: MOCK_INSTALLATION_ID,
        app_key: TEST_PRIVATE_KEY_PEM.to_string(),
        github_base_url: mock.base_url(),
        bind: "127.0.0.1:0".parse().unwrap(),
        api_key: None,
        client_keys: std::collections::HashMap::new(),
        purpose_key: false,
        token_ttl: 3600,
        min_ttl: 60,
        renew_before: Duration::from_secs(300),
        activity_window: Duration::from_secs(900),
        watch_interval: Duration::from_secs(30),
        probe_interval: Duration::from_millis(50),
        ready_max_age: Duration::from_secs(90),
        rate_max: 30,
        rate_window: Duration::from_secs(60),
        backoff_base: Duration::from_secs(1),
        backoff_max: Duration::from_secs(8),
        log_json: false,
    };
    let mut config = config;
    config_overrides(&mut config);

    let state = Server::state(config);
    Server::validate_startup(&state)
        .await
        .expect("startup validation should pass");
    state.mark_startup_ok();
    let server = Server::start(state.clone())
        .await
        .expect("server should start");
    let base = format!("http://{}", server.local_addr());
    TestEnv {
        state,
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
    // Let the initial reachability probe settle (it creates its own token).
    sleep(Duration::from_millis(200)).await;
    let client = Client::new();

    // First request: token is created.
    let (status, body) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
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
    assert_eq!(
        count, 3,
        "one token for the startup check + one for the probe + one for the request"
    );

    // Second request: served from cache, same token.
    let (status, body2) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_eq!(status, 200);
    assert_eq!(token_of(&body2), token, "cache should serve the same token");
    assert!(
        body2["cached"].as_bool().unwrap(),
        "second request should be cached"
    );
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
    assert!(
        body2["renewed"].as_bool().unwrap(),
        "refresh should be marked renewed"
    );
    assert!(!body2["cached"].as_bool().unwrap());
}

#[tokio::test]
async fn token_ttl_param_is_respected() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let (_, body) = get(
        &client,
        &format!("{}/token?client=hermes&ttl=900", env.base),
    )
    .await;
    let ttl: u64 = body["ttl"].as_u64().unwrap();
    assert!(
        ttl <= 900,
        "ttl should respect the requested 900s, got {ttl}"
    );
    assert!(ttl > 800, "ttl should be close to 900s, got {ttl}");

    // Clamped to the configured max (3600) and min (60).
    let (_, body) = get(&client, &format!("{}/token?client=pi&ttl=5", env.base)).await;
    let ttl: u64 = body["ttl"].as_u64().unwrap();
    assert!(
        (50..=60).contains(&ttl),
        "ttl below min should clamp to 60, got {ttl}"
    );
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

    let _ = client
        .get(format!("{}/token?client=hermes", env.base))
        .send()
        .await;
    // Trigger a rate-limited response so the counter gets a series.
    env.mock.set_fail_rate_limited(true);
    let _ = client
        .get(format!("{}/token?client=hermes&refresh=true", env.base))
        .send()
        .await;
    let resp = client
        .get(format!("{}/metrics", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let text = resp.text().await.unwrap();
    for metric in [
        "ghts_token_requests_total",
        "ghts_tokens_issued_total",
        "ghts_tokens_served_cached_total",
        "ghts_active_tokens",
        "ghts_github_reachable",
        "ghts_github_rate_limited_total",
        "ghts_github_cooldown_seconds",
        "ghts_github_requests_total",
        "ghts_github_request_duration_seconds",
        "ghts_token_request_duration_seconds",
    ] {
        assert!(text.contains(metric), "metrics should contain {metric}");
    }
    // The startup validation's `GET /app` must be recorded with path and
    // status labels.
    assert!(
        text.contains("ghts_github_requests_total{path=\"/app\",status=\"200\"}"),
        "metrics should contain the per-path GitHub request counter:\n{text}"
    );
}

#[tokio::test]
async fn token_request_gets_503_while_github_is_rate_limited() {
    let env = start_server(|_| {}).await;
    let client = Client::new();
    env.mock.set_fail_rate_limited(true);

    let resp = client
        .get(format!("{}/token?client=hermes", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    assert!(resp.headers().get("retry-after").is_some());

    // While the cooldown is active, further requests are answered without
    // reaching GitHub at all.
    let requests_before = env.mock.token_requests();
    let resp = client
        .get(format!("{}/token?client=hermes", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(env.mock.token_requests(), requests_before);

    // The rate limit and the cooldown are exposed as metrics.
    let text = client
        .get(format!("{}/metrics", env.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // Both the rate-limited request and the suppressed one are counted.
    assert!(
        text.contains("ghts_github_rate_limited_total{operation=\"create_token\"} 2"),
        "metrics should count the rate-limited token creations:\n{text}"
    );
    let cooldown = text
        .lines()
        .find(|l| l.trim_start().starts_with("ghts_github_cooldown_seconds "))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok());
    assert!(
        cooldown.is_some_and(|v| v >= 0.0),
        "cooldown gauge should be present:\n{text}"
    );
}

#[tokio::test]
async fn rate_limit_backoff_grows_exponentially_and_resets_on_success() {
    // Long probe interval: the reachability probe shares the client's
    // rate-limit backoff state, and this test asserts exact waits.
    let env = start_server(|c| c.probe_interval = Duration::from_secs(3600)).await;
    // Let the initial reachability probe finish (it shares the client's
    // rate-limit backoff state) before this test asserts exact waits.
    sleep(Duration::from_millis(200)).await;
    let gh = env.state.github.clone();
    env.mock.set_fail_rate_limited(true);

    // 1st rate limit: max(Retry-After 1s, base 1s * 2^0) = 1s.
    let err = gh.create_installation_token().await.unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(1)));
    assert!(gh.in_cooldown());

    // Consecutive rate limits double the backoff: 2s, then 4s.
    sleep(Duration::from_millis(1100)).await;
    let err = gh.create_installation_token().await.unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(2)));

    sleep(Duration::from_millis(2100)).await;
    let err = gh.create_installation_token().await.unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(4)));

    // A successful response resets the backoff to the base.
    sleep(Duration::from_millis(4100)).await;
    env.mock.set_fail_rate_limited(false);
    assert!(gh.create_installation_token().await.is_ok());
    assert!(!gh.in_cooldown());

    env.mock.set_fail_rate_limited(true);
    let err = gh.create_installation_token().await.unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(1)));
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
    assert_ne!(
        token_of(&body2),
        token,
        "client should get the renewed token"
    );
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

#[tokio::test]
async fn token_request_without_client_is_rejected() {
    let env = start_server(|_| {}).await;
    let client = Client::new();

    let (status, body) = get(&client, &format!("{}/token", env.base)).await;
    assert_eq!(status, 400);
    assert!(
        body["error"]
            .to_string()
            .contains("invalid client identifier"),
        "body: {body}"
    );
}

#[tokio::test]
async fn purpose_is_advisory_by_default() {
    let env = start_server(|_| {}).await;
    let client = Client::new();
    let url = format!("{}/token", env.base);

    let purpose_params = |purpose: &str| {
        vec![
            ("client", "hermes".to_string()),
            ("purpose", purpose.to_string()),
        ]
    };

    // First request with a purpose: issued, purpose echoed back.
    let resp = client
        .get(&url)
        .query(&purpose_params("triage of open issues in project abc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body1: Value = resp.json().await.unwrap();
    assert_eq!(body1["purpose"], "triage of open issues in project abc");
    assert_eq!(body1["cached"], false);
    let token1 = token_of(&body1);

    // Same client + same purpose: served from cache.
    let resp = client
        .get(&url)
        .query(&purpose_params("triage of open issues in project abc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body2: Value = resp.json().await.unwrap();
    assert_eq!(body2["cached"], true);
    assert_eq!(body2["purpose"], "triage of open issues in project abc");
    assert_eq!(token_of(&body2), token1);

    // Different purposes are advisory only: the same cached token is served,
    // the requested purpose is echoed, and no new tokens are created (an
    // agent doing 100 different things stays at one GitHub call).
    let created_before = env.mock.create_count();
    for i in 0..5 {
        let purpose = format!("purpose number {i}");
        let resp = client
            .get(&url)
            .query(&purpose_params(&purpose))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(
            body["cached"], true,
            "advisory purpose must not issue a new token"
        );
        assert_eq!(body["purpose"], purpose);
        assert_eq!(token_of(&body), token1);
    }
    assert_eq!(env.mock.create_count(), created_before);

    // No purpose: same cached token, purpose absent.
    let (status, body4) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_eq!(status, 200);
    assert_eq!(body4["cached"], true);
    assert_eq!(body4["purpose"], Value::Null);
    assert_eq!(token_of(&body4), token1);
}

#[tokio::test]
async fn purpose_key_mode_isolates_tokens_per_purpose() {
    let env = start_server(|c| c.purpose_key = true).await;
    let client = Client::new();
    let url = format!("{}/token", env.base);

    let purpose_params = |purpose: &str| {
        vec![
            ("client", "hermes".to_string()),
            ("purpose", purpose.to_string()),
        ]
    };

    // First purpose: issued.
    let resp = client
        .get(&url)
        .query(&purpose_params("purpose a"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body1: Value = resp.json().await.unwrap();
    assert_eq!(body1["cached"], false);
    let token_a = token_of(&body1);

    // Different purpose: a fresh token is issued (renewal).
    let resp = client
        .get(&url)
        .query(&purpose_params("purpose b"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body2: Value = resp.json().await.unwrap();
    assert_eq!(body2["cached"], false);
    assert_eq!(body2["renewed"], true);
    assert_ne!(token_of(&body2), token_a);

    // Back to "purpose a": the last-wins record holds "purpose b", so a
    // fresh token is issued again...
    let resp = client
        .get(&url)
        .query(&purpose_params("purpose a"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body3: Value = resp.json().await.unwrap();
    assert_eq!(body3["cached"], false);
    assert_eq!(body3["purpose"], "purpose a");
    let token_a2 = token_of(&body3);

    // ...and then serves as the cached entry for "purpose a".
    let resp = client
        .get(&url)
        .query(&purpose_params("purpose a"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body4: Value = resp.json().await.unwrap();
    assert_eq!(body4["cached"], true);
    assert_eq!(token_of(&body4), token_a2);

    // Requests without a purpose are unaffected (their own cache entry).
    let (status, body5) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_eq!(status, 200);
    assert_eq!(body5["cached"], false);
    assert_eq!(body5["purpose"], Value::Null);
    let (status, body6) = get(&client, &format!("{}/token?client=hermes", env.base)).await;
    assert_eq!(status, 200);
    assert_eq!(body6["cached"], true);
}

#[tokio::test]
async fn purpose_validation_rejects_bad_input() {
    let env = start_server(|_| {}).await;
    let client = Client::new();
    let url = format!("{}/token", env.base);

    // Control character.
    let resp = client
        .get(&url)
        .query(&[("client", "hermes"), ("purpose", "bad\npurpose")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"].to_string().contains("control characters"),
        "body: {body}"
    );

    // Too long (257 chars).
    let long = "x".repeat(257);
    let resp = client
        .get(&url)
        .query(&[("client", "hermes"), ("purpose", long.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].to_string().contains("256"), "body: {body}");

    // Whitespace-only purpose is treated as absent.
    let resp = client
        .get(&url)
        .query(&[("client", "hermes"), ("purpose", "   ")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["purpose"], Value::Null);
}

#[tokio::test]
async fn client_keys_enforcement() {
    let mut keys = std::collections::HashMap::new();
    keys.insert("hermes".to_string(), "hermes-key".to_string());
    // probe_interval 1h: keep the background probe from creating tokens and
    // racing the create-count assertions.
    let env = start_server(move |c| {
        c.client_keys = keys;
        c.probe_interval = Duration::from_secs(3600);
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = Client::new();
    let url = format!("{}/token?client=hermes", env.base);
    let created_before = env.mock.create_count();

    // Keyed client without key.
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .to_string()
            .contains("missing API key for this client"),
        "body: {body}"
    );

    // Keyed client with wrong key.
    let resp = client
        .get(&url)
        .header("x-gh-token-key", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .to_string()
            .contains("invalid API key for this client"),
        "body: {body}"
    );

    // Rejections must not create tokens.
    assert_eq!(env.mock.create_count(), created_before);

    // Keyed client with the right key (also via bearer).
    let resp = client
        .get(&url)
        .header("x-gh-token-key", "hermes-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let resp = client
        .get(&url)
        .bearer_auth("hermes-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    // Unkeyed client is unaffected.
    let (status, _) = get(&client, &format!("{}/token?client=pi", env.base)).await;
    assert_eq!(status, 200);
}
