//! Test utilities: a mock GitHub API server and a mock gh-token server,
//! plus a generated RSA keypair for GitHub App JWT tests.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Mock GitHub App ID used by the mock server.
pub const MOCK_APP_ID: u64 = 1234567;
/// Mock installation ID used by the mock server.
pub const MOCK_INSTALLATION_ID: u64 = 7654321;
/// Mock app name.
pub const MOCK_APP_NAME: &str = "gh-token-mock-app";
/// Mock bot login.
pub const MOCK_BOT_LOGIN: &str = "gh-token-mock-app[bot]";

/// Test RSA private key (PKCS#8 PEM).
pub const TEST_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCvg+m0qvXVjD1i
2i7/BUFk0LCxNzm1rkGShTkc2oRnKAaAmN0G+TclrmyRsMQ6lHJd933nuV19UoRs
HA/0YU2hVHdNlOuc8P7Xu5ebiqopK1pTTHFuCZUxPYd+cyQps2eJM/GcD7npGEuH
rrMa0Oizwy+6JKZ6MxLmleum5Ui/nYaUVnpGXLibc6voxpya8+1bkUBAZkFxgM2F
jnFI0kOxe6ey39o73L3sjFo9QMu4yrUahcVCSK8+tRFadvAaPaCr0U8xPJGmzZ9I
C+STHl5YJ1nAaiFbreC3v1uIH3lsKh31b92XjCBElV/Zsvo77K7UoaIddebEKZKx
bI3E8cEnAgMBAAECggEABS5mY8hNjDRLoPNtQmdWpYc+RWeqncu7G9GFp227+DTy
Rtw8X3b+mG+BvPtJj6vPhH6d341VTocfvvaacfhoC5nWu8ToVQW4QDNwJ6NRn/gr
qRyEl17+YdRof48emNc2+JHpUVYVguVjYBSu14fQlsiYTBfGN61HouRMxJctgpqI
qJSP44nq5n8X0TZzheS3SR07qSSoOsmadAP6vQDkqwr0XgnMWgc+F77K0c9dzdSz
T8IcGhTswJNNfN1xPqayZeizvjyp1xgSLnpKceZEvXGPsWAh773fcqN2/fcO0iOC
LSgXHwEk0Zp9vDy2EjzJGNlQ/wA1HUwUGEPrxyF2YQKBgQDhcLRQWCuH/vaeHFG5
ZNrN/HLgdQMSNAbtGYmg45oyUWJJ7DJYZeIwabP+KgGs4HYCpDS0709Dmnji7/ou
Kd7WvD+KWZQisCk/HOyacxs+NMKNfPa6LvDBy6F4GdRZdRc34NSHzLlkCAAixlOH
OUifLLDEGvQ+A6YbOuMGZvHp7QKBgQDHTrKCfCfehDrweIdQupDDNZbF/GSXOx3K
3BU145d4xZimgqRekqLZ3OJA00QckcfBOLsd4kasuN7A1KnSKHuVoeIAnbwu85D5
2si3fIsoHZ6xaQ2xpRbawK6yuS5wb3GZmSiebMA7yO+og29gGFEM+EAEQM2vbwV8
4woj/HIk4wKBgQDAqHeAHgl9nFeYAW2Z+WXAGVj3Fyh7uvicUXG7jCavj7tmnzLb
Np6ZbwwCTlXGLsDLOxdL9SORC4b5scnk/6VPb+sBSdJ93nwxUn+k8m+6to35Km0T
6Z+YjnQ8Vh6SIK2p9DrMnxlkMi4jicfCTSMR3Lhnrv1Brg6bQLTZOMrOdQKBgAfL
YRnPRzPhkfiDji8vDKhFuT7G7xwm+LRNRRsVaoATojOo2krEi0/f3o1QddJ0NQ6u
bErv5XmtxTBfYMDSSnsAmLkTch+X8OyEjOL4IO0Rs3oWtK1k2m71qB9fNJy7DwNF
xRvk/uOXqR+6/CkS6mCKeN5xtlmNCvbmXigoIEKnAoGBAIHRa399PNoXMNXxWpiE
IobC8zLip8hPfT6VEPe6r+qd+9lE7WjYdvM1PsT+FYKIISwJOtJ5Pnkk+yuMDWJH
BTOqHt+nwA1IHPgcoS0eb4cR9j7Y6DAk06JXLeWk3Y1w5UgbXZ3UR/mryqLlCkqs
1MWLmEFhOlgzQebHb2X6RGFK
-----END PRIVATE KEY-----
"#;
/// Test RSA public key (SPKI PEM), matching the private key above.
pub const TEST_PUBLIC_KEY_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAr4PptKr11Yw9Ytou/wVB
ZNCwsTc5ta5BkoU5HNqEZygGgJjdBvk3Ja5skbDEOpRyXfd957ldfVKEbBwP9GFN
oVR3TZTrnPD+17uXm4qqKStaU0xxbgmVMT2HfnMkKbNniTPxnA+56RhLh66zGtDo
s8MvuiSmejMS5pXrpuVIv52GlFZ6Rly4m3Or6MacmvPtW5FAQGZBcYDNhY5xSNJD
sXunst/aO9y97IxaPUDLuMq1GoXFQkivPrURWnbwGj2gq9FPMTyRps2fSAvkkx5e
WCdZwGohW63gt79biB95bCod9W/dl4wgRJVf2bL6O+yu1KGiHXXmxCmSsWyNxPHB
JwIDAQAB
-----END PUBLIC KEY-----
"#;
/// The private key, base64-encoded (what goes into `GH_APP_KEY_B64`).
pub const TEST_APP_KEY_B64: &str = "LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCk1JSUV2Z0lCQURBTkJna3Foa2lHOXcwQkFRRUZBQVNDQktnd2dnU2tBZ0VBQW9JQkFRQ3ZnK20wcXZYVmpEMWkKMmk3L0JVRmswTEN4TnptMXJrR1NoVGtjMm9SbktBYUFtTjBHK1RjbHJteVJzTVE2bEhKZDkzM251VjE5VW9ScwpIQS8wWVUyaFZIZE5sT3VjOFA3WHU1ZWJpcW9wSzFwVFRIRnVDWlV4UFlkK2N5UXBzMmVKTS9HY0Q3bnBHRXVICnJyTWEwT2l6d3krNkpLWjZNeExtbGV1bTVVaS9uWWFVVm5wR1hMaWJjNnZveHB5YTgrMWJrVUJBWmtGeGdNMkYKam5GSTBrT3hlNmV5MzlvNzNMM3NqRm85UU11NHlyVWFoY1ZDU0s4K3RSRmFkdkFhUGFDcjBVOHhQSkdtelo5SQpDK1NUSGw1WUoxbkFhaUZicmVDM3YxdUlIM2xzS2gzMWI5MlhqQ0JFbFYvWnN2bzc3SzdVb2FJZGRlYkVLWkt4CmJJM0U4Y0VuQWdNQkFBRUNnZ0VBQlM1bVk4aE5qRFJMb1BOdFFtZFdwWWMrUldlcW5jdTdHOUdGcDIyNytEVHkKUnR3OFgzYittRytCdlB0Smo2dlBoSDZkMzQxVlRvY2Z2dmFhY2Zob0M1bld1OFRvVlFXNFFETndKNk5Sbi9ncgpxUnlFbDE3K1lkUm9mNDhlbU5jMitKSHBVVllWZ3VWallCU3UxNGZRbHNpWVRCZkdONjFIb3VSTXhKY3RncHFJCnFKU1A0NG5xNW44WDBUWnpoZVMzU1IwN3FTU29Pc21hZEFQNnZRRGtxd3IwWGduTVdnYytGNzdLMGM5ZHpkU3oKVDhJY0doVHN3Sk5OZk4xeFBxYXlaZWl6dmp5cDF4Z1NMbnBLY2VaRXZYR1BzV0FoNzczZmNxTjIvZmNPMGlPQwpMU2dYSHdFazBacDl2RHkyRWp6SkdObFEvd0ExSFV3VUdFUHJ4eUYyWVFLQmdRRGhjTFJRV0N1SC92YWVIRkc1ClpOck4vSExnZFFNU05BYnRHWW1nNDVveVVXSko3REpZWmVJd2FiUCtLZ0dzNEhZQ3BEUzA3MDlEbW5qaTcvb3UKS2Q3V3ZEK0tXWlFpc0NrL0hPeWFjeHMrTk1LTmZQYTZMdkRCeTZGNEdkUlpkUmMzNE5TSHpMbGtDQUFpeGxPSApPVWlmTExERUd2UStBNlliT3VNR1p2SHA3UUtCZ1FESFRyS0NmQ2ZlaERyd2VJZFF1cERETlpiRi9HU1hPeDNLCjNCVTE0NWQ0eFppbWdxUmVrcUxaM09KQTAwUWNrY2ZCT0xzZDRrYXN1TjdBMUtuU0tIdVZvZUlBbmJ3dTg1RDUKMnNpM2ZJc29IWjZ4YVEyeHBSYmF3SzZ5dVM1d2IzR1ptU2llYk1BN3lPK29nMjlnR0ZFTStFQUVRTTJ2YndWOAo0d29qL0hJazR3S0JnUURBcUhlQUhnbDluRmVZQVcyWitXWEFHVmozRnloN3V2aWNVWEc3akNhdmo3dG1uekxiCk5wNlpid3dDVGxYR0xzRExPeGRMOVNPUkM0YjVzY25rLzZWUGIrc0JTZEo5M253eFVuK2s4bSs2dG8zNUttMFQKNlorWWpuUThWaDZTSUsycDlEck1ueGxrTWk0amljZkNUU01SM0xobnJ2MUJyZzZiUUxUWk9Nck9kUUtCZ0FmTApZUm5QUnpQaGtmaURqaTh2REtoRnVUN0c3eHdtK0xSTlJSc1Zhb0FUb2pPbzJrckVpMC9mM28xUWRkSjBOUTZ1CmJFcnY1WG10eFRCZllNRFNTbnNBbUxrVGNoK1g4T3lFak9MNElPMFJzM29XdEsxazJtNzFxQjlmTkp5N0R3TkYKeFJ2ay91T1hxUis2L0NrUzZtQ0tlTjV4dGxtTkN2Ym1YaWdvSUVLbkFvR0JBSUhSYTM5OVBOb1hNTlh4V3BpRQpJb2JDOHpMaXA4aFBmVDZWRVBlNnIrcWQrOWxFN1dqWWR2TTFQc1QrRllLSUlTd0pPdEo1UG5rayt5dU1EV0pICkJUT3FIdCtud0ExSUhQZ2NvUzBlYjRjUjlqN1k2REFrMDZKWExlV2szWTF3NVVnYlhaM1VSL21yeXFMbENrcXMKMU1XTG1FRmhPbGd6UWViSGIyWDZSR0ZLCi0tLS0tRU5EIFBSSVZBVEUgS0VZLS0tLS0K";

