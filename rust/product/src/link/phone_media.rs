//! One prepared phone receive generation, using the common ring and shared graph.
use crate::link::phone_speech::SpeechAnalysis;
use crate::{
    Error,
    link::ring::{InboundConsumer, InboundObserver, InboundPolicy, InboundProducer, InboundRing},
    services::{PcmProcessor, ProcessedPeer, SourceBirth, SourceTimeReference},
};
use rpt_advanced_core::audio::{LinkAudioConsumer, LinkAudioProducer, LinkAudioQueue};
use rpt_advanced_core::link::{
    PeerInput, PeerReply, PeerSignals, PhoneSource, ReceivePolicy, RenderedPeerBlock,
    SourceCoordinate, SourceTimeBound, TimedPeerSample,
};
use std::num::NonZeroU32;
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
};

const BUCKETS: usize = 64;
const RETIRED: u8 = 0;
const PREPARED: u8 = 1;
const ACTIVE: u8 = 2;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Bucket {
    revision: AtomicU64,
    index: AtomicU64,
    epoch: AtomicU64,
    birth: AtomicU64,
    voice_start: AtomicU64,
    voice_end: AtomicU64,
}

/// Exact call and one independently revocable media generation; no node identity.
pub(super) struct Shared {
    reply: PeerReply,
    observer: InboundObserver,
    phase: AtomicU8,
    generation: u64,
    width: u64,
    voiced: bool,
    oldest: AtomicU64,
    buckets: [Bucket; BUCKETS],
}
impl Shared {
    pub(super) fn occupied(&self) -> bool {
        self.phase.load(Ordering::Acquire) != RETIRED && self.reply.phone_authenticated()
    }
    fn active(&self) -> bool {
        self.phase.load(Ordering::Acquire) == ACTIVE && self.reply.phone_authenticated()
    }
    pub(super) fn retire(&self) {
        self.phase.store(RETIRED, Ordering::Release);
        self.observer.signals().end();
    }
    fn slot(&self, index: u64) -> &Bucket {
        &self.buckets[(index % BUCKETS as u64) as usize]
    }
    // At most two 20 ms buckets per offered ring prefix. Refuse live-slot
    // collisions before PCM acceptance. Fragment count never consumes capacity.
    fn record(&self, start: u64, end: u64, birth: SourceBirth) -> bool {
        let first = start / self.width;
        let last = (end - 1) / self.width;
        let oldest = self.oldest.load(Ordering::Acquire);
        for index in first..=last {
            let slot = self.slot(index);
            if slot.revision.load(Ordering::SeqCst) != 0 {
                let previous = slot.index.load(Ordering::SeqCst);
                if previous != index && previous >= oldest {
                    return false;
                }
            }
        }
        for index in first..=last {
            let slot = self.slot(index);
            let retained = slot.revision.load(Ordering::SeqCst) != 0
                && slot.index.load(Ordering::SeqCst) == index;
            let earliest = if retained {
                slot.birth.load(Ordering::SeqCst).min(birth.born_ns)
            } else {
                birth.born_ns
            };
            // All fields are atomic and in one total order. A racing reader
            // returns unknown rather than mixing a reused bucket's identity/time.
            slot.revision.fetch_add(1, Ordering::SeqCst);
            if !retained {
                slot.voice_start.store(0, Ordering::SeqCst);
                slot.voice_end.store(0, Ordering::SeqCst);
            }
            slot.index.store(index, Ordering::SeqCst);
            slot.epoch.store(birth.host_clock_epoch, Ordering::SeqCst);
            slot.birth.store(earliest, Ordering::SeqCst);
            slot.revision.fetch_add(1, Ordering::SeqCst);
        }
        true
    }
    // Add source-coordinate admission to existing receipt buckets, never PCM.
    // Only the serial ingress writes; an interrupted snapshot remains unqualified.
    fn mark_voice(&self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let first = (start / self.width).max(self.oldest.load(Ordering::Acquire));
        let last = (end - 1) / self.width;
        for index in first..=last {
            let slot = self.slot(index);
            if slot.revision.load(Ordering::SeqCst) == 0
                || slot.index.load(Ordering::SeqCst) != index
            {
                continue;
            }
            let left = start.max(index * self.width);
            let right = end.min((index + 1) * self.width);
            let old_end = slot.voice_end.load(Ordering::SeqCst);
            let left = if old_end == 0 {
                left
            } else {
                left.min(slot.voice_start.load(Ordering::SeqCst))
            };
            slot.revision.fetch_add(1, Ordering::SeqCst);
            slot.voice_start.store(left, Ordering::SeqCst);
            slot.voice_end.store(right.max(old_end), Ordering::SeqCst);
            slot.revision.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn voiced_frame(&self, frame: u64) -> bool {
        let slot = self.slot(frame / self.width);
        let before = slot.revision.load(Ordering::SeqCst);
        before != 0
            && before & 1 == 0
            && slot.index.load(Ordering::SeqCst) == frame / self.width
            && slot.voice_start.load(Ordering::SeqCst) <= frame
            && frame < slot.voice_end.load(Ordering::SeqCst)
            && slot.revision.load(Ordering::SeqCst) == before
    }
    fn voice_at(&self, position: SourceCoordinate) -> bool {
        position.generation == self.generation
            && !position.negative
            && self.voiced_frame(position.frame)
            && (position.fraction_billionths == 0
                || position
                    .frame
                    .checked_add(1)
                    .is_some_and(|next| self.voiced_frame(next)))
    }
    fn bound(&self, frame: u64) -> Option<SourceTimeBound> {
        let index = frame / self.width;
        let slot = self.slot(index);
        let before = slot.revision.load(Ordering::SeqCst);
        if before == 0 || before & 1 != 0 {
            return None;
        }
        let found = slot.index.load(Ordering::SeqCst);
        let bound = SourceTimeBound {
            host_clock_epoch: slot.epoch.load(Ordering::SeqCst),
            not_after_ns: slot.birth.load(Ordering::SeqCst),
        };
        (found == index && slot.revision.load(Ordering::SeqCst) == before).then_some(bound)
    }
    fn locate(&self, position: SourceCoordinate) -> Option<SourceTimeBound> {
        if position.generation != self.generation || position.negative {
            return None;
        }
        // Keep the preceding bucket for converter boundary interpolation.
        self.oldest.store(
            (position.frame / self.width).saturating_sub(1),
            Ordering::Release,
        );
        let mut bound = self.bound(position.frame)?;
        if position.fraction_billionths != 0 {
            let next = self.bound(position.frame.checked_add(1)?)?;
            if next.host_clock_epoch != bound.host_clock_epoch {
                return None;
            }
            bound.not_after_ns = bound.not_after_ns.min(next.not_after_ns);
        }
        Some(bound)
    }
}

/// Serial source endpoint; the existing phone I/O owner is its sole writer.
pub(super) struct Ingress {
    shared: Arc<Shared>,
    expected_revision: NonZeroU32,
    writer: InboundProducer,
    minimum_frame: u64,
    admission: SourceTimeReference,
    cursor: Option<u64>,
    outbound: Option<LinkAudioConsumer>,
    speech: Option<SpeechAnalysis>,
    last_voice: Option<u64>,
    voice_until: u64,
}
impl Ingress {
    fn voice_decision(&mut self, last: Option<u64>) {
        let Some(last) = last.filter(|last| self.last_voice.is_none_or(|old| *last > old)) else {
            return;
        };
        self.last_voice = Some(last);
        let end = last.saturating_add(1);
        // Reuse the existing 160-ms retained onset and the design's 1000-ms
        // default hang. Neither adds a programme buffer to shared analysis.
        self.shared
            .mark_voice(end.saturating_sub(self.shared.width * 8), end);
        self.voice_until = end.saturating_add(self.shared.width * 50);
    }
    pub(super) fn active(&self) -> bool {
        self.shared.active()
    }
    pub(super) fn guard(&self) -> PeerReply {
        self.shared.observer.signals().reply_target()
    }
    pub(super) fn take_output(&mut self) -> Option<LinkAudioConsumer> {
        self.outbound.take()
    }
    pub(super) fn prepared(&self) -> bool {
        self.shared.phase.load(Ordering::Acquire) == PREPARED
    }
    pub(super) fn occupied(&self) -> bool {
        self.shared.occupied()
    }
    pub(super) fn expected_revision(&self) -> NonZeroU32 {
        self.expected_revision
    }
    /// Publish once, and only to the serial owner of this exact call token.
    /// Active or retired generations never receive a replacement admission anchor.
    pub(super) fn adopt(
        &mut self,
        reply: &PeerReply,
        minimum_frame: u64,
        admission: SourceTimeReference,
    ) {
        if !self.prepared() {
            return;
        }
        if admission.host_clock_epoch == 0 || self.shared.reply != *reply || !self.shared.occupied()
        {
            self.shared.retire();
        } else {
            self.minimum_frame = minimum_frame;
            self.admission = admission;
            let _ = self.shared.phase.compare_exchange(
                PREPARED,
                ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
    pub(super) fn retire(&self) {
        self.shared.retire();
    }
    pub(super) fn audio(
        &mut self,
        start: u64,
        samples: &[f32],
        birth: Option<SourceBirth>,
    ) -> Result<usize, Error> {
        if !self.shared.active() {
            return Ok(samples.len());
        }
        let Some(birth) =
            birth.filter(|birth| birth.host_clock_epoch == self.admission.host_clock_epoch)
        else {
            // Treat lost timing like source loss, not a later fatal cursor jump.
            // Only the control owner may prepare a newly qualified generation.
            self.retire();
            return Ok(samples.len());
        };
        if birth.born_ns < self.admission.now_ns {
            // Jitter may still release a known pre-cut prefix after adoption.
            // Admit none of it and keep the immutable cut until fresh PCM arrives.
            // Once PCM has entered this generation, stale input is a real loss.
            if self.cursor.is_some() {
                self.retire();
            }
            return Ok(samples.len());
        }
        if start < self.minimum_frame {
            return Ok(samples.len().min((self.minimum_frame - start) as usize));
        }
        if self.cursor.is_some_and(|cursor| cursor != start) {
            return Err(Error::InvalidFrame);
        }
        let offered = samples.len().min(self.shared.width as usize);
        if offered == 0 {
            return Ok(0);
        }
        let end = start
            .checked_add(offered as u64)
            .ok_or(Error::InvalidFrame)?;
        if !self.shared.record(start, end, birth) {
            return Ok(0);
        }
        let accepted = self
            .writer
            .write_tagged(&samples[..offered], self.shared.generation, start, &[])
            .map_err(|_| Error::Write)?;
        self.cursor = Some(start + accepted as u64);
        if accepted != 0 {
            if let Some(speech) = self.speech.as_mut() {
                let decision = speech.process(start, &samples[..accepted])?;
                self.voice_decision(decision);
                self.shared
                    .mark_voice(start, (start + accepted as u64).min(self.voice_until));
            }
        }
        Ok(accepted)
    }
    pub(super) fn gap(&mut self, start: u64, count: usize) -> Result<usize, Error> {
        // A missing/codec-concealed interval has no qualifying original voice.
        // Retire this admission rather than let PLC acquire a fresh age bound.
        self.retire();
        self.cursor = start.checked_add(count as u64);
        Ok(count)
    }
}
impl Drop for Ingress {
    fn drop(&mut self) {
        self.retire();
    }
}

struct TimedInput {
    reader: InboundConsumer,
    shared: Arc<Shared>,
    qualified: u64,
}
impl PeerInput for TimedInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        // Graph history is prepared around this inner owner, before the outer
        // media consumer exists. Preserve the same authenticated call token.
        Some(&self.shared.reply)
    }
    fn source_rate(&self) -> u32 {
        self.reader.source_rate()
    }
    fn receive_policy(&self) -> ReceivePolicy {
        ReceivePolicy::Phone
    }
    fn source_timing_available(&self) -> bool {
        self.reader.timing_available()
    }
    fn signals(&self) -> &PeerSignals {
        self.reader.signals()
    }
    fn available(&self) -> u64 {
        self.reader.available()
    }
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        let mut sample = self.reader.render_timed_sample().ok()?;
        if let Some(provenance) = sample.provenance.as_ref() {
            if provenance.real && !provenance.plc && !provenance.gap && !provenance.zero_extension {
                sample.age_bound = provenance
                    .position
                    .and_then(|position| self.shared.locate(position));
            }
        }
        if self.shared.voiced {
            let qualified = sample.age_bound.is_some()
                && sample.provenance.as_ref().is_some_and(|p| {
                    p.real
                        && !p.plc
                        && !p.gap
                        && !p.zero_extension
                        && !p.muted
                        && p.position
                            .is_some_and(|position| self.shared.voice_at(position))
                });
            if qualified {
                self.qualified = self.qualified.wrapping_add(1);
            } else {
                sample.sample = 0.0;
                if let Some(provenance) = sample.provenance.as_mut() {
                    provenance.muted = true;
                }
            }
        }
        Some(sample)
    }
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        let mut real = 0;
        let mut age_bound = None;
        let mut contributing = 0;
        for sample in output.iter_mut() {
            let Some(value) = self.render_timed_sample() else {
                output.fill(0.0);
                return None;
            };
            *sample = value.sample;
            real += usize::from(value.provenance.is_some_and(|provenance| provenance.real));
            // Source-muted VOX output is known zero, not an unknown-age
            // contribution. A real sample with missing timing is muted above.
            if !self.shared.voiced || value.provenance.is_some_and(|p| !p.muted) {
                age_bound = if contributing == 0 {
                    value.age_bound
                } else {
                    age_bound.zip(value.age_bound).and_then(|(previous, next)| {
                        (previous.host_clock_epoch != 0
                            && previous.host_clock_epoch == next.host_clock_epoch)
                            .then_some(SourceTimeBound {
                                host_clock_epoch: previous.host_clock_epoch,
                                not_after_ns: previous.not_after_ns.min(next.not_after_ns),
                            })
                    })
                };
                contributing += 1;
            }
        }
        Some(RenderedPeerBlock { real, age_bound })
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        self.render_timed_block(output).map(|block| block.real)
    }
    fn render_sample(&mut self) -> Option<f32> {
        let sample = self.render_timed_sample()?;
        sample
            .provenance
            .is_some_and(|p| p.real)
            .then_some(sample.sample)
    }
}

/// Unique prepared receive consumer. This is not a node/RF attachment or permission grant.
/// Exact call and media retirement fence the common ring and shared graph before and
/// after every render; a retired generation is never reactivated.
pub struct PhoneMediaInput {
    input: ProcessedPeer<TimedInput>,
    shared: Arc<Shared>,
    outbound: Option<LinkAudioProducer>,
}
impl PhoneMediaInput {
    /// Transfer the existing mix-minus queue producer to exactly one typed graph slot.
    pub fn take_output(&mut self) -> Option<LinkAudioProducer> {
        self.outbound.take()
    }
    /// Actual provider/account/call/ingress identity retained by this consumer.
    pub fn source(&self) -> &PhoneSource {
        self.shared
            .reply
            .phone_source()
            .expect("prepared private phone source")
    }
    /// Whether the serial source owner adopted this still-live prepared generation.
    pub fn active(&self) -> bool {
        self.shared.active()
    }
}
impl PeerInput for PhoneMediaInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        Some(&self.shared.reply)
    }
    fn source_rate(&self) -> u32 {
        self.input.source_rate()
    }
    fn receive_policy(&self) -> ReceivePolicy {
        ReceivePolicy::Phone
    }
    fn source_timing_available(&self) -> bool {
        self.input.source_timing_available()
    }
    fn signals(&self) -> &PeerSignals {
        self.input.signals()
    }
    fn available(&self) -> u64 {
        if self.active() {
            self.input.available()
        } else {
            0
        }
    }
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        if !self.active() {
            return None;
        }
        let sample = self.input.render_timed_sample()?;
        self.active().then_some(sample)
    }
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        if self.active() {
            let before = self.input.source().qualified;
            let result = self.input.render_timed_block(output);
            if self.active() {
                if !self.shared.voiced {
                    return result;
                }
                let qualified = self.input.source().qualified.wrapping_sub(before) as usize;
                let active = self.input.output_qualified().unwrap_or(qualified != 0);
                if let Some(mut block) = result.filter(|block| active && block.age_bound.is_some())
                {
                    // Stateful output may contain the previous block's final voice,
                    // even when every newly admitted source sample was muted.
                    block.real = if self.input.output_qualified().is_some() {
                        output.len()
                    } else {
                        qualified
                    };
                    return Some(block);
                }
            }
        }
        output.fill(0.0);
        None
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        self.render_timed_block(output).map(|block| block.real)
    }
    fn render_sample(&mut self) -> Option<f32> {
        if !self.active() {
            return None;
        }
        let sample = self.input.render_sample()?;
        self.active().then_some(sample)
    }
    fn silence(&mut self, output: &mut [f32]) {
        if self.active() {
            self.input.silence(output);
        } else {
            output.fill(0.0);
        }
    }
}
impl Drop for PhoneMediaInput {
    fn drop(&mut self) {
        self.shared.retire();
    }
}

