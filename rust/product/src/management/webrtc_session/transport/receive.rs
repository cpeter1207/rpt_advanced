//! Admit Opus only after the exact station grant and the receiver's timed sample lease.

use super::*;
use crate::{
    host::webrtc::Endpoint,
    status_meter::{Reader, Span, Tap},
};
use rptadv_jitter_client::{Admission, Jitter, JitterProvider, Kind};
use webrtc::{
    Frequency, ReceiveGate, RtpPacket, SenderFeedback, Ssrc,
    channel::{Control, Coordinate, Lease, Reference},
    opus::{CLOCK_HZ, DecodeSpan, OpusDecoder, RECEIVE_FRAMES},
    quality::ReceiverQuality,
    reports::SenderReports,
};

pub(super) const MAX_RTP_PAYLOAD: usize = 65_527;
const KEY_LIFETIME: Duration = Duration::from_millis(TIMING_LIFETIME_MS as u64);
const INITIAL_KEY_WAIT: Duration = Duration::from_millis(500);
const MAX_MEDIA_AGE: Duration = Duration::from_millis(200);
const MAX_RTP_GAP: u64 = CLOCK_HZ as u64 / 5;
const RTP_OBSERVATION_INTERVAL: Duration = Duration::from_millis(250);
const FRAME_ORIGINS: usize = 48;

pub(super) struct Receiver {
    clock: Instant,
    binding: String,
    generation: u64,
    next_grant: u64,
    observation: Arc<Mutex<WebRtcObservation>>,
    keys: Arc<ReceiveStatus>,
    decoded: Tap,
    decoded_reader: Reader,
    reports: SenderReports,
    source: Option<(Ssrc, u64)>,
    talk: Option<Talk>,
    last_timestamp_end: Option<u64>,
    writer_frame: Option<u64>,
    decoder: OpusDecoder,
    jitter: Jitter,
    received: HashMap<u64, Instant>,
    packet: Vec<u8>,
    pcm: Vec<f32>,
    spans: [DecodeSpan; FRAME_ORIGINS],
    waiting: Option<Pending>,
}

struct Talk {
    id: u64,
    start_deadline: Instant,
    observed: bool,
    watermark: Option<u64>,
    gate: ReceiveGate,
    reference: Option<Reference>,
    next_rtp: Option<u64>,
    last_observation: Option<ReportedRtp>,
}

struct ReportedRtp {
    end: u64,
    received_at: Instant,
    reported_at: Instant,
}

struct Pending {
    rtp_start: u64,
    skip: usize,
    end: usize,
    gap: u64,
    spans: usize,
    birth: Instant,
}

impl Receiver {
    pub(super) fn new(
        provider: &OpusProvider,
        jitter: &Arc<JitterProvider>,
        key: WorkKey,
        binding: Id,
        observation: Arc<Mutex<WebRtcObservation>>,
        keys: Arc<ReceiveStatus>,
    ) -> Result<Self, ()> {
        let decoder = provider.decoder(MAX_RTP_PAYLOAD).map_err(|_| ())?;
        if !decoder.supports_provenance() {
            return Err(());
        }
        let (decoded, decoded_reader) = Tap::new();
        Ok(Self {
            clock: Instant::now(),
            binding: binding.to_string(),
            generation: key.ice_generation,
            next_grant: 0,
            observation,
            keys,
            decoded,
            decoded_reader,
            reports: SenderReports::default(),
            source: None,
            talk: None,
            last_timestamp_end: None,
            writer_frame: None,
            decoder,
            jitter: jitter
                .create(CLOCK_HZ, CLOCK_HZ / 400, MAX_RTP_PAYLOAD, [20, 40, 100])
                .map_err(|_| ())?,
            received: HashMap::new(),
            packet: vec![0; MAX_RTP_PAYLOAD],
            pcm: vec![0.0; RECEIVE_FRAMES],
            spans: [DecodeSpan::default(); FRAME_ORIGINS],
            waiting: None,
        })
    }