fn static_decode_key() -> &'static DecodingKey {
    use std::sync::OnceLock;
    static KEY: OnceLock<DecodingKey> = OnceLock::new();
    KEY.get_or_init(|| {
        DecodingKey::from_rsa_pem(TEST_PUBLIC_KEY_PEM.as_bytes()).expect("valid test public key")
    })
}

/// Verifies an app JWT: RS256 signature against the embedded test public
/// key and `iss == MOCK_APP_ID`.
fn verify_app_jwt(token: &str) -> Result<(), Box<Response>> {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = true;
    let data =
        jsonwebtoken::decode::<Value>(token, static_decode_key(), &validation).map_err(|e| {
            Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "message": "Bad credentials", "jwt_error": e.to_string() })),
                )
                    .into_response(),
            )
        })?;
    let iss = data.claims.get("iss").and_then(Value::as_u64).unwrap_or(0);
    if iss != MOCK_APP_ID {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "message": "Bad credentials",
                    "jwt_error": format!("iss {iss} != {MOCK_APP_ID}"),
                })),
            )
                .into_response(),
        ));
    }
    Ok(())
}

struct MockGitHubInner {
    create_count: AtomicU64,
    token_requests: AtomicU64,
    fail_app: AtomicBool,
    fail_installation: AtomicBool,
    fail_create: AtomicBool,
    fail_probe: AtomicBool,
    fail_rate_limited: AtomicBool,
    issued: Mutex<Vec<String>>,
}

