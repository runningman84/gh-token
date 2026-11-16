//! Prometheus metrics.
//!
//! Every application metric (the `ghts_*` family) carries the GitHub App id
//! and installation id as the leading `app_id` / `install_id` labels, so a
//! single Prometheus scrape is self-describing even when several instances
//! (one per GitHub App) feed the same backend. The standard `process_*`
//! metrics are left label-free to follow the Prometheus process-collector
//! conventions.

use prometheus::{
    Counter, HistogramOpts, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};
use std::sync::Arc;

pub const TOKEN_REQ_BUCKETS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0];

pub const GITHUB_REQ_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];

/// All Prometheus metrics exposed by the server.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    /// GitHub App id, as a stable metric label value.
    app_id: String,
    /// GitHub App installation id, as a stable metric label value.
    install_id: String,
    token_requests: IntCounterVec,
    token_request_duration: prometheus::HistogramVec,
    tokens_issued: IntCounterVec,
    tokens_renewed: IntCounterVec,
    tokens_served_cached: IntCounterVec,
    tokens_expired: IntCounterVec,
    github_api_errors: IntCounterVec,
    github_rate_limited: IntCounterVec,
    github_cooldown_seconds: IntGaugeVec,
    active_tokens: IntGaugeVec,
    token_lifetime: prometheus::HistogramVec,
    rate_limit_rejections: IntCounterVec,
    github_reachable: IntGaugeVec,
    startup_check_ok: IntGaugeVec,
    github_requests: IntCounterVec,
    github_request_duration: prometheus::HistogramVec,
    // GitHub API rate limit metrics (from GET /rate_limit)
    github_rate_limit_limit: IntGaugeVec,
    github_rate_limit_used: IntGaugeVec,
    github_rate_limit_remaining: IntGaugeVec,
    github_rate_limit_reset: IntGaugeVec,
    // Standard process metrics (names match the Prometheus process
    // collector conventions, no ghts_ prefix, no app/installation labels).
    process_resident_memory_bytes: IntGauge,
    process_resident_memory_peak_bytes: IntGauge,
    process_virtual_memory_bytes: IntGauge,
    process_cpu_seconds_total: Counter,
    process_num_threads: IntGauge,
    process_open_fds: IntGauge,
    process_start_time_seconds: IntGauge,
    process_uptime_seconds: IntGauge,
    /// Last reported CPU tick count (to increment the CPU counter by the
    /// delta; CPU time is cumulative per process).
    process_cpu_ticks_last: std::sync::atomic::AtomicU64,
}

