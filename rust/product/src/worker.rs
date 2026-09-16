//! Fixed direct radio callbacks and control-only reservation lifetime.
use crate::{Error, link::ring::InboundConsumer, services::Radio};
use rpt_advanced_core::{
    audio::{LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue},
    link::LinkAudio,
    runtime::{
        OwnedReceiveOwner, OwnedTransmitOwner, RuntimeAudioOwners, RuntimeTransmit,
        dtmf::DtmfWorker,
    },
};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

#[cfg(test)]
thread_local! {
    static NEXT_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
/// Test-only lifecycle failure.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum TestFailure {
    /// Fail preparation while preserving the reservation.
    Prepare = 1,
    /// Inject a receive-processing panic for boundary coverage.
    ReceivePanic = 2,
    /// Inject a transmit-processing panic for boundary coverage.
    TransmitPanic = 3,
}
#[cfg(test)]
pub(crate) fn take_test_failure(failure: TestFailure) -> bool {
    NEXT_FAILURE.with(|next| {
        if next.get() == failure as u8 {
            next.set(0);
            true
        } else {
            false
        }
    })
}
/// Unique generation-owned transmit mixer.
pub type Audio = LinkAudio<InboundConsumer>;
/// Fixed registrations transferred once on control.
pub type AudioOwners = RuntimeAudioOwners<Audio>;
type ReceiveOwner = OwnedReceiveOwner<DtmfWorker, RuntimeTransmit<Audio>>;
type TransmitOwner = OwnedTransmitOwner<DtmfWorker, RuntimeTransmit<Audio>>;

/// Control-readable carrier edge with coherent monotonic transition time.
#[derive(Clone, Default)]
pub struct RadioStatus(Arc<AtomicU64>);
impl RadioStatus {
    /// Current receive state and original transition time.
    pub fn snapshot(&self) -> (bool, u64) {
        let value = self.0.load(Ordering::Acquire);
        (value & 1 != 0, value >> 1)
    }
    fn update(&self, receiving: bool, now_ms: u64) {
        if self.0.load(Ordering::Relaxed) & 1 != u64::from(receiving) {
            self.0.store(
                (now_ms.min(u64::MAX >> 1) << 1) | u64::from(receiving),
                Ordering::Release,
            );
        }
    }
}
struct ReceiveState {
    producer: LinkAudioProducer,
    elapsed_samples: u64,
}
struct ReceiveContext {
    active: Arc<AtomicBool>,
    owner: UnsafeCell<Option<ReceiveOwner>>,
    state: UnsafeCell<ReceiveState>,
    maximum: usize,
    origin_ms: u64,
    status: RadioStatus,
}
struct TransmitContext {
    active: Arc<AtomicBool>,
    owner: UnsafeCell<Option<TransmitOwner>>,
    consumer: UnsafeCell<LinkAudioConsumer>,
    maximum: usize,
    status: RadioStatus,
}
// SAFETY: callbacks are serialized independently by the host. Control writes owner slots
// only before release-publishing active, and takes them only after synchronous radio destroy.
unsafe impl Send for ReceiveContext {}
// SAFETY: shared fields are immutable or atomic. Control initializes the owner cell
// only while inactive, then release-publishes it. The host serializes RX accesses
// to the owner/state cells and synchronously stops RX before control reclaims them.
unsafe impl Sync for ReceiveContext {}
// SAFETY: the transmit endpoint has the same single-callback/control publication contract.
unsafe impl Send for TransmitContext {}
// SAFETY: shared fields are immutable or atomic; the acquire/release activation
// handshake publishes the owner cell once. One serial TX callback owns its mutable
// cells until synchronous radio destroy makes control reclamation exclusive again.
unsafe impl Sync for TransmitContext {}