/// A mock GitHub API server on 127.0.0.1:<random port>.
///
/// It verifies app JWT signatures (RS256 with the embedded test key) and
/// simulates the endpoints the gh-token server uses:
/// - `GET /app`
/// - `GET /app/installations/{id}`
/// - `POST /app/installations/{id}/access_tokens`
/// - `GET /installation/repositories`
/// - `GET /users/{login}`
/// - `GET /rate_limit`
pub struct MockGitHub {
    inner: Arc<MockGitHubInner>,
    addr: SocketAddr,
    stop: CancellationToken,
    _task: Option<tokio::task::JoinHandle<()>>,
}

impl MockGitHub {
    /// Starts a new mock GitHub server.
    pub async fn start() -> Self {
        let inner = Arc::new(MockGitHubInner {
            create_count: AtomicU64::new(0),
            token_requests: AtomicU64::new(0),
            fail_app: AtomicBool::new(false),
            fail_installation: AtomicBool::new(false),
            fail_create: AtomicBool::new(false),
            fail_probe: AtomicBool::new(false),
            fail_rate_limited: AtomicBool::new(false),
            issued: Mutex::new(Vec::new()),
        });

        let stop = CancellationToken::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock github");
        let addr = listener.local_addr().expect("local addr");

        let router = Router::new()
            .route("/app", get(handler_get_app))
            .route("/app/installations/{id}", get(handler_get_installation))
            .route(
                "/app/installations/{id}/access_tokens",
                post(handler_create_token),
            )
            .route(
                "/installation/repositories",
                get(handler_installation_repositories),
            )
            .route("/users/{login}", get(handler_get_user_by_login))
            .route("/rate_limit", get(handler_rate_limit))
            .with_state(inner.clone());

        let serve_stop = stop.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = serve_stop.cancelled() => {}
                _ = axum::serve(listener, router) => {}
            }
        });

        Self {
            inner,
            addr,
            stop,
            _task: Some(task),
        }
    }

    /// Base URL, e.g. `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.addr.ip(), self.addr.port())
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// How many installation tokens the mock has issued.
    pub fn create_count(&self) -> u64 {
        self.inner.create_count.load(Ordering::Relaxed)
    }

    /// All POST access_tokens requests received (including failures).
    pub fn token_requests(&self) -> u64 {
        self.inner.token_requests.load(Ordering::Relaxed)
    }

    /// All tokens issued so far.
    pub async fn issued_tokens(&self) -> Vec<String> {
        self.inner.issued.lock().await.clone()
    }

    pub fn set_fail_app(&self, fail: bool) {
        self.inner.fail_app.store(fail, Ordering::Relaxed);
    }

    pub fn set_fail_installation(&self, fail: bool) {
        self.inner.fail_installation.store(fail, Ordering::Relaxed);
    }

    pub fn set_fail_create(&self, fail: bool) {
        self.inner.fail_create.store(fail, Ordering::Relaxed);
    }

    pub fn set_fail_probe(&self, fail: bool) {
        self.inner.fail_probe.store(fail, Ordering::Relaxed);
    }

    /// Makes token creation and the rate-limit probe answer with
    /// rate-limit responses (429 / 403 with `Retry-After`).
    pub fn set_fail_rate_limited(&self, fail: bool) {
        self.inner.fail_rate_limited.store(fail, Ordering::Relaxed);
    }

    /// Stops the mock server.
    pub async fn shutdown(&mut self) {
        self.stop.cancel();
        if let Some(task) = self._task.take() {
            let _ = task.await;
        }
    }
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

