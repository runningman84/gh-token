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
    /// The purpose this token was requested for (advisory metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

/// Maximum length of a token request purpose.
pub const MAX_PURPOSE_LEN: usize = 256;

/// Validates a token request purpose: 1-256 chars, no control characters
/// (so it stays safe as a single log line).
pub fn validate_purpose(purpose: &str) -> Result<(), String> {
    let len = purpose.chars().count();
    if len == 0 {
        return Err("purpose must not be empty".into());
    }
    if len > MAX_PURPOSE_LEN {
        return Err(format!(
            "purpose must be at most {MAX_PURPOSE_LEN} characters (got {len})"
        ));
    }
    if purpose.chars().any(char::is_control) {
        return Err("purpose must not contain control characters".into());
    }
    Ok(())
}

/// Validates a client identifier: lowercase alphanumerics, `_`, `-`, max 64 chars.
///
/// This is the same character set Kubernetes uses for pod names, so any
/// in-cluster workload can identify itself with its pod name.
pub fn validate_client_id(client: &str) -> Result<(), String> {
    if client.is_empty() {
        return Err(
            "invalid client identifier (expected 1-64 lowercase alphanumerics, '_' or '-')".into(),
        );
    }
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

/// GitHub token prefixes (installation tokens, PATs, fine-grained PATs).
const GH_TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];

/// Masks GitHub tokens anywhere in a string, for safe logging.
///
/// `{"token":"ghs_5745…"}` -> `{"token":"ghs_5745… (len 383)"}`.
/// Short strings after a prefix (e.g. prose like `ghs_x`) are left alone.
pub fn mask_gh_tokens(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let mut best: Option<(usize, &str)> = None;
        for prefix in GH_TOKEN_PREFIXES {
            if let Some(i) = rest.find(prefix)
                && best.is_none_or(|(bi, _)| i < bi)
            {
                best = Some((i, prefix));
            }
        }
        let Some((i, prefix)) = best else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let body_start = i + prefix.len();
        let end = rest[body_start..]
            .find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '.' | '-'))
            .map(|o| body_start + o)
            .unwrap_or(rest.len());
        let token = &rest[i..end];
        if end - body_start >= 6 {
            out.push_str(&mask_token(token));
        } else {
            out.push_str(token);
        }
        rest = &rest[end..];
    }
    out
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
        assert!(
            !masked.contains(&token[8..token.len().saturating_sub(4)]),
            "masked leaks middle"
        );
        assert!(
            masked.starts_with("ghs_abcd"),
            "masked keeps prefix: {masked}"
        );
        assert!(
            masked.ends_with(&(format!("(len {})", token.len()))),
            "masked has length"
        );

        let short = mask_token("ghs_abc");
        assert!(short.contains("****"));
        assert!(!short.contains("ghs_abc"));
    }

    #[test]
    fn masking_embedded_tokens() {
        let s = r#"{"token":"ghs_5745733_eyJhbGciOiJIUzI1NiJ9.payload.signature","key":"ghp_ABCDEF1234567890"}"#;
        let masked = mask_gh_tokens(s);
        assert!(
            !masked.contains("eyJhbGciOiJIUzI1NiJ9.payload.signature"),
            "{masked}"
        );
        assert!(!masked.contains("ghp_ABCDEF1234567890"), "{masked}");
        assert!(masked.contains("ghs_5745"), "{masked}");
        assert!(masked.contains("(len 50)"), "{masked}");

        // Short prefix-lookalikes and plain text pass through unchanged.
        assert_eq!(
            mask_gh_tokens("ghs_abc and ghp_1234"),
            "ghs_abc and ghp_1234"
        );
        assert_eq!(mask_gh_tokens("no tokens here"), "no tokens here");
    }
}