    /// The old graph retires its accepted prefix. A compatible redirect keeps
    /// the exact connection's clock, codec history and unaccepted suffix.
    pub(super) fn endpoint_changed(&mut self) {
        self.writer_frame = None;
    }

    /// Keep the grant and RTP high-water marks while retiring the old ICE media.
    pub(super) fn restart(&mut self, key: WorkKey, keys: Arc<ReceiveStatus>) -> Result<(), ()> {
        self.generation = key.ice_generation;
        self.keys = keys;
        self.writer_frame = None;
        self.reset()
    }

    fn reset(&mut self) -> Result<(), ()> {
        self.talk = None;
        self.waiting = None;
        self.received.clear();
        self.reports = SenderReports::default();
        self.source = None;
        {
            let mut view = self
                .observation
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            view.sender_rate = None;
            view.decoded = None;
            view.receiver_quality.clear();
        }
        self.jitter.reset().map_err(|_| ())?;
        self.decoder.reset().map_err(|_| ())
    }

    fn release(&mut self, endpoint: &Endpoint) -> Result<(), ()> {
        endpoint.ingress.signals().revoke_webrtc_receive();
        self.keys
            .observe(endpoint.clock.now_ns() / 1_000_000, false);
        self.reset()
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.talk
            .as_ref()
            .map(|talk| talk.gate.deadline().unwrap_or(talk.start_deadline))
    }

    pub(super) fn expire(&mut self, now: Instant, endpoint: &Endpoint) -> Result<(), ()> {
        if self.deadline().is_some_and(|deadline| now >= deadline)
            || self.talk.is_some()
                && (!endpoint.admitted() || endpoint.ingress.signals().rx_muted())
        {
            self.release(endpoint)?;
        }
        Ok(())
    }

