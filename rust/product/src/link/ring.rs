//! Checked binding to the released F32 ring; no conversion implementation lives here.
use crate::abi as ffi;
use rpt_advanced_core::link::{
    PeerInput, PeerSignals, ReceivePolicy, RegenerationOrigin, SampleProvenance, SourceCoordinate,
    TimedPeerSample,
};
use std::{
    ffi::CStr,
    mem::{offset_of, size_of},
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Invalid or unavailable ring capability, construction, or operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingError;

/// Immutable source policy selected while preparing a ring generation.
#[derive(Clone, Copy)]
pub enum InboundPolicy {
    /// Decoded network PCM uses G.711 PLC and the peer playout target.
    Peer,
    /// Phone 8–48kHz PCM retains 160ms pre-roll in this existing inbound owner.
    Phone,
    /// EchoLink selected-talker PCM, with explicit finite-burst drainage.
    Echo,
    /// Local RF uses silence on shortfall and only its captured squelch delay.
    Local {
        /// Delay captured at worker construction; changing it requires a new ring.
        squelch_delay_ms: u64,
    },
    /// Finite file/speech media: immediate playout, no PLC, and no clock target.
    Media,
}

/// Exact outcome of the most recently published finite source boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinishState {
    /// SRC or PLC still retains accepted audio.
    Pending,
    /// Every accepted source position played and the warmed stream reset.
    Drained,
    /// The consumer explicitly reset/discarded this burst.
    Cancelled,
    /// Conversion or its final reset failed.
    Failed,
}

/// Copy only a fully readable compatible descriptor prefix. Nullable function
/// pointers are zero-valid; absent tails stay unavailable instead of borrowing
/// a larger Rust record over an older allocation.
pub(crate) unsafe fn checked_descriptor(
    pointer: *const ffi::rpcr3_descriptor,
) -> Result<ffi::rpcr3_descriptor, RingError> {
    if pointer.is_null() {
        return Err(RingError);
    }
    let length = unsafe { ptr::addr_of!((*pointer).struct_size).read() } as usize;
    let prefix = offset_of!(ffi::rpcr3_descriptor, ring_producer_finish);
    if length < prefix
        || unsafe { ptr::addr_of!((*pointer).abi_version).read() } != ffi::RPCR3_ABI_VERSION
    {
        return Err(RingError);
    }
    let finite = offset_of!(ffi::rpcr3_descriptor, ring_consumer_render_sample_timed);
    let count = if length >= size_of::<ffi::rpcr3_descriptor>() {
        size_of::<ffi::rpcr3_descriptor>()
    } else if length >= finite {
        finite
    } else {
        prefix
    };
    let mut api: ffi::rpcr3_descriptor = unsafe { std::mem::zeroed() };
    unsafe {
        ptr::copy_nonoverlapping(
            pointer.cast::<u8>(),
            (&mut api as *mut ffi::rpcr3_descriptor).cast::<u8>(),
            count,
        )
    };
    Ok(api)
}

/// Individually current released-ring diagnostics; fields are not transactional.
pub type Observation = ffi::rpcr3_observation;
/// Half-open absolute source interval muted at actual ring playout.
pub type MuteSpan = ffi::rpcr3_mute_span;

