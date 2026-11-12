//! Released source-time polling; one mode and one serialized owner per private call.
use super::{IaxError, IaxPeer};
use crate::abi;
use std::mem::{size_of, size_of_val};

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
        self.api.struct_size as usize
            >= std::mem::offset_of!(abi::rptadv_iax2_client_descriptor_v2, poll_timed)
                + size_of_val(&self.api.poll_timed)
            && self.api.poll_timed.is_some()
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
        let poll = self.api.poll_timed.ok_or(IaxError::IncompatibleAdapter)?;
        // Legal text is consumed without interpretation. This is transient stack
        // storage, not an additional retained buffer for every connected call.
        let mut text = [0u8; 65507];
        let mut record = abi::rptadv_iax2_timed_event_v1 {
            struct_size: size_of::<abi::rptadv_iax2_timed_event_v1>() as u32,
            abi_version: 1,
            host_clock_epoch: host_epoch,
            reference_now_ns,
            acknowledged_media_epoch,
            event_kind: 0,
            flags: 0,
            event_length: 0,
            media_epoch: 0,
            source_start: 0,
            born_ns: 0,
        };
        let status = unsafe {
            poll(
                self.handle.as_ptr(),
                pcm.as_mut_ptr(),
                pcm.len(),
                text.as_mut_ptr(),
                text.len(),
                &mut record,
            )
        };
        if status != 0
            || record.struct_size as usize != size_of::<abi::rptadv_iax2_timed_event_v1>()
            || record.abi_version != 1
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
