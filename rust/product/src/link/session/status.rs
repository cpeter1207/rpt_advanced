//! Bounded transport receive-key observations, independent of DTMF backpressure.
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// The serial provider publishes actual transitions before another packet can replace them.
pub(in crate::link) struct ReceiveStatus {
    edges: ArrayQueue<(u64, bool)>,
    key: AtomicU8,
    lost: AtomicBool,
}

impl Default for ReceiveStatus {
    fn default() -> Self {
        Self {
            edges: ArrayQueue::new(64),
            key: AtomicU8::new(0),
            lost: AtomicBool::new(false),
        }
    }
}

impl ReceiveStatus {
    pub(in crate::link) fn observe(&self, at_ms: u64, keyed: bool) {
        let state = if keyed { 2 } else { 1 };
        if self.key.swap(state, Ordering::AcqRel) != state
            && self.edges.push((at_ms, keyed)).is_err()
        {
            self.lost.store(true, Ordering::Release);
        }
    }

    pub(in crate::link) fn discard(&self) {
        for _ in 0..self.edges.capacity() {
            if self.edges.pop().is_none() {
                break;
            }
        }
        let _ = self.lost();
    }

    pub(in crate::link) fn key(&self) -> Option<bool> {
        match self.key.load(Ordering::Acquire) {
            0 => None,
            state => Some(state == 2),
        }
    }

    pub(in crate::link) fn next(&self) -> Option<(u64, bool)> {
        self.edges.pop()
    }

    pub(in crate::link) fn lost(&self) -> bool {
        self.lost.swap(false, Ordering::AcqRel)
    }
}