struct Shared {
    handle: NonNull<ffi::rpcr3_ring>,
    api: ffi::rpcr3_descriptor,
    input_rate: u32,
    capacity_samples: u64,
    policy: InboundPolicy,
    finite: AtomicU64,
    // Each local direction exclusively updates its snapshot from the co-owned RF gate.
    local_capture_epoch: AtomicU64,
    local_playout_epoch: AtomicU64,
    // Only the serial producer advances the native accepted source timeline.
    writer_frame: AtomicU64,
    writer_generation: AtomicU64,
    first_frame: AtomicU64,
    first_generation: AtomicU64,
    first_epoch: AtomicU64,
    played_epoch: AtomicU64,
    native_real: AtomicU64,
    signals: PeerSignals,
}
// SAFETY: exactly one non-cloneable producer and one consumer call the released SPSC API.
// Descriptor storage is process-lifetime immutable; Arc keeps the ring alive for both owners.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}
impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: both endpoints have gone; there can be no outstanding call.
        unsafe {
            (self.api.ring_destroy.unwrap())(self.handle.as_ptr());
        }
    }
}
/// Ring constructor assigning exactly one owner to each endpoint.
pub struct InboundRing;
/// Unique network producer; its owner serializes every publish operation.
pub struct InboundProducer(Arc<Shared>);
/// Unique native 48 kHz consumer, independent of producer progress.
pub struct InboundConsumer(Arc<Shared>, Option<Box<MarkerReader>>);
struct OriginMarker {
    start: u64,
    origin: Option<RegenerationOrigin>,
}
pub(crate) struct RegenerationWriter {
    pcm: InboundProducer,
    next: rtrb::Producer<OriginMarker>,
    retired: rtrb::Consumer<OriginMarker>,
}
struct MarkerReader {
    next: rtrb::Consumer<OriginMarker>,
    retired: rtrb::Producer<OriginMarker>,
    current: Option<OriginMarker>,
}
impl MarkerReader {
    fn permits(&mut self, frame: u64) -> bool {
        while self.next.peek().is_ok_and(|m| m.start <= frame) {
            if self.current.is_some() && self.retired.is_full() {
                return false;
            }
            if let Some(old) = self.current.take() {
                // The control-side producer reclaims every retained connection off audio.
                let _ = self.retired.push(old);
            }
            self.current = self.next.pop().ok();
        }
        self.current
            .as_ref()
            .is_some_and(|m| m.origin.as_ref().is_none_or(RegenerationOrigin::current))
    }
}
/// Read-only diagnostics handle; it cannot consume or publish PCM.
#[derive(Clone)]
pub struct InboundObserver(Arc<Shared>);

