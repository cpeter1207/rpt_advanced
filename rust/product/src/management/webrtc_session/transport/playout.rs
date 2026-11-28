//! Bounded native station spans handed to the WebRTC codec and RTP owner.

use super::*;
use crate::{
    host::webrtc::Endpoint,
    status_meter::{Measurement, Reader, Span, Tap},
};
use rpt_advanced_core::audio::AudioSpan;
use webrtc::{
    RtpWrite, SeqNo, Ssrc,
    channel::{Control, Coordinate, Reference, TimeBasis},
    opus::{Encoder, PACKET_FRAMES},
};

const FRAMES: usize = PACKET_FRAMES as usize;
const EXPIRY_NS: u64 = 200_000_000;
const FRAME_NS: u64 = 20_000_000;
const CONTROL_BYTES: usize = 4096;

pub(super) struct Playout {
    binding: String,
    observation: Arc<Mutex<WebRtcObservation>>,
    meter: Tap,
    readings: Reader,
    measured: Option<Measurement>,
    encoder: Encoder,
    encoded: Vec<u8>,
    pcm: [f32; FRAMES],
    used: usize,
    basis: Option<AudioSpan>,
    run: Option<Run>,
    sequence: u64,
    rtp_origin: u64,
    first_native_sample: Option<u64>,
    wire_end: u64,
    epochs: Arc<AtomicU64>,
    epoch: u64,
    isolation_epoch: Option<u64>,
    anchor: Option<Control>,
}

struct Run {
    origin: AudioSpan,
    end: u64,
    wire_position: u64,
    first_packet: bool,
    rtp_start: u64,
    last_end_ns: u64,
}

impl Playout {
    pub(super) fn new(
        provider: &OpusProvider,
        key: WorkKey,
        binding: Id,
        epochs: Arc<AtomicU64>,
        observation: Arc<Mutex<WebRtcObservation>>,
    ) -> Result<Self, ()> {
        let seed = key.session.uuid().as_u128();
        let (meter, readings) = Tap::new();
        Ok(Self {
            binding: binding.to_string(),
            observation,
            meter,
            readings,
            measured: None,
            encoder: provider.encoder().map_err(|_| ())?,
            encoded: vec![0; provider.encoder_bytes()],
            pcm: [0.0; FRAMES],
            used: 0,
            basis: None,
            run: None,
            sequence: (seed & 0xffff) as u64,
            rtp_origin: ((seed >> 96) as u64) & 0x7fff_ffff,
            first_native_sample: None,
            wire_end: 0,
            epochs,
            epoch: 0,
            isolation_epoch: None,
            anchor: None,
        })
    }

    pub(super) fn discontinue(&mut self) -> Result<(), ()> {
        self.used = 0;
        self.basis = None;
        self.clear_observation();
        if self.run.take().is_some() {
            self.encoder.reset().map_err(|_| ())?;
        }
        Ok(())
    }

    fn current(endpoint: &Endpoint, span: AudioSpan, now: u64) -> bool {
        span.stream == endpoint.clock.stream()
            && span
                .born_ns
                .is_some_and(|born| born <= now && now - born < EXPIRY_NS)
    }

