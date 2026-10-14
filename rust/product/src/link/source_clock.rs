//! Native sample time mapped off audio; published spans retain their original age.

use std::{
    sync::atomic::{AtomicI64, AtomicU64, Ordering},
    time::Instant,
};

/// Stable per-device clock, retained across configuration changes and physical reopen.
pub(crate) struct SourceClock {
    epoch: Instant,
    next: AtomicU64,
    stream: AtomicU64,
    published_stream: AtomicU64,
    published_start: AtomicU64,
    observed_stream: AtomicU64,
    observed_start: AtomicU64,
    offset: AtomicI64,
    mapped_stream: AtomicU64,
}

/// Copied once by the native callback; no later mapping change can renew this time.
pub(crate) struct Stamp {
    pub(crate) sample: u64,
    pub(crate) stream: u64,
    pub(crate) born_ns: Option<u64>,
}

fn sample_ns(sample: u64) -> i128 {
    i128::from(sample) * 1_000_000_000 / 48_000
}

impl SourceClock {
    pub(crate) fn stream(&self) -> u64 {
        self.stream.load(Ordering::Acquire)
    }
    pub(crate) fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            next: AtomicU64::new(0),
            stream: AtomicU64::new(0),
            published_stream: AtomicU64::new(0),
            published_start: AtomicU64::new(0),
            observed_stream: AtomicU64::new(0),
            observed_start: AtomicU64::new(0),
            offset: AtomicI64::new(0),
            mapped_stream: AtomicU64::new(0),
        }
    }
    /// Control calls this only with old callbacks quiescent, before activating a new stream.
    pub(crate) fn restart(&self) {
        self.stream.fetch_add(1, Ordering::Release);
    }
    /// Sole TX callback: sample arithmetic and atomics only, including after dropped output.
    pub(crate) fn advance(&self, count: usize) -> Stamp {
        let stream = self.stream();
        let sample = self.next.fetch_add(count as u64, Ordering::Relaxed);
        let born_ns = (stream != 0 && self.mapped_stream.load(Ordering::Acquire) == stream)
            .then(|| sample_ns(sample) + i128::from(self.offset.load(Ordering::Relaxed)))
            .and_then(|time| u64::try_from(time).ok());
        self.published_start.store(sample, Ordering::Relaxed);
        self.published_stream.store(stream, Ordering::Release);
        Stamp {
            sample,
            stream,
            born_ns,
        }
    }
    /// Sole media owner observes real callback progress, never preparation or idle wall time.
    pub(crate) fn observe(&self) {
        self.observe_at(self.epoch.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }
    fn observe_at(&self, now_ns: u64) {
        let stream = self.stream.load(Ordering::Acquire);
        if stream == 0 || self.published_stream.load(Ordering::Acquire) != stream {
            return;
        }
        let sample = self.published_start.load(Ordering::Relaxed);
        if self.observed_stream.load(Ordering::Relaxed) == stream
            && self.observed_start.load(Ordering::Relaxed) == sample
        {
            return;
        }
        let Ok(offset) = i64::try_from(i128::from(now_ns) - sample_ns(sample)) else {
            return;
        };
        self.offset.store(offset, Ordering::Relaxed);
        // An observation racing reopen retains its old serial, so new callbacks reject it.
        self.mapped_stream.store(stream, Ordering::Release);
        self.observed_start.store(sample, Ordering::Relaxed);
        self.observed_stream.store(stream, Ordering::Relaxed);
    }
}

impl Default for SourceClock {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_clock_waits_for_native_progress_and_never_rejuvenates_a_span() {
        let clock = SourceClock::new(std::time::Instant::now());
        clock.restart();
        clock.observe_at(700_000_000);
        let first = clock.advance(960);
        assert_eq!(first.born_ns, None);
        clock.observe_at(1_000_000_000);
        let next = clock.advance(480);
        assert_eq!(next.sample, 960);
        assert_eq!(next.born_ns, Some(1_020_000_000));
        clock.observe_at(1_020_100_000);
        clock.observe_at(5_000_000_000); // No source progress: no new mapping.
        let resumed = clock.advance(240);
        assert_eq!(resumed.born_ns, Some(1_030_100_000));
        clock.observe_at(5_000_000_000);
        assert_eq!(resumed.born_ns, Some(1_030_100_000));
        let following = clock.advance(240);
        assert_eq!(following.born_ns, Some(5_005_000_000));
    }

    #[test]
    fn egress_clock_reopen_keeps_position_but_invalidates_old_mapping() {
        let clock = SourceClock::new(std::time::Instant::now());
        clock.restart();
        clock.advance(4096);
        clock.observe_at(1_000_000_000);
        let old = clock.advance(1);
        clock.restart();
        clock.observe_at(9_000_000_000); // Open is not a callback.
        let new = clock.advance(480);
        assert_ne!(old.stream, new.stream);
        assert_eq!(new.sample, 4097);
        assert_eq!(new.born_ns, None);
        clock.observe_at(10_000_000_000);
        assert_eq!(clock.advance(480).born_ns, Some(10_010_000_000));
    }
}
