//! Single-owner media conversion, source-clock timestamps, and bounded retry storage.
use super::{egress::Egress, source_clock::SourceClock};
use crate::{Error, services::PeerIo};
use rpt_advanced_core::audio::{AudioSpan, LinkAudioConsumer};
use std::sync::Arc;

const BLOCK: usize = 960;
const AGE_NS: u64 = 200_000_000;
const BLOCK_NS: u64 = 20_000_000;

struct Pending {
    count: usize,
    timestamp: u32,
    expires: u64,
    stream: u64,
}
struct Conversion {
    span: AudioSpan,
    next: u64,
    output_samples: u64,
    born_ns: u64,
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

/// One prepared packet at most; provider-committed reliable packets belong to IAX.
pub(super) struct Outbound {
    queue: LinkAudioConsumer,
    converter: Egress,
    rate: u32,
    native: [f32; BLOCK],
    encoded: [u8; BLOCK * 2 + 64],
    pending: Option<Pending>,
    conversion: Option<Conversion>,
    anchor: Option<Anchor>,
    clock: Option<Arc<SourceClock>>,
    next_audio_ms: u64,
    unkey_ns: Option<u64>,
    sent: bool,
}
fn source_ns(sample: u64) -> u128 {
    u128::from(sample) * 1_000_000_000 / 48_000
}
impl Outbound {
    pub(super) fn new(rate: u32, queue: LinkAudioConsumer) -> Result<Self, Error> {
        Ok(Self {
            queue,
            converter: Egress::new(rate, BLOCK).map_err(|_| Error::Allocation)?,
            rate,
            native: [0.0; BLOCK],
            encoded: [0; BLOCK * 2 + 64],
            pending: None,
            conversion: None,
            anchor: None,
            clock: None,
            next_audio_ms: 0,
            unkey_ns: None,
            sent: false,
        })
    }
    pub(super) fn bind_clock(&mut self, clock: Arc<SourceClock>) {
        self.clock = Some(clock);
    }
    pub(super) fn redirect(&mut self, queue: LinkAudioConsumer) -> Result<(), Error> {
        self.queue = queue;
        self.discard()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.pending = None;
        self.conversion = None;
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
        if let Some(pending) = &self.pending {
            if now >= pending.expires || !self.current(pending.stream) {
                self.discard()?;
            } else {
                self.send(io, now)?;
            }
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
                self.discard()?;
            } else {
                self.encode(io, read.span, read.frames, now)?;
                self.send(io, now)?;
            }
        }
        if let Some(pending) = &self.pending {
            return Ok(Some(pending.expires));
        }
        let next = self
            .queue
            .peek_span()
            .map(|next| next.span.born_ns.unwrap_or(now));
        if next.is_some_and(|deadline| deadline <= now) {
            return Ok(next);
        }
        if self.sent && self.unkey_ns.is_some_and(|deadline| now >= deadline) {
            io.write(&[])?;
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
            previous.span.stream == span.stream
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
            });
        }
        let conversion = self.conversion.as_mut().expect("conversion origin");
        // Output count, not input partitions, locates samples retained by the FIR.
        let origin = conversion.source_ns(self.rate);
        let born = conversion.born_ns;
        conversion.next = span.source_start.wrapping_add(count as u64);
        let audio = self
            .converter
            .process(&self.native[..count])
            .map_err(|_| Error::Translation)?;
        conversion.output_samples = conversion.output_samples.saturating_add(audio.len() as u64);
        let next_origin = conversion.source_ns(self.rate);
        // Retained FIR output keeps its prior birth. Only output reaching this
        // input span may adopt this span's new clock mapping.
        conversion.born_ns = if next_origin >= source_ns(span.source_start) {
            span.born_ns
                .expect("validated birth")
                .saturating_add((next_origin - source_ns(span.source_start)) as u64)
        } else {
            born.saturating_add((next_origin - origin) as u64)
        };
        if audio.is_empty() {
            return Ok(());
        }
        let expires = born.saturating_add(AGE_NS);
        let anchor = self.anchor.get_or_insert_with(|| Anchor {
            stream: span.stream,
            source_ns: origin,
            call_ms: io
                .elapsed_ms()
                .wrapping_sub((now.saturating_sub(born) / 1_000_000) as u32),
        });
        if anchor.stream != span.stream {
            *anchor = Anchor {
                stream: span.stream,
                source_ns: origin,
                call_ms: io
                    .elapsed_ms()
                    .wrapping_sub((now.saturating_sub(born) / 1_000_000) as u32),
            };
        }
        let delta_ms = (origin as i128 - anchor.source_ns as i128).div_euclid(1_000_000);
        let timestamp = anchor.call_ms.wrapping_add(delta_ms as u32);
        io.prepare_audio(audio, &mut self.encoded)?;
        self.pending = Some(Pending {
            count: audio.len(),
            timestamp,
            expires,
            stream: span.stream,
        });
        Ok(())
    }
    fn send(&mut self, io: &mut PeerIo, now: u64) -> Result<(), Error> {
        if let Some(pending) = &self.pending {
            if io.send_prepared(&self.encoded[..pending.count], pending.timestamp)? {
                self.pending = None;
                self.sent = true;
                self.unkey_ns = Some(now.saturating_add(BLOCK_NS));
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
            io.write(&[])?;
            self.sent = false;
        }
        Ok(())
    }
}
