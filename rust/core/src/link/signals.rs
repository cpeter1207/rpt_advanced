//! Bounded lock-free snapshots between one audio consumer and one channel owner.
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// PCM publication, terminal EOF, activity edges and advisory selected source.
/// Audio is the sole activity writer; the channel owner is the sole source writer.
pub struct PeerSignals {
    pcm: AtomicU64,
    ended: AtomicBool,
    edge: AtomicU64,
    source_version: AtomicU64,
    source_edge: AtomicU64,
    source: [AtomicU8; 64],
}
impl Default for PeerSignals {
    fn default() -> Self {
        Self::new()
    }
}
impl PeerSignals {
    /// Prepare empty state before either endpoint is exposed.
    pub fn new() -> Self {
        Self {
            pcm: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            edge: AtomicU64::new(0),
            source_version: AtomicU64::new(0),
            source_edge: AtomicU64::new(0),
            source: std::array::from_fn(|_| AtomicU8::new(0)),
        }
    }
    /// Publish only after real decoded samples become visible in the shared ring.
    pub fn publish_pcm(&self) {
        self.pcm.fetch_add(1, Ordering::Release);
    }
    /// Observe the last completed PCM publication.
    pub fn pcm_epoch(&self) -> u64 {
        self.pcm.load(Ordering::Acquire)
    }
    /// Mark terminal transport failure before detachment or reclaim.
    pub fn end(&self) {
        self.ended.store(true, Ordering::Release);
    }
    /// Terminal failure wins over any buffered activity.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
    /// Audio-owner activity edge; an odd serial means active. No allocation or wait.
    pub fn set_active(&self, active: bool) -> u64 {
        let edge = self.edge.load(Ordering::Relaxed);
        if (edge & 1 != 0) == active {
            return edge;
        }
        let next = edge.wrapping_add(1);
        self.edge.store(next, Ordering::Release);
        next
    }
    /// Channel-owner snapshot used to cancel queries at unkey or a newer burst.
    pub fn activity_edge(&self) -> u64 {
        self.edge.load(Ordering::Acquire)
    }
    /// Publish a source only for the still-current active receive burst.
    pub fn select_source(&self, edge: u64, source: &str) -> bool {
        if edge & 1 == 0 || self.activity_edge() != edge || !super::identity(source) {
            return false;
        }
        self.source_version.fetch_add(1, Ordering::SeqCst);
        for (index, slot) in self.source.iter().enumerate() {
            slot.store(
                source.as_bytes().get(index).copied().unwrap_or(0),
                Ordering::SeqCst,
            );
        }
        self.source_edge.store(edge, Ordering::SeqCst);
        self.source_version.fetch_add(1, Ordering::SeqCst);
        self.activity_edge() == edge
    }
    /// Read once without retrying: concurrent publication safely falls back to direct identity.
    /// Passing the completed active edge preserves the advisory source at unkey.
    pub fn selected_source<'a>(&self, edge: u64, storage: &'a mut [u8; 64]) -> Option<&'a str> {
        let version = self.source_version.load(Ordering::SeqCst);
        if version & 1 != 0 || self.source_edge.load(Ordering::SeqCst) != edge {
            return None;
        }
        for (output, input) in storage.iter_mut().zip(&self.source) {
            *output = input.load(Ordering::SeqCst);
        }
        if self.source_version.load(Ordering::SeqCst) != version {
            return None;
        }
        let len = storage.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&storage[..len]).ok()
    }
}