impl Metrics {
    /// Creates the metric set for one GitHub App installation.
    ///
    /// `app_id` and `installation_id` are recorded on every `ghts_*` metric
    /// as the leading `app_id` / `install_id` labels.
    pub fn new(app_id: u64, installation_id: u64) -> Arc<Self> {
        let registry = Registry::new();
        let app_id = app_id.to_string();
        let install_id = installation_id.to_string();

        let token_requests = IntCounterVec::new(
            Opts::new(
                "ghts_token_requests_total",
                "Token requests by app, installation, client and status.",
            ),
            &["app_id", "install_id", "client", "status"],
        )
        .expect("valid metric");

        let token_request_duration = prometheus::HistogramVec::new(
            HistogramOpts::new(
                "ghts_token_request_duration_seconds",
                "Token request latency by app, installation and client.",
            )
            .buckets(TOKEN_REQ_BUCKETS.to_vec()),
            &["app_id", "install_id", "client"],
        )
        .expect("valid metric");

        let tokens_issued = IntCounterVec::new(
            Opts::new(
                "ghts_tokens_issued_total",
                "Installation tokens created from GitHub.",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let tokens_renewed = IntCounterVec::new(
            Opts::new(
                "ghts_tokens_renewed_total",
                "Tokens renewed (fresh GitHub token for an existing client).",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let tokens_served_cached = IntCounterVec::new(
            Opts::new(
                "ghts_tokens_served_cached_total",
                "Token requests served from the cache.",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let tokens_expired = IntCounterVec::new(
            Opts::new(
                "ghts_tokens_expired_total",
                "Tokens that expired (soft or GitHub-side).",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let github_api_errors = IntCounterVec::new(
            Opts::new(
                "ghts_github_api_errors_total",
                "GitHub API failures by operation (transport errors and unexpected \
                 HTTP responses; rate-limited responses are counted in \
                 ghts_github_rate_limited_total instead).",
            ),
            &["app_id", "install_id", "operation"],
        )
        .expect("valid metric");

        let github_rate_limited = IntCounterVec::new(
            Opts::new(
                "ghts_github_rate_limited_total",
                "GitHub rate-limit responses by operation.",
            ),
            &["app_id", "install_id", "operation"],
        )
        .expect("valid metric");

        let github_cooldown_seconds = IntGaugeVec::new(
            Opts::new(
                "ghts_github_cooldown_seconds",
                "Remaining seconds of the GitHub rate-limit backoff cooldown (0 = none).",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let active_tokens = IntGaugeVec::new(
            Opts::new(
                "ghts_active_tokens",
                "Currently tracked tokens in the cache.",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let token_lifetime = prometheus::HistogramVec::new(
            HistogramOpts::new(
                "ghts_token_lifetime_seconds",
                "Lifetime of issued tokens (from issuance to expiry).",
            )
            .buckets(vec![60.0, 300.0, 600.0, 900.0, 1800.0, 3600.0]),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let rate_limit_rejections = IntCounterVec::new(
            Opts::new(
                "ghts_rate_limit_rejections_total",
                "Token requests rejected by the per-client rate limiter.",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let github_reachable = IntGaugeVec::new(
            Opts::new(
                "ghts_github_reachable",
                "1 if the GitHub API was reachable on the last readiness probe.",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let startup_check_ok = IntGaugeVec::new(
            Opts::new(
                "ghts_startup_check_ok",
                "1 if the startup credential validation passed (0 = never or last attempt failed).",
            ),
            &["app_id", "install_id"],
        )
        .expect("valid metric");

        let build_info = IntGaugeVec::new(
            Opts::new(
                "ghts_build_info",
                "Build information (version) of the server.",
            ),
            &["app_id", "install_id", "version"],
        )
        .expect("valid metric");
        build_info
            .with_label_values(&[&app_id, &install_id, env!("CARGO_PKG_VERSION")])
            .set(1);

        let github_requests = IntCounterVec::new(
            Opts::new(
                "ghts_github_requests_total",
                "Requests made to the GitHub API by path and status. \
                 status is the HTTP code, or \"err\" for transport failures.",
            ),
            &["app_id", "install_id", "path", "status"],
        )
        .expect("valid metric");

        // GitHub API rate limit metrics (from GET /rate_limit)
        // Label: resource (e.g., "core", "search", "graphql", "integration_manifest")
        let github_rate_limit_limit = IntGaugeVec::new(
            Opts::new(
                "ghts_github_rate_limit_limit",
                "GitHub API rate limit (maximum requests) by resource.",
            ),
            &["app_id", "install_id", "resource"],
        )
        .expect("valid metric");

        let github_rate_limit_used = IntGaugeVec::new(
            Opts::new(
                "ghts_github_rate_limit_used",
                "GitHub API rate limit used (requests consumed) by resource.",
            ),
            &["app_id", "install_id", "resource"],
        )
        .expect("valid metric");

        let github_rate_limit_remaining = IntGaugeVec::new(
            Opts::new(
                "ghts_github_rate_limit_remaining",
                "GitHub API rate limit remaining (requests available) by resource.",
            ),
            &["app_id", "install_id", "resource"],
        )
        .expect("valid metric");

        let github_rate_limit_reset = IntGaugeVec::new(
            Opts::new(
                "ghts_github_rate_limit_reset_timestamp",
                "GitHub API rate limit reset time (Unix timestamp) by resource.",
            ),
            &["app_id", "install_id", "resource"],
        )
        .expect("valid metric");

        let github_request_duration = prometheus::HistogramVec::new(
            HistogramOpts::new(
                "ghts_github_request_duration_seconds",
                "Duration of requests made to the GitHub API by path.",
            )
            .buckets(GITHUB_REQ_BUCKETS.to_vec()),
            &["app_id", "install_id", "path"],
        )
        .expect("valid metric");

        let process_resident_memory_bytes = IntGauge::new(
            "process_resident_memory_bytes",
            "Resident memory size of the process in bytes.",
        )
        .expect("valid metric");

        let process_resident_memory_peak_bytes = IntGauge::new(
            "process_resident_memory_peak_bytes",
            "Peak resident memory size of the process in bytes.",
        )
        .expect("valid metric");

        let process_virtual_memory_bytes = IntGauge::new(
            "process_virtual_memory_bytes",
            "Virtual memory size of the process in bytes.",
        )
        .expect("valid metric");

        let process_cpu_seconds_total = Counter::new(
            "process_cpu_seconds_total",
            "Total user and system CPU time spent in seconds.",
        )
        .expect("valid metric");

        let process_num_threads = IntGauge::new(
            "process_num_threads",
            "Number of OS threads in the process.",
        )
        .expect("valid metric");

        let process_open_fds =
            IntGauge::new("process_open_fds", "Number of open file descriptors.")
                .expect("valid metric");

        let process_start_time_seconds = IntGauge::new(
            "process_start_time_seconds",
            "Unix timestamp of the process start.",
        )
        .expect("valid metric");

        let process_uptime_seconds = IntGauge::new(
            "process_uptime_seconds",
            "Seconds since the process started.",
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
            Box::new(github_rate_limited.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_cooldown_seconds.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(active_tokens.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(token_lifetime.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(rate_limit_rejections.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_reachable.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(startup_check_ok.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(build_info.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_request_duration.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_rate_limit_limit.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_rate_limit_used.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_rate_limit_remaining.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_rate_limit_reset.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_resident_memory_bytes.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_resident_memory_peak_bytes.clone())
                as Box<dyn prometheus::core::Collector>,
            Box::new(process_virtual_memory_bytes.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_cpu_seconds_total.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_num_threads.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_open_fds.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_start_time_seconds.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(process_uptime_seconds.clone()) as Box<dyn prometheus::core::Collector>,
        ] {
            registry.register(m).expect("metric registration");
        }

        let metrics = Arc::new(Self {
            registry,
            app_id,
            install_id,
            token_requests,
            token_request_duration,
            tokens_issued,
            tokens_renewed,
            tokens_served_cached,
            tokens_expired,
            github_api_errors,
            github_rate_limited,
            github_cooldown_seconds,
            active_tokens,
            token_lifetime,
            rate_limit_rejections,
            github_reachable,
            startup_check_ok,
            github_requests,
            github_request_duration,
            github_rate_limit_limit,
            github_rate_limit_used,
            github_rate_limit_remaining,
            github_rate_limit_reset,
            process_resident_memory_bytes,
            process_resident_memory_peak_bytes,
            process_virtual_memory_bytes,
            process_cpu_seconds_total,
            process_num_threads,
            process_open_fds,
            process_start_time_seconds,
            process_uptime_seconds,
            process_cpu_ticks_last: std::sync::atomic::AtomicU64::new(0),
        });

        // Pre-initialize the always-present state gauges so a scrape
        // immediately after startup exposes a stable series. (Counters with
        // dynamic labels stay absent until first use; absence means zero.)
        metrics.set_github_cooldown(None);
        metrics.set_active_tokens(0);
        metrics.set_github_reachable(false);
        metrics.set_startup_check_ok(false);
        metrics
    }

    /// The leading `app_id` / `install_id` label values shared by every
    /// `ghts_*` metric.
    fn id_labels(&self) -> [&str; 2] {
        [&self.app_id, &self.install_id]
    }

    pub fn token_request(&self, client: &str, status: &str) {
        self.token_requests
            .with_label_values(&[&self.app_id, &self.install_id, client, status])
            .inc();
    }

    pub fn token_request_duration(&self, client: &str, secs: f64) {
        self.token_request_duration
            .with_label_values(&[&self.app_id, &self.install_id, client])
            .observe(secs);
    }

    pub fn tokens_issued(&self) {
        self.tokens_issued
            .with_label_values(&self.id_labels())
            .inc();
    }

    pub fn tokens_renewed(&self) {
        self.tokens_renewed
            .with_label_values(&self.id_labels())
            .inc();
    }

    pub fn tokens_served_cached(&self) {
        self.tokens_served_cached
            .with_label_values(&self.id_labels())
            .inc();
    }

    pub fn tokens_expired(&self) {
        self.tokens_expired
            .with_label_values(&self.id_labels())
            .inc();
    }

    pub fn github_api_error(&self, operation: &str) {
        self.github_api_errors
            .with_label_values(&[&self.app_id, &self.install_id, operation])
            .inc();
    }

    pub fn github_rate_limited(&self, operation: &str) {
        self.github_rate_limited
            .with_label_values(&[&self.app_id, &self.install_id, operation])
            .inc();
    }

    pub fn set_github_cooldown(&self, remaining: Option<u64>) {
        self.github_cooldown_seconds
            .with_label_values(&self.id_labels())
            .set(remaining.unwrap_or(0) as i64);
    }

    pub fn set_active_tokens(&self, count: usize) {
        self.active_tokens
            .with_label_values(&self.id_labels())
            .set(count as i64);
    }

    pub fn token_lifetime(&self, secs: f64) {
        if secs > 0.0 {
            self.token_lifetime
                .with_label_values(&self.id_labels())
                .observe(secs);
        }
    }

    pub fn rate_limit_rejected(&self) {
        self.rate_limit_rejections
            .with_label_values(&self.id_labels())
            .inc();
    }

    pub fn set_github_reachable(&self, reachable: bool) {
        self.github_reachable
            .with_label_values(&self.id_labels())
            .set(if reachable { 1 } else { 0 });
    }

    pub fn set_startup_check_ok(&self, ok: bool) {
        self.startup_check_ok
            .with_label_values(&self.id_labels())
            .set(if ok { 1 } else { 0 });
    }

    /// Records one request to the GitHub API (count + duration).
    ///
    /// `path` is the request path exactly as sent (the real ids are kept,
    /// not masked), so the label carries the concrete GitHub resource.
    pub fn github_request(&self, path: &str, status: &str, secs: f64) {
        self.github_requests
            .with_label_values(&[&self.app_id, &self.install_id, path, status])
            .inc();
        self.github_request_duration
            .with_label_values(&[&self.app_id, &self.install_id, path])
            .observe(secs);
    }

    /// Updates GitHub API rate limit metrics from a /rate_limit response.
    /// `resource` is the rate limit category (e.g., "core", "search", "graphql",
    /// "integration_manifest").
    pub fn github_rate_limit(
        &self,
        resource: &str,
        limit: i64,
        used: i64,
        remaining: i64,
        reset: i64,
    ) {
        self.github_rate_limit_limit
            .with_label_values(&[&self.app_id, &self.install_id, resource])
            .set(limit);
        self.github_rate_limit_used
            .with_label_values(&[&self.app_id, &self.install_id, resource])
            .set(used);
        self.github_rate_limit_remaining
            .with_label_values(&[&self.app_id, &self.install_id, resource])
            .set(remaining);
        self.github_rate_limit_reset
            .with_label_values(&[&self.app_id, &self.install_id, resource])
            .set(reset);
    }

    /// Refreshes the standard `process_*` metrics for the current process
    /// (start time/uptime on all platforms; memory/CPU/threads/fds on
    /// Linux from `/proc/self`). Called on each `/metrics` scrape.
    pub fn update_process_metrics(&self) {
        let start = crate::process::start_time_unix();
        self.process_start_time_seconds.set(start as i64);
        self.process_uptime_seconds
            .set(crate::process::uptime_seconds() as i64);
        if let Some(s) = crate::process::current() {
            self.process_resident_memory_bytes
                .set(s.resident_memory_bytes as i64);
            self.process_resident_memory_peak_bytes
                .set(s.resident_memory_peak_bytes as i64);
            self.process_virtual_memory_bytes
                .set(s.virtual_memory_bytes as i64);
            // CPU time is cumulative; the counter tracks the delta since
            // the last scrape (starts at process start on first scrape).
            let prev = self
                .process_cpu_ticks_last
                .swap(s.cpu_ticks, std::sync::atomic::Ordering::Relaxed);
            self.process_cpu_seconds_total
                .inc_by((s.cpu_ticks - prev) as f64 / 100.0);
            self.process_num_threads.set(s.num_threads as i64);
            self.process_open_fds.set(s.open_fds as i64);
        }
    }

    /// Encodes all metrics in Prometheus text exposition format.
    pub fn encode(&self) -> String {
        TextEncoder::new()
            .encode_to_string(&self.registry.gather())
            .expect("encoding metrics")
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn ghts_metrics_carry_app_and_installation_labels() {
        let metrics = Metrics::new(111, 222);
        metrics.tokens_issued();
        metrics.token_request("hermes", "200");
        metrics.github_request("/app/installations/222", "200", 0.05);
        let text = metrics.encode();

        // Scalar-turned-vec metric: the ids are the only labels.
        assert!(
            text.contains("ghts_tokens_issued_total{app_id=\"111\",install_id=\"222\"} 1"),
            "tokens_issued should carry app_id/install_id:\n{text}"
        );
        // Multi-label metric: the ids are present alongside the existing
        // labels (the encoder sorts label names alphabetically).
        assert!(
            text.contains(
                "ghts_token_requests_total{app_id=\"111\",client=\"hermes\",install_id=\"222\",status=\"200\"} 1"
            ),
            "token_requests should carry app_id/install_id:\n{text}"
        );
        // The GitHub request path keeps the real installation id (no `{id}`
        // masking) and the ids lead the label set.
        assert!(
            text.contains(
                "ghts_github_requests_total{app_id=\"111\",install_id=\"222\",path=\"/app/installations/222\",status=\"200\"} 1"
            ),
            "github_requests should carry the real path and the ids:\n{text}"
        );
        // Process metrics stay label-free.
        assert!(
            text.contains("process_start_time_seconds "),
            "process metrics should remain label-free:\n{text}"
        );
    }
}
