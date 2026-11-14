//! Minimal GitHub App API client (app JWTs, app/installation info,
//! installation access tokens, rate limit probe).

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

/// Errors from the GitHub API.
#[derive(Debug, Error)]
#[error("github api error: {0}")]
pub struct GithubError(String);

impl GithubError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Information about the GitHub App itself (`GET /app`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// App-level permission declarations.
    #[serde(default)]
    pub permissions: serde_json::Value,
}

/// Information about an installation (`GET /app/installations/{id}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallationInfo {
    pub id: u64,
    /// Effective installation permissions, e.g. `{"contents":"write"}`.
    #[serde(default)]
    pub permissions: serde_json::Value,
    /// `"all"` or `"selected"`.
    #[serde(default)]
    pub repository_selection: String,
}

/// An installation access token (`POST /app/installations/{id}/access_tokens`).
///
/// GitHub tokens always live exactly one hour; the `expires_at` is what
/// GitHub reports.
#[derive(Debug, Clone)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// A thin GitHub App API client.
#[derive(Debug, Clone)]
pub struct GithubClient {
    http: reqwest::Client,
    app_id: u64,
    installation_id: u64,
    app_key: String,
    base_url: String,
}

impl GithubClient {
    pub fn new(app_id: u64, installation_id: u64, app_key: String, base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("gh-token")
            .build()
            .expect("valid reqwest client");
        Self {
            http,
            app_id,
            installation_id,
            app_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Creates an RS256 JWT authenticating this GitHub App.
    ///
    /// - `iss` = app id
    /// - `iat` = now - 60s (clock skew margin)
    /// - `exp` = iat + 9m (GitHub allows up to 10 minutes)
    pub fn app_jwt(&self) -> Result<String, GithubError> {
        let iat = Utc::now() - chrono::Duration::seconds(60);
        let exp = iat + chrono::Duration::minutes(9);
        let claims = json!({
            "iss": self.app_id,
            "iat": iat.timestamp(),
            "exp": exp.timestamp(),
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.app_key.as_bytes())
            .map_err(|e| GithubError::new(format!("invalid RSA app key: {e}")))?;
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| GithubError::new(format!("failed to sign app JWT: {e}")))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        bearer: Option<&str>,
    ) -> Result<T, GithubError> {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.http.get(&url);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GithubError::new(format!("request to {url} failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GithubError::new(format!(
                "GET {path} returned {status}: {}",
                truncate(&body, 300)
            )));
        }
        resp.json::<T>()
            .await
            .map_err(|e| GithubError::new(format!("invalid JSON from GET {path}: {e}")))
    }

    /// `GET /app` — validates the app key and returns app metadata.
    pub async fn get_app(&self) -> Result<AppInfo, GithubError> {
        let jwt = self.app_jwt()?;
        self.get_json("/app", Some(&jwt)).await
    }

    /// `GET /app/installations/{id}` — validates the installation id.
    pub async fn get_installation(&self) -> Result<InstallationInfo, GithubError> {
        let jwt = self.app_jwt()?;
        self.get_json(
            &format!("/app/installations/{}", self.installation_id),
            Some(&jwt),
        )
        .await
    }

    /// `POST /app/installations/{id}/access_tokens` — creates a new
    /// installation token (valid for one hour, full installation permissions).
    pub async fn create_installation_token(&self) -> Result<InstallationToken, GithubError> {
        let jwt = self.app_jwt();
        let resp = self
            .http
            .post(format!(
                "{}/app/installations/{}/access_tokens",
                self.base_url, self.installation_id
            ))
            .bearer_auth(jwt?)
            .send()
            .await
            .map_err(|e| GithubError::new(format!("access_tokens request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GithubError::new(format!(
                "POST /app/installations/{}/access_tokens returned {status}: {}",
                self.installation_id,
                truncate(&body, 300)
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GithubError::new(format!("invalid JSON from access_tokens: {e}")))?;
        let token = body["token"]
            .as_str()
            .ok_or_else(|| GithubError::new("access_tokens response missing `token`"))?
            .to_string();
        let expires_at_raw = body["expires_at"]
            .as_str()
            .ok_or_else(|| GithubError::new("access_tokens response missing `expires_at`"))?;
        let expires_at = DateTime::parse_from_rfc3339(expires_at_raw)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| {
                GithubError::new(format!("access_tokens: unparseable expires_at {expires_at_raw:?}: {e}"))
            })?;
        Ok(InstallationToken { token, expires_at })
    }

    /// `GET /user` with an installation token — proves the token works and
    /// returns the bot login.
    pub async fn user_for_token(&self, token: &str) -> Result<String, GithubError> {
        let user: serde_json::Value = self.get_json("/user", Some(token)).await?;
        user["login"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| GithubError::new("GET /user response missing `login`"))
    }

    /// `GET /rate_limit` — cheap unauthenticated reachability probe.
    pub async fn probe(&self) -> Result<(), GithubError> {
        let url = format!("{}/rate_limit", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| GithubError::new(format!("probe request to {url} failed: {e}")))?;
        let status = resp.status();
        if !(status.is_success() || status == reqwest::StatusCode::FORBIDDEN) {
            // 403 with rate-limit headers still proves reachability, so we
            // accept success plus 403-with-limits here; any other 4xx/5xx fails.
            if status.is_client_error() && status.as_u16() != 403 {
                return Err(GithubError::new(format!("probe returned {status}")));
            }
            if status.is_server_error() {
                return Err(GithubError::new(format!("probe returned {status}")));
            }
        }
        Ok(())
    }
}

fn truncate(s: &str, max: usize) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() <= max {
        return s;
    }
    match std::str::from_utf8(&bytes[..max]) {
        Ok(t) => t,
        Err(e) => &s[..e.valid_up_to()],
    }
}
