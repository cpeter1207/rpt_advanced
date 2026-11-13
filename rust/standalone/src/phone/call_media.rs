//! SIP's thin RTP/codec/jitter translation into the existing product source callbacks.
use super::media::{
    G711Codec, MediaDescription, MediaError, RtpEndpoint, RtpEvent, SendStatus, TimedAudio,
};
use crate::abi;
use rptadv_jitter_client::{Admission, Jitter, Kind};
use std::{mem::size_of, net::SocketAddr};

enum Pending {
    Audio {
        start: u64,
        count: usize,
        born: u64,
        prefix: u32,
    },
    Gap {
        start: u64,
        count: u32,
    },
    Digit(u8),
}

/// One serial media owner; product Outbound still owns PCM conversion and pacing.
pub(crate) struct Media {
    endpoint: RtpEndpoint,
    codec: G711Codec,
    jitter: Jitter,
    clock: Option<(u32, u64)>,
    births: [Option<(u64, u64)>; 64],
    birth_slot: usize,
    pcm: [f32; 160],
    pending: Option<Pending>,
    offset: usize,
    decision_pending: bool,
    host_epoch: Option<u64>,
    discontinuity: bool,
    epoch: Option<u64>,
    marker: bool,
    answer: Vec<u8>,
    pub(crate) inband: bool,
}
impl Media {
    /// Prepare only the already-selected G.711 profile on the existing I/O runtime.
    /// The host retains the released jitter descriptor through call destruction.
    pub(crate) async fn new(
        offer: &[u8],
        call: u64,
        jitter: &'static abi::rptadv_jitter_descriptor_v1,
    ) -> Result<Self, MediaError> {
        let description = MediaDescription::parse(offer)?;
        let local: SocketAddr = if description.remote.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()
        .expect("literal endpoint");
        let inband = description.telephone_event.is_none();
        let endpoint = RtpEndpoint::bind(local, description.clone(), call as u32, 0).await?;
        let answer = description.answer(endpoint.local_addr()?, call)?;
        let codec = G711Codec::load(description.codec)?;
        let jitter = unsafe { Jitter::phone(std::ptr::from_ref(jitter).cast()) }
            .map_err(|_| MediaError::Codec)?;
        Ok(Self {
            endpoint,
            codec,
            jitter,
            clock: None,
            births: [None; 64],
            birth_slot: 0,
            pcm: [0.0; 160],
            pending: None,
            offset: 0,
            decision_pending: false,
            host_epoch: None,
            discontinuity: true,
            epoch: None,
            marker: true,
            answer,
            inband,
        })
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
    pub(crate) fn send(
        &mut self,
        bytes: &[u8; 160],
        timestamp_ms: u32,
        deadline_ms: u32,
    ) -> Result<i32, MediaError> {
        let epoch = self.epoch.ok_or(MediaError::Retired)?;
        match self.endpoint.try_send_audio(TimedAudio {
            payload: bytes,
            marker: self.marker,
            timestamp: timestamp_ms.wrapping_mul(8),
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
        Ok(extended)
    }
    fn poll(&mut self, now: u64) -> Result<(), MediaError> {
        let mut packet = [0; 160];
        for _ in 0..8 {
            match self.endpoint.try_receive(&mut packet) {
                Ok(None) => break,
                Ok(Some(RtpEvent::None | RtpEvent::Digit { ended: false, .. })) => {}
                Ok(Some(RtpEvent::Digit { digit, ended: true })) => {
                    self.pending = Some(Pending::Digit(digit));
                    return Ok(());
                }
                Ok(Some(RtpEvent::Audio { timestamp, .. })) => {
                    let timestamp = self.timestamp(timestamp)?;
                    if self
                        .jitter
                        .put(timestamp, 160, now, &packet)
                        .map_err(|_| MediaError::Codec)?
                        == Admission::Accepted
                    {
                        self.births[self.birth_slot] = Some((timestamp, now));
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
            .get(now, 160, &mut packet)
            .map_err(|_| MediaError::Codec)?;
        match decision.kind {
            Kind::NotDue => return Ok(()),
            Kind::Packet => {
                // The neutral ABI returns the packet timestamp, not the requested
                // cursor. Positive offsets are missing prefixes; negative ones
                // trim samples that have already been released (as in IAX playout).
                let skip = decision.start_offset.saturating_neg().max(0) as usize;
                let span = decision.span as usize;
                if decision.length != 160 || span != 160 || skip >= span {
                    return Err(MediaError::InvalidPacket);
                }
                let count = span - skip;
                let prefix = decision.start_offset.max(0) as u32;
                let start = decision
                    .timestamp
                    .checked_add(skip as u64)
                    .ok_or(MediaError::InvalidPacket)?;
                self.codec.decode(&packet, &mut self.pcm)?;
                self.pcm.copy_within(skip..span, 0);
                if let Some((_, born)) = self
                    .births
                    .iter()
                    .flatten()
                    .find(|(timestamp, _)| *timestamp == decision.timestamp)
                {
                    self.pending = Some(Pending::Audio {
                        start,
                        count,
                        born: *born,
                        prefix,
                    });
                } else {
                    self.pending = Some(Pending::Gap {
                        start: start
                            .checked_sub(u64::from(prefix))
                            .ok_or(MediaError::InvalidPacket)?,
                        count: (count as u32)
                            .checked_add(prefix)
                            .ok_or(MediaError::InvalidPacket)?,
                    });
                }
            }
            Kind::Missing => {
                self.pending = Some(Pending::Gap {
                    start: decision.timestamp,
                    count: decision.span,
                });
            }
            Kind::Insertion => {
                // Only playout time advances. The existing source/ring must not
                // see fictitious missing media or have its source cursor moved.
                return self.jitter.advance(now, 0).map_err(|_| MediaError::Codec);
            }
        }
        // Keep one provider decision leased until all exact source credit lands.
        // No new RTP put/get occurs while its prefix or decoded suffix is retained.
        self.decision_pending = true;
        Ok(())
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
            return Err(MediaError::InvalidPacket);
        }
        if sink.is_null() {
            return Ok(());
        }
        if unsafe { (*sink).struct_size } < size_of::<abi::rptadv_peer_source_sink_v1>() as u32
            || unsafe { (*sink).abi_version } != 1
        {
            return Err(MediaError::InvalidPacket);
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
            return Err(MediaError::InvalidPacket);
        }
        self.host_epoch = Some(sink.host_clock_epoch);
        if self.discontinuity {
            match unsafe { sink.source_discontinuity.unwrap()(sink.context) } {
                0 => self.discontinuity = false,
                1 => {
                    output.progress = 2;
                    return Ok(());
                }
                _ => return Err(MediaError::InvalidPacket),
            }
        }
        if self.pending.is_none() {
            self.offset = 0;
            self.poll(sink.reference_now_ns)?;
        }
        if let Some(Pending::Audio { start, prefix, .. }) = &mut self.pending {
            if *prefix != 0 {
                let accepted = unsafe {
                    sink.gap.unwrap()(sink.context, *start - u64::from(*prefix), *prefix)
                };
                if accepted > *prefix {
                    return Err(MediaError::InvalidPacket);
                }
                *prefix -= accepted;
                output.progress = if *prefix == 0 { 1 } else { 2 };
                return Ok(());
            }
        }
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        let accepted = match *pending {
            Pending::Digit(symbol) => {
                match unsafe { sink.digit.unwrap()(sink.context, u32::from(symbol)) } {
                    0 => true,
                    1 => false,
                    _ => return Err(MediaError::InvalidPacket),
                }
            }
            Pending::Audio {
                start, count, born, ..
            } => {
                if born > sink.reference_now_ns {
                    return Err(MediaError::InvalidPacket);
                }
                let birth = abi::rptadv_source_birth_v1 {
                    struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
                    abi_version: 1,
                    host_clock_epoch: sink.host_clock_epoch,
                    born_ns: born,
                };
                let remaining = count - self.offset;
                let accepted = unsafe {
                    sink.audio_timed.unwrap()(
                        sink.context,
                        start + self.offset as u64,
                        self.pcm[self.offset..count].as_ptr(),
                        remaining,
                        0,
                        &birth,
                    )
                };
                if accepted > remaining {
                    return Err(MediaError::InvalidPacket);
                }
                self.offset += accepted;
                self.offset == count
            }
            Pending::Gap { start, count } => {
                let remaining = count - self.offset as u32;
                let accepted = unsafe {
                    sink.gap.unwrap()(sink.context, start + self.offset as u64, remaining)
                };
                if accepted > remaining {
                    return Err(MediaError::InvalidPacket);
                }
                self.offset += accepted as usize;
                self.offset == count as usize
            }
        };
        output.progress = if accepted { 1 } else { 2 };
        if accepted {
            if self.decision_pending {
                self.jitter
                    .advance(sink.reference_now_ns, 0)
                    .map_err(|_| MediaError::Codec)?;
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
