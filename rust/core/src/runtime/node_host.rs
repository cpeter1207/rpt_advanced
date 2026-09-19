//! Fixed hazard-slot protection. Raw pointers never escape these RAII guards.

use super::{GenerationSettings, GenerationWork, LifecycleError, RuntimeGeneration};
use std::{
    cell::Cell,
    marker::PhantomData,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicPtr, AtomicU64, Ordering},
    },
};

struct Shared<R: Send, T: Send> {
    active: AtomicPtr<RuntimeGeneration<R, T>>,
    hazards: [AtomicPtr<RuntimeGeneration<R, T>>; 2],
    adopted: [AtomicU64; 2],
}

struct Store<R: Send, T: Send> {
    active: Option<Box<RuntimeGeneration<R, T>>>,
    retired: Option<Box<RuntimeGeneration<R, T>>>,
    changed_ms: u64,
    stopping: bool,
    reported_timeout: Option<(Option<u64>, LifecycleState)>,
    timeouts: u64,
}

/// Long-lived node allocation, borrowed into one control owner and two audio owners.
///
/// The lifetime of `split` prevents dropping storage until every callback owner and
/// guard is gone. Only control can publish/reclaim; receive and transmit get one
/// non-cloneable handle each. Ordinary reload preserves this host and device lease.
/// Explicitly stop, detach external contexts, drain work, and reclaim before drop.
/// Violating that protocol retains unsafe-to-free resources rather than force-freeing.
pub struct NodeHost<R: Send, T: Send> {
    shared: Arc<Shared<R, T>>,
    store: Store<R, T>,
    registered: bool,
}

impl<R: Send, T: Send> Drop for NodeHost<R, T> {
    fn drop(&mut self) {
        // Rust owner lifetimes have ended, but external producers and admitted work
        // may still exist. Destruction is not an alternative quiescence mechanism.
        self.shared.active.store(ptr::null_mut(), Ordering::SeqCst);
        for slot in [&mut self.store.active, &mut self.store.retired] {
            if let Some(generation) = slot.take() {
                generation.work.admitted.store(false, Ordering::Release);
                if !generation.detached.load(Ordering::Acquire)
                    || generation.work.outstanding.load(Ordering::Acquire) != 0
                    || self
                        .shared
                        .hazards
                        .iter()
                        .any(|slot| ptr::eq(slot.load(Ordering::SeqCst), generation.as_ref()))
                {
                    std::mem::forget(generation);
                }
            }
        }
    }
}

impl<R: Send, T: Send> NodeHost<R, T> {
    /// Start with a completely prepared generation; no callback runs during construction.
    pub fn new(generation: RuntimeGeneration<R, T>) -> Self {
        let mut generation = Box::new(generation);
        let pointer = ptr::from_mut(generation.as_mut());
        Self {
            shared: Arc::new(Shared {
                active: AtomicPtr::new(pointer),
                hazards: [
                    AtomicPtr::new(ptr::null_mut()),
                    AtomicPtr::new(ptr::null_mut()),
                ],
                adopted: [AtomicU64::new(0), AtomicU64::new(0)],
            }),
            registered: false,
            store: Store {
                active: Some(generation),
                retired: None,
                changed_ms: 0,
                stopping: false,
                reported_timeout: None,
                timeouts: 0,
            },
        }
    }

    /// Assign the pre-registered owners before starting callbacks. Drop them after stop/join.
    pub fn split(
        &mut self,
    ) -> (
        NodeControl<'_, R, T>,
        ReceiveOwner<'_, R, T>,
        TransmitOwner<'_, R, T>,
    ) {
        assert!(!self.registered, "audio owners already registered");
        self.registered = true;
        (
            NodeControl {
                shared: &self.shared,
                store: &mut self.store,
            },
            ReceiveOwner {
                shared: &self.shared,
                exclusive: PhantomData,
            },
            TransmitOwner {
                shared: &self.shared,
                exclusive: PhantomData,
            },
        )
    }