    pub(super) fn send(
        &mut self,
        endpoint: &mut Endpoint,
        driver: &mut UdpDriver,
        (mid, payload): (Mid, Pt),
    ) -> Result<(), ()> {
        let now = endpoint.clock.now_ns();
        let epoch = endpoint.isolation.epoch();
        if self.isolation_epoch.replace(epoch) != Some(epoch) {
            self.discontinue()?;
        }
        if self
            .basis
            .is_some_and(|span| !Self::current(endpoint, span, now))
        {
            self.discontinue()?;
        }
        for _ in 0..8 {
            let Some(next) = endpoint.egress.peek_span() else {
                self.finish_if_idle(endpoint, driver, (mid, payload), now)?;
                break;
            };
            if next.span.stream == endpoint.clock.stream()
                && next.isolation_epoch == epoch
                && epoch & 1 == 0
                && next.span.born_ns.is_some_and(|born| born > now)
            {
                break;
            }
            if !Self::current(endpoint, next.span, now)
                || next.isolation_epoch != epoch
                || epoch & 1 != 0
            {
                self.discontinue()?;
                endpoint.egress.read_span(&mut self.pcm).ok_or(())?;
                continue;
            }
            if self.basis.is_some_and(|first| {
                first.stream != next.span.stream
                    || first.generation != next.span.generation
                    || first.burst != next.span.burst
                    || first.source_start.checked_add(self.used as u64)
                        != Some(next.span.source_start)
            }) {
                self.discontinue()?;
            }
            self.basis.get_or_insert(next.span);
            let read = endpoint
                .egress
                .read_span(&mut self.pcm[self.used..])
                .ok_or(())?;
            self.used += read.frames;
            if self.used == FRAMES {
                let span = self.basis.take().ok_or(())?;
                self.used = 0;
                if endpoint.admitted() && endpoint.isolation.epoch() == epoch {
                    self.packet(endpoint, driver, (mid, payload), span, FRAMES, false)?;
                }
                break;
            }
        }
        if endpoint.admitted()
            && endpoint.isolation.epoch() == epoch
            && let Some(anchor) = &self.anchor
        {
            let bytes = anchor.encode().ok_or(())?;
            if driver
                .write_channel(
                    ChannelKind::Control,
                    false,
                    &bytes,
                    CONTROL_BYTES,
                    CONTROL_BYTES,
                )
                .map_err(|_| ())?
            {
                self.anchor = None;
            }
        }
        Ok(())
    }

    /// The queue is empty after the ordinary next source turn. Finish the codec's
    /// retained samples without treating a scheduling gap as fresh source time.
    fn finish_if_idle(
        &mut self,
        endpoint: &Endpoint,
        driver: &mut UdpDriver,
        track: (Mid, Pt),
        now: u64,
    ) -> Result<(), ()> {
        let (span, count, last_end) = if let Some(span) = self.basis {
            let Some(last_end) = end_time_ns(span, self.used) else {
                return Ok(());
            };
            (span, self.used, last_end)
        } else if let Some(run) = &self.run {
            (run.origin, 0, run.last_end_ns)
        } else {
            return Ok(());
        };
        if now >= last_end.saturating_add(EXPIRY_NS) {
            self.discontinue()?;
        } else if now >= last_end.saturating_add(FRAME_NS) {
            self.basis = None;
            self.used = 0;
            self.packet(endpoint, driver, track, span, count, true)?;
            // The Opus provider retains less than one frame of lookahead.
            // Finish any tail and the empty completion before accepting new PCM.
            for _ in 0..2 {
                if self.run.is_none() {
                    break;
                }
                self.packet(endpoint, driver, track, span, 0, true)?;
            }
            if self.run.is_some() {
                return Err(());
            }
        }
        Ok(())
    }

