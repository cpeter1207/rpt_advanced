//! SIP's thin RTP/codec/jitter translation into the existing product source callbacks.
use super::media::{
    AudioCodec, AudioParameters, DecodeProvenance, MediaDescription, MediaError, RtpEndpoint,
    RtpEvent, SendStatus, TimedAudio,
};
use crate::abi;
use crate::phone::media::TelephoneEvent;
use rptadv_codec_client::{CodecCatalog, DecodeSpan};
use rptadv_jitter_client::{Admission, Jitter, Kind, Observation};
use std::{mem::size_of, net::SocketAddr, sync::Arc};

#[derive(Clone, Copy)]
enum Pending {
    Audio {
        start: u64,
        count: usize,
        born: u64,
        prefix: u32,
        pcm_start: usize,
        // Retain encoded packet until its missing prefix has finished using PCM scratch.
        decode: Option<(usize, usize)>,
    },
    Gap {
        start: u64,
        count: u32,
    },
    Digit(u8),
}

/// Cumulative diagnostic projection; occupancy and delay are never cause authority.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Counters {
    duplicate_drops: u64,
    late_drops: u64,
    full_drops: u64,
    missing_span: u64,
    insertion_span: u64,
    skipped_span: u64,
    limit_resets: u64,
    overload_drops: u64,
    overload_span: u64,
}
impl Counters {
    fn read(jitter: &Jitter) -> Option<Self> {
        let o = jitter.observe().ok()?;
        let values = [
            o.duplicate_drops,
            o.late_drops,
            o.full_drops,
            o.missing_span,
            o.insertion_span,
            o.skipped_span,
            o.limit_resets,
            o.overload_drops,
            o.overload_span,
        ];
        if values.contains(&u64::MAX) {
            return None;
        }
        Some(Self {
            duplicate_drops: o.duplicate_drops,
            late_drops: o.late_drops,
            full_drops: o.full_drops,
            missing_span: o.missing_span,
            insertion_span: o.insertion_span,
            skipped_span: o.skipped_span,
            limit_resets: o.limit_resets,
            overload_drops: o.overload_drops,
            overload_span: o.overload_span,
        })
    }
}
/// One completed Packet/advance proof, consumed by its exact successor only.
#[derive(Clone, Copy)]
struct AdaptationWitness {
    start: u64,
    count: u32,
    counters: Counters,
}

/// The old digit tail remains readable without the optional adaptation field.
const DIGIT_TAIL_SIZE: usize = std::mem::offset_of!(abi::rptadv_peer_source_sink_v1, digit)
    + size_of::<abi::rptadv_peer_source_digit_v1>();
const ADAPTATION_TAIL_SIZE: usize =
    std::mem::offset_of!(abi::rptadv_peer_source_sink_v1, source_adaptation)
        + size_of::<abi::rptadv_peer_source_adaptation_v1>();

/// Slice only the current validated origin span; offsets never relabel retained PCM.
fn retained_span(
    spans: &[DecodeSpan],
    offset: usize,
) -> Result<(usize, DecodeProvenance), MediaError> {
    spans
        .iter()
        .find_map(|span| {
            let start = span.offset_frames as usize;
            let end = start + span.frame_count as usize;
            (start <= offset && offset < end).then_some((
                end.saturating_sub(offset),
                if span.origin == 0 {
                    DecodeProvenance::Original
                } else {
                    DecodeProvenance::Concealed
                },
            ))
        })
        .ok_or(MediaError::Codec)
}

/// A single offered range, expressed on both clocks. Each accepted frame moves both.
#[derive(Clone, Copy)]
struct CreditRange {
    wire: u64,
    emitted: u64,
}

/// One bounded missing unit, prepared once and retained through exact sink credit.
struct Loss {
    count: usize,
    offset: usize,
    born: u64,
    provenance: DecodeProvenance,
}

