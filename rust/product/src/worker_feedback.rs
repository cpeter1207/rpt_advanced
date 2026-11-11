//! Sized physical feedback consumed once by the existing serial TX owner.
//! These observations never prove receiver settling, DAC time or a usable RF window.
use crate::abi;
use std::mem::{offset_of, size_of};

const PHYSICAL_BYTES: usize =
    offset_of!(abi::UrpRadioFeedback, transmitter_state) + size_of::<i32>();

/// Qualified facts from one physical lifecycle and the immediately following callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhysicalFeedback {
    generation: u64,
    sample: u64,
    frames: u32,
    duplex: Option<bool>,
    requested: Option<bool>,
    rendered: Option<bool>,
    serviced: Option<bool>,
    observed: Option<bool>,
    acquisition_age: Option<u64>,
    state: Option<i32>,
}

fn boolean(value: u32) -> Option<bool> {
    (value <= 1).then_some(value != 0)
}

impl PhysicalFeedback {
    /// Copy only the published physical prefix. The SDK lends readable size bytes,
    /// not necessarily the complete newer record with its optional programme tail.
    unsafe fn read(pointer: *const abi::UrpRadioFeedback) -> Option<Self> {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: every non-null borrowed record provides its readable size word.
        let declared = unsafe { pointer.cast::<u32>().read_unaligned() } as usize;
        if declared < PHYSICAL_BYTES {
            return None;
        }
        // The C record contains only integer scalar fields and nested scalar records.
        let mut value: abi::UrpRadioFeedback = unsafe { std::mem::zeroed() };
        // SAFETY: size was checked before borrowing any later field. Copying the
        // finite prefix neither reads nor requires an optional SDK programme tail.
        unsafe {
            std::ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                std::ptr::from_mut(&mut value).cast(),
                PHYSICAL_BYTES,
            );
        }
        if value.abi_version != abi::URP_RADIO_FEEDBACK_ABI_VERSION
            || value.generation_id == 0
            || value.frame_count == 0
        {
            return None;
        }
        let valid = |flag| value.flags & flag != 0;
        let request_valid = valid(abi::URP_RADIO_FEEDBACK_REQUEST_VALID) && value.request_id != 0;
        let requested = request_valid
            .then(|| boolean(value.requested_ptt))
            .flatten();
        let rendered = request_valid.then(|| boolean(value.rendered_ptt)).flatten();
        let acquisition_age = (valid(abi::URP_RADIO_FEEDBACK_ACQUISITION_VALID)
            && value.sample_index.checked_sub(value.acquired_sample_index)
                == Some(value.acquisition_age_frames))
        .then_some(value.acquisition_age_frames);
        // A diagnostic snapshot qualifies readback/service only within one
        // callback span. A future RF arbiter must select its own explicit age
        // policy and still account for settling; this is not a completed check.
        let current_request = requested.is_some()
            && rendered.is_some()
            && acquisition_age.is_some_and(|age| age <= u64::from(value.frame_count));
        let output = |flag, generation, request, ptt| {
            (current_request
                && valid(flag)
                && generation == value.generation_id
                && request == value.request_id)
                .then(|| boolean(ptt))
                .flatten()
        };
        Some(Self {
            generation: value.generation_id,
            sample: value.sample_index,
            frames: value.frame_count,
            duplex: valid(abi::URP_RADIO_FEEDBACK_DUPLEX_VALID)
                .then(|| boolean(value.physical_duplex))
                .flatten(),
            requested,
            rendered,
            serviced: output(
                abi::URP_RADIO_FEEDBACK_SERVICE_VALID,
                value.service_generation,
                value.service_request_id,
                value.serviced_ptt,
            ),
            observed: output(
                abi::URP_RADIO_FEEDBACK_READBACK_VALID,
                value.readback_generation,
                value.readback_request_id,
                value.observed_ptt,
            ),
            acquisition_age,
            state: (valid(abi::URP_RADIO_FEEDBACK_PHASE_VALID)
                && matches!(value.transmitter_state, 0 | 1 | 2 | 4 | 5))
            .then_some(value.transmitter_state),
        })
    }
}

/// No queue, polling, clock read or allocation: feedback precedes PCM on this owner.
#[derive(Default)]
pub(super) struct FeedbackState {
    pending: Option<PhysicalFeedback>,
    pub(super) current: Option<PhysicalFeedback>,
    last: Option<(u64, u64)>,
}
impl FeedbackState {
    /// Replace pending evidence even when malformed, so old OFF cannot survive it.
    pub(super) unsafe fn publish(&mut self, pointer: *const abi::UrpRadioFeedback) {
        // SAFETY: the same SDK callback lends this sized record synchronously.
        self.pending = unsafe { PhysicalFeedback::read(pointer) };
    }

    /// Consume one publication for exactly its promised callback. A worker belongs
    /// to one physical lifecycle; reopening creates a fresh callback context.
    pub(super) fn begin(&mut self, count: usize, maximum: usize) {
        self.current = self.pending.take().filter(|value| {
            count <= maximum
                && count == value.frames as usize
                && self.last.is_none_or(|(generation, sample)| {
                    generation == value.generation && sample < value.sample
                })
        });
        if let Some(value) = self.current {
            self.last = Some((value.generation, value.sample));
        }
    }
}

fn state(value: Option<bool>, off: &'static str, on: &'static str) -> &'static str {
    match value {
        None => "unknown",
        Some(false) => off,
        Some(true) => on,
    }
}

/// Format only on station control from the existing bounded meter snapshot.
pub(super) fn status(value: Option<PhysicalFeedback>) -> String {
    let Some(value) = value else {
        return "  radio-feedback: unavailable".into();
    };
    format!(
        "  radio-feedback: generation={} sample={} duplex={} requested={} rendered={} serviced={} observed={} acquisition-age={}frames state={}",
        value.generation,
        value.sample,
        state(value.duplex, "half", "full"),
        state(value.requested, "off", "on"),
        state(value.rendered, "off", "on"),
        state(value.serviced, "off", "on"),
        state(value.observed, "off", "on"),
        value
            .acquisition_age
            .map_or_else(|| "unknown".into(), |age| age.to_string()),
        value
            .state
            .map_or_else(|| "unknown".into(), |state| state.to_string()),
    )
}
