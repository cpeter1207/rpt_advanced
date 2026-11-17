//! Opt-in Linux lifecycle observations for existing tests, never a production allocator policy.
use std::{fs, path::PathBuf, time::Instant};

/// Writes a few lifecycle snapshots only when the suite reporter requests them.
pub(crate) struct Recorder {
    directory: Option<PathBuf>,
    workload: &'static str,
    started: Instant,
}

impl Recorder {
    /// Enable observations only for a caller-provided artifact directory.
    pub(crate) fn new(workload: &'static str) -> Self {
        Self {
            directory: std::env::var_os("RPTADV_RESOURCE_SNAPSHOT_DIR").map(PathBuf::from),
            workload,
            started: Instant::now(),
        }
    }

    /// Observe the entire test process, including fixtures and any concurrent tests.
    pub(crate) fn record(&self, phase: &str, connected_peers: usize) {
        let Some(directory) = &self.directory else {
            return;
        };
        let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
        let value = |name: &str| {
            status.lines().find_map(|line| {
                line.strip_prefix(name)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        };
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        let allocator = {
            // glibc's aggregate counters include native providers. No trim or tunable change.
            let info = unsafe { libc::mallinfo2() };
            serde_json::json!({
                "arena_bytes": info.arena,
                "arena_in_use_bytes": info.uordblks,
                "arena_free_retained_bytes": info.fordblks,
                "mmap_bytes": info.hblkhd,
                "mmap_regions": info.hblks,
                "note": "glibc aggregate, not allocation stacks; tcache and other allocators limit live-object attribution"
            })
        };
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        let allocator = serde_json::Value::Null;
        let report = serde_json::json!({
            "schema_version": 1,
            "workload": self.workload,
            "phase": phase,
            "pid": std::process::id(),
            "elapsed_seconds": self.started.elapsed().as_secs_f64(),
            "connected_peers": connected_peers,
            "sample_rate_hz": 48000,
            "rss_bytes": value("VmRSS:").map(|kb| kb * 1024),
            "process_lifetime_peak_rss_bytes": value("VmHWM:").map(|kb| kb * 1024),
            "threads": value("Threads:"),
            "glibc": allocator,
            "scope": "whole test process including fixtures; use serial tests; retained RSS is not a leak diagnosis"
        });
        fs::create_dir_all(directory).unwrap();
        let filename = format!("{}-{}-{phase}.json", self.workload, std::process::id());
        fs::write(
            directory.join(filename),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
