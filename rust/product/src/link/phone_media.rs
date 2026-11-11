//! One prepared phone receive generation, using the common ring and shared graph.
use crate::{
    Error,
    link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing},
    services::{PcmProcessor, ProcessedPeer, SourceBirth, SourceTimeReference},
};
use rpt_advanced_core::link::{
    PeerInput, PeerReply, PeerSignals, PhoneSource, ReceivePolicy, SourceCoordinate,
    SourceTimeBound, TimedPeerSample,
};
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
}

/// Exact call and one independently revocable media generation; no node identity.
pub(super) struct Shared {
    reply: PeerReply,
    phase: AtomicU8,
    generation: u64,
    width: u64,
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
            slot.index.store(index, Ordering::SeqCst);
            slot.epoch.store(birth.host_clock_epoch, Ordering::SeqCst);
            slot.birth.store(earliest, Ordering::SeqCst);
            slot.revision.fetch_add(1, Ordering::SeqCst);
        }
        true
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
    writer: InboundProducer,
    minimum_frame: u64,
    admission: SourceTimeReference,
    cursor: Option<u64>,
}
impl Ingress {
    pub(super) fn adopt(&mut self, minimum_frame: u64, admission: SourceTimeReference) {
        self.minimum_frame = minimum_frame;
        self.admission = admission;
        if admission.host_clock_epoch == 0 || !self.shared.occupied() {
            self.shared.retire();
        } else {
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
        self.writer.signals().end();
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
        let Some(birth) = birth.filter(|birth| {
            birth.host_clock_epoch == self.admission.host_clock_epoch
                && birth.born_ns >= self.admission.now_ns
        }) else {
            // Treat lost timing like source loss, not a later fatal cursor jump.
            // Only the control owner may prepare a newly qualified generation.
            self.retire();
            return Ok(samples.len());
        };
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
}
impl PeerInput for TimedInput {
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
        Some(sample)
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        let mut real = 0;
        for sample in output.iter_mut() {
            let Some(value) = self.render_timed_sample() else {
                output.fill(0.0);
                return None;
            };
            *sample = value.sample;
            real += usize::from(value.provenance.is_some_and(|p| p.real));
        }
        Some(real)
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
}
impl PhoneMediaInput {
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
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        if self.active() {
            let result = self.input.render(output);
            if self.active() {
                return result;
            }
        }
        output.fill(0.0);
        None
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
    processor: Option<PcmProcessor>,
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
        phase: AtomicU8::new(PREPARED),
        generation,
        width: u64::from(rate) / 50,
        oldest: AtomicU64::new(0),
        buckets: std::array::from_fn(|_| Bucket::default()),
    });
    let input = PhoneMediaInput {
        input: ProcessedPeer::new(
            TimedInput {
                reader,
                shared: Arc::clone(&shared),
            },
            processor,
        ),
        shared: Arc::clone(&shared),
    };
    Ok((
        Box::new(Ingress {
            shared: Arc::clone(&shared),
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