fn unauthorized(msg: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "message": msg }))).into_response()
}

async fn handler_get_app(State(inner): State<Arc<MockGitHubInner>>) -> Response {
    if inner.fail_app.load(Ordering::Relaxed) {
        return unauthorized("Bad credentials");
    }
    Json(json!({
        "id": MOCK_APP_ID,
        "name": MOCK_APP_NAME,
        "slug": "gh-token-mock-app",
        "owner": { "login": "mock-owner" },
        "permissions": { "contents": "write", "metadata": "read" },
        "events": [],
    }))
    .into_response()
}

async fn handler_get_installation(
    State(inner): State<Arc<MockGitHubInner>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let token = match bearer(&headers) {
        Some(t) => t,
        None => return unauthorized("Bad credentials"),
    };
    if let Err(resp) = verify_app_jwt(&token) {
        return *resp;
    }
    if inner.fail_installation.load(Ordering::Relaxed) || id != MOCK_INSTALLATION_ID {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        )
            .into_response();
    }
    Json(json!({
        "id": MOCK_INSTALLATION_ID,
        "app_id": MOCK_APP_ID,
        "target_type": "User",
        "permissions": { "contents": "write", "pull_requests": "write", "metadata": "read" },
        "repository_selection": "all",
    }))
    .into_response()
}

