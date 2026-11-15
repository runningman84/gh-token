//! Prometheus metrics.

use prometheus::{
    Counter, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use std::sync::Arc;

pub const TOKEN_REQ_BUCKETS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0];

pub const GITHUB_REQ_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];

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
    github_rate_limited: IntCounterVec,
    github_cooldown_seconds: IntGauge,
    active_tokens: IntGauge,
    token_lifetime: Histogram,
    rate_limit_rejections: IntCounter,
    github_reachable: IntGauge,
    startup_check_ok: IntGauge,
    github_requests: IntCounterVec,
    github_request_duration: prometheus::HistogramVec,
    // Standard process metrics (names match the Prometheus process
    // collector conventions, no ghts_ prefix).
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
    pub fn new() -> Arc<Self> {
        let registry = Registry::new();

        let token_requests = IntCounterVec::new(
            Opts::new(
                "ghts_token_requests_total",
                "Token requests by client and status.",
            ),
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
            Opts::new(
                "ghts_github_api_errors_total",
                "GitHub API errors by operation.",
            ),
            &["operation"],
        )
        .expect("valid metric");

        let github_rate_limited = IntCounterVec::new(
            Opts::new(
                "ghts_github_rate_limited_total",
                "GitHub rate-limit responses by operation.",
            ),
            &["operation"],
        )
        .expect("valid metric");

        let github_cooldown_seconds = IntGauge::new(
            "ghts_github_cooldown_seconds",
            "Remaining seconds of the GitHub rate-limit backoff cooldown (0 = none).",
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

        let startup_check_ok = IntGauge::new(
            "ghts_startup_check_ok",
            "1 if the startup credential validation passed (0 = never or last attempt failed).",
        )
        .expect("valid metric");

        let github_requests = IntCounterVec::new(
            Opts::new(
                "ghts_github_requests_total",
                "Requests made to the GitHub API by path (normalized) and status. \
                 status is the HTTP code, or \"err\" for transport failures.",
            ),
            &["path", "status"],
        )
        .expect("valid metric");

        let github_request_duration = prometheus::HistogramVec::new(
            HistogramOpts::new(
                "ghts_github_request_duration_seconds",
                "Duration of requests made to the GitHub API by path (normalized).",
            )
            .buckets(GITHUB_REQ_BUCKETS.to_vec()),
            &["path"],
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
            Box::new(github_requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(github_request_duration.clone()) as Box<dyn prometheus::core::Collector>,
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

        Arc::new(Self {
            registry,
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
            process_resident_memory_bytes,
            process_resident_memory_peak_bytes,
            process_virtual_memory_bytes,
            process_cpu_seconds_total,
            process_num_threads,
            process_open_fds,
            process_start_time_seconds,
            process_uptime_seconds,
            process_cpu_ticks_last: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn token_request(&self, client: &str, status: &str) {
        self.token_requests
            .with_label_values(&[client, status])
            .inc();
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

    pub fn github_rate_limited(&self, operation: &str) {
        self.github_rate_limited
            .with_label_values(&[operation])
            .inc();
    }

    pub fn set_github_cooldown(&self, remaining: Option<u64>) {
        self.github_cooldown_seconds
            .set(remaining.unwrap_or(0) as i64);
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

    pub fn set_startup_check_ok(&self, ok: bool) {
        self.startup_check_ok.set(if ok { 1 } else { 0 });
    }

    /// Records one request to the GitHub API (count + duration).
    pub fn github_request(&self, path: &str, status: &str, secs: f64) {
        self.github_requests
            .with_label_values(&[path, status])
            .inc();
        self.github_request_duration
            .with_label_values(&[path])
            .observe(secs);
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
