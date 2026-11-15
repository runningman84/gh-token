//! Resource usage of the current process (Linux, via `/proc/self`).
//!
//! Powers the standard Prometheus `process_*` metrics exposed on
//! `/metrics` (names intentionally match the Prometheus process
//! collector conventions, without a prefix).

use std::sync::OnceLock;

/// Snapshot of the current process's resource usage.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessStats {
    /// Resident set size in bytes.
    pub resident_memory_bytes: u64,
    /// Peak resident set size in bytes.
    pub resident_memory_peak_bytes: u64,
    /// Virtual memory size in bytes.
    pub virtual_memory_bytes: u64,
    /// Total CPU time (user + system) in seconds.
    pub cpu_seconds: f64,
    /// Total CPU time (user + system) in clock ticks (CLK_TCK = 100).
    pub cpu_ticks: u64,
    /// Number of threads.
    pub num_threads: u64,
    /// Number of open file descriptors.
    pub open_fds: u64,
}

/// Unix timestamp of the process start (captured once, on first use).
pub fn start_time_unix() -> u64 {
    static START: OnceLock<u64> = OnceLock::new();
    *START.get_or_init(now_unix)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Seconds since process start.
pub fn uptime_seconds() -> f64 {
    now_unix() as f64 - start_time_unix() as f64
}

/// Reads the current process's resource usage from `/proc` (Linux only;
/// returns `None` on other platforms).
#[cfg(target_os = "linux")]
pub fn current() -> Option<ProcessStats> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut stats = ProcessStats::default();
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // Lines like Name/State carry no number; skip anything that does
        // not start with one.
        let Some(num) = value
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        match key {
            "VmRSS" => stats.resident_memory_bytes = num * 1024,
            "VmHWM" => stats.resident_memory_peak_bytes = num * 1024,
            "VmSize" => stats.virtual_memory_bytes = num * 1024,
            "Threads" => stats.num_threads = num,
            _ => {}
        }
    }
    if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
        stats.cpu_ticks = cpu_ticks(&stat).unwrap_or(0);
        stats.cpu_seconds = stats.cpu_ticks as f64 / 100.0;
    }
    stats.open_fds = std::fs::read_dir("/proc/self/fd")
        .map(|entries| {
            entries
                .filter(Result::<std::fs::DirEntry, _>::is_ok)
                .count() as u64
        })
        .unwrap_or(0);
    Some(stats)
}

#[cfg(not(target_os = "linux"))]
pub fn current() -> Option<ProcessStats> {
    None
}

/// Total CPU clock ticks (user + system) from the contents of
/// `/proc/self/stat` (fields 14/15, CLK_TCK = 100 ticks per second). The
/// comm name (field 2) may contain spaces and parentheses, so parsing
/// starts after the last `)`.
pub fn cpu_ticks(stat: &str) -> Option<u64> {
    let fields = stat
        .rsplit(')')
        .next()?
        .split_whitespace()
        .collect::<Vec<_>>();
    // After comm, index 0 is field 3 (state); field N maps to index N - 3.
    let utime = fields.get(14 - 3)?.parse::<u64>().ok()?;
    let stime = fields.get(15 - 3)?.parse::<u64>().ok()?;
    Some(utime + stime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_ticks_parses_utime_and_stime() {
        // comm with spaces/parens exercises the "after last )" logic;
        // utime = 99, stime = 81 ticks -> 180.
        let stat = "123 (my app (forked)) S 1 123 123 0 -1 4194560 100 0 0 0 99 81 0 0 20 0 1 0 1234 5000 100";
        assert_eq!(cpu_ticks(stat), Some(180));
    }

    #[test]
    fn cpu_ticks_rejects_short_input() {
        assert_eq!(cpu_ticks("1 (x) S 1"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn current_reports_live_process_stats() {
        let stats = current().expect("should read /proc/self on linux");
        assert!(stats.resident_memory_bytes > 0, "RSS should be non-zero");
        assert!(stats.virtual_memory_bytes >= stats.resident_memory_bytes);
        assert!(stats.num_threads >= 1);
        assert!(stats.open_fds > 0);
        assert!(stats.cpu_seconds >= 0.0);
    }
}