fn rate_limited_response() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, "1")],
        Json(json!({
            "message": "API rate limit exceeded for installation.",
            "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-github-api",
        })),
    )
        .into_response()
}

async fn handler_create_token(
    State(inner): State<Arc<MockGitHubInner>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    inner.token_requests.fetch_add(1, Ordering::Relaxed);
    let token = match bearer(&headers) {
        Some(t) => t,
        None => return unauthorized("Bad credentials"),
    };
    if let Err(resp) = verify_app_jwt(&token) {
        return *resp;
    }
    if inner.fail_rate_limited.load(Ordering::Relaxed) {
        return rate_limited_response();
    }
    if id != MOCK_INSTALLATION_ID {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        )
            .into_response();
    }
    if inner.fail_create.load(Ordering::Relaxed) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": "Internal Server Error" })),
        )
            .into_response();
    }
    let count = inner.create_count.fetch_add(1, Ordering::Relaxed) + 1;
    let new_token = format!(
        "ghs_mock{count:06x}_{:x}",
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let expires_at =
        (Utc::now() + Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    inner.issued.lock().await.push(new_token.clone());
    (
        StatusCode::CREATED,
        Json(json!({
            "token": new_token,
            "expires_at": expires_at,
            "permissions": { "contents": "write", "pull_requests": "write", "metadata": "read" },
            "repository_selection": "all",
        })),
    )
        .into_response()
}

async fn handler_installation_repositories(
    State(_inner): State<Arc<MockGitHubInner>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let token = match bearer(&headers) {
        Some(t) => t,
        None => return unauthorized("Bad credentials"),
    };
    if token.starts_with("ghs_mock") {
        // Like real GitHub: an object with a `repositories` array.
        Json(json!({
            "total_count": 2,
            "repository_selection": "all",
            "repositories": [
                { "id": 1, "full_name": "mock-owner/mock-repo" },
                { "id": 2, "full_name": "mock-owner/mock-repo-2" },
            ],
        }))
        .into_response()
    } else {
        unauthorized("Bad credentials")
    }
}

async fn handler_get_user_by_login(
    State(_inner): State<Arc<MockGitHubInner>>,
    Path(login): Path<String>,
) -> Response {
    if login == MOCK_BOT_LOGIN {
        Json(json!({
            "login": MOCK_BOT_LOGIN,
            "id": 987654321,
            "type": "Bot",
            "bot_type": "App",
        }))
        .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        )
            .into_response()
    }
}

