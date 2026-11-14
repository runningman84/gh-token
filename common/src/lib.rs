//! Shared types and helpers for the gh-token server and client.

use serde::{Deserialize, Serialize};

/// Regex: lowercase alphanumerics, underscore, dash; 1-64 chars.
pub const CLIENT_ID_RE: &str = r"^[a-z0-9_-]{1,64}$";

/// Response returned by the server for token requests.
///
/// `expires_at` / `ttl` describe the *effective* lifetime the server
/// guarantees (which may be shorter than the underlying GitHub token),
/// while `github_expires_at` is the actual expiration reported by
/// GitHub (installation tokens are valid for one hour).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The GitHub installation access token.
    pub token: String,
    /// The client identifier this token was issued for.
    pub client: String,
    /// Effective (server-enforced) expiration, RFC 3339 / ISO 8601.
    pub expires_at: String,
    /// Seconds remaining until the effective expiration.
    pub ttl: u64,
    /// Expiration of the underlying GitHub token, RFC 3339 / ISO 8601.
    pub github_expires_at: String,
    /// Whether the token was served from the server cache.
    pub cached: bool,
    /// Whether an existing token was renewed with a fresh GitHub token.
    pub renewed: bool,
}

/// Validates a client identifier: lowercase alphanumerics, `_`, `-`, max 64 chars.
///
/// This is the same character set Kubernetes uses for pod names, so any
/// in-cluster workload can identify itself with its pod name.
pub fn validate_client_id(client: &str) -> Result<(), String> {
    if !is_valid_client_id(client) {
        return Err(format!(
            "invalid client identifier {client:?} (expected lowercase alphanumerics, '_' or '-', max 64 chars)"
        ));
    }
    Ok(())
}

pub fn is_valid_client_id(client: &str) -> bool {
    let bytes = client.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Masks a token for safe logging: keeps a short prefix and the length.
///
/// `ghs_abc123...` -> `ghs_ab…3x (len 86)`.
pub fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return format!("**** (len {})", token.len());
    }
    let head = &token[..8.min(token.len())];
    let tail = &token[token.len().saturating_sub(4)..];
    format!("{head}…{tail} (len {})", token.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_client_ids() {
        let ids: Vec<String> = vec![
            "hermes".to_string(),
            "pi".to_string(),
            "agent-runner-01".to_string(),
            "a-b_c".to_string(),
            "x".repeat(64),
        ];
        for id in &ids {
            assert!(validate_client_id(id).is_ok(), "{id:?} should be valid");
        }
    }

    #[test]
    fn invalid_client_ids() {
        let ids: Vec<String> = vec![
            String::new(),
            "UpperCase".to_string(),
            "has space".to_string(),
            "dot.name".to_string(),
            "semi;colon".to_string(),
            "slash/here".to_string(),
            "a".repeat(65),
            "ünïcode".to_string(),
        ];
        for id in &ids {
            assert!(validate_client_id(id).is_err(), "{id:?} should be invalid");
        }
    }

    #[test]
    fn masking() {
        let token = "ghs_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let masked = mask_token(token);
        assert!(!masked.contains(&token[8..token.len().saturating_sub(4)]), "masked leaks middle");
        assert!(masked.starts_with("ghs_abcd"), "masked keeps prefix: {masked}");
        assert!(masked.ends_with(&(format!("(len {})", token.len()))), "masked has length");

        let short = mask_token("ghs_abc");
        assert!(short.contains("****"));
        assert!(!short.contains("ghs_abc"));
    }
}