    /// Register one fixed owned RX/TX pair before callbacks start. Arc changes occur only here
    /// and when the external lifecycle owner drops these handles after stop/join, never per call.
    pub fn register_audio(
        &mut self,
    ) -> Option<(OwnedReceiveOwner<R, T>, OwnedTransmitOwner<R, T>)> {
        if self.registered {
            return None;
        }
        self.registered = true;
        Some((
            OwnedReceiveOwner {
                shared: Arc::clone(&self.shared),
                exclusive: PhantomData,
            },
            OwnedTransmitOwner {
                shared: Arc::clone(&self.shared),
                exclusive: PhantomData,
            },
        ))
    }

    /// Borrow serialized control independently of the fixed externally registered audio owners.
    pub fn control(&mut self) -> NodeControl<'_, R, T> {
        NodeControl {
            shared: &self.shared,
            store: &mut self.store,
        }
    }
    /// Reclaim from a host which may not yet have registered any callback owners.
    pub fn reclaim(&mut self) -> bool {
        if !self.registered {
            if let Some(active) = &self.store.active {
                for slot in &self.shared.adopted {
                    slot.store(active.id, Ordering::Release);
                }
            }
        }
        self.control().reclaim()
    }
}

/// Fixed input registration. Construct/drop on lifecycle control, not the audio callback.
pub struct OwnedReceiveOwner<R: Send, T: Send> {
    shared: Arc<Shared<R, T>>,
    exclusive: PhantomData<Cell<()>>,
}
/// Fixed output registration. Construct/drop on lifecycle control, not the audio callback.
pub struct OwnedTransmitOwner<R: Send, T: Send> {
    shared: Arc<Shared<R, T>>,
    exclusive: PhantomData<Cell<()>>,
}
impl<R: Send, T: Send> OwnedReceiveOwner<R, T> {
    /// Protect native input state for exactly one callback without touching the Arc count.
    pub fn acquire(&mut self) -> Option<GenerationGuard<'_, R, T, true>> {
        self.shared.acquire(0).map(|pointer| GenerationGuard {
            shared: &self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
    /// Protect one shared-clock pair without changing either owner's fixed registration.
    pub fn acquire_pair<'a>(
        &'a mut self,
        transmit: &'a mut OwnedTransmitOwner<R, T>,
    ) -> Option<PairedGuard<'a, R, T>> {
        if !Arc::ptr_eq(&self.shared, &transmit.shared) {
            return None;
        }
        let pointer = self.shared.acquire(0)?;
        self.shared.hazards[1].store(pointer, Ordering::SeqCst);
        // SAFETY: the already published receive hazard protects this generation.
        self.shared.adopted[1].store(unsafe { (*pointer).id }, Ordering::Release);
        Some(PairedGuard {
            shared: &self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
}
impl<R: Send, T: Send> OwnedTransmitOwner<R, T> {
    /// Protect native output state for exactly one callback without touching the Arc count.
    pub fn acquire(&mut self) -> Option<GenerationGuard<'_, R, T, false>> {
        self.shared.acquire(1).map(|pointer| GenerationGuard {
            shared: &self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
}

/// Status distinguishes callback adoption from safe reclamation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    /// Both workers may run the published generation and no retirement remains.
    Running,
    /// One or both workers have not adopted the published replacement.
    AdoptionPending,
    /// Adoption is complete, but a retired owner/work/context is still held.
    RetirementPending,
    /// New callbacks are gated; retained resources are still quiescing.
    Stopping,
    /// All generation resources have been reclaimed.
    Stopped,
}

/// Control-plane snapshot for diagnosing a stalled worker or callback context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleStatus {
    /// Published generation, absent after stop gates callback entry.
    pub active: Option<u64>,
    /// Previous retained generation.
    pub retiring: Option<u64>,
    /// Publication/adoption/reclamation state.
    pub state: LifecycleState,
    /// Elapsed monotonic time since publication or stop.
    pub age_ms: u64,
    /// Most recently acquired receive generation.
    pub receive_adopted: u64,
    /// Most recently acquired transmit generation.
    pub transmit_adopted: u64,
    /// Callbacks currently protecting any generation.
    pub protected_owners: usize,
    /// Outstanding retained-generation work (active plus retired).
    pub outstanding_work: usize,
    /// Distinct adoption/retirement/stop timeout reports, never forced reclamations.
    pub quiescence_timeouts: u64,
}

/// Serialized generation publication and reclamation, never used on audio.
pub struct NodeControl<'a, R: Send, T: Send> {
    shared: &'a Shared<R, T>,
    store: &'a mut Store<R, T>,
}

/// Control-only radio lease operations supplied by the selected device adapter.
pub trait DeviceHandoff: Send {
    /// Request TX-safe idle, deassert PTT, detach both callbacks and join external users.
    /// Return true only once complete; false leaves the old device retained for retry.
    fn quiesce(&mut self) -> bool;
    /// Close the old lease after quiescence, leaving PTT deasserted.
    fn close(&mut self);
    /// Open the requested lease with RF-safe output; failure retains no device ownership.
    fn open(&mut self, settings: &GenerationSettings) -> bool;
}

impl<R: Send, T: Send> NodeControl<'_, R, T> {
    pub(super) fn gate_callbacks(&mut self) {
        self.shared.active.store(ptr::null_mut(), Ordering::SeqCst);
    }
    pub(super) fn restore_callbacks(&mut self) {
        if !self.store.stopping {
            self.store.active.as_mut().into_iter().for_each(|active| {
                self.shared
                    .active
                    .store(ptr::from_mut(active.as_mut()), Ordering::SeqCst);
            });
        }
    }
    /// Replace a hardware lease only after RF idle and complete callback quiescence.
    /// An opening failure restores the old lease or leaves callback entry safely gated.
    pub fn handoff(
        &mut self,
        generation: RuntimeGeneration<R, T>,
        device: &mut impl DeviceHandoff,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        if self.store.stopping {
            return Err(LifecycleError::Stopped);
        }
        if self.store.retired.is_some() {
            return Err(LifecycleError::RetirementPending);
        }
        let old = self.store.active.as_ref().ok_or(LifecycleError::Stopped)?;
        if generation.id <= old.id || generation.settings.node != old.settings.node {
            return Err(LifecycleError::InvalidGeneration);
        }
        if !device.quiesce() {
            return Err(LifecycleError::CallbacksActive);
        }
        self.shared.active.store(ptr::null_mut(), Ordering::SeqCst);
        if self
            .shared
            .hazards
            .iter()
            .any(|h| !h.load(Ordering::SeqCst).is_null())
            || old.work.outstanding.load(Ordering::Acquire) != 0
        {
            self.shared
                .active
                .store(ptr::from_ref(old.as_ref()).cast_mut(), Ordering::SeqCst);
            return Err(LifecycleError::CallbacksActive);
        }
        device.close();
        if device.open(&generation.settings) {
            old.detached.store(true, Ordering::Release);
            return self.publish_prepared(generation, now_ms);
        }
        if device.open(&old.settings) {
            self.shared
                .active
                .store(ptr::from_ref(old.as_ref()).cast_mut(), Ordering::SeqCst);
            return Err(LifecycleError::HandoffRestored);
        }
        old.detached.store(true, Ordering::Release);
        self.stop(now_ms)?;
        Err(LifecycleError::HandoffFailed)
    }

    /// Publish only a complete candidate; reject before changing any active state.
    pub fn publish(
        &mut self,
        generation: RuntimeGeneration<R, T>,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        if self
            .store
            .active
            .as_ref()
            .is_some_and(|old| old.settings.device != generation.settings.device)
        {
            return Err(LifecycleError::HandoffRequired);
        }
        self.publish_prepared(generation, now_ms)
    }

    pub(super) fn publish_prepared(
        &mut self,
        generation: RuntimeGeneration<R, T>,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        if self.store.stopping {
            return Err(LifecycleError::Stopped);
        }
        if self.store.retired.is_some() {
            return Err(LifecycleError::RetirementPending);
        }
        if self.store.active.as_ref().is_some_and(|old| {
            generation.id <= old.id || generation.settings.node != old.settings.node
        }) {
            return Err(LifecycleError::InvalidGeneration);
        }
        let mut candidate = Box::new(generation);
        let pointer = ptr::from_mut(candidate.as_mut());
        self.store.active.iter().for_each(|old| {
            old.work.admitted.store(false, Ordering::Release);
        });
        self.store.retired = self.store.active.replace(candidate);
        // SeqCst makes publication, hazard publication and the reclaimer's scan one order.
        self.shared.active.store(pointer, Ordering::SeqCst);
        self.store.changed_ms = now_ms;
        Ok(())
    }

    pub(super) fn publish_transferring(
        &mut self,
        mut generation: RuntimeGeneration<R, T>,
        now_ms: u64,
        transfer: impl FnOnce(&mut R, &mut T, &mut R, &mut T),
    ) -> Result<(), LifecycleError> {
        if self.store.stopping {
            return Err(LifecycleError::Stopped);
        }
        if self.store.retired.is_some() {
            return Err(LifecycleError::RetirementPending);
        }
        let old = self.store.active.as_mut().ok_or(LifecycleError::Stopped)?;
        if generation.id <= old.id || generation.settings != old.settings {
            return Err(LifecycleError::InvalidGeneration);
        }
        self.shared.active.store(ptr::null_mut(), Ordering::SeqCst);
        if self
            .shared
            .hazards
            .iter()
            .any(|slot| !slot.load(Ordering::SeqCst).is_null())
        {
            self.shared
                .active
                .store(ptr::from_mut(old.as_mut()), Ordering::SeqCst);
            return Err(LifecycleError::CallbacksActive);
        }
        // No callback can now enter or retain the old state. Internal callers only
        // perform infallible swaps, leaving valid placeholders in retired storage.
        transfer(
            old.receive.get_mut(),
            old.transmit.get_mut(),
            generation.receive.get_mut(),
            generation.transmit.get_mut(),
        );
        self.publish_prepared(generation, now_ms)
    }

    /// Reserve owned non-audio work while this control owner admits the generation.
    pub fn work(&self) -> Option<GenerationWork> {
        if self.store.stopping {
            None
        } else {
            self.store.active.as_ref().map(|g| g.work())
        }
    }

    /// Confirm that external producers/callbacks detached and queues were drained or abandoned.
    ///
    /// The adapter must report this only after joining/unregistering its callbacks. The
    /// host additionally checks hazards and work, so this cannot force reclamation.
    pub fn mark_detached(&mut self, id: u64) -> Result<(), LifecycleError> {
        for generation in [&self.store.active, &self.store.retired]
            .into_iter()
            .flatten()
        {
            if generation.id == id && !generation.work.admitted.load(Ordering::Acquire) {
                generation.detached.store(true, Ordering::Release);
                return Ok(());
            }
        }
        Err(LifecycleError::UnknownGeneration)
    }

    /// Reclaim only safely retired storage. Pending adoption alone also bounds retention.
    pub fn reclaim(&mut self) -> bool {
        let adopted = self.store.stopping
            || self.store.active.as_ref().is_none_or(|active| {
                self.shared
                    .adopted
                    .iter()
                    .all(|slot| slot.load(Ordering::Acquire) == active.id)
            });
        if !adopted {
            return false;
        }
        let mut released = false;
        for generation in [&mut self.store.retired, &mut self.store.active] {
            let ready = generation.as_ref().is_some_and(|g| {
                g.detached.load(Ordering::Acquire)
                    // Only non-admitting retired/stopped generations can become detached.
                    && g.work.outstanding.load(Ordering::Acquire) == 0
                    && self
                        .shared
                        .hazards
                        .iter()
                        .all(|slot| !ptr::eq(slot.load(Ordering::SeqCst), g.as_ref()))
            });
            if ready {
                *generation = None;
                released = true;
            }
        }
        released
    }

    /// Gate callback entry and invalidate work; reclamation still requires safe detachment.
    pub fn stop(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        self.store.stopping = true;
        self.shared.active.store(ptr::null_mut(), Ordering::SeqCst);
        for generation in [&self.store.active, &self.store.retired]
            .into_iter()
            .flatten()
        {
            generation.work.admitted.store(false, Ordering::Release);
        }
        self.store.changed_ms = now_ms;
        Ok(())
    }

    /// Record a bounded control-plane timeout once for each pending stage.
    /// Return true when the caller should emit a diagnostic outside audio.
    pub fn report_timeout(&mut self, now_ms: u64, timeout_ms: u64) -> bool {
        let status = self.status(now_ms);
        let key = (status.retiring, status.state);
        if matches!(
            status.state,
            LifecycleState::Running | LifecycleState::Stopped
        ) || status.age_ms < timeout_ms
            || self.store.reported_timeout == Some(key)
        {
            return false;
        }
        self.store.reported_timeout = Some(key);
        self.store.timeouts = self.store.timeouts.saturating_add(1);
        true
    }

    /// Snapshot lifecycle without blocking either callback owner.
    pub fn status(&self, now_ms: u64) -> LifecycleStatus {
        let receive_adopted = self.shared.adopted[0].load(Ordering::Acquire);
        let transmit_adopted = self.shared.adopted[1].load(Ordering::Acquire);
        let active = if self.store.stopping {
            None
        } else {
            self.store.active.as_ref().map(|g| g.id)
        };
        let retiring = self
            .store
            .retired
            .as_ref()
            .or(if self.store.stopping {
                self.store.active.as_ref()
            } else {
                None
            })
            .map(|g| g.id);
        let state = if self.store.stopping {
            if self.store.active.is_none() && self.store.retired.is_none() {
                LifecycleState::Stopped
            } else {
                LifecycleState::Stopping
            }
        } else if retiring.is_some() {
            if active == Some(receive_adopted) && active == Some(transmit_adopted) {
                LifecycleState::RetirementPending
            } else {
                LifecycleState::AdoptionPending
            }
        } else {
            LifecycleState::Running
        };
        LifecycleStatus {
            active,
            retiring,
            state,
            age_ms: now_ms.saturating_sub(self.store.changed_ms),
            receive_adopted,
            transmit_adopted,
            protected_owners: self
                .shared
                .hazards
                .iter()
                .filter(|s| !s.load(Ordering::SeqCst).is_null())
                .count(),
            outstanding_work: [&self.store.active, &self.store.retired]
                .into_iter()
                .flatten()
                .map(|g| g.work.outstanding.load(Ordering::Acquire))
                .sum(),
            quiescence_timeouts: self.store.timeouts,
        }
    }
}

/// Unique input callback owner; `&mut self` prevents overlapping receive calls.
pub struct ReceiveOwner<'a, R: Send, T: Send> {
    shared: &'a Shared<R, T>,
    exclusive: PhantomData<Cell<()>>,
}
/// Unique output callback owner; `&mut self` prevents overlapping transmit calls.
pub struct TransmitOwner<'a, R: Send, T: Send> {
    shared: &'a Shared<R, T>,
    exclusive: PhantomData<Cell<()>>,
}