pub(super) fn prepare(
    reply: PeerReply,
    rate: u32,
    expected_revision: NonZeroU32,
    processor: Option<PcmProcessor>,
) -> Result<(Box<Ingress>, PhoneMediaInput, Arc<Shared>), Error> {
    prepare_with_speech(reply, rate, expected_revision, processor, None)
}

pub(super) fn prepare_voiced(
    reply: PeerReply,
    rate: u32,
    expected_revision: NonZeroU32,
    processor: Option<PcmProcessor>,
) -> Result<(Box<Ingress>, PhoneMediaInput, Arc<Shared>), Error> {
    if !reply.phone_authenticated() {
        return Err(Error::Admission);
    }
    let speech = SpeechAnalysis::new(rate, (rate / 50) as usize)?;
    prepare_with_speech(reply, rate, expected_revision, processor, Some(speech))
}

fn prepare_with_speech(
    reply: PeerReply,
    rate: u32,
    expected_revision: NonZeroU32,
    processor: Option<PcmProcessor>,
    speech: Option<SpeechAnalysis>,
) -> Result<(Box<Ingress>, PhoneMediaInput, Arc<Shared>), Error> {
    if !reply.phone_authenticated() {
        return Err(Error::Admission);
    }
    let generation = NEXT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map_err(|_| Error::Admission)?;
    let (writer, reader) =
        InboundRing::open(rate, InboundPolicy::Phone).map_err(|_| Error::Allocation)?;
    if !reader.timing_available() {
        return Err(Error::UnsupportedFormat);
    }
    let shared = Arc::new(Shared {
        reply,
        observer: writer.observer(),
        phase: AtomicU8::new(PREPARED),
        generation,
        width: u64::from(rate) / 50,
        voiced: speech.is_some(),
        oldest: AtomicU64::new(0),
        buckets: std::array::from_fn(|_| Bucket::default()),
    });
    let (output, outbound) = LinkAudioQueue::new(48_000 / 5, 64)
        .map_err(|_| Error::Allocation)?
        .into_endpoints();
    let input = PhoneMediaInput {
        outbound: Some(output),
        input: ProcessedPeer::new(
            TimedInput {
                reader,
                shared: Arc::clone(&shared),
                qualified: 0,
            },
            processor,
        ),
        shared: Arc::clone(&shared),
    };
    Ok((
        Box::new(Ingress {
            outbound: Some(outbound),
            speech,
            last_voice: None,
            voice_until: 0,
            shared: Arc::clone(&shared),
            expected_revision,
            writer,
            minimum_frame: 0,
            admission: SourceTimeReference::default(),
            cursor: None,
        }),
        input,
        shared,
    ))
}

#[cfg(test)]
#[path = "phone_media_tests.rs"]
mod tests;