async fn handler_rate_limit(
    State(inner): State<Arc<MockGitHubInner>>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Like real GitHub: `GET /rate_limit` accepts installation tokens and
    // no auth, but rejects app JWTs with 401.
    if let Some(t) = bearer(&headers)
        && !t.starts_with("ghs_")
    {
        return unauthorized("Bad credentials");
    }
    if inner.fail_rate_limited.load(Ordering::Relaxed) {
        return (
            StatusCode::FORBIDDEN,
            [("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "0")],
            Json(json!({
                "message": "API rate limit exceeded for 127.0.0.1.",
                "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-github-api",
            })),
        )
            .into_response();
    }
    if inner.fail_probe.load(Ordering::Relaxed) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": "Internal Server Error" })),
        )
            .into_response();
    }
    Json(json!({
        "resources": { "core": { "limit": 5000, "remaining": 4999, "reset": 0 } },
        "rate": { "limit": 5000, "remaining": 4999, "reset": 0 },
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Mock gh-token server (for client tests)
// ---------------------------------------------------------------------------

struct MockTokenServiceInner {
    token: String,
    fail_first_n: AtomicU64,
    hits: AtomicU64,
    api_key: Option<String>,
    client_keys: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

/// A minimal mock of the gh-token server for client tests.
pub struct MockTokenService {
    inner: Arc<MockTokenServiceInner>,
    addr: SocketAddr,
    stop: CancellationToken,
    _task: Option<tokio::task::JoinHandle<()>>,
}

impl MockTokenService {
    /// Starts a mock server that always returns `token`.
    ///
    /// `fail_first_n` makes the first N `/token` requests fail with 500
    /// (used to test client retries).
    pub async fn start(token: &str, fail_first_n: u64, api_key: Option<String>) -> Self {
        let inner = Arc::new(MockTokenServiceInner {
            token: token.to_string(),
            fail_first_n: AtomicU64::new(fail_first_n),
            hits: AtomicU64::new(0),
            api_key,
            client_keys: std::sync::Mutex::new(std::collections::HashMap::new()),
        });

        let stop = CancellationToken::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock token service");
        let addr = listener.local_addr().expect("local addr");

        let router = Router::new()
            .route("/health", get(handler_mock_health))
            .route("/token", get(handler_mock_token))
            .with_state(inner.clone());

        let serve_stop = stop.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = serve_stop.cancelled() => {}
                _ = axum::serve(listener, router) => {}
            }
        });

        Self {
            inner,
            addr,
            stop,
            _task: Some(task),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.addr.ip(), self.addr.port())
    }

    pub fn hits(&self) -> u64 {
        self.inner.hits.load(Ordering::Relaxed)
    }

    /// Enables per-client keys (client -> key); requests for clients present
    /// in the map must send the matching `X-GH-Token-Key` header.
    pub fn with_client_keys(self, keys: impl IntoIterator<Item = (String, String)>) -> Self {
        self.inner
            .client_keys
            .lock()
            .expect("client keys lock")
            .extend(keys);
        self
    }

    pub async fn shutdown(&mut self) {
        self.stop.cancel();
        if let Some(task) = self._task.take() {
            let _ = task.await;
        }
    }
}

async fn handler_mock_health() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

#[derive(Deserialize)]
struct MockTokenQuery {
    client: Option<String>,
    purpose: Option<String>,
}

async fn handler_mock_token(
    State(inner): State<Arc<MockTokenServiceInner>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<MockTokenQuery>,
) -> Response {
    let hit = inner.hits.fetch_add(1, Ordering::Relaxed) + 1;
    if inner.fail_first_n.load(Ordering::Relaxed) >= hit {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "simulated failure" })),
        )
            .into_response();
    }
    let provided: Option<String> = headers
        .get("x-gh-token-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(expected) = &inner.api_key
        && provided.as_deref() != Some(expected.as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid API key" })),
        )
            .into_response();
    }
    let client_key_ok = match (q.client.as_deref(), provided.as_deref()) {
        (Some(client), Some(expected))
            if inner
                .client_keys
                .lock()
                .expect("client keys lock")
                .contains_key(client) =>
        {
            inner
                .client_keys
                .lock()
                .expect("client keys lock")
                .get(client)
                .is_some_and(|k| k.as_str() == expected)
        }
        (Some(client), _)
            if inner
                .client_keys
                .lock()
                .expect("client keys lock")
                .contains_key(client) =>
        {
            false
        }
        _ => true,
    };
    if !client_key_ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid API key for this client" })),
        )
            .into_response();
    }
    let expires =
        (Utc::now() + Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Json(json!({
        "token": inner.token,
        "client": q.client.unwrap_or_else(|| "default".into()),
        "expires_at": expires,
        "ttl": 3600,
        "github_expires_at": expires,
        "cached": false,
        "renewed": false,
        "purpose": q.purpose,
    }))
    .into_response()
}

/// A default `ghs_`-style token for tests.
pub fn mock_token() -> String {
    "ghs_mockdefault_0000000000000001".to_string()
}
