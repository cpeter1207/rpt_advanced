//! Released source-time polling; one mode and one serialized owner per private call.
use super::{IaxError, IaxPeer};
use crate::abi;
use std::{mem::size_of, ptr};

/// One bounded copied event. Birth remains tied to the supplied host clock.
#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub kind: Kind,
    pub media_epoch: u64,
    pub source_start: u64,
    pub born_ns: Option<u64>,
    pub discontinuity: bool,
}
/// Private source events never reinterpret ASL text or radio key state as commands.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    None,
    Audio(usize),
    Gap(u32),
    Digit(u8),
    Hangup,
    Ignored,
}
impl IaxPeer {
    /// Verify the optional tail before transferring a private call to its host.
    pub(crate) fn supports_timed_source(&self) -> bool {
        // IaxClient validates the complete current descriptor before any peer
        // can be constructed; each required optional callback must be present.
        self.api.poll_timed.is_some()
            && self.api.send_prepared_audio_timed.is_some()
            && self.api.retire_egress.is_some()
    }

    /// Retire private encoder history and old TX eligibility on its sole owner.
    /// A fresh private call never sends legacy full-frame PCM; pending retirement
    /// therefore fails closed rather than restoring untimed media or its retries.
    pub(crate) fn retire_private_egress(&mut self) -> Result<u64, IaxError> {
        let retire = self
            .api
            .retire_egress
            .ok_or(IaxError::IncompatibleAdapter)?;
        let mut epoch = 0;
        match unsafe { retire(self.handle.as_ptr(), &mut epoch) } {
            0 if epoch != 0 => Ok(epoch),
            _ => Err(IaxError::Operation),
        }
    }

    /// Forward one immutable packet/deadline/token without re-encoding or restamping.
    /// Status two is uncommitted expiry, distinct from acceptance and congestion.
    pub(crate) fn send_private_prepared(
        &mut self,
        bytes: &[u8],
        frames: u32,
        timestamp: u32,
        epoch: u64,
        deadline_ms: u32,
    ) -> Result<i32, IaxError> {
        let send = self
            .api
            .send_prepared_audio_timed
            .ok_or(IaxError::IncompatibleAdapter)?;
        let timing = abi::rptadv_iax2_timed_send_v1 {
            struct_size: size_of::<abi::rptadv_iax2_timed_send_v1>() as u32,
            abi_version: 1,
            egress_epoch: epoch,
            deadline_ms,
            reserved: 0,
        };
        match unsafe {
            send(
                self.handle.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                frames,
                timestamp,
                &timing,
            )
        } {
            status @ 0..=2 => Ok(status),
            _ => Err(IaxError::Operation),
        }
    }

    /// Read one event into a caller-sized PCM prefix, retaining provider suffixes.
    /// Never call legacy poll on a peer once this operation has been selected.
    pub(crate) fn poll_source(
        &mut self,
        pcm: &mut [f32],
        host_epoch: u64,
        reference_now_ns: u64,
        acknowledged_media_epoch: u64,
    ) -> Result<Event, IaxError> {
        // Legal text is consumed without interpretation. This is transient stack
        // storage, not an additional retained buffer for every connected call.
        let mut text = [0u8; 65507];
        self.source_record(
            pcm,
            &mut text,
            host_epoch,
            reference_now_ns,
            acknowledged_media_epoch,
            false,
        )
    }

    /// Service ACKs/retries with no source buffers or source cursor consumption.
    /// The released provider bounds each turn to 64 datagrams and 64 held records.
    pub(crate) fn service_source(
        &mut self,
        host_epoch: u64,
        reference_now_ns: u64,
        acknowledged_media_epoch: u64,
    ) -> Result<Event, IaxError> {
        self.source_record(
            &mut [],
            &mut [],
            host_epoch,
            reference_now_ns,
            acknowledged_media_epoch,
            true,
        )
    }

    fn source_record(
        &mut self,
        pcm: &mut [f32],
        text: &mut [u8],
        host_epoch: u64,
        reference_now_ns: u64,
        acknowledged_media_epoch: u64,
        service: bool,
    ) -> Result<Event, IaxError> {
        let poll = self.api.poll_timed.ok_or(IaxError::IncompatibleAdapter)?;
        let request_flags = if service {
            abi::RPTADV_IAX2_SERVICE_ONLY
        } else {
            0
        };
        let mut request = abi::rptadv_iax2_timed_event_v2 {
            prefix: abi::rptadv_iax2_timed_event_v1 {
                struct_size: size_of::<abi::rptadv_iax2_timed_event_v2>() as u32,
                abi_version: 2,
                host_clock_epoch: host_epoch,
                reference_now_ns,
                acknowledged_media_epoch,
                event_kind: 0,
                flags: 0,
                event_length: 0,
                media_epoch: 0,
                source_start: 0,
                born_ns: 0,
            },
            request_flags,
            reserved: 0,
        };
        let status = unsafe {
            poll(
                self.handle.as_ptr(),
                if pcm.is_empty() {
                    ptr::null_mut()
                } else {
                    pcm.as_mut_ptr()
                },
                pcm.len(),
                if text.is_empty() {
                    ptr::null_mut()
                } else {
                    text.as_mut_ptr()
                },
                text.len(),
                // Derive the ABI1-typed pointer from the complete ABI2 object:
                // the provider reads its sized tail, not just the prefix field.
                ptr::from_mut(&mut request).cast(),
            )
        };
        let record = request.prefix;
        if status != 0
            || request.request_flags != request_flags
            || request.reserved != 0
            || record.struct_size as usize != size_of::<abi::rptadv_iax2_timed_event_v2>()
            || record.abi_version != 2
            || (service && !matches!(record.event_kind, 0 | 8))
            || record.host_clock_epoch != host_epoch
            || record.reference_now_ns != reference_now_ns
            || record.acknowledged_media_epoch != acknowledged_media_epoch
            || record.media_epoch == 0
            || record.flags & !3 != 0
            || (record.flags & 1 != 0
                && (record.event_kind != 1 || record.born_ns > reference_now_ns))
            || (record.flags & 2 != 0 && matches!(record.event_kind, 1 | 7))
        {
            return Err(IaxError::Operation);
        }
        let length = record.event_length;
        let kind = match record.event_kind {
            0 if length == 0 => Kind::None,
            1 if length > 0 && length <= pcm.len() => Kind::Audio(length),
            2 if length <= text.len() => Kind::Ignored,
            3 if length == 0 => Kind::Hangup,
            4 if length == 1 && matches!(text[0], b'0'..=b'9' | b'A'..=b'D' | b'*' | b'#') => {
                Kind::Digit(text[0])
            }
            5 | 6 if length == 0 => Kind::Ignored,
            7 if length > 0 && length <= u32::MAX as usize => Kind::Gap(length as u32),
            8 if length == 0 && record.flags & 2 != 0 => Kind::None,
            _ => return Err(IaxError::Operation),
        };
        if matches!(kind, Kind::Audio(_) | Kind::Gap(_))
            && record.source_start.checked_add(length as u64).is_none()
        {
            return Err(IaxError::Operation);
        }
        Ok(Event {
            kind,
            media_epoch: record.media_epoch,
            source_start: record.source_start,
            born_ns: (record.flags & 1 != 0).then_some(record.born_ns),
            discontinuity: record.flags & 2 != 0,
        })
    }
}