impl InboundObserver {
    /// Whether both observations belong to the same prepared ring allocation.
    /// Control owners use this identity to acknowledge only the intended redirection.
    pub fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Immutable decoded input sample rate.
    pub fn rate(&self) -> u32 {
        self.0.input_rate
    }
    /// Preserve the exact per-protocol policy during a prepared generation swap.
    pub fn policy(&self) -> InboundPolicy {
        self.0.policy
    }
    /// Activity/source/terminal atomics belonging to this exact ring generation.
    pub fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    /// Read the released ring's individually current counters without blocking audio.
    pub fn observe(&self) -> Result<Observation, RingError> {
        self.0.observe()
    }
}
impl InboundRing {
    /// Prepare one released ring with fixed source timing and 4096-sample block bounds.
    pub fn open(
        input_rate: u32,
        policy: InboundPolicy,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        // SAFETY: the linked provider retains its immutable descriptor for process lifetime.
        unsafe { Self::from_descriptor(input_rate, policy, ffi::rpcr3_descriptor()) }
    }
    /// Prepare a controller ingress only when the actual provider can fence
    /// buffered output by its original admitted source coordinates.
    pub(crate) fn open_receiver(
        input_rate: u32,
        policy: InboundPolicy,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        // SAFETY: process-lifetime linked provider descriptor.
        unsafe {
            Self::receiver_from_descriptor(
                input_rate,
                policy,
                ffi::rpcr3_descriptor(),
                PeerSignals::new(),
            )
        }
    }
    unsafe fn receiver_from_descriptor(
        input_rate: u32,
        policy: InboundPolicy,
        pointer: *const ffi::rpcr3_descriptor,
        signals: PeerSignals,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        let api = unsafe { checked_descriptor(pointer)? };
        if api.ring_consumer_render_sample_timed.is_none()
            || api.ring_producer_push_tagged.is_none()
        {
            return Err(RingError);
        }
        unsafe { Self::prepare(input_rate, policy, pointer, signals) }
    }
    pub(crate) fn regeneration() -> Result<(RegenerationWriter, InboundConsumer), RingError> {
        Self::prepare_regeneration(Self::open(48000, InboundPolicy::Media)?)
    }
    fn prepare_regeneration(
        (pcm, mut reader): (InboundProducer, InboundConsumer),
    ) -> Result<(RegenerationWriter, InboundConsumer), RingError> {
        if pcm.0.api.ring_producer_push_tagged.is_none() || !reader.timing_available() {
            return Err(RingError);
        }
        let (next, receive) = rtrb::RingBuffer::new(128);
        let (retire, retired) = rtrb::RingBuffer::new(128);
        let writer = RegenerationWriter { pcm, next, retired };
        reader.1 = Some(Box::new(MarkerReader {
            next: receive,
            retired: retire,
            current: None,
        }));
        Ok((writer, reader))
    }
    /// Redirect the same surviving connection without losing its operational receive mute.
    pub(crate) fn redirected(
        input_rate: u32,
        policy: InboundPolicy,
        reply: &rpt_advanced_core::link::PeerReply,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        // SAFETY: same process-lifetime provider descriptor as open().
        unsafe {
            Self::receiver_from_descriptor(
                input_rate,
                policy,
                ffi::rpcr3_descriptor(),
                PeerSignals::redirected(reply),
            )
        }
    }
    // The provider retains the descriptor and callback code until all endpoints are dropped.
    unsafe fn from_descriptor(
        input_rate: u32,
        policy: InboundPolicy,
        pointer: *const ffi::rpcr3_descriptor,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        // SAFETY: the caller owns this descriptor for the lifetime of all endpoints.
        unsafe { Self::prepare(input_rate, policy, pointer, PeerSignals::new()) }
    }
    unsafe fn prepare(
        input_rate: u32,
        policy: InboundPolicy,
        pointer: *const ffi::rpcr3_descriptor,
        signals: PeerSignals,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        let media_rate = matches!(policy, InboundPolicy::Media);
        if input_rate == 0
            || (!media_rate && input_rate > 48000)
            || (media_rate && !(8000..=192000).contains(&input_rate))
            || (matches!(policy, InboundPolicy::Phone) && input_rate < 8000)
        {
            return Err(RingError);
        }
        // SAFETY: linked released library guarantees a process-lifetime descriptor header.
        unsafe {
            let api = checked_descriptor(pointer)?;
            if api.capability_name.is_null()
                || CStr::from_ptr(api.capability_name) != c"rptadv.rate-adjusting-pcm-ring.f32"
                || api.ring_create.is_none()
                || api.ring_destroy.is_none()
                || api.ring_producer_push.is_none()
                || api.ring_producer_gap.is_none()
                || api.ring_consumer_render.is_none()
                || api.ring_consumer_render_sample.is_none()
                || api.ring_consumer_reset.is_none()
                || (matches!(policy, InboundPolicy::Local { .. })
                    && (api.ring_producer_push_tagged.is_none()
                        || api.ring_producer_mute.is_none()))
                || api.ring_output_delay.is_none()
                || api.ring_observe.is_none()
                || (matches!(policy, InboundPolicy::Echo)
                    && (api.ring_producer_finish.is_none()
                        || api.ring_finish_status.is_none()
                        || api.ring_producer_push_tagged.is_none()
                        || api.ring_producer_mute.is_none()))
            {
                return Err(RingError);
            }
            let rate = u64::from(input_rate);
            let (reserve, target, capacity, plc_mode) = match policy {
                InboundPolicy::Peer | InboundPolicy::Echo => {
                    // Input is at most 48 kHz, so qmin's 1/256 floor is inactive.
                    // Include one maximum callback at -1000 ppm and its successor.
                    let budget = (4096 * rate * 1000).div_ceil(48000 * 999) + 1;
                    let reserve = (rate * 60 / 1000).max(budget);
                    let target = rate * 260 / 1000;
                    let capacity = (rate * 300 / 1000).max(512).max(target + 4096);
                    (reserve, target, capacity, 1)
                }
                InboundPolicy::Phone => {
                    (rate * 160 / 1000, rate * 160 / 1000, rate * 300 / 1000, 1)
                }
                InboundPolicy::Local { squelch_delay_ms } => {
                    let delay = rate.checked_mul(squelch_delay_ms).ok_or(RingError)? / 1000;
                    (delay, delay, 14400, 0)
                }
                InboundPolicy::Media => (0, 0, 8192, 0),
            };
            let config = ffi::rpcr3_config {
                struct_size: size_of::<ffi::rpcr3_config>() as u32,
                abi_version: ffi::RPCR3_ABI_VERSION,
                capacity_samples: capacity,
                input_rate_hz: input_rate,
                output_rate_hz: 48000,
                reserve_samples: reserve,
                target_samples: target,
                max_producer_samples: if matches!(policy, InboundPolicy::Phone) {
                    rate / 50
                } else {
                    4096
                },
                max_output_samples: 4096,
                plc_mode,
            };
            let mut handle = ptr::null_mut();
            if api.ring_create.unwrap()(&config, &mut handle) != 0 {
                return Err(RingError);
            }
            let shared = Arc::new(Shared {
                handle: NonNull::new(handle).ok_or(RingError)?,
                api,
                input_rate,
                capacity_samples: capacity,
                policy,
                finite: AtomicU64::new(0),
                local_capture_epoch: AtomicU64::new(0),
                local_playout_epoch: AtomicU64::new(0),
                writer_frame: AtomicU64::new(0),
                writer_generation: AtomicU64::new(0),
                first_frame: AtomicU64::new(0),
                first_generation: AtomicU64::new(0),
                first_epoch: AtomicU64::new(0),
                played_epoch: AtomicU64::new(0),
                native_real: AtomicU64::new(0),
                signals,
            });
            Ok((
                InboundProducer(shared.clone()),
                InboundConsumer(shared, None),
            ))
        }
    }
}
impl RegenerationWriter {
    pub(super) fn output_delay_samples(&self) -> Result<u64, RingError> {
        self.pcm.output_delay_samples()
    }
    pub(super) fn reclaim_origins(&mut self) {
        while self.retired.pop().is_ok() {}
    }
    pub(super) fn mark_origin(&mut self, start: u64, origin: &Option<RegenerationOrigin>) -> bool {
        self.next
            .push(OriginMarker {
                start,
                origin: origin.clone(),
            })
            .is_ok()
    }
    pub(super) fn write_generated(
        &mut self,
        samples: &[f32],
        start: u64,
    ) -> Result<usize, RingError> {
        self.pcm.write_tagged(samples, 1, start, &[])
    }
}
impl InboundProducer {
    /// Obtain a diagnostics-only control handle outside audio work.
    pub fn observer(&self) -> InboundObserver {
        InboundObserver(self.0.clone())
    }
    /// Decoded source rate established when this endpoint was prepared.
    pub fn rate(&self) -> u32 {
        self.0.input_rate
    }
    /// Shared publication and activity state for the serial channel owner.
    pub fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    pub(crate) fn local_epoch(&mut self, epoch: u64) {
        self.0.local_capture_epoch.store(epoch, Ordering::Relaxed);
    }
    pub(crate) fn epoch(&self) -> u64 {
        if matches!(self.0.policy, InboundPolicy::Local { .. }) {
            self.0.local_capture_epoch.load(Ordering::Relaxed)
        } else {
            self.0.signals.receive_epoch()
        }
    }
    /// Publish once and return the accepted leading count. Phone writes offer at
    /// most 20ms; the producer retains and retries the complete unaccepted tail.
    pub fn write(&mut self, samples: &[f32]) -> Result<usize, RingError> {
        self.write_received(samples, self.epoch())
    }
    pub(super) fn write_received(
        &mut self,
        samples: &[f32],
        epoch: u64,
    ) -> Result<usize, RingError> {
        if self.epoch() != 0 && self.0.api.ring_producer_push_tagged.is_some() {
            self.write_received_tagged(
                samples,
                self.0.writer_generation.load(Ordering::Relaxed),
                self.0.writer_frame.load(Ordering::Relaxed),
                &[],
                epoch,
            )
        } else {
            let n = self.push(samples)?;
            self.0.writer_frame.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
    }
    fn push(&self, samples: &[f32]) -> Result<usize, RingError> {
        let mut accepted = 0;
        // SAFETY: this noncloneable endpoint is the only producer; the slice is live.
        let code = unsafe {
            (self.0.api.ring_producer_push.unwrap())(
                self.0.handle.as_ptr(),
                samples.as_ptr(),
                self.0.producer_prefix(samples.len() as u64),
                &mut accepted,
            )
        };
        if code == 0 {
            if accepted != 0 {
                self.0.finite.store(0, Ordering::Release);
                self.0.signals.publish_pcm();
            }
            Ok(accepted as usize)
        } else {
            Err(RingError)
        }
    }
    /// Publish tags before their accepted PCM prefix. Phone offers validate the
    /// complete original intervals, then forward only their offered prefix.
    /// Retries retain original source positions and intervals with unaccepted PCM;
    /// a new generation requires a quiescent consumer reset.
    pub fn write_tagged(
        &mut self,
        samples: &[f32],
        generation: u64,
        start: u64,
        spans: &[MuteSpan],
    ) -> Result<usize, RingError> {
        self.write_received_tagged(samples, generation, start, spans, self.epoch())
    }
    pub(super) fn write_received_tagged(
        &mut self,
        samples: &[f32],
        generation: u64,
        start: u64,
        spans: &[MuteSpan],
        epoch: u64,
    ) -> Result<usize, RingError> {
        let accepted = self.push_tagged(samples, generation, start, spans)?;
        if accepted != 0 {
            self.0
                .writer_frame
                .store(start + accepted as u64, Ordering::Relaxed);
            self.0
                .writer_generation
                .store(generation, Ordering::Relaxed);
            if epoch != 0
                && epoch & 1 == 0
                && epoch == self.epoch()
                && (self.0.first_epoch.load(Ordering::Relaxed) != epoch
                    || self.0.first_generation.load(Ordering::Relaxed) != generation)
            {
                // Publish the first actually accepted fresh coordinate. A reader can
                // temporarily suppress a just-admitted sample, never authorize an older one.
                self.0.first_epoch.store(0, Ordering::Release);
                self.0.first_frame.store(start, Ordering::Relaxed);
                self.0.first_generation.store(generation, Ordering::Relaxed);
                self.0.first_epoch.store(epoch, Ordering::Release);
            }
        }
        Ok(accepted)
    }
    fn push_tagged(
        &self,
        samples: &[f32],
        generation: u64,
        start: u64,
        spans: &[MuteSpan],
    ) -> Result<usize, RingError> {
        let operation = self.0.api.ring_producer_push_tagged.ok_or(RingError)?;
        let source_len = samples.len();
        let offered = self.0.producer_prefix(samples.len() as u64);
        let mut prefix_spans =
            [MuteSpan { start: 0, end: 0 }; ffi::RPCR3_MAXIMUM_MUTE_SPANS as usize];
        let spans = if offered < source_len as u64 {
            let original_end = start.checked_add(source_len as u64).ok_or(RingError)?;
            let oldest = start.saturating_sub(self.0.capacity_samples);
            if spans.len() > prefix_spans.len()
                || spans.iter().any(|span| {
                    span.start >= span.end || span.end > original_end || span.start < oldest
                })
            {
                #[cfg(test)]
                if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                    eprintln!("private PHONE write rejected: tagged ring prefix span bounds");
                }
                return Err(RingError);
            }
            let prefix_end = start + offered;
            let mut count = 0;
            for span in spans {
                if span.start < prefix_end {
                    prefix_spans[count] = MuteSpan {
                        start: span.start,
                        end: span.end.min(prefix_end),
                    };
                    count += 1;
                }
            }
            &prefix_spans[..count]
        } else {
            spans
        };
        let mut accepted = 0;
        let code = unsafe {
            operation(
                self.0.handle.as_ptr(),
                samples.as_ptr(),
                offered,
                generation,
                start,
                spans.as_ptr(),
                u32::try_from(spans.len()).map_err(|_| RingError)?,
                &mut accepted,
            )
        };
        if code != 0 {
            #[cfg(test)]
            if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                let class = if spans.is_empty() {
                    "unmarked"
                } else if spans.iter().any(|span| span.start < start.saturating_sub(self.0.capacity_samples)) {
                    "mark older than ring capacity"
                } else {
                    "marked within ring capacity"
                };
                eprintln!("private PHONE write rejected: native tagged ring, {class}");
            }
            return Err(RingError);
        }
        if accepted != 0 {
            self.0.finite.store(0, Ordering::Release);
            self.0.signals.publish_pcm();
        }
        Ok(accepted as usize)
    }
    /// Add a retained lookback interval to already accepted PCM, including SRC
    /// output caches. Return exact interval credit; never advance PCM on refusal.
    pub fn mute(&mut self, generation: u64, spans: &[MuteSpan]) -> Result<usize, RingError> {
        let operation = self.0.api.ring_producer_mute.ok_or(RingError)?;
        let mut accepted = 0;
        let code = unsafe {
            operation(
                self.0.handle.as_ptr(),
                generation,
                spans.as_ptr(),
                u32::try_from(spans.len()).map_err(|_| RingError)?,
                &mut accepted,
            )
        };
        (code == 0).then_some(accepted as usize).ok_or(RingError)
    }
    /// Publish at most one bounded block of counted missing source samples.
    pub fn gap(&mut self, samples: u64) -> Result<u64, RingError> {
        let mut accepted = 0;
        // SAFETY: this noncloneable endpoint is the only producer; the checked
        // descriptor retains this tail callback for the ring's whole lifetime.
        let code = unsafe {
            (self.0.api.ring_producer_gap.unwrap())(
                self.0.handle.as_ptr(),
                self.0.producer_prefix(samples),
                &mut accepted,
            )
        };
        if code == 0 {
            if accepted != 0 {
                self.0.writer_frame.fetch_add(accepted, Ordering::Relaxed);
                self.0.finite.store(0, Ordering::Release);
            }
            Ok(accepted)
        } else {
            Err(RingError)
        }
    }
    /// Fence this private source's retained PCM, including converted samples.
    /// Keep the SPSC ring and its credited timeline on the same call generation.
    pub(super) fn retire_phone_media(
        &mut self,
        generation: u64,
        previous_origin: u64,
    ) -> Result<Option<u64>, RingError> {
        let end = self.0.writer_frame.load(Ordering::Relaxed);
        self.0.first_epoch.store(0, Ordering::Release);
        self.0.played_epoch.store(0, Ordering::Release);
        let start = previous_origin.max(end.saturating_sub(self.0.capacity_samples));
        if start < end && self.0.writer_generation.load(Ordering::Relaxed) == generation {
            let accepted = self.mute(generation, &[MuteSpan { start, end }])?;
            if accepted == 0 {
                return Ok(None);
            }
        }
        Ok(Some(end))
    }

    /// Intrinsic resampler delay in native output samples for end-padding finite media.
    pub fn output_delay_samples(&self) -> Result<u64, RingError> {
        self.0.output_delay_samples()
    }
    /// Publish EchoLink's accepted finite prefix, retaining one exact ring token.
    pub fn finish(&mut self) -> Result<u64, RingError> {
        if !matches!(self.0.policy, InboundPolicy::Echo) {
            return Err(RingError);
        }
        let mut token = 0;
        let code =
            unsafe { self.0.api.ring_producer_finish.unwrap()(self.0.handle.as_ptr(), &mut token) };
        if code != 0 || token == 0 {
            return Err(RingError);
        }
        self.0.finite.store(token, Ordering::Release);
        Ok(token)
    }
    /// Acquire played-versus-cancelled/failed completion; occupancy alone cannot
    /// establish whether accepted FIR and PLC lookahead has reached the radio.
    pub fn finish_status(&self, token: u64) -> Result<FinishState, RingError> {
        self.0.finish_status(token)
    }
}
impl Drop for InboundProducer {
    fn drop(&mut self) {
        self.0.signals.end();
    }
}
impl PeerInput for InboundConsumer {
    fn receive_audio_current(&self) -> bool {
        self.fresh()
    }
    fn native_real_count(&self, audible: usize) -> usize {
        if self.epoch() == 0 {
            audible
        } else {
            self.0.native_real.load(Ordering::Relaxed) as usize
        }
    }
    fn source_rate(&self) -> u32 {
        self.0.input_rate
    }
    fn receive_policy(&self) -> ReceivePolicy {
        match self.0.policy {
            InboundPolicy::Phone => ReceivePolicy::Phone,
            _ => ReceivePolicy::Peer,
        }
    }
    fn source_timing_available(&self) -> bool {
        self.timing_available()
    }
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        InboundConsumer::render_timed_sample(self).ok()
    }
    fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    fn available(&self) -> u64 {
        self.observe()
            .map_or(0, |observation| observation.available_samples)
    }
    fn finite_playout(&self) -> Option<bool> {
        let token = self.0.finite.load(Ordering::Acquire);
        (token != 0).then(|| self.0.finish_status(token) == Ok(FinishState::Pending))
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        InboundConsumer::render(self, output).ok()
    }
    fn render_sample(&mut self) -> Option<f32> {
        InboundConsumer::render_sample(self).ok().flatten()
    }
}
impl InboundConsumer {
    pub(crate) fn local_epoch(&mut self, epoch: u64) {
        self.0.local_playout_epoch.store(epoch, Ordering::Relaxed);
    }
    pub(crate) fn fresh(&self) -> bool {
        let epoch = self.epoch();
        epoch & 1 == 0 && self.0.played_epoch.load(Ordering::Relaxed) == epoch
    }
    fn epoch(&self) -> u64 {
        if matches!(self.0.policy, InboundPolicy::Local { .. }) {
            self.0.local_playout_epoch.load(Ordering::Relaxed)
        } else {
            self.0.signals.receive_epoch()
        }
    }

