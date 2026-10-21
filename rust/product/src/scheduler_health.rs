//! Shared control-executor diagnostics, never sampled from the audio callbacks.

use std::sync::atomic::{AtomicU64, Ordering};

/// One calendar-check period is a materially late control-task start.
const DELAY_NOTICE_US: u64 = 1_000_000;
/// Bound each class of host warnings to one per minute, using monotonic time.
const NOTICE_INTERVAL_MS: u64 = 60_000;

/// Lifetime counters survive configuration reloads and reset with the product host.
/// Fields are independently sampled; the status line is not a transactional snapshot.
#[derive(Default)]
pub(crate) struct SchedulerHealth {
    submission_failures: AtomicU64,
    cycle_failures: AtomicU64,
    queue_delay_last_us: AtomicU64,
    queue_delay_max_us: AtomicU64,
    delayed_tasks: AtomicU64,
    next_fault_notice_ms: AtomicU64,
    next_delay_notice_ms: AtomicU64,
}

impl SchedulerHealth {
    /// Count only executor admission failures, not retired-generation rejection.
    pub(crate) fn submission_failed(&self, now_ms: u64) -> bool {
        self.submission_failures.fetch_add(1, Ordering::Relaxed);
        notice_due(&self.next_fault_notice_ms, now_ms)
    }

    /// Count failed live scheduler cycles, sharing the admission-warning throttle.
    pub(crate) fn cycle_failed(&self, now_ms: u64) -> bool {
        self.cycle_failures.fetch_add(1, Ordering::Relaxed);
        notice_due(&self.next_fault_notice_ms, now_ms)
    }

    /// Measure enqueue-to-start latency without changing the task's captured clock.
    pub(crate) fn started(&self, delay_us: u64, now_ms: u64) -> bool {
        self.queue_delay_last_us.store(delay_us, Ordering::Relaxed);
        self.queue_delay_max_us
            .fetch_max(delay_us, Ordering::Relaxed);
        if delay_us < DELAY_NOTICE_US {
            return false;
        }
        self.delayed_tasks.fetch_add(1, Ordering::Relaxed);
        notice_due(&self.next_delay_notice_ms, now_ms)
    }

    /// Append process-wide executor health to the existing text-status interface.
    pub(crate) fn status(&self, text: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(
            text,
            "  scheduler submission_failures={} cycle_failures={} queue_delay_last_us={} queue_delay_max_us={} delayed_tasks={}",
            self.submission_failures.load(Ordering::Relaxed),
            self.cycle_failures.load(Ordering::Relaxed),
            self.queue_delay_last_us.load(Ordering::Relaxed),
            self.queue_delay_max_us.load(Ordering::Relaxed),
            self.delayed_tasks.load(Ordering::Relaxed),
        );
    }
}

/// Atomically admit the first notice and each subsequent monotonic-minute boundary.
fn notice_due(next: &AtomicU64, now_ms: u64) -> bool {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |deadline| {
        (now_ms >= deadline).then(|| now_ms.saturating_add(NOTICE_INTERVAL_MS))
    })
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_are_counted_but_not_logged_on_every_scheduler_cycle() {
        let health = SchedulerHealth::default();
        assert!(health.submission_failed(0));
        assert!(!health.submission_failed(1));
        assert!(!health.cycle_failed(NOTICE_INTERVAL_MS - 1));
        assert!(health.cycle_failed(NOTICE_INTERVAL_MS));
        let mut status = String::new();
        health.status(&mut status);
        assert_eq!(
            status,
            "  scheduler submission_failures=2 cycle_failures=2 queue_delay_last_us=0 queue_delay_max_us=0 delayed_tasks=0\n"
        );
    }

    #[test]
    fn delay_metrics_and_notice_threshold_do_not_hide_shorter_delays() {
        let health = SchedulerHealth::default();
        assert!(!health.started(DELAY_NOTICE_US - 1, 0));
        assert!(health.started(DELAY_NOTICE_US, 0));
        assert!(!health.started(DELAY_NOTICE_US + 20, NOTICE_INTERVAL_MS - 1));
        assert!(health.started(DELAY_NOTICE_US, NOTICE_INTERVAL_MS));
        assert!(!health.started(20, NOTICE_INTERVAL_MS + 1));
        assert!(health.submission_failed(NOTICE_INTERVAL_MS + 1));
        let mut status = String::new();
        health.status(&mut status);
        assert_eq!(
            status,
            "  scheduler submission_failures=1 cycle_failures=0 queue_delay_last_us=20 queue_delay_max_us=1000020 delayed_tasks=3\n"
        );
    }

    #[test]
    fn concurrent_faults_have_only_one_notice_slot() {
        let health = SchedulerHealth::default();
        std::thread::scope(|scope| {
            let threads = (0..8)
                .map(|_| scope.spawn(|| usize::from(health.submission_failed(0))))
                .collect::<Vec<_>>();
            assert_eq!(
                threads
                    .into_iter()
                    .map(|thread| thread.join().unwrap())
                    .sum::<usize>(),
                1
            );
        });
        let mut status = String::new();
        health.status(&mut status);
        assert!(status.contains("submission_failures=8"));
    }
}
