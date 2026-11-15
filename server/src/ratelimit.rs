//! Per-client sliding-window rate limiting for token requests.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Tracks token-request timestamps per client in a sliding window.
#[derive(Debug)]
pub struct RateLimiter {
    window: Duration,
    max: u64,
    entries: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(window: Duration, max: u64) -> Self {
        Self {
            window,
            max: max.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Records a request for `client` and reports whether it is allowed.
    ///
    /// Returns `Some(retry_after)` when the request is rejected.
    pub fn allow(&self, client: &str) -> Option<Duration> {
        let mut map = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let cutoff = now - self.window;

        let deque = map.entry(client.to_string()).or_default();
        while let Some(&front) = deque.front() {
            if front <= cutoff {
                deque.pop_front();
            } else {
                break;
            }
        }

        if deque.len() as u64 >= self.max {
            let oldest = deque.front().copied().unwrap_or(now);
            let retry_after = (oldest + self.window - now).max(Duration::from_secs(1));
            Some(retry_after)
        } else {
            deque.push_back(now);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleep_ms(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    #[test]
    fn allows_up_to_max() {
        let limiter = RateLimiter::new(Duration::from_secs(60), 3);
        for i in 0..3 {
            assert!(limiter.allow("c").is_none(), "request {i} should pass");
        }
        assert!(
            limiter.allow("c").is_some(),
            "4th request should be rejected"
        );
    }

    #[test]
    fn clients_are_independent() {
        let limiter = RateLimiter::new(Duration::from_secs(60), 1);
        assert!(limiter.allow("a").is_none());
        assert!(limiter.allow("a").is_some());
        assert!(limiter.allow("b").is_none(), "other client unaffected");
    }

    #[test]
    fn window_slides_and_retry_after_is_sane() {
        let limiter = RateLimiter::new(Duration::from_millis(80), 1);
        assert!(limiter.allow("c").is_none());
        assert!(limiter.allow("c").is_some());
        sleep_ms(120);
        assert!(
            limiter.allow("c").is_none(),
            "window expired, allowed again"
        );

        let limiter = RateLimiter::new(Duration::from_secs(60), 1);
        assert!(limiter.allow("c").is_none());
        let retry = limiter.allow("c").unwrap();
        assert!((1..=60).contains(&retry.as_secs()));
    }
}