    /// Whether the copied readable descriptor includes the optional timed primitive.
    pub fn timing_available(&self) -> bool {
        self.0.api.ring_consumer_render_sample_timed.is_some()
    }

    /// Render one actual source-timed sample. Missing capability is an explicit
    /// error without consumption; no source age or wall-clock value is invented.
    pub fn render_timed_sample(&mut self) -> Result<TimedPeerSample, RingError> {
        let render = self
            .0
            .api
            .ring_consumer_render_sample_timed
            .ok_or(RingError)?;
        let mut sample = 0.0;
        let mut timing = ffi::rpcr3_timed_sample_v1 {
            struct_size: size_of::<ffi::rpcr3_timed_sample_v1>() as u32,
            flags: 0,
            generation: 0,
            source_frame: 0,
            source_fraction_billionths: 0,
            reserved: 0,
        };
        // SAFETY: this unique consumer owns the shared PCM path; both complete
        // outputs remain writable throughout the synchronous provider call.
        let code = unsafe { render(self.0.handle.as_ptr(), &mut sample, &mut timing) };
        if code != 0
            || timing.struct_size < size_of::<ffi::rpcr3_timed_sample_v1>() as u32
            || timing.reserved != 0
            || timing.source_fraction_billionths >= 1_000_000_000
        {
            return Err(RingError);
        }
        let mut provenance = SampleProvenance {
            position: (timing.flags & ffi::RPCR3_TIMED_POSITION_VALID != 0).then_some(
                SourceCoordinate {
                    generation: timing.generation,
                    frame: timing.source_frame,
                    fraction_billionths: timing.source_fraction_billionths,
                    negative: timing.flags & ffi::RPCR3_TIMED_NEGATIVE != 0,
                },
            ),
            real: timing.flags & ffi::RPCR3_TIMED_REAL != 0,
            plc: timing.flags & ffi::RPCR3_TIMED_PLC != 0,
            gap: timing.flags & ffi::RPCR3_TIMED_GAP != 0,
            muted: timing.flags & ffi::RPCR3_TIMED_MUTED != 0,
            zero_extension: timing.flags & ffi::RPCR3_TIMED_ZERO_EXTENSION != 0,
        };
        self.0
            .native_real
            .store(u64::from(provenance.real), Ordering::Relaxed);
        let epoch = self.epoch();
        if epoch != 0 {
            let fresh_position = epoch & 1 == 0
                && self.0.first_epoch.load(Ordering::Acquire) == epoch
                && provenance.position.is_some_and(|p| {
                    !p.negative
                        && p.generation == self.0.first_generation.load(Ordering::Relaxed)
                        && p.frame >= self.0.first_frame.load(Ordering::Relaxed)
                })
                && self.0.first_epoch.load(Ordering::Acquire) == epoch;
            if fresh_position && provenance.real && !provenance.gap && !provenance.zero_extension {
                self.0.played_epoch.store(epoch, Ordering::Relaxed);
            }
            if !self.fresh() || (!fresh_position && !provenance.plc) || self.epoch() != epoch {
                sample = 0.;
                provenance.real = false;
                provenance.muted = true;
            }
        }
        if self.0.signals.dtmf_muted() {
            sample = 0.0;
            provenance.muted = true;
        }
        if let Some(markers) = &mut self.1
            && provenance.real
            && !provenance
                .position
                .filter(|p| p.generation == 1)
                .is_some_and(|p| markers.permits(if p.negative { 0 } else { p.frame }))
        {
            sample = 0.0;
            provenance.real = false;
            provenance.muted = true;
        }
        Ok(TimedPeerSample {
            sample,
            provenance: Some(provenance),
        })
    }

