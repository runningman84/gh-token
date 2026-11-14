//! Prometheus metrics.

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};
use std::sync::Arc;

pub const TOKEN_REQ_BUCKETS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0];

/// All Prometheus metrics exposed by the server.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    token_requests: IntCounterVec,
    token_request_duration: prometheus::HistogramVec,
    tokens_issued: IntCounter,
    tokens_renewed: IntCounter,
    tokens_served_cached: IntCounter,
    tokens_expired: IntCounter,
    github_api_errors: IntCounterVec,
    active_tokens: IntGauge,
    token_lifetime: Histogram,
    rate_limit_rejections: IntCounter,
    github_reachable: IntGauge,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        let registry = Registry::new();

        let token_requests = IntCounterVec::new(
            Opts::new("ghts_token_requests_total", "Token requests by client and status."),
            &["client", "status"],
        )
        .expect("valid metric");

        let token_request_duration = prometheus::HistogramVec::new(
            HistogramOpts::new(
                "ghts_token_request_duration_seconds",
                "Token request latency by client.",
            )
            .buckets(TOKEN_REQ_BUCKETS.to_vec()),
            &["client"],
        )
        .expect("valid metric");

        let tokens_issued = IntCounter::new(
            "ghts_tokens_issued_total",
            "Installation tokens created from GitHub.",
        )
        .expect("valid metric");

        let tokens_renewed = IntCounter::new(
            "ghts_tokens_renewed_total",
            "Tokens renewed (fresh GitHub token for an existing client).",
        )
        .expect("valid metric");

        let tokens_served_cached = IntCounter::new(
            "ghts_tokens_served_cached_total",
            "Token requests served from the cache.",
        )
        .expect("valid metric");

        let tokens_expired = IntCounter::new(
            "ghts_tokens_expired_total",
            "Tokens that expired (soft or GitHub-side).",
        )
        .expect("valid metric");

        let github_api_errors = IntCounterVec::new(
            Opts::new("ghts_github_api_errors_total", "GitHub API errors by operation."),
            &["operation"],
        )
        .expect("valid metric");

        let active_tokens = IntGauge::new(
            "ghts_active_tokens",
            "Currently tracked tokens in the cache.",
        )
        .expect("valid metric");

        let token_lifetime = Histogram::with_opts(
            HistogramOpts::new(
                "ghts_token_lifetime_seconds",
                "Lifetime of issued tokens (from issuance to expiry).",
            )
            .buckets(vec![60.0, 300.0, 600.0, 900.0, 1800.0, 3600.0]),
        )
        .expect("valid metric");

        let rate_limit_rejections = IntCounter::new(
            "ghts_rate_limit_rejections_total",
            "Token requests rejected by the per-client rate limiter.",
        )
        .expect("valid metric");

        let github_reachable = IntGauge::new(
            "ghts_github_reachable",
            "1 if the GitHub API was reachable on the last readiness probe.",
        )
        .expect("valid metric");

        for m in [
            Box::new(token_requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(token_request_duration.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(tokens_issued.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(tokens_renewed.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(tokens_served_cached.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(tokens_expired.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_api_errors.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(active_tokens.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(token_lifetime.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(rate_limit_rejections.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_reachable.clone()) as Box<dyn prometheus::core::Collector>,
        ] {
            registry.register(m).expect("metric registration");
        }

        Arc::new(Self {
            registry,
            token_requests,
            token_request_duration,
            tokens_issued,
            tokens_renewed,
            tokens_served_cached,
            tokens_expired,
            github_api_errors,
            active_tokens,
            token_lifetime,
            rate_limit_rejections,
            github_reachable,
        })
    }

    pub fn token_request(&self, client: &str, status: &str) {
        self.token_requests.with_label_values(&[client, status]).inc();
    }

    pub fn token_request_duration(&self, client: &str, secs: f64) {
        self.token_request_duration
            .with_label_values(&[client])
            .observe(secs);
    }

    pub fn tokens_issued(&self) {
        self.tokens_issued.inc();
    }

    pub fn tokens_renewed(&self) {
        self.tokens_renewed.inc();
    }

    pub fn tokens_served_cached(&self) {
        self.tokens_served_cached.inc();
    }

    pub fn tokens_expired(&self) {
        self.tokens_expired.inc();
    }

    pub fn github_api_error(&self, operation: &str) {
        self.github_api_errors.with_label_values(&[operation]).inc();
    }

    pub fn set_active_tokens(&self, count: usize) {
        self.active_tokens.set(count as i64);
    }

    pub fn token_lifetime(&self, secs: f64) {
        if secs > 0.0 {
            self.token_lifetime.observe(secs);
        }
    }

    pub fn rate_limit_rejected(&self) {
        self.rate_limit_rejections.inc();
    }

    pub fn set_github_reachable(&self, reachable: bool) {
        self.github_reachable.set(if reachable { 1 } else { 0 });
    }

    /// Encodes all metrics in Prometheus text exposition format.
    pub fn encode(&self) -> String {
        TextEncoder::new()
            .encode_to_string(&self.registry.gather())
            .expect("encoding metrics")
    }
}
