//! Prepared generation storage; only the registered owner accesses each mutable state.

use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Setup-time identity and independent callback bounds at the fixed native rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationSettings {
    /// Configured local node identity.
    pub node: String,
    /// Adapter-selected device identity, retained for controlled rollback.
    pub device: String,
    /// Largest supported receive callback, in frames.
    pub receive_maximum: usize,
    /// Largest supported transmit callback, in frames.
    pub transmit_maximum: usize,
}

/// A candidate or lifecycle operation that cannot safely proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleError {
    /// Invalid identity, frame bounds, or non-increasing generation ID.
    InvalidGeneration,
    /// One prior generation still needs adoption or retirement.
    RetirementPending,
    /// The host has stopped accepting generations.
    Stopped,
    /// Detachment was reported for a generation this host does not retain.
    UnknownGeneration,
    /// Callback owners have not yet quiesced for a device handoff.
    CallbacksActive,
    /// Ordinary publication cannot switch the selected hardware lease.
    HandoffRequired,
    /// The new device failed; the previous device was restored.
    HandoffRestored,
    /// Neither replacement nor previous device could be opened; RF remains idle.
    HandoffFailed,
}

pub(super) struct WorkState {
    pub admitted: AtomicBool,
    pub outstanding: AtomicUsize,
}

/// Owned off-audio work permit. Retirement invalidates it without freeing its payload.
pub struct GenerationWork {
    id: u64,
    state: Arc<WorkState>,
}

impl GenerationWork {
    /// Generation against which this operation was prepared.
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Revalidate immediately before external work and again before applying its result.
    pub fn is_current(&self) -> bool {
        self.state.admitted.load(Ordering::Acquire)
    }
}

impl Drop for GenerationWork {
    fn drop(&mut self) {
        self.state.outstanding.fetch_sub(1, Ordering::Release);
    }
}

/// A fully prepared coherent generation with distinct receive/transmit private state.
///
/// Construct the states and all adapter/media resources before calling `prepare`.
/// They are owned here immediately, including on validation failure. No constructor,
/// allocation, state destructor, or general reference count runs from a callback.
pub struct RuntimeGeneration<R: Send, T: Send> {
    pub(super) id: u64,
    pub(super) settings: GenerationSettings,
    pub(super) receive: UnsafeCell<R>,
    pub(super) transmit: UnsafeCell<T>,
    pub(super) work: Arc<WorkState>,
    pub(super) detached: AtomicBool,
}

impl<R: Send, T: Send> RuntimeGeneration<R, T> {
    /// Validate setup identities/bounds and take ownership of already-prepared resources.
    pub fn prepare(
        id: u64,
        settings: GenerationSettings,
        receive: R,
        transmit: T,
    ) -> Result<Self, LifecycleError> {
        if id == 0
            || settings.node.is_empty()
            || settings.node.contains('\0')
            || settings.device.is_empty()
            || settings.device.contains('\0')
            || settings.receive_maximum == 0
            || settings.transmit_maximum == 0
        {
            return Err(LifecycleError::InvalidGeneration);
        }
        Ok(Self {
            id,
            settings,
            receive: UnsafeCell::new(receive),
            transmit: UnsafeCell::new(transmit),
            work: Arc::new(WorkState {
                admitted: AtomicBool::new(true),
                outstanding: AtomicUsize::new(0),
            }),
            detached: AtomicBool::new(false),
        })
    }

    pub(super) fn work(&self) -> GenerationWork {
        self.work.outstanding.fetch_add(1, Ordering::Relaxed);
        GenerationWork {
            id: self.id,
            state: Arc::clone(&self.work),
        }
    }
}