/// One pre-answer socket/offer, never an incoming receipt or a usable media handle.
pub(crate) struct Offer {
    endpoint: RtpEndpoint,
    description: Vec<u8>,
    catalog: Arc<CodecCatalog>,
}
impl Offer {
    pub(crate) async fn new(
        route: SocketAddr,
        session: u64,
        catalog: Arc<CodecCatalog>,
    ) -> Result<Self, MediaError> {
        let media = MediaDescription {
            remote: route,
            codec: super::media::AudioProfile::Mulaw,
            receive_payload: super::media::AudioProfile::Mulaw.payload(),
            send_payload: super::media::AudioProfile::Mulaw.payload(),
            telephone_events: [Some(TelephoneEvent::symmetric(101, 8000)), None],
        };
        let local = if route.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .expect("literal endpoint");
        let endpoint = RtpEndpoint::bind(local, media, session as u32, 0).await?;
        let description = MediaDescription::offer(endpoint.local_addr()?, session)?;
        Ok(Self {
            endpoint,
            description,
            catalog,
        })
    }
    pub(crate) fn description(&self) -> &[u8] {
        &self.description
    }
    pub(crate) async fn accept(
        self,
        answer: &[u8],
        jitter: &'static abi::rptadv_jitter_descriptor_v1,
    ) -> Result<Media, MediaError> {
        let description = MediaDescription::parse_answer(answer)?;
        let endpoint = self.endpoint.select_answer(description.clone()).await?;
        Media::from_endpoint(endpoint, description, Vec::new(), jitter, &self.catalog)
    }
}

/// Per-call bounded playout observations, never a network-loss percentage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaObservation {
    pub(crate) jitter: Observation,
    pub(crate) decode_recovery_frames: u64,
    pub(crate) native_plc_frames: u64,
    pub(crate) fallback_frames: u64,
}

