//! Fixed direct radio callbacks and control-only reservation lifetime.
use crate::{
    Error,
    link::ring::{
        InboundConsumer, InboundObserver, InboundPolicy, InboundProducer, InboundRing, Observation,
        RingError,
    },
    services::{PeerIo, Radio},
};
use rpt_advanced_core::{
    link::LinkAudio,
    runtime::{
        OwnedReceiveOwner, OwnedTransmitOwner, RuntimeAudioOwners, RuntimeTransmit,
        dtmf::DtmfWorker,
    },
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    cell::UnsafeCell,
    ffi::{CString, c_char, c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

const SNAPSHOT_QUEUE_CAPACITY: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RingSnapshot {
    occupancy: u64,
    reserve: u64,
    target: u64,
    capacity: u64,
    ratio_ppm: i32,
    missing_samples: u64,
    discarded_samples: u64,
}

impl From<&Observation> for RingSnapshot {
    fn from(ring: &Observation) -> Self {
        Self {
            occupancy: ring.available_samples,
            reserve: ring.reserve_samples,
            target: ring.target_samples,
            capacity: ring.capacity_samples,
            ratio_ppm: ring.ratio_correction_ppm,
            missing_samples: ring.missing_samples,
            discarded_samples: ring.discarded_samples,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MeterSnapshot {
    timestamp_ms: u64,
    samples: u64,
    peak: f32,
    rms: f32,
    clipped_samples: u64,
    dropped_snapshots: u64,
    ring: Option<RingSnapshot>,
}

struct MeterWindow {
    interval_samples: u64,
    next_snapshot_sample: u64,
    elapsed_samples: u64,
    samples: u64,
    sum_squares: f64,
    peak: f32,
    clipped_samples: u64,
    origin_ms: u64,
}

impl MeterWindow {
    fn new(interval_ms: u64, origin_ms: u64) -> Self {
        let interval_samples = interval_ms.saturating_mul(48).max(1);
        Self {
            interval_samples,
            next_snapshot_sample: interval_samples,
            elapsed_samples: 0,
            samples: 0,
            sum_squares: 0.0,
            peak: 0.0,
            clipped_samples: 0,
            origin_ms,
        }
    }

    fn configure(&mut self, interval_ms: u64) {
        let interval_samples = interval_ms.saturating_mul(48).max(1);
        if self.interval_samples != interval_samples {
            self.interval_samples = interval_samples;
            self.next_snapshot_sample = self.elapsed_samples.saturating_add(interval_samples);
            self.samples = 0;
            self.sum_squares = 0.0;
            self.peak = 0.0;
            self.clipped_samples = 0;
        }
    }

    fn observe(&mut self, audio: &[f32]) -> Option<MeterSnapshot> {
        if audio.is_empty() {
            return None;
        }
        for sample in audio {
            let value = if sample.is_finite() { *sample } else { 0.0 };
            let magnitude = value.abs();
            self.peak = self.peak.max(magnitude);
            self.sum_squares += f64::from(value) * f64::from(value);
            self.clipped_samples = self
                .clipped_samples
                .saturating_add(u64::from(magnitude >= 1.0));
        }
        self.samples = self.samples.saturating_add(audio.len() as u64);
        self.elapsed_samples = self.elapsed_samples.saturating_add(audio.len() as u64);
        if self.elapsed_samples < self.next_snapshot_sample {
            return None;
        }

        let snapshot = MeterSnapshot {
            timestamp_ms: self.origin_ms.saturating_add(self.elapsed_samples / 48),
            samples: self.samples,
            peak: self.peak,
            rms: (self.sum_squares / self.samples as f64).sqrt() as f32,
            clipped_samples: self.clipped_samples,
            ..MeterSnapshot::default()
        };
        self.samples = 0;
        self.sum_squares = 0.0;
        self.peak = 0.0;
        self.clipped_samples = 0;
        while self.next_snapshot_sample <= self.elapsed_samples {
            let next = self
                .next_snapshot_sample
                .saturating_add(self.interval_samples);
            if next == self.next_snapshot_sample {
                break;
            }
            self.next_snapshot_sample = next;
        }
        Some(snapshot)
    }
}

struct MeterPublisher {
    window: MeterWindow,
    snapshots: Producer<MeterSnapshot>,
    dropped_snapshots: u64,
}

impl MeterPublisher {
    fn publish(&mut self, audio: &[f32], observer: Option<&InboundObserver>) {
        let Some(mut snapshot) = self.window.observe(audio) else {
            return;
        };
        snapshot.dropped_snapshots = self.dropped_snapshots;
        if let Some(observer) = observer {
            // The observer copies lock-free ring counters; it does not consume PCM.
            snapshot.ring = observer.observe().ok().as_ref().map(RingSnapshot::from);
        }
        if let Err(rtrb::PushError::Full(_)) = self.snapshots.push(snapshot) {
            self.dropped_snapshots = self.dropped_snapshots.saturating_add(1);
        }
    }
}

fn dbfs(value: f32) -> f32 {
    if value > 0.0 {
        20.0 * value.log10()
    } else {
        f32::NEG_INFINITY
    }
}

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
    /// Fail one consumer reset to exercise fail-closed retry.
    Reset = 4,
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
pub struct RadioStatus {
    carrier: Arc<AtomicU64>,
    dtmf: Arc<AtomicBool>,
    transmit_keyed: Arc<AtomicBool>,
    handoff: Arc<LocalHandoff>,
}
#[derive(Default)]
/// RX owns the qualification token; TX acknowledges each completed tail reset.
/// The low bit is current qualification and the other bits count falling edges,
/// so a complete unkey/rekey between TX callbacks cannot hide a stale burst.
struct LocalHandoff {
    token: AtomicU64,
    reset_ack: AtomicU64,
    pending_reset_dropped: AtomicU64,
    write_failed_samples: AtomicU64,
    reset_failures: AtomicU64,
    render_failures: AtomicU64,
}

struct ReceiveRing {
    producer: InboundProducer,
    delay_ms: u64,
    handoff: Arc<LocalHandoff>,
}
struct TransmitRing {
    consumer: InboundConsumer,
    observer: InboundObserver,
    delay_ms: u64,
    handoff: Arc<LocalHandoff>,
}

/// Control-prepared replacement; callbacks only move its already allocated endpoints.
pub(crate) struct LocalRingUpdate {
    receive: ReceiveRing,
    transmit: TransmitRing,
}
impl LocalRingUpdate {
    /// Allocate one empty ring and independent qualification state on control.
    pub(crate) fn prepare(delay_ms: u64) -> Result<Self, RingError> {
        Self::with_handoff(delay_ms, Arc::default())
    }
    fn with_handoff(delay_ms: u64, handoff: Arc<LocalHandoff>) -> Result<Self, RingError> {
        let (producer, consumer) = InboundRing::open(
            48_000,
            InboundPolicy::Local {
                squelch_delay_ms: delay_ms,
            },
        )?;
        let observer = producer.observer();
        Ok(Self {
            receive: ReceiveRing {
                producer,
                delay_ms,
                handoff: handoff.clone(),
            },
            transmit: TransmitRing {
                consumer,
                observer,
                delay_ms,
                handoff,
            },
        })
    }
}

struct EndpointControl<T> {
    ready: Producer<(u64, T)>,
    retired: Consumer<T>,
    adopted: Arc<AtomicU64>,
}
struct EndpointOwner<T> {
    current: T,
    ready: Consumer<(u64, T)>,
    retired: Producer<T>,
    pending: Option<(u64, T)>,
    adopted: Arc<AtomicU64>,
}
impl<T> EndpointOwner<T> {
    fn new(current: T) -> (Self, EndpointControl<T>) {
        let (send, ready) = RingBuffer::new(1);
        let (retired, reclaim) = RingBuffer::new(1);
        let adopted = Arc::new(AtomicU64::new(0));
        (
            Self {
                current,
                ready,
                retired,
                pending: None,
                adopted: adopted.clone(),
            },
            EndpointControl {
                ready: send,
                retired: reclaim,
                adopted,
            },
        )
    }

    // One candidate is retained until both acknowledgments return. No endpoint is
    // destroyed here, including when its bounded return queue is temporarily full.
    fn adopt(&mut self, generation: u64) {
        if self.pending.is_none() {
            self.pending = self.ready.pop().ok();
        }
        let Some((id, next)) = self.pending.take() else {
            return;
        };
        if id != generation {
            self.pending = Some((id, next));
            return;
        }
        let old = std::mem::replace(&mut self.current, next);
        if let Err(rtrb::PushError::Full(old)) = self.retired.push(old) {
            self.pending = Some((id, std::mem::replace(&mut self.current, old)));
            return;
        }
        self.adopted.store(id, Ordering::Release);
    }
}
impl<T> EndpointControl<T> {
    fn reclaim(&mut self) {
        while let Ok(retired) = self.retired.pop() {
            drop(retired);
        }
    }
}

impl RadioStatus {
    /// Current receive state and original transition time.
    pub fn snapshot(&self) -> (bool, u64) {
        let value = self.carrier.load(Ordering::Acquire);
        (value & 1 != 0, value >> 1)
    }
    pub(crate) fn transmit_keyed(&self) -> bool {
        self.transmit_keyed.load(Ordering::Acquire)
    }
    fn update(&self, receiving: bool, now_ms: u64) {
        self.update_carrier(receiving, now_ms);
        self.update_qualification(receiving, &self.handoff);
    }
    fn update_qualification(&self, receiving: bool, handoff: &LocalHandoff) {
        let previous = handoff.token.load(Ordering::Relaxed);
        if previous & 1 != u64::from(receiving) {
            // Publish only after the serial RX endpoint finished its previous write.
            handoff.token.store(
                if receiving {
                    previous | 1
                } else {
                    previous.wrapping_add(1)
                },
                Ordering::Release,
            );
        }
    }
    fn update_carrier(&self, receiving: bool, now_ms: u64) {
        if self.carrier.load(Ordering::Relaxed) & 1 != u64::from(receiving) {
            self.carrier.store(
                (now_ms.min(u64::MAX >> 1) << 1) | u64::from(receiving),
                Ordering::Release,
            );
        }
    }
    fn set_dtmf_muted(&self, muted: bool) {
        self.dtmf.store(muted, Ordering::Release);
    }
    fn dtmf_muted(&self) -> bool {
        self.dtmf.load(Ordering::Acquire)
    }
}
struct ReceiveState {
    ring: EndpointOwner<ReceiveRing>,
    elapsed_samples: u64,
    meter: MeterPublisher,
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
    ring: UnsafeCell<EndpointOwner<TransmitRing>>,
    maximum: usize,
    status: RadioStatus,
    meter: UnsafeCell<MeterPublisher>,
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
    // SAFETY: the host lends aligned writable PCM for this complete callback.
    let Some(samples) = (unsafe { samples(pointer, count) }) else {
        return -1;
    };
    let result = match catch_unwind(AssertUnwindSafe(|| unsafe {
        receive(context, receiving, samples)
    })) {
        Ok(result) => result,
        Err(_) => {
            samples.fill(0.0);
            -1
        }
    };
    // SAFETY: receive() and its panic boundary have returned; this serial callback
    // is now the sole access to its meter publisher.
    if let Some(context) = unsafe { context.cast::<ReceiveContext>().as_ref() } {
        unsafe { &mut *context.state.get() }
            .meter
            .publish(samples, None);
    }
    result
}
unsafe fn receive(context: *mut c_void, receiving: u32, samples: &mut [f32]) -> i32 {
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
    let count = samples.len() as u64;
    state.elapsed_samples = state.elapsed_samples.saturating_add(count);
    let now_ms = context.origin_ms.saturating_add(state.elapsed_samples / 48);
    context.status.update_carrier(receiving != 0, now_ms);
    context
        .status
        .update_qualification(receiving != 0, &state.ring.current.handoff);
    let Some(mut generation) = owner.as_mut().expect("active receive owner").acquire() else {
        samples.fill(0.0);
        return 0;
    };
    state
        .meter
        .window
        .configure(generation.settings().status_snapshot_interval_ms);
    state.ring.adopt(generation.id());
    let ring = &mut state.ring.current;
    context
        .status
        .update_qualification(receiving != 0, &ring.handoff);
    if ring.delay_ms != generation.settings().squelch_delay_ms {
        samples.fill(0.0);
        return 0;
    }
    #[cfg(test)]
    if take_test_failure(TestFailure::ReceivePanic) {
        panic!("injected receive processing panic");
    }
    generation.state().process(receiving != 0, samples, now_ms);
    let muted = receiving != 0 && generation.state().suppressing();
    context.status.set_dtmf_muted(muted);
    if receiving != 0 {
        let handoff = &ring.handoff;
        let epoch = handoff.token.load(Ordering::Acquire) & !1;
        if epoch != handoff.reset_ack.load(Ordering::Acquire) {
            // Bound the handoff to one reset acknowledgement. Do not let new audio
            // enter storage that TX is about to discard with the previous burst.
            handoff
                .pending_reset_dropped
                .fetch_add(count, Ordering::Relaxed);
        } else if ring.producer.write(samples).is_err() {
            handoff
                .write_failed_samples
                .fetch_add(count, Ordering::Relaxed);
        }
    }
    0
}
unsafe extern "C" fn transmit_callback(
    context: *mut c_void,
    pointer: *mut f32,
    count: u32,
    keyed: *mut u32,
    ctcss_enabled: *mut u32,
) -> i32 {
    // SAFETY: the host retains this callback context through synchronous destroy.
    let callback_context = unsafe { context.cast::<TransmitContext>().as_ref() };
    let status = callback_context.map(|context| &context.status);
    // SAFETY: the host supplies writable key storage for this call.
    let keyed = unsafe { keyed.as_mut() };
    // SAFETY: the host supplies writable CTCSS state storage for this call.
    let ctcss_enabled = unsafe { ctcss_enabled.as_mut() };
    // SAFETY: the host lends aligned writable PCM for this complete callback.
    let Some(samples) = (unsafe { samples(pointer, count) }) else {
        if let Some(keyed) = keyed {
            *keyed = 0;
        }
        if let Some(ctcss_enabled) = ctcss_enabled {
            *ctcss_enabled = 0;
        }
        publish_transmit_keyed(status, false);
        return -1;
    };
    if keyed.is_none() || ctcss_enabled.is_none() {
        samples.fill(0.0);
        if let Some(keyed) = keyed {
            *keyed = 0;
        }
        if let Some(ctcss_enabled) = ctcss_enabled {
            *ctcss_enabled = 0;
        }
        publish_transmit_keyed(status, false);
        publish_transmit_samples(callback_context, samples);
        return -1;
    }
    let keyed = keyed.expect("validated PTT output");
    let ctcss_enabled = ctcss_enabled.expect("validated CTCSS output");
    *keyed = 0;
    *ctcss_enabled = 0;
    let result = match catch_unwind(AssertUnwindSafe(|| unsafe {
        transmit(context, samples, keyed, ctcss_enabled)
    })) {
        Ok(result) => result,
        Err(_) => {
            samples.fill(0.0);
            *keyed = 0;
            *ctcss_enabled = 0;
            -1
        }
    };
    publish_transmit_keyed(status, result == 0 && *keyed != 0);
    publish_transmit_samples(callback_context, samples);
    result
}
fn publish_transmit_samples(context: Option<&TransmitContext>, samples: &[f32]) {
    if let Some(context) = context {
        // SAFETY: the serialized TX callback is the sole writer of this publisher.
        let ring = unsafe { &*context.ring.get() };
        unsafe { &mut *context.meter.get() }.publish(samples, Some(&ring.current.observer));
    }
}
fn publish_transmit_keyed(status: Option<&RadioStatus>, keyed: bool) {
    if let Some(status) = status {
        status.transmit_keyed.store(keyed, Ordering::Release);
    }
}
unsafe fn transmit(
    context: *mut c_void,
    samples: &mut [f32],
    keyed: &mut u32,
    ctcss_enabled: &mut u32,
) -> i32 {
    // SAFETY: the host retains this context until synchronous destroy returns.
    let Some(context) = (unsafe { context.cast::<TransmitContext>().as_ref() }) else {
        samples.fill(0.0);
        *keyed = 0;
        *ctcss_enabled = 0;
        return -1;
    };
    if samples.len() > context.maximum {
        samples.fill(0.0);
        *keyed = 0;
        *ctcss_enabled = 0;
        return -1;
    }
    if !context.active.load(Ordering::Acquire) {
        samples.fill(0.0);
        *keyed = 0;
        *ctcss_enabled = 0;
        return 0;
    }
    // SAFETY: only this serial transmit endpoint accesses these cells while active.
    let (owner, endpoint) = unsafe { (&mut *context.owner.get(), &mut *context.ring.get()) };
    samples.fill(0.0);
    let Some(mut generation) = owner.as_mut().expect("active transmit owner").acquire() else {
        return 0;
    };
    // SAFETY: this serial TX callback exclusively owns its meter window.
    unsafe { &mut *context.meter.get() }
        .window
        .configure(generation.settings().status_snapshot_interval_ms);
    endpoint.adopt(generation.id());
    let ring = &mut endpoint.current;
    if ring.delay_ms != generation.settings().squelch_delay_ms {
        return 0;
    }
    let consumer = &mut ring.consumer;
    let handoff = &ring.handoff;
    let token = handoff.token.load(Ordering::Acquire);
    let epoch = token & !1;
    let mut reset_ok = true;
    if epoch != handoff.reset_ack.load(Ordering::Acquire) {
        if reset_local(consumer).is_ok() {
            handoff.reset_ack.store(epoch, Ordering::Release);
        } else {
            // Retry on the next TX callback; never acknowledge a failed reset.
            handoff.reset_failures.fetch_add(1, Ordering::Relaxed);
            reset_ok = false;
        }
    }
    let mut receiving = token & 1 != 0 && reset_ok;
    // An idle consumer must not drain a newly published burst or synthesize PLC.
    if receiving && consumer.render(samples).is_err() {
        handoff.render_failures.fetch_add(1, Ordering::Relaxed);
        samples.fill(0.0);
        receiving = false;
    }
    // Cancel the rendered tail if RX unkeyed or changed bursts during this call.
    receiving &= handoff.token.load(Ordering::Acquire) == token;
    if !receiving || context.status.dtmf_muted() {
        samples.fill(0.0);
    }
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
            *keyed = u32::from(value);
            *ctcss_enabled = u32::from(transmit.controller.ctcss_enabled());
            0
        }
        Err(_) => {
            samples.fill(0.0);
            *keyed = 0;
            *ctcss_enabled = 0;
            -1
        }
    }
}
fn reset_local(consumer: &mut InboundConsumer) -> Result<(), RingError> {
    #[cfg(test)]
    if take_test_failure(TestFailure::Reset) {
        return Err(RingError);
    }
    consumer.reset()
}
/// Stable preallocated callback contexts; no product audio OS thread is created.
pub struct RadioWorker {
    radio: Option<Radio>,
    receive: Box<ReceiveContext>,
    transmit: Box<TransmitContext>,
    observer: InboundObserver,
    handoff: Arc<LocalHandoff>,
    squelch_delay_ms: u64,
    receive_updates: EndpointControl<ReceiveRing>,
    transmit_updates: EndpointControl<TransmitRing>,
    pending_update: Option<(u64, u64, InboundObserver, Arc<LocalHandoff>)>,
    receive_snapshots: Consumer<MeterSnapshot>,
    transmit_snapshots: Consumer<MeterSnapshot>,
    latest_receive: Option<MeterSnapshot>,
    latest_transmit: Option<MeterSnapshot>,
    reported_faults: [u64; 6],
    last_report_ms: u64,
}
impl RadioWorker {
    /// Bind a peer on control while this worker retains its live radio lease.
    pub fn bind_peer(&self, peer: &mut PeerIo) -> Result<(), Error> {
        peer.bind_radio(self.radio.as_ref().expect("live radio reservation"))
    }

    /// Effective reserve currently installed, or prepared for this worker's next adoption.
    pub(crate) fn squelch_delay_ms(&self) -> u64 {
        self.pending_update
            .as_ref()
            .map_or(self.squelch_delay_ms, |update| update.1)
    }

    /// Publish a prepared pair without changing the stream or fixed callback registrations.
    pub(crate) fn update_local_ring(
        &mut self,
        generation: u64,
        update: LocalRingUpdate,
    ) -> Result<(), LocalRingUpdate> {
        self.receive_updates.reclaim();
        self.transmit_updates.reclaim();
        if self.pending_update.is_some()
            || self.receive_updates.ready.slots() == 0
            || self.transmit_updates.ready.slots() == 0
        {
            return Err(update);
        }
        self.pending_update = Some((
            generation,
            update.transmit.delay_ms,
            update.transmit.observer.clone(),
            update.transmit.handoff.clone(),
        ));
        // This is the sole producer and each queue has an available slot.
        assert!(
            self.receive_updates
                .ready
                .push((generation, update.receive))
                .is_ok()
        );
        assert!(
            self.transmit_updates
                .ready
                .push((generation, update.transmit))
                .is_ok()
        );
        Ok(())
    }

    /// Reclaim returned endpoints on control and report when both owners acknowledge the pair.
    pub(crate) fn local_ring_applied(&mut self, generation: u64) -> bool {
        self.receive_updates.reclaim();
        self.transmit_updates.reclaim();
        let applied = self.receive_updates.adopted.load(Ordering::Acquire) == generation
            && self.transmit_updates.adopted.load(Ordering::Acquire) == generation;
        if !applied {
            return false;
        }
        if let Some((_, delay_ms, observer, handoff)) = self.pending_update.take() {
            self.squelch_delay_ms = delay_ms;
            self.observer = observer;
            self.handoff = handoff;
        }
        true
    }

    /// Attach both inactive endpoints and start the reserved channel transactionally.
    /// Failed activation synchronously detaches endpoints and returns the reservation.
    pub fn prepare(
        mut radio: Radio,
        epoch: Instant,
        status: RadioStatus,
        squelch_delay_ms: u64,
        status_snapshot_interval_ms: u64,
    ) -> Result<Self, (Error, Radio)> {
        let maximum = radio.maximum_frames();
        let ring = LocalRingUpdate::with_handoff(squelch_delay_ms, status.handoff.clone());
        #[cfg(test)]
        let ring = if take_test_failure(TestFailure::Prepare) {
            Err(RingError)
        } else {
            ring
        };
        let Ok(update) = ring else {
            return Err((Error::Allocation, radio));
        };
        let observer = update.transmit.observer.clone();
        let handoff = update.transmit.handoff.clone();
        let (receive_ring, receive_updates) = EndpointOwner::new(update.receive);
        let (transmit_ring, transmit_updates) = EndpointOwner::new(update.transmit);
        let origin_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let (receive_publisher, receive_snapshots) = RingBuffer::new(SNAPSHOT_QUEUE_CAPACITY);
        let (transmit_publisher, transmit_snapshots) = RingBuffer::new(SNAPSHOT_QUEUE_CAPACITY);
        let active = Arc::new(AtomicBool::new(false));
        let receive = Box::new(ReceiveContext {
            active: active.clone(),
            owner: UnsafeCell::new(None),
            state: UnsafeCell::new(ReceiveState {
                ring: receive_ring,
                elapsed_samples: 0,
                meter: MeterPublisher {
                    window: MeterWindow::new(status_snapshot_interval_ms, origin_ms),
                    snapshots: receive_publisher,
                    dropped_snapshots: 0,
                },
            }),
            maximum,
            origin_ms,
            status: status.clone(),
        });
        let transmit = Box::new(TransmitContext {
            active,
            owner: UnsafeCell::new(None),
            ring: UnsafeCell::new(transmit_ring),
            maximum,
            status,
            meter: UnsafeCell::new(MeterPublisher {
                window: MeterWindow::new(status_snapshot_interval_ms, origin_ms),
                snapshots: transmit_publisher,
                dropped_snapshots: 0,
            }),
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
            observer,
            handoff,
            squelch_delay_ms,
            receive_updates,
            transmit_updates,
            pending_update: None,
            receive_snapshots,
            transmit_snapshots,
            latest_receive: None,
            latest_transmit: None,
            reported_faults: [0; 6],
            last_report_ms: 0,
        })
    }
    fn fault_counts(&self, ring: &Observation) -> [u64; 6] {
        let handoff = &self.handoff;
        [
            ring.missing_samples,
            ring.discarded_samples,
            handoff.pending_reset_dropped.load(Ordering::Relaxed),
            handoff.write_failed_samples.load(Ordering::Relaxed),
            handoff.reset_failures.load(Ordering::Relaxed),
            handoff.render_failures.load(Ordering::Relaxed),
        ]
    }
    fn format_status(&self, ring: &Observation) -> String {
        let [
            missing,
            discarded,
            pending,
            write_failed,
            reset_failed,
            render_failed,
        ] = self.fault_counts(ring);
        format!(
            "  local-rx: occupancy={}/{}ms reserve={}ms target={}ms configured-delay={}ms ratio={:+}ppm missing={} discarded={} pending-reset-dropped={} write-failed-samples={} reset-failures={} render-failures={}",
            ring.available_samples / 48,
            ring.capacity_samples / 48,
            ring.reserve_samples / 48,
            ring.target_samples / 48,
            self.squelch_delay_ms,
            ring.ratio_correction_ppm,
            missing,
            discarded,
            pending,
            write_failed,
            reset_failed,
            render_failed
        )
    }
    fn format_meter(label: &str, snapshot: Option<MeterSnapshot>) -> String {
        let Some(snapshot) = snapshot else {
            return format!("  {label}: awaiting first snapshot");
        };
        format!(
            "  {label}: at={}ms samples={} peak={:.1}dBFS rms={:.1}dBFS clipped-samples={} dropped-snapshots={}",
            snapshot.timestamp_ms,
            snapshot.samples,
            dbfs(snapshot.peak),
            dbfs(snapshot.rms),
            snapshot.clipped_samples,
            snapshot.dropped_snapshots
        )
    }
    fn format_ring_snapshot(ring: RingSnapshot) -> String {
        format!(
            "  local-rx-ring-snapshot: occupancy={}ms reserve={}ms target={}ms capacity={}ms ratio={:+}ppm missing={} discarded={}",
            ring.occupancy / 48,
            ring.reserve / 48,
            ring.target / 48,
            ring.capacity / 48,
            ring.ratio_ppm,
            ring.missing_samples,
            ring.discarded_samples
        )
    }
    /// Control-only snapshot of this worker's actual local receive ring.
    pub(crate) fn local_status_text(&self) -> String {
        let mut text = self.observer.observe().map_or_else(
            |_| "  local-rx: observation failed".to_owned(),
            |ring| self.format_status(&ring),
        );
        text.push('\n');
        text.push_str(&Self::format_meter("local-rx-audio", self.latest_receive));
        text.push('\n');
        text.push_str(&Self::format_meter("transmit-audio", self.latest_transmit));
        if let Some(ring) = self.latest_transmit.and_then(|snapshot| snapshot.ring) {
            text.push('\n');
            text.push_str(&Self::format_ring_snapshot(ring));
        }
        text
    }
    /// Drain bounded callback snapshots and retain only each direction's latest window.
    pub(crate) fn collect_snapshots(&mut self) {
        while let Ok(snapshot) = self.receive_snapshots.pop() {
            self.latest_receive = Some(snapshot);
        }
        while let Ok(snapshot) = self.transmit_snapshots.pop() {
            self.latest_transmit = Some(snapshot);
        }
    }
    /// Record changed fault counters at most every five seconds, never from audio.
    /// Syslog retains evidence even when the service discards stdout and stderr.
    pub(crate) fn report_faults(&mut self, local: &str, now_ms: u64) {
        if now_ms.saturating_sub(self.last_report_ms) < 5_000 {
            return;
        }
        self.last_report_ms = now_ms;
        if let Ok(ring) = self.observer.observe() {
            let counts = self.fault_counts(&ring);
            if counts != self.reported_faults {
                self.reported_faults = counts;
                if let Ok(message) = CString::new(format!(
                    "rpt_advanced {local} at {now_ms}ms: {}",
                    self.format_status(&ring)
                )) {
                    unsafe extern "C" {
                        fn syslog(priority: c_int, format: *const c_char, ...);
                    }
                    // SAFETY: constant format and live NUL-terminated string match
                    // the variadic C signature. Priority 5 is LOG_NOTICE.
                    unsafe {
                        syslog(5, c"%s".as_ptr(), message.as_ptr());
                    }
                }
            }
        }
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
        self.transmit
            .status
            .transmit_keyed
            .store(false, Ordering::Release);
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
        self.transmit
            .status
            .transmit_keyed
            .store(false, Ordering::Release);
    }
}
#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