    /// Render with the prepared immutable policy; the library classifies real PCM.
    pub fn render(&mut self, samples: &mut [f32]) -> Result<usize, RingError> {
        let epoch = self.epoch();
        // Only the transition into a new unmuted source interval needs
        // individual coordinates. Stable blocks use the single native bulk call.
        if self.1.is_some() || (epoch & 1 == 0 && !self.fresh() && self.timing_available()) {
            let mut real = 0;
            let mut native = 0;
            for output in samples.iter_mut() {
                let timed = self.render_timed_sample()?;
                *output = timed.sample;
                native += self.0.native_real.load(Ordering::Relaxed);
                real += usize::from(timed.provenance.is_some_and(|p| p.real));
            }
            self.0.native_real.store(native, Ordering::Relaxed);
            if self.epoch() != epoch {
                samples.fill(0.);
                return Ok(0);
            }
            return Ok(real);
        }
        let mut real = 0;
        // SAFETY: the unique consumer owns conversion state and this output slice.
        let code = unsafe {
            (self.0.api.ring_consumer_render.unwrap())(
                self.0.handle.as_ptr(),
                samples.as_mut_ptr(),
                samples.len() as u64,
                &mut real,
            )
        };
        if code == 0 {
            self.0.native_real.store(real, Ordering::Relaxed);
            if self.epoch() != epoch || !self.fresh() {
                samples.fill(0.);
                return Ok(0);
            }
            if self.0.signals.dtmf_muted() {
                samples.fill(0.0);
            }
            Ok(real as usize)
        } else {
            samples.fill(0.0);
            Err(RingError)
        }
    }
    /// Render one native sample without creating a synthetic media sample on shortfall.
    pub fn render_sample(&mut self) -> Result<Option<f32>, RingError> {
        let epoch = self.epoch();
        let mut sample = 0.0;
        // Only the transition into a new unmuted source interval needs
        // individual coordinates. Stable blocks use the single native bulk call.
        if self.1.is_some() || (epoch & 1 == 0 && !self.fresh() && self.timing_available()) {
            let timed = self.render_timed_sample()?;
            return Ok(timed
                .provenance
                .is_some_and(|p| p.real)
                .then_some(timed.sample));
        }
        let mut real = false;
        // SAFETY: this endpoint is the unique consumer and both outputs are writable.
        let code = unsafe {
            (self.0.api.ring_consumer_render_sample.unwrap())(
                self.0.handle.as_ptr(),
                &mut sample,
                &mut real,
            )
        };
        if code == 0 {
            if self.epoch() != epoch || !self.fresh() {
                return Ok(None);
            }
            if self.0.signals.dtmf_muted() {
                sample = 0.0;
            }
            Ok(real.then_some(sample))
        } else {
            Err(RingError)
        }
    }
    /// Copy current diagnostics without locking either endpoint.
    pub fn observe(&self) -> Result<Observation, RingError> {
        self.0.observe()
    }
    /// End a receive burst so the next one must refill its reserve.
    pub fn reset(&mut self) -> Result<(), RingError> {
        let code = unsafe { (self.0.api.ring_consumer_reset.unwrap())(self.0.handle.as_ptr()) };
        (code == 0).then_some(()).ok_or(RingError)
    }
}
impl Shared {
    fn producer_prefix(&self, samples: u64) -> u64 {
        if matches!(self.policy, InboundPolicy::Phone) {
            samples.min(u64::from(self.input_rate) / 50)
        } else {
            samples
        }
    }

