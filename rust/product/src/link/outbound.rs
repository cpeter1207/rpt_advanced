//! Single-owner media conversion, source-clock timestamps, and bounded retry storage.
use super::{egress::Egress, source_clock::SourceClock};
use crate::{Error, services::PeerIo};
use rpt_advanced_core::audio::{AudioSpan, LinkAudioConsumer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const BLOCK: usize = 960;
const AGE_NS: u64 = 200_000_000;
const BLOCK_NS: u64 = 20_000_000;

/// Connection-lifetime egress counters; observations may straddle an owner turn.
pub struct EgressObservation {
    /// Prepared packets accepted by the provider, not necessarily sent yet.
    pub accepted: u64,
    /// Uncommitted send attempts rejected by backpressure.
    pub blocked: u64,
    /// Expired media duration expressed at the 48 kHz native rate.
    pub expired_native_samples: u64,
    /// Newest samples rejected by the bounded native queue.
    pub rejected_native_samples: u64,
    /// Maximum observed oldest queued or pending media age.
    pub maximum_queue_age_ns: u64,
    /// Accepted packet age histogram: integer milliseconds, final bucket >=200 ms.
    pub send_age_ms: [u64; 201],
}
pub(super) struct EgressStatistics {
    accepted: AtomicU64,
    blocked: AtomicU64,
    expired: AtomicU64,
    rejected: AtomicU64,
    queue_age: AtomicU64,
    ages: [AtomicU64; 201],
}
impl EgressStatistics {
    fn new() -> Self {
        Self {
            accepted: 0.into(),
            blocked: 0.into(),
            expired: 0.into(),
            rejected: 0.into(),
            queue_age: 0.into(),
            ages: std::array::from_fn(|_| 0.into()),
        }
    }
    pub(super) fn snapshot(&self) -> EgressObservation {
        EgressObservation {
            accepted: self.accepted.load(Ordering::Relaxed),
            blocked: self.blocked.load(Ordering::Relaxed),
            expired_native_samples: self.expired.load(Ordering::Relaxed),
            rejected_native_samples: self.rejected.load(Ordering::Relaxed),
            maximum_queue_age_ns: self.queue_age.load(Ordering::Relaxed),
            send_age_ms: self.ages.each_ref().map(|age| age.load(Ordering::Relaxed)),
        }
    }
}

struct Pending {
    bytes: usize,
    frames: u32,
    source_frames: u32,
    timestamp: u32,
    end_timestamp: u32,
    expires: u64,
    stream: u64,
}
struct Conversion {
    span: AudioSpan,
    next: u64,
    output_samples: u64,
    born_ns: u64,
    drained: bool,
}
impl Conversion {
    fn source_ns(&self, rate: u32) -> u128 {
        source_ns(self.span.source_start)
            + u128::from(self.output_samples) * 1_000_000_000 / u128::from(rate)
    }
}
struct Anchor {
    stream: u64,
    source_ns: u128,
    call_ms: u32,
}
struct EncoderClock {
    stream: u64,
    origin_ns: u128,
    born_ns: u64,
    wire_frames: u64,
    source_end: u64,
    input_frames: u64,
    drained: bool,
}

/// One prepared packet at most; provider-committed reliable packets belong to IAX.
pub(super) struct Outbound {
    pub(super) statistics: Arc<EgressStatistics>,
    queue: LinkAudioConsumer,
    retired_rejections: u64,
    converter: Egress,
    rate: u32,
    native: [f32; BLOCK],
    encoded: [u8; BLOCK * 2 + 64],
    pcm_used: usize,
    pcm_position: usize,
    encoder: Option<EncoderClock>,
    reset_encoder: bool,
    pending: Option<Pending>,
    conversion: Option<Conversion>,
    anchor: Option<Anchor>,
    clock: Option<Arc<SourceClock>>,
    next_audio_ms: u64,
    unkey_ns: Option<u64>,
    idle_timestamp: u32,
    sent: bool,
}
fn source_ns(sample: u64) -> u128 {
    u128::from(sample) * 1_000_000_000 / 48_000
}
impl Outbound {
    pub(super) fn new(rate: u32, queue: LinkAudioConsumer) -> Result<Self, Error> {
        Ok(Self {
            statistics: Arc::new(EgressStatistics::new()),
            queue,
            retired_rejections: 0,
            converter: Egress::new(rate, BLOCK).map_err(|_| Error::Allocation)?,
            rate,
            native: [0.0; BLOCK],
            encoded: [0; BLOCK * 2 + 64],
            pcm_used: 0,
            pcm_position: 0,
            encoder: None,
            reset_encoder: true,
            pending: None,
            conversion: None,
            anchor: None,
            clock: None,
            next_audio_ms: 0,
            unkey_ns: None,
            idle_timestamp: 0,
            sent: false,
        })
    }
    pub(super) fn bind_clock(&mut self, clock: Arc<SourceClock>) {
        self.clock = Some(clock);
    }
    pub(super) fn redirect(&mut self, queue: LinkAudioConsumer) -> Result<(), Error> {
        self.retired_rejections = self
            .retired_rejections
            .saturating_add(self.queue.dropped_samples());
        self.queue = queue;
        self.discard()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.pending = None;
        self.conversion = None;
        self.pcm_used = 0;
        self.pcm_position = 0;
        self.encoder = None;
        self.reset_encoder = true;
        self.converter.reset().map_err(|_| Error::Translation)
    }
    fn current(&self, stream: u64) -> bool {
        self.clock
            .as_ref()
            .is_none_or(|clock| clock.stream() == stream)
    }
    /// Return an absolute source/expiry wake. Each call handles at most one media item.
    pub(super) fn step(&mut self, io: &mut PeerIo, now_ms: u64) -> Result<Option<u64>, Error> {
        if !io.prepared_media() {
            self.pcm_step(io, now_ms)?;
            return Ok(Some(self.next_audio_ms.saturating_mul(1_000_000)));
        }
        let now = now_ms.saturating_mul(1_000_000);
        self.statistics.rejected.store(
            self.retired_rejections
                .saturating_add(self.queue.dropped_samples()),
            Ordering::Relaxed,
        );
        let oldest = self
            .pending
            .as_ref()
            .map(|pending| pending.expires.saturating_sub(AGE_NS))
            .or_else(|| self.encoder.as_ref().map(|encoder| encoder.born_ns))
            .or_else(|| self.queue.peek_span().and_then(|span| span.span.born_ns));
        if let Some(born) = oldest {
            self.statistics
                .queue_age
                .fetch_max(now.saturating_sub(born), Ordering::Relaxed);
        }
        if let Some(pending) = &self.pending {
            if now >= pending.expires || !self.current(pending.stream) {
                self.statistics.expired.fetch_add(
                    u64::from(pending.source_frames) * 48_000 / u64::from(self.rate),
                    Ordering::Relaxed,
                );
                self.discard()?;
            } else {
                self.send(io, now)?;
            }
        } else if self.pcm_position < self.pcm_used {
            self.prepare(io, now, false)?;
            self.send(io, now)?;
        } else if self.queue.peek_span().is_some_and(|next| {
            next.span.born_ns.is_none_or(|born| born <= now) || !self.current(next.span.stream)
        }) {
            let read = self
                .queue
                .read_span(&mut self.native)
                .expect("peeked source span");
            if read
                .span
                .born_ns
                .is_none_or(|born| now >= born.saturating_add(AGE_NS))
                || !self.current(read.span.stream)
            {
                self.statistics
                    .expired
                    .fetch_add(read.frames as u64, Ordering::Relaxed);
                self.discard()?;
            } else {
                self.encode(io, read.span, read.frames, now)?;
                self.send(io, now)?;
            }
        }
        if let Some(pending) = &self.pending {
            return Ok(Some(pending.expires));
        }
        if self.pcm_position < self.pcm_used {
            return Ok(Some(now));
        }
        let next = self
            .queue
            .peek_span()
            .map(|next| next.span.born_ns.unwrap_or(now));
        if next.is_some_and(|deadline| deadline <= now) {
            return Ok(next);
        }
        if let Some(deadline) = self.unkey_ns.filter(|deadline| now >= *deadline) {
            if self
                .conversion
                .as_ref()
                .is_some_and(|conversion| !conversion.drained)
            {
                let (audio, finished) = self.converter.finish().map_err(|_| Error::Translation)?;
                let count = audio.len();
                self.conversion.as_mut().expect("converter origin").drained = finished;
                self.converted(io, count, now, None)?;
                self.send(io, now)?;
                if count != 0 || !finished {
                    return Ok(Some(now));
                }
            }
            if self
                .encoder
                .as_ref()
                .is_some_and(|encoder| !encoder.drained)
            {
                self.prepare(io, now, true)?;
                self.send(io, now)?;
                if self.pending.is_some()
                    || self
                        .encoder
                        .as_ref()
                        .is_some_and(|encoder| !encoder.drained)
                {
                    return Ok(Some(now));
                }
            }
            // An idle marker is uncommitted media too: keep its source timestamp
            // while waiting for writable, but never retain it indefinitely.
            let expires = deadline.saturating_add(AGE_NS);
            if self.sent && now < expires && !io.send_prepared(&[], 0, self.idle_timestamp)? {
                return Ok(next.into_iter().chain(Some(expires)).min());
            }
            self.sent = false;
            self.unkey_ns = None;
        }
        Ok(next.into_iter().chain(self.unkey_ns).min())
    }
    fn encode(
        &mut self,
        io: &mut PeerIo,
        span: AudioSpan,
        count: usize,
        now: u64,
    ) -> Result<(), Error> {
        let continuous = self.conversion.as_ref().is_some_and(|previous| {
            !previous.drained
                && previous.span.stream == span.stream
                && previous.span.generation == span.generation
                && previous.span.burst == span.burst
                && previous.next == span.source_start
                && now < previous.born_ns.saturating_add(AGE_NS)
        });
        if !continuous {
            self.discard()?;
            self.conversion = Some(Conversion {
                span,
                next: span.source_start,
                output_samples: 0,
                born_ns: span.born_ns.expect("validated birth"),
                drained: false,
            });
        }
        self.conversion.as_mut().expect("conversion origin").next =
            span.source_start.wrapping_add(count as u64);
        self.unkey_ns = Some(now.saturating_add(BLOCK_NS));
        let count = self
            .converter
            .process(&self.native[..count])
            .map_err(|_| Error::Translation)?
            .len();
        self.converted(io, count, now, Some(span))
    }
    fn converted(
        &mut self,
        io: &mut PeerIo,
        count: usize,
        now: u64,
        input: Option<AudioSpan>,
    ) -> Result<(), Error> {
        let conversion = self.conversion.as_mut().expect("conversion origin");
        let span = conversion.span;
        // Output count, not input partitions, locates samples retained by the FIR.
        let origin = conversion.source_ns(self.rate);
        let born = conversion.born_ns;
        conversion.output_samples = conversion.output_samples.saturating_add(count as u64);
        let next_origin = conversion.source_ns(self.rate);
        // Retained FIR output keeps its prior birth. Only output reaching this
        // input span may adopt this span's new clock mapping.
        conversion.born_ns =
            if let Some(span) = input.filter(|span| next_origin >= source_ns(span.source_start)) {
                span.born_ns
                    .expect("validated birth")
                    .saturating_add((next_origin - source_ns(span.source_start)) as u64)
            } else {
                born.saturating_add((next_origin - origin) as u64)
            };
        if count == 0 {
            return Ok(());
        }
        self.pcm_used = count;
        self.pcm_position = 0;
        self.unkey_ns = Some(now.saturating_add(BLOCK_NS));
        if self.encoder.is_none() {
            self.encoder = Some(EncoderClock {
                stream: span.stream,
                origin_ns: origin,
                born_ns: born,
                wire_frames: 0,
                source_end: 0,
                input_frames: 0,
                drained: false,
            });
        } else {
            let encoder = self.encoder.as_mut().expect("existing encoder origin");
            // Only unbuffered codec input may adopt a newer clock mapping.
            // `born` still names the FIR's first retained sample, not this call.
            if encoder.source_end == encoder.input_frames {
                encoder.born_ns = born;
            }
        }
        self.prepare(io, now, false)
    }
    fn prepare(&mut self, io: &mut PeerIo, now: u64, flush: bool) -> Result<(), Error> {
        if self.reset_encoder {
            io.reset_encoder()?;
            self.reset_encoder = false;
        }
        let encoder = self.encoder.as_ref().expect("prepared encoder origin");
        if now >= encoder.born_ns.saturating_add(AGE_NS) || !self.current(encoder.stream) {
            self.statistics.expired.fetch_add(
                ((self.pcm_used - self.pcm_position) as u64
                    + encoder.input_frames.saturating_sub(encoder.source_end))
                    * 48_000
                    / u64::from(self.rate),
                Ordering::Relaxed,
            );
            return self.discard();
        }
        let result = io.prepare_audio(
            &self.converter.retained_output(self.pcm_used)[self.pcm_position..],
            &mut self.encoded,
            flush,
        )?;
        if result.status == 2
            || (result.consumed_frames == 0 && result.encoded_bytes == 0 && !flush)
        {
            return Err(Error::Translation);
        }
        self.pcm_position += result.consumed_frames;
        let encoder = self.encoder.as_mut().expect("encoder origin");
        encoder.input_frames = encoder
            .input_frames
            .saturating_add(result.consumed_frames as u64);
        if result.encoded_bytes == 0 {
            encoder.drained = flush && result.status == 0;
            return Ok(());
        }
        let origin = encoder.origin_ns
            + u128::from(encoder.wire_frames) * 1_000_000_000 / u128::from(self.rate);
        let born = encoder.born_ns;
        let expires = born.saturating_add(AGE_NS);
        let stream = encoder.stream;
        encoder.wire_frames = encoder
            .wire_frames
            .saturating_add(u64::from(result.packet_frames));
        let source_end = result
            .source_start_frames
            .saturating_add(u64::from(result.source_frames));
        encoder.born_ns = encoder.born_ns.saturating_add(
            (u128::from(source_end.saturating_sub(encoder.source_end)) * 1_000_000_000
                / u128::from(self.rate)) as u64,
        );
        encoder.source_end = source_end;
        let anchor = self.anchor.get_or_insert_with(|| Anchor {
            stream,
            source_ns: origin,
            call_ms: io
                .elapsed_ms()
                .wrapping_sub((now.saturating_sub(born) / 1_000_000) as u32),
        });
        if anchor.stream != stream {
            *anchor = Anchor {
                stream,
                source_ns: origin,
                call_ms: io
                    .elapsed_ms()
                    .wrapping_sub((now.saturating_sub(born) / 1_000_000) as u32),
            };
        }
        let delta_ms = (origin as i128 - anchor.source_ns as i128).div_euclid(1_000_000);
        let timestamp = anchor.call_ms.wrapping_add(delta_ms as u32);
        let end_origin = encoder.origin_ns
            + u128::from(encoder.wire_frames) * 1_000_000_000 / u128::from(self.rate);
        let end_timestamp = anchor.call_ms.wrapping_add(
            (end_origin as i128 - anchor.source_ns as i128).div_euclid(1_000_000) as u32,
        );
        self.pending = Some(Pending {
            bytes: result.encoded_bytes,
            frames: result.packet_frames,
            source_frames: result.source_frames,
            timestamp,
            end_timestamp,
            expires,
            stream,
        });
        Ok(())
    }
    fn send(&mut self, io: &mut PeerIo, now: u64) -> Result<(), Error> {
        if let Some(pending) = &self.pending {
            if io.send_prepared(
                &self.encoded[..pending.bytes],
                pending.frames,
                pending.timestamp,
            )? {
                let age = now.saturating_sub(pending.expires.saturating_sub(AGE_NS));
                self.statistics.ages[(age / 1_000_000).min(200) as usize]
                    .fetch_add(1, Ordering::Relaxed);
                self.statistics.accepted.fetch_add(1, Ordering::Relaxed);
                self.idle_timestamp = pending.end_timestamp;
                let duration_ns = u64::from(pending.frames) * 1_000_000_000 / u64::from(self.rate);
                self.pending = None;
                self.sent = true;
                self.unkey_ns = Some(now.saturating_add(duration_ns));
            } else {
                self.statistics.blocked.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
    fn pcm_step(&mut self, io: &mut PeerIo, now_ms: u64) -> Result<(), Error> {
        if now_ms < self.next_audio_ms {
            return Ok(());
        }
        let real = BLOCK - self.queue.read(&mut self.native);
        self.next_audio_ms = if real == 0 || !self.sent {
            now_ms
        } else {
            self.next_audio_ms
        }
        .saturating_add(20);
        if real != 0 {
            let audio = self
                .converter
                .process(&self.native[..real])
                .map_err(|_| Error::Translation)?;
            if !audio.is_empty() {
                io.write(audio)?;
                self.sent = true;
            }
        } else if self.sent {
            let (tail, finished) = self.converter.finish().map_err(|_| Error::Translation)?;
            if !tail.is_empty() {
                io.write(tail)?;
            } else if finished {
                io.write(&[])?;
                self.sent = false;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::session::tests::peer_with_egress;
    use rpt_advanced_core::audio::LinkAudioQueue;

    #[test]
    fn finite_drain_can_need_more_than_one_bounded_converter_turn() {
        let (mut io, _state) = peer_with_egress(8000, true, true);
        let (mut input, output) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
        let mut outbound = Outbound::new(8000, output).unwrap();
        outbound.converter = Egress::new(8000, 1).unwrap();
        input.write(
            AudioSpan {
                born_ns: Some(0),
                ..AudioSpan::default()
            },
            &[0.25],
        );
        outbound.step(&mut io, 0).unwrap();
        assert_eq!(outbound.step(&mut io, 20).unwrap(), Some(BLOCK_NS));
        assert!(!outbound.conversion.as_ref().unwrap().drained);

        // A previously keyed PCM host can retain its idle marker across a
        // discontinuity while a new short source is still priming the FIR.
        let (mut io, _state) = peer_with_egress(8000, false, false);
        let (_, output) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
        let mut outbound = Outbound::new(8000, output).unwrap();
        outbound.converter = Egress::new(8000, 1).unwrap();
        assert!(outbound.converter.process(&[0.25]).unwrap().is_empty());
        outbound.sent = true;
        outbound.pcm_step(&mut io, 20).unwrap();
        assert!(outbound.sent, "unfinished drain must not publish unkey");
    }
}
