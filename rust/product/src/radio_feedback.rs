//! Validate released radio observations without inferring electrical PTT from output intent.
use crate::abi;
use std::{mem::offset_of, ptr};

pub(crate) const FRESH_MS: u64 = 500;

/// Distinct software render intent, completed output service and independent readback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhysicalTransmit {
    pub(crate) at_ms: u64,
    pub(crate) rendered: Option<bool>,
    pub(crate) serviced: Option<bool>,
    pub(crate) observed: Option<bool>,
}

fn bit(value: u32) -> Option<bool> {
    match value {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

impl PhysicalTransmit {
    pub(crate) fn word(self) -> u64 {
        fn state(value: Option<bool>) -> u64 {
            value.map_or(0, |enabled| 1 + u64::from(enabled))
        }
        self.at_ms.min(u64::MAX >> 6) << 6
            | state(self.rendered)
            | state(self.serviced) << 2
            | state(self.observed) << 4
    }

    pub(crate) fn from_word(word: u64, now_ms: u64) -> Option<Self> {
        let at_ms = word >> 6;
        if word & 63 == 0 || now_ms.saturating_sub(at_ms) > FRESH_MS {
            return None;
        }
        let decode = |shift: u32| match (word >> shift) & 3 {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
        Some(Self {
            at_ms,
            rendered: decode(0),
            serviced: decode(2),
            observed: decode(4),
        })
    }

    /// Copy only the released physical prefix, never the optional programme tail.
    ///
    /// # Safety
    /// A non-null pointer lends a readable SDK size/version and its advertised storage.
    pub(crate) unsafe fn read(message: *const abi::UrpRadioFeedback, at_ms: u64) -> Option<Self> {
        const PHYSICAL: usize = offset_of!(abi::UrpRadioFeedback, programme_buffering);
        if message.is_null() {
            return None;
        }
        // SAFETY: a conforming provider lends the size/version prefix on each callback.
        let [size, version] = unsafe { message.cast::<[u32; 2]>().read_unaligned() };
        if (size as usize) < PHYSICAL || version != abi::URP_RADIO_FEEDBACK_ABI_VERSION {
            return None;
        }
        // SAFETY: the flat SDK record is scalar-only. Read exactly the verified prefix.
        let mut feedback: abi::UrpRadioFeedback = unsafe { std::mem::zeroed() };
        unsafe {
            ptr::copy_nonoverlapping(
                message.cast::<u8>(),
                ptr::from_mut(&mut feedback).cast(),
                PHYSICAL,
            );
        }
        if feedback.generation_id == 0 {
            return None;
        }
        let active = |flag: u32| feedback.flags & flag != 0;
        let valid_request =
            active(abi::URP_RADIO_FEEDBACK_REQUEST_VALID) && feedback.request_id != 0;
        let rendered = valid_request.then(|| bit(feedback.rendered_ptt)).flatten();
        let serviced = (valid_request
            && active(abi::URP_RADIO_FEEDBACK_SERVICE_VALID)
            && feedback.service_generation == feedback.generation_id
            && feedback.service_request_id == feedback.request_id)
            .then(|| bit(feedback.serviced_ptt))
            .flatten();
        let observed = (valid_request
            && active(abi::URP_RADIO_FEEDBACK_READBACK_VALID)
            && active(abi::URP_RADIO_FEEDBACK_ACQUISITION_VALID)
            && feedback.readback_generation == feedback.generation_id
            && feedback.readback_request_id == feedback.request_id
            && feedback.acquisition_age_frames <= 48 * FRESH_MS
            && feedback
                .sample_index
                .checked_sub(feedback.acquired_sample_index)
                == Some(feedback.acquisition_age_frames))
        .then(|| bit(feedback.observed_ptt))
        .flatten();
        Some(Self {
            at_ms,
            rendered,
            serviced,
            observed,
        })
    }
}

/// Settings-independent receive facts from the current native provider.
#[derive(Clone, Copy)]
pub(crate) struct PhysicalReceive {
    /// Time this cached provider snapshot was copied on the serialized observer.
    pub(crate) at_ms: u64,
    pub(crate) carrier_detected: bool,
    pub(crate) subaudible_detected: bool,
    pub(crate) qualified: bool,
    pub(crate) ctcss_decode_index: Option<u32>,
    pub(crate) dcs_valid: bool,
}

impl PhysicalReceive {
    pub(crate) fn from_status(status: &abi::UrpNativePhysicalStatus, at_ms: u64) -> Option<Self> {
        if status.running != 1 || status.ctcss_decode_index < -1 {
            return None;
        }
        Some(Self {
            at_ms,
            carrier_detected: bit(status.carrier_active)?,
            subaudible_detected: bit(status.subaudible_active)?,
            qualified: bit(status.receiver_keyed)?,
            ctcss_decode_index: u32::try_from(status.ctcss_decode_index).ok(),
            dcs_valid: bit(status.dcs_valid)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn electrical_ptt_disagrees_from_successful_service_and_expires() {
        let mut wire: abi::UrpRadioFeedback = unsafe { std::mem::zeroed() };
        wire.struct_size = offset_of!(abi::UrpRadioFeedback, programme_buffering) as u32;
        wire.abi_version = abi::URP_RADIO_FEEDBACK_ABI_VERSION;
        wire.generation_id = 5;
        wire.request_id = 9;
        wire.service_generation = 5;
        wire.service_request_id = 9;
        wire.readback_generation = 5;
        wire.readback_request_id = 9;
        wire.flags = abi::URP_RADIO_FEEDBACK_REQUEST_VALID
            | abi::URP_RADIO_FEEDBACK_SERVICE_VALID
            | abi::URP_RADIO_FEEDBACK_READBACK_VALID
            | abi::URP_RADIO_FEEDBACK_ACQUISITION_VALID;
        wire.rendered_ptt = 1;
        wire.serviced_ptt = 1;
        let read =
            |wire: &abi::UrpRadioFeedback| unsafe { PhysicalTransmit::read(wire, 20).unwrap() };
        let first = read(&wire);
        assert_eq!(first.rendered, Some(true));
        assert_eq!(first.serviced, Some(true));
        assert_eq!(first.observed, Some(false));
        assert_eq!(PhysicalTransmit::from_word(first.word(), 520), Some(first));
        assert_eq!(PhysicalTransmit::from_word(first.word(), 521), None);

        wire.readback_request_id = 8;
        assert_eq!(read(&wire).observed, None);
        wire.readback_request_id = 9;
        wire.acquisition_age_frames = 24_001;
        assert_eq!(read(&wire).observed, None);
        wire.acquisition_age_frames = 0;
        wire.sample_index = 1;
        assert_eq!(read(&wire).observed, None);
        wire.sample_index = 0;
        wire.flags &= !abi::URP_RADIO_FEEDBACK_READBACK_VALID;
        assert_eq!(read(&wire).observed, None);
        wire.struct_size -= 1;
        assert!(unsafe { PhysicalTransmit::read(&wire, 20) }.is_none());
    }

    #[test]
    fn native_signal_status_never_treats_a_stopped_station_as_off() {
        let mut wire: abi::UrpNativePhysicalStatus = unsafe { std::mem::zeroed() };
        assert!(PhysicalReceive::from_status(&wire, 50).is_none());
        wire.running = 1;
        wire.carrier_active = 1;
        wire.subaudible_active = 1;
        wire.ctcss_decode_index = 13;
        let sampled = PhysicalReceive::from_status(&wire, 50).unwrap();
        assert_eq!(sampled.at_ms, 50);
        assert!(sampled.carrier_detected && sampled.subaudible_detected);
        assert_eq!(sampled.ctcss_decode_index, Some(13));
        assert!(!sampled.qualified && !sampled.dcs_valid);
    }
}
