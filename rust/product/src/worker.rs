//! Fixed direct radio callbacks and control-only reservation lifetime.
use crate::{
    Error,
    link::ring::{
        InboundConsumer, InboundObserver, InboundPolicy, InboundProducer, InboundRing, Observation,
        RingError,
    },
    radio_feedback::{self, PhysicalReceive, PhysicalTransmit},
    services::{PeerIo, ProcessedPeer, Radio},
    status_meter::{self, Measurement, Quality, Span, Tap},
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
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
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
pub type Audio = LinkAudio<ProcessedPeer<InboundConsumer>>;
/// Fixed registrations transferred once on control.
pub type AudioOwners = RuntimeAudioOwners<Audio>;
type ReceiveOwner = OwnedReceiveOwner<DtmfWorker, RuntimeTransmit<Audio>>;
type TransmitOwner = OwnedTransmitOwner<DtmfWorker, RuntimeTransmit<Audio>>;

/// Control-readable carrier edge with coherent monotonic transition time.
#[derive(Clone, Default)]
pub struct RadioStatus {
    pub(crate) source_clock: Arc<crate::link::source_clock::SourceClock>,
    carrier: Arc<AtomicU64>,
    physical_feedback: Arc<AtomicU64>,
    receive_sample_ms: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    pub(crate) edges: Arc<crate::management::status::edges::RadioEdges>,
    #[cfg(target_os = "linux")]
    rx_detail: Arc<AtomicU32>,
    #[cfg(target_os = "linux")]
    qualified_detail: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    qualified_burst: Arc<AtomicU64>,
    /// The last exact admission closed by RX, with a validity bit independent of the next key.
    #[cfg(target_os = "linux")]
    qualified_end: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    tx_detail: Arc<AtomicU32>,
    #[cfg(target_os = "linux")]
    transmit_policy: Arc<AtomicU64>,
    dtmf: Arc<AtomicBool>,
    transmit_keyed: Arc<AtomicU64>,
    // The control owner publishes monotonic time. The audio callback only copies its stamp.
    transmit_control_ms: Arc<AtomicU64>,
    remote_controls: Arc<AtomicU32>,
    handoff: Arc<LocalHandoff>,
    vad_delay_ms: Arc<AtomicU64>,
    current_handoff: Arc<AtomicPtr<LocalHandoff>>,
}
#[derive(Default)]
/// RX owns raw qualification and speech admission; TX acknowledges required resets.
/// Normal VAD RF edges advance the token while retaining tagged PCM. Actual faults
/// separately request a reset, including faults observed after raw RF has closed.
struct LocalHandoff {
    token: AtomicU64,
    admission: AtomicU64,
    #[cfg(target_os = "linux")]
    status_burst: AtomicU64,
    reset_required: AtomicU64,
    reset_ack: AtomicU64,
    pending_reset_dropped: AtomicU64,
    write_failed_samples: AtomicU64,
    reset_failures: AtomicU64,
    render_failures: AtomicU64,
}

struct ReceiveRing {
    producer: InboundProducer,
    delay_ms: u64,
    fresh: bool,
    tagged_start: u64,
    tagged_end: Option<u64>,
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
        let (producer, consumer) = InboundRing::open_receiver(
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
                fresh: true,
                tagged_start: 0,
                tagged_end: None,
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
    pub(crate) fn with_epoch(epoch: Instant) -> Self {
        Self {
            source_clock: Arc::new(crate::link::source_clock::SourceClock::new(epoch)),
            ..Self::default()
        }
    }
    /// Latest admitted independent TX observations, subject to source freshness.
    /// Last valid RX callback sample, in the controller's monotonic time domain.
    pub(crate) fn receive_sample_time(&self) -> Option<u64> {
        self.receive_sample_ms
            .load(Ordering::Acquire)
            .checked_sub(1)
    }

    pub(crate) fn physical_transmit(&self, now_ms: u64) -> Option<PhysicalTransmit> {
        PhysicalTransmit::from_word(self.physical_feedback.load(Ordering::Acquire), now_ms)
    }

    /// Current receive state and original transition time.
    pub fn snapshot(&self) -> (bool, u64) {
        let value = self.carrier.load(Ordering::Acquire);
        (value & 1 != 0, value >> 1)
    }
    /// Actual local activity for explicit command-source waiting. An intentionally
    /// open, unqualified non-FM receiver cannot supply an unkey edge.
    pub fn remote_command_source_active(&self) -> bool {
        self.snapshot().0 && self.remote_controls.load(Ordering::Acquire) & 25 != 25
    }
    pub(crate) fn transmit_observation(&self, now_ms: u64) -> Option<bool> {
        self.transmit_control_ms
            .store(now_ms.min(u64::MAX >> 2), Ordering::Release);
        self.recent_transmit_request(now_ms)
    }
    /// Callback-requested TX state. This is distinct from electrical PTT readback.
    pub(crate) fn recent_transmit_request(&self, now_ms: u64) -> Option<bool> {
        // A 4096-frame callback covers about 85 ms at 48 kHz. Allow several buffers of jitter.
        const MAX_AGE_MS: u64 = 500;
        let now_ms = now_ms.min(u64::MAX >> 2);
        let observation = self.transmit_keyed.load(Ordering::Acquire);
        if now_ms.saturating_sub(observation >> 2) > MAX_AGE_MS {
            return None;
        }
        match observation & 3 {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }
    /// Last successfully rendered policy from the native TX owner.
    #[cfg(target_os = "linux")]
    pub(crate) fn recent_transmit_policy(
        &self,
        now_ms: u64,
    ) -> Option<(u64, rpt_advanced_core::controller::TransmitPolicyStatus)> {
        let word = self.transmit_policy.load(Ordering::Acquire);
        let at_ms = word >> 4;
        (word & 1 != 0 && self.recent_transmit_request(now_ms).is_some()).then_some((
            at_ms,
            rpt_advanced_core::controller::TransmitPolicyStatus {
                inhibited: word & 2 != 0,
                timed_out: word & 4 != 0,
                traffic_holding: word & 8 != 0,
            },
        ))
    }

    #[cfg(target_os = "linux")]
    fn publish_transmit_policy(
        &self,
        policy: Option<rpt_advanced_core::controller::TransmitPolicyStatus>,
    ) {
        let stamp = self
            .transmit_control_ms
            .load(Ordering::Acquire)
            .min(u64::MAX >> 4);
        let detail = policy.map_or(0, |policy| {
            1 | u64::from(policy.inhibited) << 1
                | u64::from(policy.timed_out) << 2
                | u64::from(policy.traffic_holding) << 3
        });
        let previous = self.transmit_policy.load(Ordering::Acquire);
        if previous & 15 != detail {
            self.transmit_policy
                .store(stamp << 4 | detail, Ordering::Release);
            self.record_radio_edge(
                stamp,
                crate::management::status::edges::RadioSignal::TransmitPolicy(policy),
            );
        }
    }

    #[cfg(test)]
    fn update(&self, receiving: bool, now_ms: u64) {
        self.update_carrier(receiving, now_ms);
        self.update_qualification(receiving, &self.handoff);
    }
    fn update_qualification(&self, receiving: bool, handoff: &LocalHandoff) {
        let previous = handoff.token.load(Ordering::Relaxed);
        if !receiving {
            handoff.admission.store(0, Ordering::Release);
        }
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
        let previous = self.carrier.load(Ordering::Relaxed);
        if previous & 1 != u64::from(receiving) {
            self.carrier.store(
                (now_ms.min(u64::MAX >> 1) << 1) | u64::from(receiving),
                Ordering::Release,
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn observe_qualified(&self, receiving: Option<bool>, now_ms: u64) {
        use crate::management::status::edges::RadioSignal;
        let state = receiving.map_or(0, |active| 1 + u64::from(active));
        let previous = self.qualified_detail.load(Ordering::Relaxed);
        let prior = previous & 3;
        if state == prior {
            return;
        }
        self.qualified_detail
            .store((now_ms.min(u64::MAX >> 2) << 2) | state, Ordering::Release);
        let burst = self.qualified_burst.load(Ordering::Relaxed);
        if state == 2 {
            let next = if burst < u64::MAX >> 1 {
                burst + 1
            } else {
                u64::MAX
            };
            self.qualified_burst.store(next, Ordering::Release);
        }
        let signal = match receiving {
            Some(qualified) => RadioSignal::Receive {
                qualified,
                duration_ms: (!qualified && prior == 2)
                    .then(|| now_ms.saturating_sub(previous >> 2)),
            },
            None => RadioSignal::ReceiveUnknown,
        };
        self.record_radio_edge(now_ms, signal);
        if state != 2 && prior == 2 && (1..=u64::MAX >> 1).contains(&burst) {
            self.qualified_end
                .store(burst << 1 | u64::from(state == 1), Ordering::Release);
        }
    }
    #[cfg(target_os = "linux")]
    fn record_radio_edge(&self, at_ms: u64, signal: crate::management::status::edges::RadioSignal) {
        self.edges
            .push(crate::management::status::edges::RadioEdge { at_ms, signal });
    }
    #[cfg(target_os = "linux")]
    fn record_rx_detail(&self, metadata: &crate::abi::UrpNativeReceiveMetadata, at_ms: u64) {
        let detail = 1
            | metadata.raw_receiver_keyed << 1
            | metadata.receiver_keyed << 2
            | metadata.vad_mode << 3
            | metadata.vad_voice_active << 5;
        if self.rx_detail.swap(detail, Ordering::AcqRel) != detail {
            self.record_radio_edge(
                at_ms,
                crate::management::status::edges::RadioSignal::Source {
                    raw: metadata.raw_receiver_keyed != 0,
                    carrier: metadata.receiver_keyed != 0,
                    vad_mode: metadata.vad_mode,
                    voice: metadata.vad_voice_active != 0,
                },
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
    source_generation: u64,
    next_source_sample: u64,
    rf_burst_id: u64,
    voice_after: u64,
    discontinuous: bool,
    meter: MeterPublisher,
    status_taps: [Tap; 2],
}
struct ReceiveContext {
    active: Arc<AtomicBool>,
    owner: UnsafeCell<Option<ReceiveOwner>>,
    state: UnsafeCell<ReceiveState>,
    maximum: usize,
    origin_ms: u64,
    status: RadioStatus,
}
#[cfg(target_os = "linux")]
#[derive(Default)]
struct ReceiveReleaseTracker {
    graph: u64,
    active: Option<u64>,
    pending: Option<PendingReceiveRelease>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct PendingReceiveRelease {
    burst: u64,
    at_ms: u64,
    release: rpt_advanced_core::controller::LocalReceiveRelease,
}

#[cfg(target_os = "linux")]
enum ReceiveTrace {
    Admitted(u64),
    Closed,
    Other,
}

#[cfg(target_os = "linux")]
impl ReceiveReleaseTracker {
    fn observe(
        &mut self,
        status: &RadioStatus,
        graph: u64,
        at_ms: u64,
        input: ReceiveTrace,
        release: Option<rpt_advanced_core::controller::LocalReceiveRelease>,
    ) {
        if self.graph != graph {
            *self = Self {
                graph,
                ..Self::default()
            };
        }
        if let Some(release) = release
            && let Some(burst) = self.active.take()
            && matches!(input, ReceiveTrace::Closed)
            && status.receive_sample_time().is_some_and(|sample| {
                at_ms.saturating_sub(sample) <= crate::radio_feedback::FRESH_MS
            })
        {
            if self.pending.is_some() {
                status.edges.mark_lost();
            }
            self.pending = Some(PendingReceiveRelease {
                burst,
                at_ms,
                release,
            });
        }
        if let ReceiveTrace::Admitted(burst) = input {
            self.active = (1..=u64::MAX >> 1).contains(&burst).then_some(burst);
        }
        let Some(pending) = self.pending else { return };
        if at_ms.saturating_sub(pending.at_ms) > crate::radio_feedback::FRESH_MS {
            self.pending = None;
            return;
        }
        let end = status.qualified_end.load(Ordering::Acquire);
        let ended_burst = end >> 1;
        if ended_burst == pending.burst && end & 1 != 0 {
            status.record_radio_edge(
                pending.at_ms,
                crate::management::status::edges::RadioSignal::ReceiveRelease(pending.release),
            );
        } else if ended_burst < pending.burst {
            return;
        } else if ended_burst > pending.burst {
            // The bounded receipt slot was superseded before TX could correlate it.
            status.edges.mark_lost();
        }
        self.pending = None;
    }
}

struct TransmitContext {
    active: Arc<AtomicBool>,
    owner: UnsafeCell<Option<TransmitOwner>>,
    ring: UnsafeCell<EndpointOwner<TransmitRing>>,
    maximum: usize,
    status: RadioStatus,
    meter: UnsafeCell<MeterPublisher>,
    status_taps: UnsafeCell<[Tap; 8]>,
    /// Borrowed only while measuring the serial TX callback's programme inputs.
    status_pcm: UnsafeCell<[Box<[f32]>; 6]>,
    remote_controls: UnsafeCell<u32>,
    linked_active: UnsafeCell<bool>,
    #[cfg(target_os = "linux")]
    receive_release: UnsafeCell<ReceiveReleaseTracker>,
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

/// Same serial TX endpoint as PCM: no cross-thread controller mutation.
unsafe extern "C" fn metadata_callback(
    context: *mut c_void,
    controls: u32,
    linked: *mut u32,
) -> i32 {
    // SAFETY: the host retains the context and serializes metadata with transmit.
    let (Some(context), Some(linked)) = (
        unsafe { context.cast::<TransmitContext>().as_ref() },
        unsafe { linked.as_mut() },
    ) else {
        return -1;
    };
    *linked = 0;
    if controls & !31 != 0 {
        return -1;
    }
    // SAFETY: only the host's serial TX owner accesses these cells.
    unsafe {
        *context.remote_controls.get() = controls;
    }
    context
        .status
        .remote_controls
        .store(controls, Ordering::Release);
    if context.active.load(Ordering::Acquire) {
        // SAFETY: same owner as the writer in transmit.
        *linked = u32::from(unsafe { *context.linked_active.get() });
    }
    0
}

unsafe fn samples<'a>(pointer: *mut f32, count: u32) -> Option<&'a mut [f32]> {
    if pointer.is_null() || count == 0 {
        return None;
    }
    // SAFETY: the host supplies aligned writable storage for the complete declared count.
    Some(unsafe { std::slice::from_raw_parts_mut(pointer, count as usize) })
}
fn invalidate(handoff: &LocalHandoff) {
    handoff.admission.store(0, Ordering::Release);
    let previous = handoff.token.load(Ordering::Relaxed);
    if handoff.reset_required.load(Ordering::Acquire) > handoff.reset_ack.load(Ordering::Acquire) {
        handoff.token.store(previous & !1, Ordering::Release);
        return;
    }
    let epoch = (previous & !1).wrapping_add(2);
    handoff.reset_required.store(epoch, Ordering::Release);
    handoff.token.store(epoch, Ordering::Release);
}
fn cancel_receive(context: &ReceiveContext) {
    // SAFETY: only the serial receive endpoint calls this helper.
    let state = unsafe { &mut *context.state.get() };
    if context.active.load(Ordering::Acquire) {
        invalidate(&state.ring.current.handoff);
    } else {
        context
            .status
            .update_qualification(false, &state.ring.current.handoff);
    }
    state.ring.current.tagged_end = None;
    context.status.receive_sample_ms.store(0, Ordering::Release);
    state.source_generation = 0;
    state.discontinuous = true;
    let now_ms = context.origin_ms.saturating_add(state.elapsed_samples / 48);
    #[cfg(target_os = "linux")]
    if context.status.rx_detail.swap(0, Ordering::AcqRel) != 0 {
        context
            .status
            .record_radio_edge(now_ms, crate::management::status::edges::RadioSignal::Fault);
    }
    // The controller's internal key gate closes on source failure. A failure
    // does not qualify as a normal on-air unkey in the published status.
    let previous = context.status.carrier.load(Ordering::Relaxed);
    if previous & 1 != 0 {
        context
            .status
            .carrier
            .store(now_ms.min(u64::MAX >> 1) << 1, Ordering::Release);
    }
    #[cfg(target_os = "linux")]
    context.status.observe_qualified(None, now_ms);
    context.status.set_dtmf_muted(false);
}
unsafe extern "C" fn receive_callback(
    context: *mut c_void,
    metadata: *const crate::abi::UrpNativeReceiveMetadata,
    pointer: *mut f32,
    count: u32,
) -> i32 {
    // SAFETY: the host retains this context through synchronous destroy.
    let Some(context) = (unsafe { context.cast::<ReceiveContext>().as_ref() }) else {
        if let Some(samples) = unsafe { samples(pointer, count) } {
            samples.fill(0.0);
        }
        return -1;
    };
    // SAFETY: the host supplies a readable size/version prefix, even on mismatch.
    let prefix_valid = !metadata.is_null()
        && unsafe {
            std::ptr::addr_of!((*metadata).struct_size).read()
                == std::mem::size_of::<crate::abi::UrpNativeReceiveMetadata>() as u32
                && std::ptr::addr_of!((*metadata).abi_version).read() == 1
        };
    if !prefix_valid {
        cancel_receive(context);
        invalidate_receive_meters(context, Quality::Fault);
        return -1;
    }
    // SAFETY: the validated prefix promises this complete current record.
    let metadata = unsafe { &*metadata };
    if metadata.status < 0 {
        cancel_receive(context);
        invalidate_receive_meters(context, Quality::Unavailable);
        return 0;
    }
    if count == 0 || count as usize > context.maximum || pointer.is_null() {
        if let Some(samples) = unsafe { samples(pointer, count) } {
            samples.fill(0.0);
        }
        cancel_receive(context);
        invalidate_receive_meters(context, Quality::Fault);
        return -1;
    }
    // SAFETY: the host lends the validated writable PCM span for this callback.
    let samples = unsafe { std::slice::from_raw_parts_mut(pointer, count as usize) };
    let end = metadata.first_sample_index.checked_add(u64::from(count));
    if metadata.status != 0
        || metadata.frame_count != count
        || metadata.generation_id == 0
        || end.is_none()
        || metadata.raw_receiver_keyed > 1
        || metadata.receiver_keyed > metadata.raw_receiver_keyed
        || metadata.vad_mode > 2
        || metadata.vad_voice_active > 1
        || !metadata.vad_score.is_finite()
        || !(0.0..=1.0).contains(&metadata.vad_score)
        || metadata.vad_preroll_frames != if metadata.vad_mode == 0 { 0 } else { 7680 }
        || (metadata.raw_receiver_keyed != 0 && metadata.vad_mode != 0 && metadata.rf_burst_id == 0)
        || (metadata.vad_voice_sample_index != u64::MAX
            && metadata.vad_voice_sample_index >= end.unwrap_or(0))
        || samples.iter().any(|sample| !sample.is_finite())
    {
        samples.fill(0.0);
        cancel_receive(context);
        invalidate_receive_meters(context, Quality::Fault);
        return -1;
    }
    let result = match catch_unwind(AssertUnwindSafe(|| receive(context, metadata, samples))) {
        Ok(result) => result,
        Err(_) => {
            samples.fill(0.0);
            cancel_receive(context);
            invalidate_receive_meters(context, Quality::Fault);
            -1
        }
    };
    // SAFETY: processing and its panic boundary returned to the sole serial owner.
    unsafe { &mut *context.state.get() }
        .meter
        .publish(samples, None);
    result
}
fn invalidate_receive_meters(context: &ReceiveContext, quality: Quality) {
    // SAFETY: only the serial RX callback reads or writes these endpoints.
    for tap in &mut unsafe { &mut *context.state.get() }.status_taps {
        tap.invalidate(quality);
    }
}

fn receive(
    context: &ReceiveContext,
    metadata: &crate::abi::UrpNativeReceiveMetadata,
    samples: &mut [f32],
) -> i32 {
    let vad = metadata.vad_mode != 0;
    let minimum_delay = u64::from(metadata.vad_preroll_frames) / 48;
    context
        .status
        .vad_delay_ms
        .store(minimum_delay, Ordering::Release);
    if !context.active.load(Ordering::Acquire) {
        samples.fill(0.0);
        cancel_receive(context);
        invalidate_receive_meters(context, Quality::Unavailable);
        return 0;
    }
    // SAFETY: only this serialized receive endpoint accesses these cells while active.
    let (owner, state) = unsafe { (&mut *context.owner.get(), &mut *context.state.get()) };
    let count = samples.len() as u64;
    let end = metadata.first_sample_index + count;
    state.elapsed_samples = state.elapsed_samples.saturating_add(count);
    let now_ms = context.origin_ms.saturating_add(state.elapsed_samples / 48);
    let raw = metadata.raw_receiver_keyed != 0;
    let sample_ms = now_ms.max(context.status.transmit_control_ms.load(Ordering::Acquire));
    context
        .status
        .receive_sample_ms
        .store(sample_ms.saturating_add(1), Ordering::Release);
    #[cfg(target_os = "linux")]
    context.status.record_rx_detail(metadata, now_ms);
    context
        .status
        .update_carrier(metadata.receiver_keyed != 0, now_ms);
    context
        .status
        .update_qualification(raw, &state.ring.current.handoff);
    let Some(mut generation) = owner.as_mut().expect("active receive owner").acquire() else {
        samples.fill(0.0);
        state
            .ring
            .current
            .handoff
            .admission
            .store(0, Ordering::Release);
        state.discontinuous = true;
        #[cfg(target_os = "linux")]
        context.status.observe_qualified(None, now_ms);
        for tap in &mut state.status_taps {
            tap.invalidate(Quality::Unavailable);
        }
        return 0;
    };
    let span = Span {
        graph_generation: generation.id(),
        source_generation: metadata.generation_id,
        first_sample: metadata.first_sample_index,
        end_ms: now_ms.max(context.status.transmit_control_ms.load(Ordering::Acquire)),
    };
    state.status_taps[0].observe(span, samples);
    state
        .meter
        .window
        .configure(generation.settings().status_snapshot_interval_ms);
    state.ring.adopt(generation.id());
    context.status.current_handoff.store(
        Arc::as_ptr(&state.ring.current.handoff).cast_mut(),
        Ordering::Release,
    );
    let was_raw = state.rf_burst_id != 0;
    let gap = vad
        && state.source_generation != 0
        && (state.source_generation != metadata.generation_id
            || state.next_source_sample != metadata.first_sample_index
            || (raw && state.rf_burst_id != 0 && state.rf_burst_id != metadata.rf_burst_id));
    state.source_generation = metadata.generation_id;
    state.next_source_sample = end;
    state.rf_burst_id = if raw { metadata.rf_burst_id } else { 0 };
    let ring = &mut state.ring.current;
    if state.discontinuous || ring.fresh || (raw && !was_raw) {
        state.voice_after = metadata.first_sample_index;
        state.discontinuous = false;
        ring.fresh = false;
    }
    if gap {
        invalidate(&ring.handoff);
        ring.tagged_end = None;
        state.voice_after = end;
        state.status_taps[1].invalidate(Quality::Unavailable);
        context.status.update_carrier(false, now_ms);
        #[cfg(target_os = "linux")]
        context.status.observe_qualified(None, now_ms);
        samples.fill(0.0);
        return 0;
    }
    context.status.update_qualification(raw, &ring.handoff);
    if ring.delay_ms != generation.settings().squelch_delay_ms.max(minimum_delay) {
        if ring.handoff.token.load(Ordering::Acquire) & 1 != 0 {
            invalidate(&ring.handoff);
        }
        ring.tagged_end = None;
        state.voice_after = end;
        state.discontinuous = true;
        state.status_taps[1].invalidate(Quality::Unavailable);
        context.status.update_carrier(false, now_ms);
        #[cfg(target_os = "linux")]
        context.status.observe_qualified(None, now_ms);
        samples.fill(0.0);
        return 0;
    }
    #[cfg(test)]
    if take_test_failure(TestFailure::ReceivePanic) {
        panic!("injected receive processing panic");
    }
    ring.producer
        .local_epoch(generation.state().receive_epoch());
    generation.state().process(raw, samples, now_ms);
    context
        .status
        .set_dtmf_muted(metadata.receiver_keyed != 0 && generation.state().suppressing());
    if !raw {
        samples.fill(0.0);
    }
    state.status_taps[1].observe(span, samples);
    if !vad && !raw {
        #[cfg(target_os = "linux")]
        context.status.observe_qualified(Some(false), now_ms);
        return 0;
    }
    let token = ring.handoff.token.load(Ordering::Acquire);
    let acknowledged = ring.handoff.reset_ack.load(Ordering::Acquire);
    if token & !1 != acknowledged
        && (!vad || ring.handoff.reset_required.load(Ordering::Acquire) > acknowledged)
    {
        ring.handoff
            .pending_reset_dropped
            .fetch_add(count, Ordering::Relaxed);
        ring.handoff.admission.store(0, Ordering::Release);
        state.voice_after = end;
        context.status.update_carrier(false, now_ms);
        #[cfg(target_os = "linux")]
        context.status.observe_qualified(None, now_ms);
        return 0;
    }
    let result = if vad {
        write_vad_pcm(ring, metadata, samples, was_raw && !raw)
    } else {
        ring.producer.write(samples)
    };
    let handoff = &ring.handoff;
    match result {
        Ok(accepted) if accepted == samples.len() => {
            let admitted = metadata.receiver_keyed != 0
                && (!vad
                    || (metadata.vad_voice_sample_index != u64::MAX
                        && metadata.vad_voice_sample_index >= state.voice_after));
            #[cfg(target_os = "linux")]
            if admitted {
                context.status.observe_qualified(Some(true), now_ms);
                let burst = context.status.qualified_burst.load(Ordering::Relaxed);
                handoff.status_burst.store(burst, Ordering::Release);
            }
            handoff
                .admission
                .store(if admitted { token } else { 0 }, Ordering::Release);
            context.status.update_carrier(admitted, now_ms);
            #[cfg(target_os = "linux")]
            if !admitted {
                context.status.observe_qualified(Some(false), now_ms);
            }
        }
        _ => {
            handoff
                .write_failed_samples
                .fetch_add(count, Ordering::Relaxed);
            invalidate(handoff);
            ring.tagged_end = None;
            state.voice_after = end;
            context.status.update_qualification(raw, handoff);
            context.status.update_carrier(false, now_ms);
            #[cfg(target_os = "linux")]
            context.status.observe_qualified(None, now_ms);
        }
    }
    0
}
fn write_vad_pcm(
    ring: &mut ReceiveRing,
    metadata: &crate::abi::UrpNativeReceiveMetadata,
    samples: &[f32],
    cancel_old: bool,
) -> Result<usize, RingError> {
    if cancel_old {
        if let Some(end) = ring.tagged_end {
            let start = ring.tagged_start.max(end.saturating_sub(14_400));
            if ring.producer.mute(
                metadata.generation_id,
                &[crate::link::ring::MuteSpan { start, end }],
            )? != 1
            {
                return Err(RingError);
            }
        }
    }
    let accepted = ring.producer.write_tagged(
        samples,
        metadata.generation_id,
        metadata.first_sample_index,
        &[],
    )?;
    if accepted != 0 {
        if ring.tagged_end.is_none() {
            ring.tagged_start = metadata.first_sample_index;
        }
        ring.tagged_end = Some(metadata.first_sample_index + accepted as u64);
    }
    Ok(accepted)
}
/// The vendor invokes this on the same serial TX owner, immediately before rendering.
unsafe extern "C" fn feedback_callback(
    context: *mut c_void,
    feedback: *const crate::abi::UrpRadioFeedback,
) {
    // SAFETY: the retained product transmit box lives through synchronous radio retirement.
    let Some(context) = (unsafe { context.cast::<TransmitContext>().as_ref() }) else {
        return;
    };
    if !context.active.load(Ordering::Acquire) {
        return;
    }
    let status = &context.status;
    let at_ms = status.transmit_control_ms.load(Ordering::Acquire);
    // SAFETY: each provider call lends the sized physical prefix until this return.
    let snapshot = unsafe { PhysicalTransmit::read(feedback, at_ms) };
    let word = snapshot.map_or(0, PhysicalTransmit::word);
    let previous = status.physical_feedback.swap(word, Ordering::AcqRel);
    #[cfg(target_os = "linux")]
    if previous & 63 != word & 63 {
        status.record_radio_edge(
            at_ms,
            crate::management::status::edges::RadioSignal::PhysicalTransmit {
                rendered: snapshot.and_then(|value| value.rendered),
                serviced: snapshot.and_then(|value| value.serviced),
                observed: snapshot.and_then(|value| value.observed),
            },
        );
    }
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
        publish_transmit_keyed(status, None, None);
        invalidate_transmit_meters(callback_context, Quality::Fault);
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
        publish_transmit_keyed(status, None, None);
        invalidate_transmit_meters(callback_context, Quality::Fault);
        publish_transmit_samples(callback_context, samples);
        return -1;
    }
    let keyed = keyed.expect("validated PTT output");
    let ctcss_enabled = ctcss_enabled.expect("validated CTCSS output");
    *keyed = 0;
    *ctcss_enabled = 0;
    let mut policy = None;
    let result = match catch_unwind(AssertUnwindSafe(|| unsafe {
        transmit(context, samples, keyed, ctcss_enabled, &mut policy)
    })) {
        Ok(result) => result,
        Err(_) => {
            samples.fill(0.0);
            *keyed = 0;
            *ctcss_enabled = 0;
            -1
        }
    };
    publish_transmit_keyed(
        status,
        (result == 0).then_some(*keyed != 0),
        (result == 0).then_some(*ctcss_enabled != 0),
    );
    #[cfg(target_os = "linux")]
    if result == 0
        && let Some(status) = status
    {
        status.publish_transmit_policy(policy);
    }
    if result != 0 {
        invalidate_transmit_meters(callback_context, Quality::Fault);
    }
    publish_transmit_samples(callback_context, samples);
    result
}
fn invalidate_transmit_meters(context: Option<&TransmitContext>, quality: Quality) {
    if let Some(context) = context {
        // SAFETY: the serial TX callback owns these endpoints.
        for tap in unsafe { &mut *context.status_taps.get() } {
            tap.invalidate(quality);
        }
    }
}
fn publish_transmit_samples(context: Option<&TransmitContext>, samples: &[f32]) {
    if let Some(context) = context {
        // SAFETY: the serialized TX callback is the sole writer of this publisher.
        let ring = unsafe { &*context.ring.get() };
        unsafe { &mut *context.meter.get() }.publish(samples, Some(&ring.current.observer));
    }
}
fn publish_transmit_keyed(status: Option<&RadioStatus>, keyed: Option<bool>, _ctcss: Option<bool>) {
    if let Some(status) = status {
        let stamp = status.transmit_control_ms.load(Ordering::Acquire);
        let state = keyed.map_or(0, |value| 1 + u64::from(value));
        status
            .transmit_keyed
            .store(stamp << 2 | state, Ordering::Release);
        #[cfg(target_os = "linux")]
        {
            if keyed.is_none() {
                status.publish_transmit_policy(None);
            }
            let detail = state as u32 | _ctcss.map_or(0, |value| 1 + u32::from(value)) << 2;
            if status.tx_detail.swap(detail, Ordering::AcqRel) != detail {
                status.record_radio_edge(
                    stamp,
                    crate::management::status::edges::RadioSignal::Transmit {
                        requested: keyed,
                        ctcss_requested: _ctcss,
                    },
                );
            }
        }
    }
}
unsafe fn transmit(
    context: *mut c_void,
    samples: &mut [f32],
    keyed: &mut u32,
    ctcss_enabled: &mut u32,
    policy: &mut Option<rpt_advanced_core::controller::TransmitPolicyStatus>,
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
    let stamp = context.status.source_clock.advance(samples.len());
    if !context.active.load(Ordering::Acquire) {
        invalidate_transmit_meters(Some(context), Quality::Unavailable);
        samples.fill(0.0);
        *keyed = 0;
        *ctcss_enabled = 0;
        return 0;
    }
    // SAFETY: only this serial transmit endpoint accesses these cells while active.
    let (owner, endpoint) = unsafe { (&mut *context.owner.get(), &mut *context.ring.get()) };
    samples.fill(0.0);
    let Some(mut generation) = owner.as_mut().expect("active transmit owner").acquire() else {
        invalidate_transmit_meters(Some(context), Quality::Unavailable);
        return 0;
    };
    // SAFETY: this serial TX callback exclusively owns its meter window.
    let meter_window = &mut unsafe { &mut *context.meter.get() }.window;
    meter_window.configure(generation.settings().status_snapshot_interval_ms);
    let callback_end_ms = meter_window.origin_ms.saturating_add(
        meter_window
            .elapsed_samples
            .saturating_add(samples.len() as u64)
            / 48,
    );
    endpoint.adopt(generation.id());
    let ring = &mut endpoint.current;
    if ring.delay_ms
        != generation
            .settings()
            .squelch_delay_ms
            .max(context.status.vad_delay_ms.load(Ordering::Acquire))
    {
        invalidate_transmit_meters(Some(context), Quality::Unavailable);
        return 0;
    }
    let receive_epoch = generation.state().controller.receive_epoch();
    let consumer = &mut ring.consumer;
    consumer.local_epoch(receive_epoch);
    let handoff = &ring.handoff;
    #[cfg(target_os = "linux")]
    let rx_burst = handoff.status_burst.load(Ordering::Acquire);
    let token = handoff.token.load(Ordering::Acquire);
    let epoch = token & !1;
    let mut reset_ok = true;
    let acknowledged = handoff.reset_ack.load(Ordering::Acquire);
    let vad = context.status.vad_delay_ms.load(Ordering::Acquire) != 0;
    if epoch != acknowledged {
        if (vad && handoff.reset_required.load(Ordering::Acquire) <= acknowledged)
            || reset_local(consumer).is_ok()
        {
            handoff.reset_ack.store(epoch, Ordering::Release);
        } else {
            // Retry on the next TX callback; never acknowledge a failed reset.
            handoff.reset_failures.fetch_add(1, Ordering::Relaxed);
            reset_ok = false;
        }
    }
    let mut receiving = token & 1 != 0 && reset_ok;
    // Active VAD keeps its sole delay line flowing across normal RF edges.
    if (receiving || vad) && reset_ok && consumer.render(samples).is_err() {
        handoff.render_failures.fetch_add(1, Ordering::Relaxed);
        samples.fill(0.0);
        receiving = false;
    }
    // Cancel the rendered tail if RX unkeyed or changed bursts during this call.
    let current_token = handoff.token.load(Ordering::Acquire);
    let current_admission = handoff.admission.load(Ordering::Acquire);
    let same_ring = std::ptr::eq(
        Arc::as_ptr(&ring.handoff),
        context.status.current_handoff.load(Ordering::Acquire),
    );
    receiving &= current_token == token && current_admission == token && same_ring;
    #[cfg(target_os = "linux")]
    let rx_closed = !receiving
        && same_ring
        && (current_token != token || current_token & 1 == 0 || current_admission == 0);
    receiving &= receive_epoch & 1 == 0
        && generation.state().controller.receive_epoch() == receive_epoch
        && (receive_epoch == 0 || consumer.fresh());
    #[cfg(target_os = "linux")]
    let rx_trace = if receiving {
        let same = rx_burst == handoff.status_burst.load(Ordering::Acquire);
        ReceiveTrace::Admitted(if same { rx_burst } else { 0 })
    } else if rx_closed {
        ReceiveTrace::Closed
    } else {
        ReceiveTrace::Other
    };
    if !receiving || context.status.dtmf_muted() {
        samples.fill(0.0);
    }
    let span = Span {
        graph_generation: generation.id(),
        source_generation: stamp.stream,
        first_sample: stamp.sample,
        end_ms: stamp
            .born_ns
            .map_or(callback_end_ms, |ns| {
                ns / 1_000_000 + samples.len() as u64 / 48
            })
            .max(context.status.transmit_control_ms.load(Ordering::Acquire)),
    };
    // SAFETY: this serial TX callback is the sole writer of its metering endpoints.
    let [
        local_ring,
        transmit_program,
        voice_level,
        telemetry_level,
        station_level,
        telemetry_input_level,
        text_level,
        regeneration_level,
    ] = unsafe { &mut *context.status_taps.get() };
    // SAFETY: these buffers are allocated before publication and shared only by this TX turn.
    let [
        station_input,
        voice,
        public_telemetry_input,
        public_telemetry,
        incoming_text,
        regeneration,
    ] = unsafe { &mut *context.status_pcm.get() };
    let count = samples.len();
    let (
        station_input,
        voice,
        public_telemetry_input,
        public_telemetry,
        incoming_text,
        regeneration,
    ) = (
        &mut station_input[..count],
        &mut voice[..count],
        &mut public_telemetry_input[..count],
        &mut public_telemetry[..count],
        &mut incoming_text[..count],
        &mut regeneration[..count],
    );
    local_ring.observe(span, samples);
    let source = rpt_advanced_core::link::SourcePosition {
        generation: generation.id(),
        sample: stamp.sample,
        stream: stamp.stream,
        born_ns: stamp.born_ns,
    };
    let transmit = generation.state();
    // SAFETY: metadata and PCM execute on the same serial TX owner.
    let controls = unsafe { *context.remote_controls.get() };
    transmit.controller.set_remote_non_fm(controls & 9 == 9);
    transmit
        .controller
        .set_remote_unqualified_open(controls & 17 == 17);
    #[cfg(test)]
    if take_test_failure(TestFailure::TransmitPanic) {
        panic!("injected transmit processing panic");
    }
    let result = transmit.adapter.process_with_branches(
        &mut transmit.controller,
        receiving,
        samples,
        source,
        Some(rpt_advanced_core::controller::LocalTransmitBranches {
            station_input,
            voice,
            public_telemetry_input,
            public_telemetry,
            incoming_text,
            regeneration,
        }),
    );
    // SAFETY: this callback owns both adapter and metadata state.
    unsafe {
        *context.linked_active.get() = result.is_ok() && transmit.adapter.peer_source_active();
    }
    match result {
        Ok(value) => {
            transmit_program.observe(span, samples);
            voice_level.observe(span, voice);
            telemetry_level.observe(span, public_telemetry);
            station_level.observe(span, station_input);
            telemetry_input_level.observe(span, public_telemetry_input);
            let (text_available, regeneration_available) = transmit.adapter.local_transmit_media();
            for (available, level, pcm) in [
                (text_available, text_level, incoming_text),
                (regeneration_available, regeneration_level, regeneration),
            ] {
                if available {
                    level.observe(span, pcm);
                } else {
                    level.invalidate(Quality::Unavailable);
                }
            }
            *keyed = u32::from(value);
            *ctcss_enabled = u32::from(transmit.controller.ctcss_enabled());
            *policy = Some(transmit.controller.transmit_policy_status());
            #[cfg(target_os = "linux")]
            {
                // SAFETY: this tracker belongs only to the serial TX callback.
                let tracker = unsafe { &mut *context.receive_release.get() };
                tracker.observe(
                    &context.status,
                    span.graph_generation,
                    span.end_ms,
                    rx_trace,
                    transmit.controller.take_local_receive_release(),
                );
            }
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
    status_meters: [status_meter::Reader; status_meter::LOCAL_TAP_COUNT],
    last_physical_receive: Option<(u64, Option<PhysicalReceive>)>,
    reported_faults: [u64; 6],
    last_report_ms: u64,
}

/// Setup failure retains the exact owner whose callbacks may already be bound.
pub enum PrepareFailure {
    /// Activation has not borrowed either callback context.
    Reservation(Error, Box<Radio>),
    /// The host may retain callbacks; poll retirement before releasing this owner.
    Activation(Error, Box<RadioWorker>),
}

impl std::fmt::Debug for PrepareFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reservation(error, _) => {
                formatter.debug_tuple("Reservation").field(error).finish()
            }
            Self::Activation(error, _) => formatter.debug_tuple("Activation").field(error).finish(),
        }
    }
}
impl RadioWorker {
    /// Copy one command to the shared physical owner without performing device I/O.
    pub(crate) fn remote_submit(
        &mut self,
        request: &rpt_advanced_core::remote_base_command::RemoteOperation,
        local: bool,
        active: bool,
    ) -> Result<crate::services::RemoteTicket, i32> {
        self.radio
            .as_mut()
            .ok_or(-7)?
            .remote_submit(request, local, active)
    }
    /// Pump only existing control/DSP adoption; the physical owner retains I/O errors.
    pub(crate) fn service_remote(&mut self) -> Result<(), i32> {
        self.radio.as_mut().ok_or(-7)?.service()
    }
    /// A worker exists before its physical/DSP generation is ready for publication.
    pub(crate) fn activation_ready(&mut self) -> Result<bool, i32> {
        self.radio.as_mut().ok_or(-7)?.activation_ready()
    }

    /// Stage or poll this exact retained physical plan on the current radio.
    pub(crate) fn stage_station_binding(
        &mut self,
        plan: &crate::services::StationBinding,
    ) -> Result<crate::services::StationStage, i32> {
        self.radio.as_mut().ok_or(-7)?.stage_station_binding(plan)
    }

    /// Complete or poll the current forward or reverse transition.
    pub(crate) fn finish_station_binding(
        &mut self,
        commit: bool,
    ) -> Result<crate::services::StationFinish, i32> {
        self.radio
            .as_mut()
            .ok_or(-7)?
            .finish_station_binding(commit)
    }

    /// Retain the current stream while the shared native owner replaces its plan.
    pub(crate) fn configuration_update(&mut self, phase: u32) -> Result<bool, i32> {
        self.radio.as_mut().ok_or(-7)?.configuration_update(phase)
    }

    /// Borrow the current physical radio for one serialized maintenance request.
    pub(crate) fn maintenance(
        &mut self,
        request: &mut crate::abi::rptadv_radio_control_v1,
    ) -> Result<String, i32> {
        self.radio.as_mut().ok_or(-7)?.maintenance(request)
    }

    /// Borrow this live owner's cached site state or bounded command queue on control.
    pub(crate) fn site_operation(
        &mut self,
        bank: u32,
        operation: crate::site::radio::Operation,
    ) -> Result<Option<crate::abi::UrpAstSiteSnapshot>, i32> {
        use crate::site::radio::Operation;
        let radio = self.radio.as_mut().ok_or(-7)?;
        match operation {
            Operation::Snapshot => radio.site_snapshot(bank).map(Some),
            Operation::Bind(binding) => radio.site_bind(bank, &binding).map(|()| None),
            Operation::Publish(request) => radio.site_publish(bank, &request).map(|()| None),
        }
    }

    /// Bind a peer on control while this worker retains its live radio lease.
    pub fn bind_peer(&self, peer: &mut PeerIo) -> Result<(), Error> {
        peer.bind_radio(self.radio.as_ref().expect("live radio reservation"))
    }

    /// Resolved minimum requested by the source, read only on control.
    pub(crate) fn minimum_delay_ms(&self) -> u64 {
        self.receive.status.vad_delay_ms.load(Ordering::Acquire)
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
        self.receive_updates.adopted.store(0, Ordering::Release);
        self.transmit_updates.adopted.store(0, Ordering::Release);
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
    /// Failed activation retains the complete callback owner until safe retirement.
    pub fn prepare(
        mut radio: Radio,
        epoch: Instant,
        mut status: RadioStatus,
        squelch_delay_ms: u64,
        status_snapshot_interval_ms: u64,
    ) -> Result<Self, PrepareFailure> {
        let maximum = radio.maximum_frames();
        let policy = match radio.receive_policy() {
            Ok(policy) => policy,
            Err(_) => {
                return Err(PrepareFailure::Reservation(
                    Error::Operation,
                    Box::new(radio),
                ));
            }
        };
        let minimum_delay = if policy.mode == 0 {
            0
        } else {
            u64::from(policy.preroll_frames) / 48
        };
        status.vad_delay_ms.store(minimum_delay, Ordering::Release);
        let squelch_delay_ms = squelch_delay_ms.max(minimum_delay);
        status.handoff = Arc::default();
        let ring = LocalRingUpdate::with_handoff(squelch_delay_ms, status.handoff.clone());
        #[cfg(test)]
        let ring = if take_test_failure(TestFailure::Prepare) {
            Err(RingError)
        } else {
            ring
        };
        let Ok(update) = ring else {
            return Err(PrepareFailure::Reservation(
                Error::Allocation,
                Box::new(radio),
            ));
        };
        let observer = update.transmit.observer.clone();
        let handoff = update.transmit.handoff.clone();
        status
            .current_handoff
            .store(Arc::as_ptr(&handoff).cast_mut(), Ordering::Release);
        let (receive_ring, receive_updates) = EndpointOwner::new(update.receive);
        let (transmit_ring, transmit_updates) = EndpointOwner::new(update.transmit);
        let origin_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        status.update_carrier(false, origin_ms);
        status.set_dtmf_muted(false);
        let (receive_publisher, receive_snapshots) = RingBuffer::new(SNAPSHOT_QUEUE_CAPACITY);
        let (transmit_publisher, transmit_snapshots) = RingBuffer::new(SNAPSHOT_QUEUE_CAPACITY);
        let [
            (rx_provider, rx_provider_reader),
            (rx_gate, rx_gate_reader),
            (rx_ring, rx_ring_reader),
            (tx_program, tx_program_reader),
            (tx_voice, tx_voice_reader),
            (tx_telemetry, tx_telemetry_reader),
            (rx_station, rx_station_reader),
            (tx_telemetry_input, tx_telemetry_input_reader),
            (tx_text, tx_text_reader),
            (tx_regeneration, tx_regeneration_reader),
        ] = std::array::from_fn(|_| Tap::new());
        let active = Arc::new(AtomicBool::new(false));
        status.source_clock.restart();
        status.physical_feedback.store(0, Ordering::Release);
        status.receive_sample_ms.store(0, Ordering::Release);
        let receive = Box::new(ReceiveContext {
            active: active.clone(),
            owner: UnsafeCell::new(None),
            state: UnsafeCell::new(ReceiveState {
                ring: receive_ring,
                elapsed_samples: 0,
                source_generation: 0,
                next_source_sample: 0,
                rf_burst_id: 0,
                voice_after: 0,
                discontinuous: true,
                meter: MeterPublisher {
                    window: MeterWindow::new(status_snapshot_interval_ms, origin_ms),
                    snapshots: receive_publisher,
                    dropped_snapshots: 0,
                },
                status_taps: [rx_provider, rx_gate],
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
            status_taps: UnsafeCell::new([
                rx_ring,
                tx_program,
                tx_voice,
                tx_telemetry,
                rx_station,
                tx_telemetry_input,
                tx_text,
                tx_regeneration,
            ]),
            status_pcm: UnsafeCell::new(std::array::from_fn(|_| {
                vec![0.0; maximum].into_boxed_slice()
            })),
            remote_controls: UnsafeCell::new(0),
            linked_active: UnsafeCell::new(false),
            #[cfg(target_os = "linux")]
            receive_release: UnsafeCell::new(ReceiveReleaseTracker::default()),
        });
        if radio.bind_metadata(Some(metadata_callback)).is_err() {
            return Err(PrepareFailure::Reservation(
                Error::Operation,
                Box::new(radio),
            ));
        }
        let mut worker = Self {
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
            status_meters: [
                rx_provider_reader,
                rx_gate_reader,
                rx_ring_reader,
                tx_program_reader,
                tx_voice_reader,
                tx_telemetry_reader,
                rx_station_reader,
                tx_telemetry_input_reader,
                tx_text_reader,
                tx_regeneration_reader,
            ],
            last_physical_receive: None,
            reported_faults: [0; 6],
            last_report_ms: 0,
        };
        // SAFETY: the complete owner retains both stable callback boxes on success
        // and failure. Only checked retirement may release them after activation.
        let activated = unsafe {
            let radio = worker.radio.as_mut().unwrap();
            let port = crate::abi::UrpRadioFeedbackPort {
                struct_size: std::mem::size_of::<crate::abi::UrpRadioFeedbackPort>() as u32,
                abi_version: crate::abi::URP_RADIO_FEEDBACK_ABI_VERSION,
                context: std::ptr::from_ref(&*worker.transmit).cast_mut().cast(),
                publish: Some(feedback_callback),
            };
            radio.bind_feedback(port);
            radio.activate(
                Some(receive_callback),
                std::ptr::from_ref(&*worker.receive).cast_mut().cast(),
                Some(transmit_callback),
                std::ptr::from_ref(&*worker.transmit).cast_mut().cast(),
            )
        };
        if activated.is_err() {
            return Err(PrepareFailure::Activation(
                Error::Operation,
                Box::new(worker),
            ));
        }
        Ok(worker)
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
    #[cfg(target_os = "linux")]
    pub(crate) fn meters(&self) -> [Option<crate::management::live::Meter>; 2] {
        [self.latest_receive, self.latest_transmit].map(|snapshot| {
            snapshot.map(|m| crate::management::live::Meter {
                timestamp_ms: m.timestamp_ms,
                samples: m.samples,
                peak: m.peak,
                rms: m.rms,
                clipped_samples: m.clipped_samples,
                dropped_snapshots: m.dropped_snapshots,
            })
        })
    }

    /// Collect the native owner's optional receive snapshot at most four times per second.
    pub(crate) fn physical_receive(&mut self, now_ms: u64) -> Option<PhysicalReceive> {
        let sample = self
            .receive
            .status
            .receive_sample_ms
            .load(Ordering::Acquire);
        if sample == 0 || now_ms.saturating_sub(sample - 1) > radio_feedback::FRESH_MS {
            self.last_physical_receive = None;
            return None;
        }
        if let Some((at, cached)) = self.last_physical_receive
            && now_ms.saturating_sub(at) < 250
        {
            return cached;
        }
        let status = self
            .radio
            .as_mut()
            .and_then(|radio| radio.physical_receive(now_ms));
        self.last_physical_receive = Some((now_ms, status));
        status
    }

    /// Return the most recent local measurements without polling either provider callback.
    pub(crate) fn status_meters(&self) -> [Option<Measurement>; status_meter::LOCAL_TAP_COUNT] {
        self.status_meters
            .each_ref()
            .map(status_meter::Reader::current)
    }

    /// Drain bounded callback snapshots and retain only each direction's latest window.
    pub(crate) fn collect_snapshots(&mut self) {
        while let Ok(snapshot) = self.receive_snapshots.pop() {
            self.latest_receive = Some(snapshot);
        }
        while let Ok(snapshot) = self.transmit_snapshots.pop() {
            self.latest_transmit = Some(snapshot);
        }
        for reader in &mut self.status_meters {
            let _ = reader.latest();
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
    /// Poll retirement without relinquishing callback storage or audio ownership.
    /// A false result or error keeps the radio for a later control-plane poll.
    pub fn poll_stop(&mut self) -> Result<bool, i32> {
        if let Some(radio) = &mut self.radio {
            if !radio.poll_quiesce()? {
                return Ok(false);
            }
        }
        drop(self.radio.take());
        self.receive.active.store(false, Ordering::Release);
        self.receive
            .status
            .physical_feedback
            .store(0, Ordering::Release);
        self.receive
            .status
            .receive_sample_ms
            .store(0, Ordering::Release);
        publish_transmit_keyed(Some(&self.transmit.status), None, None);
        Ok(true)
    }

    /// Synchronous final cleanup for callers that cannot retain a pending owner.
    /// Managed device handoff uses `poll_stop` before entering this destructor path.
    pub fn stop(mut self) -> Option<AudioOwners> {
        while self.poll_stop() != Ok(true) {
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
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
        publish_transmit_keyed(Some(&self.transmit.status), None, None);
    }
}
#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