    fn packet(
        &mut self,
        endpoint: &Endpoint,
        driver: &mut UdpDriver,
        (mid, payload): (Mid, Pt),
        span: AudioSpan,
        frames: usize,
        flush: bool,
    ) -> Result<(), ()> {
        if frames != 0
            && !self.run.as_ref().is_some_and(|run| {
                run.end == span.source_start
                    && run.origin.stream == span.stream
                    && run.origin.generation == span.generation
                    && run.origin.burst == span.burst
            })
        {
            self.encoder.reset().map_err(|_| ())?;
            self.epoch = self
                .epochs
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |epoch| {
                    epoch.checked_add(1)
                })
                .map_err(|_| ())?
                .checked_add(1)
                .ok_or(())?;
            self.clear_observation();
            let first = *self.first_native_sample.get_or_insert(span.source_start);
            let relative = span.source_start.checked_sub(first).ok_or(())?;
            let rtp_start = self
                .rtp_origin
                .checked_add(relative)
                .ok_or(())?
                .max(self.wire_end);
            self.run = Some(Run {
                origin: span,
                rtp_start,
                end: span.source_start,
                wire_position: 0,
                first_packet: true,
                last_end_ns: span.born_ns.ok_or(())?,
            });
        }
        let run = self.run.as_mut().ok_or(())?;
        let result = self
            .encoder
            .encode(&self.pcm[..frames], &mut self.encoded, flush)
            .map_err(|_| ())?;
        if result.consumed_frames != frames {
            return Err(());
        }
        if frames != 0 {
            run.last_end_ns = end_time_ns(span, frames).ok_or(())?;
            self.meter.observe(
                Span {
                    graph_generation: span.generation,
                    source_generation: span.stream,
                    first_sample: span.source_start,
                    end_ms: run.last_end_ns / 1_000_000,
                },
                &self.pcm[..frames],
            );
            if let Some(reading) = self.readings.latest() {
                self.measured = Some(reading);
            }
            run.end = run.end.checked_add(frames as u64).ok_or(())?;
        }
        if result.encoded_bytes == 0 {
            if flush && frames == 0 {
                self.run = None;
            }
            return Ok(());
        }
        if !endpoint.admitted() || Some(endpoint.isolation.epoch()) != self.isolation_epoch {
            return self.discontinue();
        }
        let timestamp = run.rtp_start.checked_add(run.wire_position).ok_or(())?;
        let packet = RtpWrite::new(
            payload,
            SeqNo::from(self.sequence),
            timestamp as u32,
            Instant::now(),
            self.encoded[..result.encoded_bytes].to_vec(),
        );
        let ssrc = driver.write_rtp(mid, packet).map_err(|_| ())?;
        self.sequence = self.sequence.checked_add(1).ok_or(())?;
        run.wire_position = run
            .wire_position
            .checked_add(u64::from(result.packet_frames))
            .ok_or(())?;
        self.wire_end = timestamp
            .checked_add(u64::from(result.packet_frames))
            .ok_or(())?;
        let covered = run
            .origin
            .source_start
            .checked_add(result.source_start_frames)
            .and_then(|start| start.checked_add(u64::from(result.source_frames)))
            .ok_or(())?;
        let anchor = if run.first_packet
            && !flush
            && result.source_frames != 0
            && result.source_start_frames == 0
        {
            let lead = u64::from(result.packet_frames - result.source_frames);
            let rtp = timestamp.checked_add(lead).ok_or(())?;
            let reference = reference(&self.binding, self.epoch, run.origin, ssrc, rtp);
            self.anchor = Some(Control::Anchor {
                reference: reference.clone(),
            });
            Some(reference)
        } else {
            None
        };
        run.first_packet = false;
        let mut status = self
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if status.graph_generation == span.generation {
            if anchor.is_some() {
                status.anchor = anchor;
            }
            if status.anchor.is_some()
                && let Some(meter) = self.measured
                && meter.graph_generation == span.generation
                && meter.source_generation == span.stream
                && meter.end_sample <= covered
            {
                status.codec_input = Some(meter);
                self.measured = None;
            }
        }
        Ok(())
    }

    fn clear_observation(&mut self) {
        self.measured = None;
        let mut status = self
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        status.anchor = None;
        status.codec_input = None;
    }
}

fn end_time_ns(span: AudioSpan, frames: usize) -> Option<u64> {
    span.born_ns?
        .checked_add((frames as u64) * 1_000_000_000 / 48_000)
}

fn reference(binding: &str, epoch: u64, initial: AudioSpan, ssrc: Ssrc, rtp: u64) -> Reference {
    Reference {
        binding: binding.to_owned(),
        ssrc: *ssrc,
        epoch: Coordinate(epoch),
        source_generation: Coordinate(initial.stream),
        source_sample: Coordinate(initial.source_start),
        rtp_timestamp: Coordinate(rtp),
        time_basis: initial.born_ns.map(|_| TimeBasis::SessionMonotonic),
        time_ns: initial.born_ns.map(Coordinate),
        uncertainty_ns: None,
    }
}