unsafe fn samples<'a>(pointer: *mut f32, count: u32) -> Option<&'a mut [f32]> {
    if pointer.is_null() || count == 0 {
        return None;
    }
    // SAFETY: the host supplies aligned writable storage for the complete declared count.
    Some(unsafe { std::slice::from_raw_parts_mut(pointer, count as usize) })
}
unsafe extern "C" fn receive_callback(
    context: *mut c_void,
    receiving: u32,
    pointer: *mut f32,
    count: u32,
) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        receive(context, receiving, pointer, count)
    })) {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: the same host-owned buffer remains borrowed until callback return.
            if let Some(samples) = unsafe { samples(pointer, count) } {
                samples.fill(0.0);
            }
            -1
        }
    }
}
unsafe fn receive(context: *mut c_void, receiving: u32, pointer: *mut f32, count: u32) -> i32 {
    let Some(samples) = (unsafe { samples(pointer, count) }) else {
        return -1;
    };
    // SAFETY: the host retains this context until synchronous destroy returns.
    let Some(context) = (unsafe { context.cast::<ReceiveContext>().as_ref() }) else {
        samples.fill(0.0);
        return -1;
    };
    if samples.len() > context.maximum {
        samples.fill(0.0);
        return -1;
    }
    if !context.active.load(Ordering::Acquire) {
        samples.fill(0.0);
        // SAFETY: only the serial receive callback accesses its elapsed sample counter.
        let elapsed = unsafe { &*context.state.get() }.elapsed_samples;
        context
            .status
            .update(false, context.origin_ms.saturating_add(elapsed / 48));
        return 0;
    }
    // SAFETY: only this serial receive endpoint accesses these cells while active.
    let (owner, state) = unsafe { (&mut *context.owner.get(), &mut *context.state.get()) };
    state.elapsed_samples = state.elapsed_samples.saturating_add(u64::from(count));
    let now_ms = context.origin_ms.saturating_add(state.elapsed_samples / 48);
    context.status.update(receiving != 0, now_ms);
    let Some(mut generation) = owner.as_mut().and_then(ReceiveOwner::acquire) else {
        samples.fill(0.0);
        return 0;
    };
    #[cfg(test)]
    if take_test_failure(TestFailure::ReceivePanic) {
        panic!("injected receive processing panic");
    }
    generation.state().process(receiving != 0, samples, now_ms);
    state.producer.write(samples);
    0
}
unsafe extern "C" fn transmit_callback(
    context: *mut c_void,
    pointer: *mut f32,
    count: u32,
    keyed: *mut u32,
) -> i32 {
    // SAFETY: the host supplies writable key storage for this call.
    if !keyed.is_null() {
        unsafe {
            keyed.write(0);
        }
    }
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        transmit(context, pointer, count, keyed)
    })) {
        Ok(result) => result,
        Err(_) => {
            // SAFETY: the borrowed host buffer remains valid through this return.
            if let Some(samples) = unsafe { samples(pointer, count) } {
                samples.fill(0.0);
            }
            if !keyed.is_null() {
                unsafe {
                    keyed.write(0);
                }
            }
            -1
        }
    }
}
unsafe fn transmit(context: *mut c_void, pointer: *mut f32, count: u32, keyed: *mut u32) -> i32 {
    let Some(samples) = (unsafe { samples(pointer, count) }) else {
        return -1;
    };
    // SAFETY: the host retains this context until synchronous destroy returns.
    let Some(context) = (unsafe { context.cast::<TransmitContext>().as_ref() }) else {
        samples.fill(0.0);
        return -1;
    };
    if keyed.is_null() || samples.len() > context.maximum {
        samples.fill(0.0);
        return -1;
    }
    if !context.active.load(Ordering::Acquire) {
        samples.fill(0.0);
        return 0;
    }
    // SAFETY: only this serial transmit endpoint accesses these cells while active.
    let (owner, consumer) = unsafe { (&mut *context.owner.get(), &mut *context.consumer.get()) };
    consumer.read(samples);
    let receiving = context.status.snapshot().0;
    let Some(mut generation) = owner.as_mut().and_then(TransmitOwner::acquire) else {
        samples.fill(0.0);
        return 0;
    };
    let transmit = generation.state();
    #[cfg(test)]
    if take_test_failure(TestFailure::TransmitPanic) {
        panic!("injected transmit processing panic");
    }
    match transmit
        .adapter
        .process(&mut transmit.controller, receiving, samples)
    {
        Ok(value) => {
            // SAFETY: checked non-null writable result, borrowed for this call.
            unsafe {
                keyed.write(u32::from(value));
            }
            0
        }
        Err(_) => {
            samples.fill(0.0);
            -1
        }
    }
}
/// Stable preallocated callback contexts; no product audio OS thread is created.
pub struct RadioWorker {
    radio: Option<Radio>,
    receive: Box<ReceiveContext>,
    transmit: Box<TransmitContext>,
}
impl RadioWorker {
    /// Attach both inactive endpoints and start the reserved channel transactionally.
    /// Failed activation synchronously detaches endpoints and returns the reservation.
    pub fn prepare(
        mut radio: Radio,
        epoch: Instant,
        status: RadioStatus,
    ) -> Result<Self, (Error, Radio)> {
        #[cfg(test)]
        if take_test_failure(TestFailure::Prepare) {
            return Err((Error::Allocation, radio));
        }
        let maximum = radio.maximum_frames();
        let Some(capacity) = maximum.checked_mul(2).filter(|n| *n != 0) else {
            return Err((Error::InvalidFrame, radio));
        };
        let (producer, consumer) = LinkAudioQueue::new(capacity)
            .expect("nonzero capacity")
            .into_endpoints();
        let active = Arc::new(AtomicBool::new(false));
        let receive = Box::new(ReceiveContext {
            active: active.clone(),
            owner: UnsafeCell::new(None),
            state: UnsafeCell::new(ReceiveState {
                producer,
                elapsed_samples: 0,
            }),
            maximum,
            origin_ms: epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            status: status.clone(),
        });
        let transmit = Box::new(TransmitContext {
            active,
            owner: UnsafeCell::new(None),
            consumer: UnsafeCell::new(consumer),
            maximum,
            status,
        });
        // SAFETY: boxes have stable addresses and outlive the activated radio. On failure
        // the host must detach both endpoints before returning the still-owned reservation.
        if unsafe {
            radio.activate(
                Some(receive_callback),
                std::ptr::from_ref(&*receive).cast_mut().cast(),
                Some(transmit_callback),
                std::ptr::from_ref(&*transmit).cast_mut().cast(),
            )
        }
        .is_err()
        {
            return Err((Error::Operation, radio));
        }
        Ok(Self {
            radio: Some(radio),
            receive,
            transmit,
        })
    }
    /// Inject a single preparation/admission failure.
    #[cfg(test)]
    pub(crate) fn fail_next_for_test(failure: TestFailure) {
        NEXT_FAILURE.with(|next| next.set(failure as u8));
    }
    /// Publish the two fixed owners once after runtime publication.
    pub fn attach(&mut self, owners: AudioOwners) -> Result<(), AudioOwners> {
        if self.receive.active.load(Ordering::Acquire) {
            return Err(owners);
        }
        // SAFETY: inactive callbacks never access either owner slot. Release publication
        // occurs only after both slots are initialized; no further control writes occur.
        unsafe {
            *self.receive.owner.get() = Some(owners.0);
            *self.transmit.owner.get() = Some(owners.1);
        }
        self.receive.active.store(true, Ordering::Release);
        Ok(())
    }
    /// Synchronously stop/destroy the channel before reclaiming either fixed owner.
    pub fn stop(mut self) -> Option<AudioOwners> {
        drop(self.radio.take());
        self.receive.active.store(false, Ordering::Release);
        // SAFETY: synchronous destroy has stopped both callbacks.
        let receive = unsafe { &mut *self.receive.owner.get() }.take();
        let transmit = unsafe { &mut *self.transmit.owner.get() }.take();
        receive.zip(transmit)
    }
}
impl Drop for RadioWorker {
    fn drop(&mut self) {
        // Must precede automatic field destruction, including partially activated setup.
        drop(self.radio.take());
    }
}
#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