impl<R: Send, T: Send> Shared<R, T> {
    fn acquire(&self, slot: usize) -> Option<*mut RuntimeGeneration<R, T>> {
        // Retry is explicitly bounded. A racing control publication yields safe shortfall,
        // never a wait/spin for the publisher. No pointee is read until validation succeeds.
        for _ in 0..2 {
            let candidate = self.active.load(Ordering::SeqCst);
            self.hazards[slot].store(candidate, Ordering::SeqCst);
            if !candidate.is_null() && self.active.load(Ordering::SeqCst) == candidate {
                // SAFETY: the slot was published before validation and remains protected.
                let id = unsafe { (*candidate).id };
                self.adopted[slot].store(id, Ordering::Release);
                return Some(candidate);
            }
            self.hazards[slot].store(ptr::null_mut(), Ordering::SeqCst);
        }
        None
    }
}

/// Guard for one complete callback. Dropping only clears its fixed hazard slot.
pub struct GenerationGuard<'a, R: Send, T: Send, const RX: bool> {
    shared: &'a Shared<R, T>,
    pointer: *mut RuntimeGeneration<R, T>,
    exclusive: PhantomData<&'a mut ()>,
}

impl<R: Send, T: Send, const RX: bool> GenerationGuard<'_, R, T, RX> {
    /// Latched generation ID.
    pub fn id(&self) -> u64 {
        unsafe { (*self.pointer).id }
    }
    /// Immutable prepared bounds and identity for this callback.
    pub fn settings(&self) -> &GenerationSettings {
        unsafe { &(*self.pointer).settings }
    }
}
impl<R: Send, T: Send> GenerationGuard<'_, R, T, true> {
    /// Exclusively borrow this generation's input-owned state.
    pub fn state(&mut self) -> &mut R {
        unsafe { &mut *(*self.pointer).receive.get() }
    }
}
impl<R: Send, T: Send> GenerationGuard<'_, R, T, false> {
    /// Exclusively borrow this generation's output-owned state.
    pub fn state(&mut self) -> &mut T {
        unsafe { &mut *(*self.pointer).transmit.get() }
    }
}
impl<R: Send, T: Send, const RX: bool> Drop for GenerationGuard<'_, R, T, RX> {
    fn drop(&mut self) {
        self.shared.hazards[usize::from(!RX)].store(ptr::null_mut(), Ordering::SeqCst);
    }
}
impl<R: Send, T: Send> ReceiveOwner<'_, R, T> {
    /// Protect one generation without allocation, locking, refcounting, or unbounded retries.
    pub fn acquire(&mut self) -> Option<GenerationGuard<'_, R, T, true>> {
        self.shared.acquire(0).map(|pointer| GenerationGuard {
            shared: self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
    /// Protect one coherent generation across verified shared-clock receive then transmit.
    pub fn acquire_pair<'b>(
        &'b mut self,
        transmit: &'b mut TransmitOwner<'_, R, T>,
    ) -> Option<PairedGuard<'b, R, T>> {
        if !ptr::eq(self.shared, transmit.shared) {
            return None;
        }
        let pointer = self.shared.acquire(0)?;
        self.shared.hazards[1].store(pointer, Ordering::SeqCst);
        // SAFETY: receive's already-published hazard protects the same generation.
        self.shared.adopted[1].store(unsafe { (*pointer).id }, Ordering::Release);
        Some(PairedGuard {
            shared: self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
}
impl<R: Send, T: Send> TransmitOwner<'_, R, T> {
    /// Protect one independently paced output call; absence requires adapter-safe silence.
    pub fn acquire(&mut self) -> Option<GenerationGuard<'_, R, T, false>> {
        self.shared.acquire(1).map(|pointer| GenerationGuard {
            shared: self.shared,
            pointer,
            exclusive: PhantomData,
        })
    }
}

/// One protected shared-clock pair with distinct mutable owner state.
pub struct PairedGuard<'a, R: Send, T: Send> {
    shared: &'a Shared<R, T>,
    pointer: *mut RuntimeGeneration<R, T>,
    exclusive: PhantomData<&'a mut ()>,
}
impl<R: Send, T: Send> PairedGuard<'_, R, T> {
    /// Coherent ID used by both halves of this pair.
    pub fn id(&self) -> u64 {
        unsafe { (*self.pointer).id }
    }
    /// Input-private state; process this before borrowing transmit.
    pub fn receive(&mut self) -> &mut R {
        unsafe { &mut *(*self.pointer).receive.get() }
    }
    /// Output-private state belonging to the same generation as receive.
    pub fn transmit(&mut self) -> &mut T {
        unsafe { &mut *(*self.pointer).transmit.get() }
    }
}
impl<R: Send, T: Send> Drop for PairedGuard<'_, R, T> {
    fn drop(&mut self) {
        for slot in &self.shared.hazards {
            slot.store(ptr::null_mut(), Ordering::SeqCst);
        }
    }
}