    pub(super) fn control(
        &mut self,
        bytes: &[u8],
        endpoint: &Endpoint,
        driver: &mut UdpDriver,
        now: Instant,
    ) -> Result<(), ()> {
        self.expire(now, endpoint)?;
        match Control::decode(bytes) {
            Some(
                ref request @ (Control::RequestTalk { ice_generation }
                | Control::RequestObservedTalk { ice_generation }),
            ) if ice_generation.0 == self.generation
                && self.talk.is_none()
                && endpoint.admitted()
                && !endpoint.ingress.signals().rx_muted() =>
            {
                let grant = self.next_grant.checked_add(1).ok_or(())?;
                let bytes = Control::TalkGranted {
                    grant: Coordinate(grant),
                    epoch: Coordinate(grant),
                }
                .encode()
                .ok_or(())?;
                if driver
                    .write_channel(ChannelKind::Control, false, &bytes, 4096, 4096)
                    .map_err(|_| ())?
                {
                    self.next_grant = grant;
                    self.talk = Some(Talk {
                        id: grant,
                        start_deadline: now + INITIAL_KEY_WAIT,
                        observed: matches!(request, Control::RequestObservedTalk { .. }),
                        watermark: None,
                        gate: ReceiveGate::new(grant, grant, KEY_LIFETIME),
                        reference: None,
                        next_rtp: None,
                        last_observation: None,
                    });
                }
            }
            Some(Control::ReleaseTalk { grant, epoch })
                if grant == epoch && self.talk.as_ref().is_some_and(|talk| talk.id == grant.0) =>
            {
                self.release(endpoint)?;
            }
            Some(Control::Anchor { reference }) => {
                if let Some(talk) = &mut self.talk
                    && !talk.observed
                    && reference.epoch.0 == talk.id
                    && reference.binding == self.binding
                    && reference.uncertainty_ns.is_some()
                {
                    if talk
                        .reference
                        .as_ref()
                        .is_some_and(|current| current != &reference)
                    {
                        self.release(endpoint)?;
                    } else {
                        talk.reference = Some(reference);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn sender_feedback(&mut self, feedback: &SenderFeedback) {
        if let Some((ssrc, rtp)) = self.source
            && feedback.sender_info.ssrc == ssrc
        {
            let rate = self.reports.record(feedback, ssrc, rtp);
            self.observation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .sender_rate = rate;
        }
    }

    pub(super) fn timing(
        &mut self,
        bytes: &[u8],
        endpoint: &Endpoint,
        now: Instant,
    ) -> Result<(), ()> {
        self.expire(now, endpoint)?;
        let Some(message) = Lease::decode(bytes) else {
            if let Some(record) = ReceiverQuality::decode(bytes) {
                self.receiver_quality(record, now);
            }
            return Ok(());
        };
        if let Some(talk) = &mut self.talk
            && talk.reference.is_some()
            && message.end.0.saturating_sub(message.start.0) <= u64::from(CLOCK_HZ)
            && let Some(lease) = message.with_local_expiry(now + KEY_LIFETIME)
        {
            let deadline = endpoint.clock.at_ns(lease.expires_at);
            if talk.gate.update(now, lease) {
                endpoint.ingress.signals().set_webrtc_deadline(deadline);
            }
        }
        Ok(())
    }

    fn receiver_quality(&mut self, record: ReceiverQuality, now: Instant) {
        let Some(talk) = &mut self.talk else { return };
        let Some(reference) = &talk.reference else {
            return;
        };
        if self.source.is_none()
            || record.rate_hz != CLOCK_HZ
            || !record.matches_source(
                &self.binding,
                Coordinate(talk.id),
                reference.source_generation,
                Coordinate(talk.id),
            )
            || !talk.gate.permits(now, Some(record.start.0..record.end.0))
        {
            return;
        }
        let mut view = self
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let quantity = record.measurement.quantity();
        if let Some(previous) = view
            .receiver_quality
            .iter_mut()
            .find(|previous| previous.measurement.quantity() == quantity)
        {
            if record.end.0 > previous.end.0 {
                *previous = record;
            }
        } else {
            view.receiver_quality.push(record);
        }
    }

    pub(super) fn packet(
        &mut self,
        packet: &RtpPacket,
        driver: &mut UdpDriver,
        payload: Pt,
        endpoint: &Endpoint,
        now: Instant,
    ) -> Result<(), ()> {
        self.expire(now, endpoint)?;
        if self.waiting.is_some()
            || packet.header.payload_type != payload
            || packet.time.frequency() != Frequency::FORTY_EIGHT_KHZ
            || !endpoint.admitted()
        {
            return Ok(());
        }
        let Some(talk) = &mut self.talk else {
            return Ok(());
        };
        let Ok(shape) = self.decoder.inspect(&packet.payload) else {
            return Ok(());
        };
        let start = packet.time.numer();
        let Some(end) = start.checked_add(u64::from(shape.frames)) else {
            return self.release(endpoint);
        };
        if start as u32 != packet.header.timestamp {
            return self.release(endpoint);
        }
        if self
            .last_timestamp_end
            .is_some_and(|previous| end <= previous)
            || now.saturating_duration_since(packet.timestamp) >= MAX_MEDIA_AGE
        {
            return Ok(());
        }
        if talk
            .reference
            .as_ref()
            .is_some_and(|reference| reference.ssrc != *packet.header.ssrc)
        {
            return self.release(endpoint);
        }
        if talk.watermark.is_some_and(|cut| start < cut) {
            return Ok(());
        }
        let first_observation = talk.reference.is_none();
        // Keep refreshes on the current audio stream even if the browser's
        // predicted lease window no longer covers the arriving packet.
        if talk.observed
            && (first_observation
                || talk.last_observation.as_ref().is_some_and(|previous| {
                    start >= previous.end
                        && packet.timestamp > previous.received_at
                        && now.duration_since(previous.reported_at) >= RTP_OBSERVATION_INTERVAL
                        && talk
                            .next_rtp
                            .is_some_and(|next| start >= next && start - next < MAX_RTP_GAP)
                }))
        {
            let reference = Reference {
                binding: self.binding.clone(),
                ssrc: *packet.header.ssrc,
                epoch: Coordinate(talk.id),
                source_generation: Coordinate(self.generation),
                source_sample: Coordinate(start),
                rtp_timestamp: Coordinate(start),
                time_basis: None,
                time_ns: None,
                uncertainty_ns: None,
            };
            let observation = Control::ObservedRtp {
                reference: reference.clone(),
                received_at_ns: Coordinate(endpoint.clock.at_ns(packet.timestamp)),
            }
            .encode()
            .ok_or(())?;
            if driver
                .write_channel(ChannelKind::Control, false, &observation, 4096, 4096)
                .map_err(|_| ())?
            {
                talk.last_observation = Some(ReportedRtp {
                    end,
                    received_at: packet.timestamp,
                    reported_at: now,
                });
                if first_observation {
                    talk.reference = Some(reference);
                    talk.watermark = Some(end);
                }
            }
            if first_observation {
                return Ok(());
            }
        }
        let Some(reference) = &talk.reference else {
            return Ok(());
        };
        let uncertainty = if talk.observed {
            // Relative RTP coordinates map to themselves. In this mode the
            // sender capture clock and one-way network delay are unavailable.
            Some(0)
        } else {
            reference.uncertainty_ns.map(|span| {
                (u128::from(span.0) * u128::from(CLOCK_HZ)).div_ceil(1_000_000_000) as u64
            })
        };
        let possible = uncertainty.and_then(|frames| {
            reference
                .positions()
                .possible_source_window(talk.id, start..end, frames)
        });
        if !talk.gate.permits(now, possible) {
            return Ok(());
        }
        if self.source.is_none_or(|(_, previous)| start > previous) {
            self.source = Some((packet.header.ssrc, start));
        }
        self.received
            .retain(|_, birth| now.saturating_duration_since(*birth) < MAX_MEDIA_AGE);
        let arrival = self.ns(packet.timestamp);
        if self
            .jitter
            .put(start, shape.frames, arrival, &packet.payload)
            .map_err(|_| ())?
            == Admission::Accepted
        {
            self.received.insert(start, packet.timestamp);
        }
        Ok(())
    }

    pub(super) fn tick(&mut self, now: Instant, endpoint: &mut Endpoint) -> Result<(), ()> {
        self.expire(now, endpoint)?;
        if !self
            .talk
            .as_mut()
            .is_some_and(|talk| talk.gate.current(now))
        {
            return Ok(());
        }
        let now_ns = self.ns(now);
        for _ in 0..8 {
            if self.waiting.is_none() {
                let decision = self
                    .jitter
                    .get(now_ns, self.jitter.release_span(), &mut self.packet)
                    .map_err(|_| ())?;
                match decision.kind {
                    Kind::NotDue => break,
                    Kind::Insertion => self.jitter.advance(now_ns, 0).map_err(|_| ())?,
                    Kind::Missing => {
                        self.jitter.advance(now_ns, 0).map_err(|_| ())?;
                        // Only real captured input can start or extend the RF source.
                        // The PCM ring owns any playout concealment of this gap.
                    }
                    Kind::Packet => {
                        let Some(birth) = self.received.remove(&decision.timestamp) else {
                            self.jitter.advance(now_ns, 0).map_err(|_| ())?;
                            continue;
                        };
                        if now.saturating_duration_since(birth) >= MAX_MEDIA_AGE {
                            self.jitter.advance(now_ns, 0).map_err(|_| ())?;
                            continue;
                        }
                        let skip = usize::try_from(
                            self.last_timestamp_end
                                .map_or(0, |end| end.saturating_sub(decision.timestamp))
                                .max(u64::from(
                                    decision.start_offset.saturating_neg().max(0) as u32
                                )),
                        )
                        .map_err(|_| ())?;
                        let Ok(result) = self.decoder.decode(
                            &self.packet[..decision.length],
                            &mut self.pcm,
                            &mut self.spans,
                        ) else {
                            return self.release(endpoint);
                        };
                        if result.frames != decision.span as usize {
                            return self.release(endpoint);
                        }
                        let spans = result.span_count.ok_or(())?;
                        let start = decision.timestamp.checked_add(skip as u64).ok_or(())?;
                        let gap = self
                            .talk
                            .as_ref()
                            .and_then(|talk| talk.next_rtp)
                            .map_or(0, |next| start.saturating_sub(next));
                        if gap >= MAX_RTP_GAP {
                            return self.release(endpoint);
                        }
                        self.waiting = Some(Pending {
                            gap,
                            rtp_start: decision.timestamp,
                            skip: skip.min(result.frames),
                            end: result.frames,
                            spans,
                            birth,
                        });
                    }
                }
            }
            if let Some(mut claim) = self.waiting.take() {
                if now.saturating_duration_since(claim.birth) >= MAX_MEDIA_AGE {
                    return self.release(endpoint);
                }
                if claim.gap != 0
                    && let Some(cursor) = self.writer_frame
                {
                    let accepted = endpoint.ingress.gap(claim.gap).map_err(|_| ())?;
                    self.writer_frame = Some(cursor.checked_add(accepted).ok_or(())?);
                    claim.gap -= accepted;
                    if claim.gap != 0 {
                        self.waiting = Some(claim);
                        return Ok(());
                    }
                }
                while claim.skip < claim.end {
                    let origin = self.spans[..claim.spans]
                        .iter()
                        .find(|span| {
                            (span.offset_frames as usize
                                ..(span.offset_frames + span.frame_count) as usize)
                                .contains(&claim.skip)
                        })
                        .ok_or(())?;
                    let end = (origin.offset_frames + origin.frame_count) as usize;
                    let frames = end - claim.skip;
                    let start = self.writer_frame.unwrap_or(0);
                    let accepted = if origin.origin == 0 {
                        let accepted = endpoint
                            .ingress
                            .write_tagged(&self.pcm[claim.skip..end], self.generation, start, &[])
                            .map_err(|_| ())?;
                        if accepted != 0 {
                            endpoint.ingress.signals().set_radio_keyed(true);
                            self.keys
                                .observe(endpoint.clock.at_ns(now) / 1_000_000, true);
                            let mut observed = self
                                .observation
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            self.decoded.observe(
                                Span {
                                    graph_generation: observed.graph_generation,
                                    source_generation: self.generation,
                                    first_sample: claim
                                        .rtp_start
                                        .checked_add(claim.skip as u64)
                                        .ok_or(())?,
                                    end_ms: endpoint.clock.at_ns(claim.birth) / 1_000_000,
                                },
                                &self.pcm[claim.skip..claim.skip + accepted],
                            );
                            if let Some(reading) = self.decoded_reader.latest() {
                                observed.decoded = Some(reading);
                            }
                        }
                        accepted
                    } else if self.writer_frame.is_some() {
                        // A recovered coding unit adds only a counted source gap.
                        endpoint.ingress.gap(frames as u64).map_err(|_| ())? as usize
                    } else {
                        // The initial source anchor belongs to the first original sample.
                        claim.skip = end;
                        continue;
                    };
                    claim.skip += accepted;
                    self.writer_frame = Some(start.checked_add(accepted as u64).ok_or(())?);
                    if accepted < frames {
                        self.waiting = Some(claim);
                        return Ok(());
                    }
                }
                let end = claim.rtp_start.checked_add(claim.end as u64).ok_or(())?;
                let end = self.last_timestamp_end.map_or(end, |last| end.max(last));
                self.last_timestamp_end = Some(end);
                if let Some(talk) = &mut self.talk {
                    talk.next_rtp = Some(end);
                }
                self.jitter.advance(now_ns, 0).map_err(|_| ())?;
            }
        }
        Ok(())
    }

    fn ns(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.clock)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }
}