/// One serial media owner; product Outbound still owns PCM conversion and pacing.
pub(crate) struct Media {
    endpoint: RtpEndpoint,
    codec: AudioCodec,
    profile: AudioParameters,
    jitter: Jitter,
    decode_recovery_frames: u64,
    native_plc_frames: u64,
    fallback_frames: u64,
    clock: Option<(u32, u64)>,
    births: [Option<(u64, u64)>; 64],
    birth_slot: usize,
    pcm: Box<[f32]>,
    packet: Box<[u8]>,
    pending: Option<Pending>,
    offset: usize,
    loss: Option<Loss>,
    // Original receipt history only; synthetic samples never refresh this lease.
    last_birth: Option<u64>,
    // Native release coordinate (includes any proven omitted packet prefix).
    wire_end: Option<u64>,
    // Next host coordinate: only actual source-sink credit moves this cursor.
    emitted_end: Option<u64>,
    decision_pending: bool,
    completed_packet_end: Option<u64>,
    adaptation_witness: Option<AdaptationWitness>,
    adaptation_cut: Option<(u64, u32)>,
    adaptation_offered: bool,
    // First source failure is terminal even while this object awaits cleanup.
    source_error: Option<MediaError>,
    host_epoch: Option<u64>,
    discontinuity: bool,
    epoch: Option<u64>,
    marker: bool,
    answer: Vec<u8>,
    pub(crate) digit_capabilities: u32,
}
impl Media {
    /// Prepare only the already-selected verified profile on the existing I/O runtime.
    /// The host retains the released jitter descriptor through call destruction.
    pub(crate) async fn new(
        offer: &[u8],
        call: u64,
        jitter: &'static abi::rptadv_jitter_descriptor_v1,
        catalog: &CodecCatalog,
    ) -> Result<Self, MediaError> {
        let description = MediaDescription::parse(offer)?;
        let local: SocketAddr = if description.remote.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .expect("literal endpoint");
        let endpoint = RtpEndpoint::bind(local, description.clone(), call as u32, 0).await?;
        let answer = description.answer(endpoint.local_addr()?, call)?;
        Self::from_endpoint(endpoint, description, answer, jitter, catalog)
    }
    fn from_endpoint(
        endpoint: RtpEndpoint,
        description: MediaDescription,
        answer: Vec<u8>,
        jitter: &'static abi::rptadv_jitter_descriptor_v1,
        catalog: &CodecCatalog,
    ) -> Result<Self, MediaError> {
        let digit_capabilities = description.digit_capabilities();
        let profile = description.codec.parameters();
        let codec = AudioCodec::from_catalog(catalog, description.codec)?;
        let jitter = unsafe {
            Jitter::phone_at_rate(
                std::ptr::from_ref(jitter).cast(),
                profile.pcm_rate_hz,
                profile.packet_frames,
                codec.receive_bytes(),
            )
        }
        .map_err(|_| MediaError::Jitter)?;
        let pcm = vec![0.0; codec.receive_frames() as usize].into_boxed_slice();
        let packet = vec![0; codec.receive_bytes()].into_boxed_slice();
        Ok(Self {
            endpoint,
            codec,
            profile,
            jitter,
            decode_recovery_frames: 0,
            native_plc_frames: 0,
            fallback_frames: 0,
            clock: None,
            births: [None; 64],
            birth_slot: 0,
            pcm,
            packet,
            pending: None,
            offset: 0,
            loss: None,
            last_birth: None,
            wire_end: None,
            emitted_end: None,
            decision_pending: false,
            completed_packet_end: None,
            adaptation_witness: None,
            adaptation_cut: None,
            adaptation_offered: false,
            source_error: None,
            host_epoch: None,
            discontinuity: true,
            epoch: None,
            marker: true,
            answer,
            digit_capabilities,
        })
    }
    /// Snapshot the serial owner's measured counters without advancing playout.
    pub(crate) fn observe(&self) -> Result<MediaObservation, MediaError> {
        Ok(MediaObservation {
            jitter: self.jitter.observe().map_err(|_| MediaError::Jitter)?,
            decode_recovery_frames: self.decode_recovery_frames,
            native_plc_frames: self.native_plc_frames,
            fallback_frames: self.fallback_frames,
        })
    }
    pub(crate) fn pcm_rate(&self) -> u32 {
        self.profile.pcm_rate_hz
    }
    pub(crate) fn packet_bytes(&self) -> usize {
        self.profile.packet_bytes
    }
    pub(crate) fn packet_frames(&self) -> u32 {
        self.profile.packet_frames
    }
    pub(crate) fn answer(&self) -> &[u8] {
        &self.answer
    }
    pub(crate) fn elapsed_ms(&self) -> u32 {
        self.endpoint.elapsed_ms()
    }
    pub(crate) fn prepare(
        &mut self,
        pcm: &[f32],
        bytes: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        if self.epoch.is_none() {
            return -1;
        }
        self.codec.prepare(pcm, bytes, flush, result)
    }
    pub(crate) fn reset(&mut self) -> Result<(), MediaError> {
        self.epoch = None;
        self.codec.reset_encoder()?;
        self.epoch = Some(self.endpoint.retire_audio()?);
        self.marker = true;
        Ok(())
    }
    /// One completed control digit uses negotiated event egress, never caller echo.
    pub(crate) fn send_digit(&mut self, digit: u8) -> i32 {
        self.endpoint.send_digit(digit)
    }
    pub(crate) fn send(
        &mut self,
        bytes: &[u8],
        timestamp_ms: u32,
        deadline_ms: u32,
    ) -> Result<i32, MediaError> {
        self.endpoint.poll_digit()?;
        let epoch = self.epoch.ok_or(MediaError::Retired)?;
        match self.endpoint.try_send_audio(TimedAudio {
            payload: bytes,
            marker: self.marker,
            timestamp: timestamp_ms.wrapping_mul(self.profile.rtp_clock_hz / 1000),
            deadline_ms,
            epoch,
        })? {
            SendStatus::Sent => {
                self.marker = false;
                Ok(0)
            }
            SendStatus::WouldBlock => Ok(1),
            SendStatus::Expired => Ok(2),
        }
    }
    fn timestamp(&mut self, value: u32) -> Result<u64, MediaError> {
        let extended = if let Some((previous, reference)) = self.clock {
            reference
                .checked_add_signed(i64::from(value.wrapping_sub(previous) as i32))
                .ok_or(MediaError::Retired)?
        } else {
            (1u64 << 32) + u64::from(value)
        };
        if self.clock.is_none_or(|(_, previous)| extended > previous) {
            self.clock = Some((value, extended));
        }
        extended
            .checked_mul(u64::from(self.profile.pcm_rate_hz))
            .map(|ticks| ticks / u64::from(self.profile.rtp_clock_hz))
            .ok_or(MediaError::Retired)
    }
    /// The exclusive advance is the only operation allowed to establish cause.
    fn advance_source(&mut self, now: u64) -> Result<(), MediaError> {
        self.adaptation_witness = None;
        let end = self
            .completed_packet_end
            .take()
            .filter(|end| Some(*end) == self.wire_end);
        let before = Counters::read(&self.jitter);
        self.jitter
            .advance(now, 0)
            .map_err(|_| MediaError::Jitter)?;
        if let (Some(start), Some(mut before), Some(after)) =
            (end, before, Counters::read(&self.jitter))
        {
            let count = after
                .skipped_span
                .checked_sub(before.skipped_span)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n != 0);
            before.skipped_span = after.skipped_span;
            if let Some(count) =
                count.filter(|n| before == after && start.checked_add(u64::from(*n)).is_some())
            {
                self.adaptation_witness = Some(AdaptationWitness {
                    start,
                    count,
                    counters: after,
                });
            }
        }
        Ok(())
    }
    /// Accepted puts may clean already-skipped slots, but cannot create a proof.
    fn put_source(
        &mut self,
        timestamp: u64,
        span: u32,
        now: u64,
        length: usize,
    ) -> Result<Admission, MediaError> {
        let witness = self.adaptation_witness.take();
        let before = Counters::read(&self.jitter);
        let admission = self
            .jitter
            .put(timestamp, span, now, &self.packet[..length])
            .map_err(|_| MediaError::Jitter)?;
        if let (Admission::Accepted, Some(mut witness), Some(before), Some(after)) =
            (&admission, witness, before, Counters::read(&self.jitter))
        {
            let mut cleaned = before;
            cleaned.late_drops = after.late_drops;
            if witness.counters == before
                && after.late_drops.checked_sub(before.late_drops).is_some()
                && cleaned == after
            {
                witness.counters = after;
                self.adaptation_witness = Some(witness);
            }
        }
        Ok(admission)
    }
    fn poll(&mut self, now: u64) -> Result<(), MediaError> {
        for _ in 0..8 {
            let codec = &self.codec;
            match self
                .endpoint
                .try_receive(&mut self.packet, &|packet| codec.inspect(packet))
            {
                Ok(None) => break,
                Ok(Some((RtpEvent::None | RtpEvent::Digit { ended: false, .. }, _))) => {}
                Ok(Some((RtpEvent::Digit { digit, ended: true }, _))) => {
                    self.pending = Some(Pending::Digit(digit));
                    return Ok(());
                }
                Ok(Some((
                    RtpEvent::Audio {
                        timestamp,
                        span,
                        length,
                        ..
                    },
                    receipt,
                ))) => {
                    let timestamp = self.timestamp(timestamp)?;
                    if self.put_source(timestamp, span, now, length)? == Admission::Accepted {
                        // The sink reference was captured before this elapsed
                        // observation, so subtraction is conservatively older.
                        // Missing/overflowed mapping takes the existing gap path.
                        self.births[self.birth_slot] = std::time::Instant::now()
                            .checked_duration_since(receipt)
                            .and_then(|age| u64::try_from(age.as_nanos()).ok())
                            .and_then(|age| now.checked_sub(age))
                            .map(|born| (timestamp, born));
                        self.birth_slot = (self.birth_slot + 1) % self.births.len();
                    }
                }
                Err(MediaError::InvalidPacket) => {
                    self.adaptation_witness = None;
                    eprintln!("phone SIP: invalid RTP packet");
                }
                Err(error) => return Err(error),
            }
        }
        if self.clock.is_none() {
            return Ok(());
        }
        let witness = self.adaptation_witness.take();
        let before = Counters::read(&self.jitter);
        let decision = self
            .jitter
            .get(now, self.profile.packet_frames, &mut self.packet)
            .map_err(|_| MediaError::Jitter)?;
        let witness = witness
            .filter(|w| before == Some(w.counters) && Counters::read(&self.jitter) == before);
        self.completed_packet_end = None;
        match decision.kind {
            Kind::NotDue => {
                self.adaptation_witness = witness;
                return Ok(());
            }
            Kind::Packet => {
                // The neutral ABI returns the packet timestamp, not the requested
                // cursor. Positive offsets are missing prefixes; negative ones
                // trim samples that have already been released (as in IAX playout).
                let span = decision.span as usize;
                if decision.length == 0
                    || decision.length > self.packet.len()
                    || span == 0
                    || span > self.pcm.len()
                {
                    return Err(MediaError::JitterShape);
                }
                let skip = u64::from(decision.start_offset.saturating_neg().max(0) as u32).max(
                    self.wire_end
                        .map_or(0, |end| end.saturating_sub(decision.timestamp)),
                );
                if skip >= span as u64 {
                    // An already credited gap may cover a packet returned later
                    // by native adaptation. Never publish that source range twice.
                    return self.advance_source(now);
                }
                let skip = skip as usize;
                let count = span - skip;
                let start = decision
                    .timestamp
                    .checked_add(skip as u64)
                    .ok_or(MediaError::JitterShape)?;
                // Native chronology can include an omitted interval before this suffix.
                let mut prefix = if let Some(end) = self.wire_end {
                    u32::try_from(start - end).map_err(|_| MediaError::JitterShape)?
                } else {
                    decision.start_offset.max(0) as u32
                };
                let born = self
                    .births
                    .iter()
                    .flatten()
                    .find(|(timestamp, _)| *timestamp == decision.timestamp)
                    .map(|(_, born)| *born);
                let proven = witness.filter(|w| {
                    !self.discontinuity
                        && born.is_some()
                        && prefix == w.count
                        && self.wire_end == Some(w.start)
                        && w.start.checked_add(u64::from(w.count)) == Some(start)
                });
                if born.is_none() || prefix > decision.start_offset.max(0) as u32 {
                    // Collapse only a fully witnessed in-packet trim with admitted
                    // original history. Bind that decision into the retained packet:
                    // no loss prefix, but decode the whole packet and keep its suffix.
                    if proven.is_some_and(|w| {
                        w.start == decision.timestamp
                            && i64::from(decision.start_offset) == -i64::from(w.count)
                    }) && self.last_birth.is_some()
                        && self.codec.inspect(&self.packet[..decision.length]) == Ok(decision.span)
                    {
                        self.wire_end = Some(start);
                        prefix = 0;
                    } else {
                        // Full skips retain the existing hard-cut authority, but
                        // the callback and its counted gap live on the host axis.
                        self.adaptation_cut = proven
                            .filter(|_| skip == 0)
                            .and_then(|w| self.emitted_end.map(|start| (start, w.count)));
                        self.discontinuity = true;
                        self.last_birth = None;
                    }
                }
                if prefix == 0 && skip == 0 && born.is_some() {
                    self.completed_packet_end = start.checked_add(count as u64);
                }
                if let Some(born) = born {
                    self.pending = Some(Pending::Audio {
                        start,
                        count,
                        born,
                        prefix,
                        pcm_start: 0,
                        decode: Some((decision.length, skip)),
                    });
                } else {
                    self.pending = Some(Pending::Gap {
                        start: start
                            .checked_sub(u64::from(prefix))
                            .ok_or(MediaError::JitterShape)?,
                        count: (count as u32)
                            .checked_add(prefix)
                            .ok_or(MediaError::JitterShape)?,
                    });
                }
            }
            Kind::Missing => {
                let end = decision
                    .timestamp
                    .checked_add(u64::from(decision.span))
                    .ok_or(MediaError::JitterShape)?;
                let start = self.wire_end.unwrap_or(decision.timestamp);
                if start < decision.timestamp {
                    // The missing decision does not prove an earlier adaptation skip.
                    self.discontinuity = true;
                    self.adaptation_cut = None;
                    self.last_birth = None;
                }
                if start >= end {
                    return self.advance_source(now);
                }
                let count = u32::try_from(end - start).map_err(|_| MediaError::JitterShape)?;
                self.pending = Some(Pending::Gap { start, count });
            }
            Kind::Insertion => {
                // Only playout time advances. The existing source/ring must not
                // see fictitious missing media or have its source cursor moved.
                return self.advance_source(now);
            }
        }
        // Keep one provider decision leased until all exact source credit lands.
        // No new RTP put/get occurs while its prefix or decoded suffix is retained.
        self.decision_pending = true;
        Ok(())
    }
    /// Validate both complete ranges before offering a single frame. Only the
    /// first range takes its host origin from the wire; no cut can rebase it.
    fn credit_range(
        &self,
        start: u64,
        offset: usize,
        count: usize,
    ) -> Result<CreditRange, MediaError> {
        let wire = start
            .checked_add(offset as u64)
            .ok_or(MediaError::JitterShape)?;
        let emitted = self.emitted_end.unwrap_or(wire);
        wire.checked_add(count as u64)
            .ok_or(MediaError::JitterShape)?;
        emitted
            .checked_add(count as u64)
            .ok_or(MediaError::SourceContract)?;
        Ok(CreditRange { wire, emitted })
    }

    /// Called only with exact sink credit within a previously checked range.
    fn credit_range_prefix(&mut self, range: CreditRange, accepted: usize) {
        self.wire_end = Some(range.wire + accepted as u64);
        self.emitted_end = Some(range.emitted + accepted as u64);
    }

    /// Consume one retained loss unit. No original history means a hard counted
    /// span, never native synthesis with a fabricated current-time receipt.
    fn read_loss(
        &mut self,
        sink: &abi::rptadv_peer_source_sink_v1,
        start: u64,
        remaining: usize,
    ) -> Result<usize, MediaError> {
        let Some(born) = self.last_birth else {
            let accepted =
                unsafe { sink.gap.unwrap()(sink.context, start, remaining as u32) } as usize;
            return (accepted <= remaining)
                .then_some(accepted)
                .ok_or(MediaError::SourceContract);
        };
        if self.loss.is_none() {
            let count = remaining.min(self.pcm.len());
            // The released provider decides native-unit support, including partial
            // negotiated packets. Unsupported returns Missing without advancing PLC.
            let provenance = self.codec.conceal(count as u32, &mut self.pcm[..count])?;
            match provenance {
                DecodeProvenance::Concealed => {
                    self.native_plc_frames = self.native_plc_frames.saturating_add(count as u64)
                }
                DecodeProvenance::Missing => {
                    self.fallback_frames = self.fallback_frames.saturating_add(count as u64)
                }
                DecodeProvenance::Original => return Err(MediaError::Codec),
            }
            self.loss = Some(Loss {
                count,
                offset: 0,
                born,
                provenance,
            });
        }
        let loss = self.loss.as_mut().expect("prepared missing unit");
        let count = loss.count - loss.offset;
        let accepted = if loss.provenance == DecodeProvenance::Concealed {
            let birth = abi::rptadv_source_birth_v1 {
                struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
                abi_version: 1,
                host_clock_epoch: sink.host_clock_epoch,
                born_ns: loss.born,
            };
            unsafe {
                sink.audio_timed.unwrap()(
                    sink.context,
                    start,
                    self.pcm[loss.offset..loss.count].as_ptr(),
                    count,
                    1,
                    &birth,
                )
            }
        } else {
            unsafe { sink.gap.unwrap()(sink.context, start, count as u32) as usize }
        };
        if accepted > count {
            return Err(MediaError::SourceContract);
        }
        loss.offset += accepted;
        if loss.offset == loss.count {
            self.loss = None;
        }
        Ok(accepted)
    }
    /// Preserve one refused PCM/digit/gap while the reactor independently services SIP.
    pub(crate) unsafe fn read(
        &mut self,
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        output: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), MediaError> {
        output.progress = 0;
        output.activity = 0;
        output.finish_pending = 0;
        output.state = 3;
        output.represented_node_number = 0;
        if let Some(error) = self.source_error {
            return Err(error);
        }
        output.state = 1;
        let result = unsafe { self.read_checked(completion, sink, output) };
        if let Err(error) = result {
            // An offered/refused cause is just as terminal as an unoffered
            // witness. Never redispatch it or substitute a generic reset.
            self.source_error = Some(error);
            self.adaptation_witness = None;
            self.completed_packet_end = None;
            self.adaptation_cut = None;
            self.adaptation_offered = false;
            output.progress = 0;
            output.state = 3;
        }
        result
    }

    /// Validate and consume one source turn; the outer boundary revokes on error.
    unsafe fn read_checked(
        &mut self,
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        output: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), MediaError> {
        if completion != 0 {
            return Err(MediaError::SourceContract);
        }
        // Call control continues even while the inbound source retains credit.
        self.endpoint.poll_digit()?;
        if sink.is_null() {
            return Ok(());
        }
        let size = unsafe { std::ptr::addr_of!((*sink).struct_size).read() } as usize;
        if size < DIGIT_TAIL_SIZE {
            return Err(MediaError::SourceContract);
        }
        // Copy only a checked readable prefix, never borrow an enlarged object
        // over an old allocation. A partial optional pointer is never copied.
        let mut copied: abi::rptadv_peer_source_sink_v1 = unsafe { std::mem::zeroed() };
        let readable = if size >= ADAPTATION_TAIL_SIZE {
            ADAPTATION_TAIL_SIZE
        } else {
            DIGIT_TAIL_SIZE
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                sink.cast::<u8>(),
                std::ptr::from_mut(&mut copied).cast::<u8>(),
                readable,
            )
        };
        let sink = &copied;
        if sink.abi_version != 1 {
            return Err(MediaError::SourceContract);
        }
        if sink.host_clock_epoch == 0
            || sink.audio_timed.is_none()
            || sink.gap.is_none()
            || sink.digit.is_none()
            || sink.source_discontinuity.is_none()
            || self
                .host_epoch
                .is_some_and(|epoch| epoch != sink.host_clock_epoch)
        {
            return Err(MediaError::SourceContract);
        }
        self.host_epoch = Some(sink.host_clock_epoch);
        if self.pending.is_none() {
            self.offset = 0;
            self.poll(sink.reference_now_ns)?;
        }
        if self.discontinuity {
            let result = match (self.adaptation_cut, sink.source_adaptation) {
                (Some((start, count)), Some(adaptation)) => {
                    self.adaptation_offered = true;
                    unsafe { adaptation(sink.context, start, count) }
                }
                (_, None) if self.adaptation_offered => return Err(MediaError::SourceContract),
                _ => {
                    // A legacy first offer establishes generic custody permanently;
                    // adding a tail on a retry cannot upgrade its cause.
                    self.adaptation_cut = None;
                    unsafe { sink.source_discontinuity.unwrap()(sink.context) }
                }
            };
            // Test diagnostics correlate custody cuts with the real SIP courtesy window.
            #[cfg(test)]
            eprintln!(
                "vox-courtesy: continuity unix_ms={} source_now_ns={} adaptation={:?} result={result} jitter={:?}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
                sink.reference_now_ns,
                self.adaptation_cut,
                self.jitter.observe(),
            );
            match result {
                0 => {
                    self.discontinuity = false;
                    self.adaptation_cut = None;
                    self.adaptation_offered = false;
                }
                1 => {
                    output.progress = 2;
                    return Ok(());
                }
                _ => return Err(MediaError::SourceContract),
            }
        }
        if let Some(Pending::Audio { start, prefix, .. }) = &self.pending {
            let (start, prefix) = (*start, *prefix);
            if prefix != 0 {
                let range = self.credit_range(
                    start
                        .checked_sub(u64::from(prefix))
                        .ok_or(MediaError::JitterShape)?,
                    0,
                    prefix as usize,
                )?;
                let accepted = self.read_loss(sink, range.emitted, prefix as usize)?;
                let Some(Pending::Audio { prefix, .. }) = self.pending.as_mut() else {
                    unreachable!()
                };
                *prefix -= accepted as u32;
                output.progress = if *prefix == 0 { 1 } else { 2 };
                self.credit_range_prefix(range, accepted);
                return Ok(());
            }
        }
        if let Some(Pending::Audio {
            count,
            decode,
            pcm_start,
            ..
        }) = self.pending.as_mut()
        {
            if let Some((length, skip)) = decode.take() {
                let frames = *count + skip;
                self.codec
                    .decode(&self.packet[..length], &mut self.pcm[..frames])?;
                *pcm_start = skip;
                let recovered = self
                    .codec
                    .decoded_spans()
                    .iter()
                    .filter(|span| span.origin == 1)
                    .map(|span| u64::from(span.frame_count))
                    .sum::<u64>();
                self.decode_recovery_frames = self.decode_recovery_frames.saturating_add(recovered);
                if self.last_birth.is_none() {
                    self.fallback_frames = self
                        .fallback_frames
                        .saturating_add(recovered.saturating_sub(skip as u64));
                }
                // PCM and provider offsets stay intact. Only source credit moves
                // the retained cursor, including negative jitter overlap.
            }
        }
        let Some(pending) = self.pending else {
            return Ok(());
        };
        let accepted = match pending {
            Pending::Digit(symbol) => {
                match unsafe { sink.digit.unwrap()(sink.context, u32::from(symbol)) } {
                    0 => true,
                    1 => false,
                    _ => return Err(MediaError::SourceContract),
                }
            }
            Pending::Audio {
                start,
                count,
                born,
                pcm_start,
                ..
            } => {
                if born > sink.reference_now_ns {
                    return Err(MediaError::SourceContract);
                }
                let pcm_offset = pcm_start + self.offset;
                let (span_remaining, provenance) =
                    retained_span(self.codec.decoded_spans(), pcm_offset)?;
                let remaining = (count - self.offset).min(span_remaining);
                // Recovered audio inherits admitted history, never this corrupt
                // packet's receipt. No history means a hard counted gap.
                let range = self.credit_range(start, self.offset, remaining)?;
                let source_birth = if provenance == DecodeProvenance::Original {
                    Some(born)
                } else {
                    self.last_birth
                };
                let accepted = if let Some(born) = source_birth {
                    let birth = abi::rptadv_source_birth_v1 {
                        struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
                        abi_version: 1,
                        host_clock_epoch: sink.host_clock_epoch,
                        born_ns: born,
                    };
                    unsafe {
                        sink.audio_timed.unwrap()(
                            sink.context,
                            range.emitted,
                            self.pcm[pcm_offset..pcm_offset + remaining].as_ptr(),
                            remaining,
                            u32::from(provenance == DecodeProvenance::Concealed),
                            &birth,
                        )
                    }
                } else {
                    unsafe {
                        sink.gap.unwrap()(sink.context, range.emitted, remaining as u32) as usize
                    }
                };
                if accepted > remaining {
                    return Err(MediaError::SourceContract);
                }
                if accepted != 0 && provenance == DecodeProvenance::Original {
                    self.last_birth = Some(born);
                }
                self.offset += accepted;
                self.credit_range_prefix(range, accepted);
                self.offset == count
            }
            Pending::Gap { start, count } => {
                let remaining = count as usize - self.offset;
                let range = self.credit_range(start, self.offset, remaining)?;
                let accepted = self.read_loss(sink, range.emitted, remaining)?;
                self.offset += accepted;
                self.credit_range_prefix(range, accepted);
                self.offset == count as usize
            }
        };
        output.progress = if accepted { 1 } else { 2 };
        if accepted {
            if self.decision_pending {
                self.advance_source(sink.reference_now_ns)?;
                self.decision_pending = false;
            }
            self.pending = None;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "call_media_tests.rs"]
mod tests;
