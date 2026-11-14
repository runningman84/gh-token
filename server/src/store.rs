//! Per-client cached token records.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::debug;

/// A cached installation token for one client, with the server-enforced
/// (soft) lifetime.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    /// The GitHub installation token (masked in all logs).
    pub token: String,
    /// The TTL (in seconds) this token was issued with.
    pub ttl: u64,
    pub issued_at: DateTime<Utc>,
    /// Effective expiration: `min(issued_at + ttl, github_expires_at)`.
    pub soft_expires_at: DateTime<Utc>,
    /// The GitHub-reported expiration (one hour after creation).
    pub github_expires_at: DateTime<Utc>,
    /// When the client last requested this token.
    pub last_used: DateTime<Utc>,
}

impl TokenRecord {
    pub fn new(token: String, ttl: u64, github_expires_at: DateTime<Utc>) -> Self {
        let now = Utc::now();
        let soft_expires_at =
            (now + chrono::Duration::seconds(ttl as i64)).min(github_expires_at);
        Self {
            token,
            ttl,
            issued_at: now,
            soft_expires_at,
            github_expires_at,
            last_used: now,
        }
    }

    /// Seconds remaining until the effective (soft) expiration.
    pub fn remaining(&self) -> Duration {
        (self.soft_expires_at - Utc::now())
            .to_std()
            .unwrap_or_default()
    }

    /// Whether the underlying GitHub token is expired on GitHub's side.
    pub fn github_expired(&self) -> bool {
        Utc::now() > self.github_expires_at
    }

    /// Updates `last_used` to now.
    pub fn touch(&mut self) {
        self.last_used = Utc::now();
    }
}

/// In-memory token cache keyed by client id.
#[derive(Debug, Default)]
pub struct TokenStore {
    inner: Mutex<HashMap<String, TokenRecord>>,
}

impl TokenStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn get(&self, client: &str) -> Option<TokenRecord> {
        self.inner.lock().await.get(client).cloned()
    }

    pub async fn put(&self, client: &str, record: TokenRecord) {
        debug!(client, "storing token record");
        self.inner
            .lock()
            .await
            .insert(client.to_string(), record);
    }

    /// Marks a token as used (keeps it from being dropped by the watcher).
    pub async fn touch(&self, client: &str) {
        if let Some(record) = self.inner.lock().await.get_mut(client) {
            record.touch();
        }
    }

    pub async fn remove(&self, client: &str) {
        self.inner.lock().await.remove(client);
    }

    /// Snapshot of all records (client -> record).
    pub async fn snapshot(&self) -> Vec<(String, TokenRecord)> {
        self.inner
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Removes records whose underlying GitHub token is expired and
    /// inactive clients whose token was soft-expired well beyond the
    /// activity window. Returns the number of removed entries.
    #[allow(dead_code)]
    pub async fn prune(&self, activity_window: Duration) -> usize {
        let now = Utc::now();
        let mut map = self.inner.lock().await;
        let before = map.len();
        map.retain(|_, rec| {
            if rec.github_expired() {
                return false;
            }
            let inactive_for = now - rec.last_used;
            let soft_expired = now > rec.soft_expires_at;
            !(soft_expired
                && inactive_for > chrono::Duration::from_std(activity_window).unwrap_or_default())
        });
        let removed = before - map.len();
        if removed > 0 {
            debug!(removed, "pruned token store");
        }
        removed
    }
}