    fn finish_status(&self, token: u64) -> Result<FinishState, RingError> {
        let query = self.api.ring_finish_status.ok_or(RingError)?;
        let mut status = u32::MAX;
        let code = unsafe { query(self.handle.as_ptr(), token, &mut status) };
        if code != 0 {
            return Err(RingError);
        }
        Ok(match status {
            0 => FinishState::Pending,
            1 => FinishState::Drained,
            2 => FinishState::Cancelled,
            3 => FinishState::Failed,
            _ => return Err(RingError),
        })
    }
    fn output_delay_samples(&self) -> Result<u64, RingError> {
        let mut delay = 0;
        // SAFETY: the ring remains live and delay is writable output storage.
        let code =
            unsafe { (self.api.ring_output_delay.unwrap())(self.handle.as_ptr(), &mut delay) };
        (code == 0).then_some(delay).ok_or(RingError)
    }

    fn observe(&self) -> Result<Observation, RingError> {
        // SAFETY: observation is plain integer ABI storage initialized with caller size.
        let mut observation: Observation = unsafe { std::mem::zeroed() };
        observation.struct_size = size_of::<Observation>() as u32;
        let code =
            unsafe { (self.api.ring_observe.unwrap())(self.handle.as_ptr(), &mut observation) };
        if code == 0 {
            Ok(observation)
        } else {
            Err(RingError)
        }
    }
}

#[cfg(test)]
#[path = "ring_tests.rs"]
pub(crate) mod tests;
