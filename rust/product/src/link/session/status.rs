//! Bounded transport receive-key observations, independent of DTMF backpressure.
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// The serial provider publishes actual transitions before another packet can replace them.
pub(crate) struct ReceiveStatus {
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
    /// A media path that knows it starts unkeyed can report the state immediately.
    pub(crate) fn unkeyed() -> Self {
        Self {
            key: AtomicU8::new(1),
            ..Self::default()
        }
    }

    pub(crate) fn pending(&self) -> bool {
        !self.edges.is_empty()
    }

    pub(crate) fn observe(&self, at_ms: u64, keyed: bool) {
        let state = if keyed { 2 } else { 1 };
        if self.key.swap(state, Ordering::AcqRel) != state
            && self.edges.push((at_ms, keyed)).is_err()
        {
            self.lost.store(true, Ordering::Release);
        }
    }

    pub(crate) fn discard(&self) {
        for _ in 0..self.edges.capacity() {
            if self.edges.pop().is_none() {
                break;
            }
        }
        let _ = self.lost();
    }

    pub(crate) fn key(&self) -> Option<bool> {
        match self.key.load(Ordering::Acquire) {
            0 => None,
            state => Some(state == 2),
        }
    }

    pub(crate) fn next(&self) -> Option<(u64, bool)> {
        self.edges.pop()
    }

    pub(crate) fn lost(&self) -> bool {
        self.lost.swap(false, Ordering::AcqRel)
    }
}
