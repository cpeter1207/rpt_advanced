//! SIP's thin RTP/codec/jitter translation into the existing product source callbacks.
use super::media::{
    AudioCodec, AudioParameters, DecodeProvenance, MediaDescription, MediaError, RtpEndpoint,
    RtpEvent, SendStatus, TimedAudio,
};
use crate::abi;
use crate::phone::media::TelephoneEvent;
use rptadv_codec_client::CodecCatalog;
use rptadv_jitter_client::{Admission, Jitter, Kind, Observation};
use std::{mem::size_of, net::SocketAddr, sync::Arc};

#[derive(Clone, Copy)]
enum Pending {
    Audio {
        start: u64,
        count: usize,
        born: u64,
        prefix: u32,
        provenance: DecodeProvenance,
        // Retain encoded packet until its missing prefix has finished using PCM scratch.
        decode: Option<(usize, usize)>,
    },
    Gap {
        start: u64,
        count: u32,
    },
    Digit(u8),
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
    // End of source media actually credited by product, independent of jitter adaptation.
    source_end: Option<u64>,
    decision_pending: bool,
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
            source_end: None,
            decision_pending: false,
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
                    if self
                        .jitter
                        .put(timestamp, span, now, &self.packet[..length])
                        .map_err(|_| MediaError::Jitter)?
                        == Admission::Accepted
                    {
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
                Err(MediaError::InvalidPacket) => eprintln!("phone SIP: invalid RTP packet"),
                Err(error) => return Err(error),
            }
        }
        if self.clock.is_none() {
            return Ok(());
        }
        let decision = self
            .jitter
            .get(now, self.profile.packet_frames, &mut self.packet)
            .map_err(|_| MediaError::Jitter)?;
        match decision.kind {
            Kind::NotDue => return Ok(()),
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
                    self.source_end
                        .map_or(0, |end| end.saturating_sub(decision.timestamp)),
                );
                if skip >= span as u64 {
                    // An already credited gap may cover a packet returned later
                    // by native adaptation. Never publish that source range twice.
                    return self.jitter.advance(now, 0).map_err(|_| MediaError::Jitter);
                }
                let skip = skip as usize;
                let count = span - skip;
                let start = decision
                    .timestamp
                    .checked_add(skip as u64)
                    .ok_or(MediaError::JitterShape)?;
                // Delay reduction may advance jitter's cursor before its next
                // packet offset is calculated. Expose that skipped source span
                // through the existing retained gap before offering any PCM.
                let prefix = if let Some(end) = self.source_end {
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
                // An adaptation skip is not a proven lost packet. Neither it nor
                // unknown receipt timing may acquire the timed-loss lease.
                if born.is_none() || prefix > decision.start_offset.max(0) as u32 {
                    self.discontinuity = true;
                    self.last_birth = None;
                }
                if let Some(born) = born {
                    self.pending = Some(Pending::Audio {
                        start,
                        count,
                        born,
                        prefix,
                        provenance: DecodeProvenance::Original,
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
                let start = self.source_end.unwrap_or(decision.timestamp);
                if start < decision.timestamp {
                    // The missing decision does not prove an earlier adaptation skip.
                    self.discontinuity = true;
                    self.last_birth = None;
                }
                if start >= end {
                    return self.jitter.advance(now, 0).map_err(|_| MediaError::Jitter);
                }
                let count = u32::try_from(end - start).map_err(|_| MediaError::JitterShape)?;
                self.pending = Some(Pending::Gap { start, count });
            }
            Kind::Insertion => {
                // Only playout time advances. The existing source/ring must not
                // see fictitious missing media or have its source cursor moved.
                return self.jitter.advance(now, 0).map_err(|_| MediaError::Jitter);
            }
        }
        // Keep one provider decision leased until all exact source credit lands.
        // No new RTP put/get occurs while its prefix or decoded suffix is retained.
        self.decision_pending = true;
        Ok(())
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
        output.state = 1;
        output.represented_node_number = 0;
        if completion != 0 {
            return Err(MediaError::SourceContract);
        }
        // Call control continues even while the inbound source retains credit.
        self.endpoint.poll_digit()?;
        if sink.is_null() {
            return Ok(());
        }
        if unsafe { (*sink).struct_size } < size_of::<abi::rptadv_peer_source_sink_v1>() as u32
            || unsafe { (*sink).abi_version } != 1
        {
            return Err(MediaError::SourceContract);
        }
        let sink = unsafe { &*sink };
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
            match unsafe { sink.source_discontinuity.unwrap()(sink.context) } {
                0 => self.discontinuity = false,
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
                let accepted = self.read_loss(sink, start - u64::from(prefix), prefix as usize)?;
                let Some(Pending::Audio { prefix, .. }) = self.pending.as_mut() else {
                    unreachable!()
                };
                *prefix -= accepted as u32;
                self.source_end = Some(start - u64::from(*prefix));
                output.progress = if *prefix == 0 { 1 } else { 2 };
                return Ok(());
            }
        }
        if let Some(Pending::Audio {
            count,
            decode,
            provenance,
            ..
        }) = self.pending.as_mut()
        {
            if let Some((length, skip)) = decode.take() {
                let span = *count + skip;
                *provenance = self
                    .codec
                    .decode(&self.packet[..length], &mut self.pcm[..span])?;
                if *provenance == DecodeProvenance::Concealed {
                    self.decode_recovery_frames =
                        self.decode_recovery_frames.saturating_add(span as u64);
                }
                if *provenance == DecodeProvenance::Missing {
                    self.fallback_frames = self.fallback_frames.saturating_add(*count as u64);
                }
                self.pcm.copy_within(skip..span, 0);
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
                provenance,
                ..
            } => {
                if born > sink.reference_now_ns {
                    return Err(MediaError::SourceContract);
                }
                let birth = abi::rptadv_source_birth_v1 {
                    struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
                    abi_version: 1,
                    host_clock_epoch: sink.host_clock_epoch,
                    born_ns: born,
                };
                let remaining = count - self.offset;
                let accepted = if provenance == DecodeProvenance::Missing {
                    unsafe {
                        sink.gap.unwrap()(
                            sink.context,
                            start + self.offset as u64,
                            remaining as u32,
                        ) as usize
                    }
                } else {
                    unsafe {
                        sink.audio_timed.unwrap()(
                            sink.context,
                            start + self.offset as u64,
                            self.pcm[self.offset..count].as_ptr(),
                            remaining,
                            u32::from(provenance == DecodeProvenance::Concealed),
                            &birth,
                        )
                    }
                };
                if accepted > remaining {
                    return Err(MediaError::SourceContract);
                }
                if accepted != 0 && provenance == DecodeProvenance::Original {
                    self.last_birth = Some(born);
                }
                self.offset += accepted;
                self.source_end = Some(start + self.offset as u64);
                self.offset == count
            }
            Pending::Gap { start, count } => {
                let remaining = count as usize - self.offset;
                let accepted = self.read_loss(sink, start + self.offset as u64, remaining)?;
                self.offset += accepted;
                self.source_end = Some(start + self.offset as u64);
                self.offset == count as usize
            }
        };
        output.progress = if accepted { 1 } else { 2 };
        if accepted {
            if self.decision_pending {
                self.jitter
                    .advance(sink.reference_now_ns, 0)
                    .map_err(|_| MediaError::Jitter)?;
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
