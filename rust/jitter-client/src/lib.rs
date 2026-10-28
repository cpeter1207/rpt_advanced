//! Client of the composed, codec-neutral jitter ABI. No jitter implementation lives here.
#![deny(missing_docs)]

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
};

const CAPABILITY: [u8; 16] = *b"rptadv.jitter\0\0\0";
/// Maximum opaque datagram payload accepted by the composed jitter capability.
#[cfg(test)]
const MAX_PAYLOAD: usize = 65_503;
/// Test source clock; production callers select their actual codec rate.
#[cfg(test)]
pub const CLOCK_RATE_HZ: u32 = 8000;
/// Smallest missing/insertion span emitted by the composed jitter capability.
#[cfg(test)]
pub const CONCEALMENT_SPAN: u32 = 80;

/// ABI-1 records mirroring rptadv_jitter_adapter.h. Algorithms remain in the loaded DSO.
#[allow(missing_docs)]
pub mod abi {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Config {
        pub struct_size: u32,
        pub abi_version: u32,
        pub clock_rate_hz: u32,
        pub minimum_delay: u32,
        pub initial_delay: u32,
        pub maximum_delay: u32,
        pub adjustment_step: u32,
        pub concealment_span: u32,
        pub maximum_packets: u32,
        pub maximum_payload_bytes: u32,
    }

    #[repr(C)]
    pub struct Packet {
        pub struct_size: u32,
        pub timestamp: u64,
        pub span: u32,
        pub arrival_ns: u64,
        pub payload: *const u8,
        pub length: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct RawDecision {
        pub struct_size: u32,
        pub kind: u32,
        pub timestamp: u64,
        pub span: u32,
        pub start_offset: i32,
        pub length: usize,
        pub deadline_ns: u64,
    }

    /// Owner-task diagnostics copied from the neutral capability.
    #[repr(C)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Observation {
        pub struct_size: u32,
        pub queued_packets: u32,
        pub delay: u32,
        pub buffered_span: u64,
        pub duplicate_drops: u64,
        pub late_drops: u64,
        pub full_drops: u64,
        pub missing_span: u64,
        pub insertion_span: u64,
        pub skipped_span: u64,
        pub limit_resets: u64,
        pub overload_drops: u64,
        pub overload_span: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Descriptor {
        pub struct_size: u32,
        pub abi_version: u32,
        pub capability: [u8; 16],
        pub create: Option<unsafe extern "C" fn(*const Config, *mut *mut c_void) -> i32>,
        pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
        pub reset: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
        pub put: Option<unsafe extern "C" fn(*mut c_void, *const Packet) -> i32>,
        pub get: Option<
            unsafe extern "C" fn(*mut c_void, u64, u32, *mut u8, usize, *mut RawDecision) -> i32,
        >,
        pub advance: Option<unsafe extern "C" fn(*mut c_void, u64, u32) -> i32>,
        pub observe: Option<unsafe extern "C" fn(*const c_void, *mut Observation) -> i32>,
    }
}
pub use abi::Observation;
use abi::{Config, Descriptor, Packet, RawDecision};

/// Invalid capability contract or an unsuccessful capability operation.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Readable size/version/capability, operation table or creation options are invalid.
    InvalidDescriptor,
    /// The provider failed or returned an invalid sized result.
    OperationFailed,
}

/// Admission outcome; rejected packets are already counted by the capability.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    /// Packet was retained for ordered playout.
    Accepted,
    /// This packet's source interval is already queued or consumed.
    Duplicate,
    /// The source interval is too old to play.
    Late,
    /// The provider's packet or payload budget is exhausted.
    Full,
}

/// Packet delivery, media loss, delay insertion, or a future playout deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Encoded payload to decode once, then trim its signed overlap if needed.
    Packet,
    /// Missing source samples; use native codec PLC before shared-ring fallback.
    Missing,
    /// Extra playout delay; do not advance codec history.
    Insertion,
    /// No decision due; wait for the supplied deadline or new input.
    NotDue,
}

/// One codec-neutral playout decision; spans and signed offsets use the configured source clock.
#[derive(Debug)]
pub struct Decision {
    /// Actual provider decision, not inferred from encoded byte count.
    pub kind: Kind,
    /// Unwrapped source-clock position.
    pub timestamp: u64,
    /// Actual source-clock duration, which can be shorter than requested.
    pub span: u32,
    /// Signed correction from the requested start to the encoded packet's start.
    pub start_offset: i32,
    /// Number of valid encoded bytes copied into caller storage.
    pub length: usize,
    /// Next monotonic deadline in the caller's nanosecond epoch.
    pub deadline_ns: u64,
}

/// One exclusive handler's jitter context; composition retains the descriptor's DSO.
pub struct Jitter {
    api: Descriptor,
    context: NonNull<c_void>,
    release_span: u32,
}

// The capability contract permits thread migration and forbids simultaneous calls.
// Jitter is Send, not Sync: its sole owning async task may move among runtime workers.
unsafe impl Send for Jitter {}

impl Jitter {
    /// Validate a readable descriptor and create the approved 160/180/500ms context.
    ///
    /// # Safety
    /// `descriptor` must be readable for its declared size, and its implementing
    /// DSO must remain loaded until this context is dropped. Functions must obey ABI1.
    #[cfg(test)]
    pub unsafe fn new(descriptor: *const c_void) -> Result<Self, Error> {
        unsafe { Self::new_at_rate(descriptor, CLOCK_RATE_HZ, CONCEALMENT_SPAN, MAX_PAYLOAD) }
    }
    /// Compose 160/180/500 ms delay bounds at the selected source clock, with a
    /// positive coding unit and caller-owned payload limit (at most 65,527 bytes,
    /// matching the neutral provider rather than a particular network transport).
    /// # Safety
    /// The descriptor must be readable for its declared size and its DSO must
    /// remain loaded until this exclusive context is destroyed.
    pub unsafe fn new_at_rate(
        descriptor: *const c_void,
        rate: u32,
        loss_unit: u32,
        maximum_payload_bytes: usize,
    ) -> Result<Self, Error> {
        if !matches!(rate, 8000 | 16000 | 48000)
            || loss_unit == 0
            || loss_unit > rate / 2
            || !(1..=65_527).contains(&maximum_payload_bytes)
        {
            return Err(Error::InvalidDescriptor);
        }
        if descriptor.is_null() {
            return Err(Error::InvalidDescriptor);
        }
        // Read only the fixed prefix until the provider proves the full table exists.
        let prefix = unsafe { ptr::read_unaligned(descriptor.cast::<[u32; 2]>()) };
        if prefix[0] < size_of::<Descriptor>() as u32 || prefix[1] != 1 {
            return Err(Error::InvalidDescriptor);
        }
        let api = unsafe { ptr::read_unaligned(descriptor.cast::<Descriptor>()) };
        if api.capability != CAPABILITY
            || ![
                api.create.is_some(),
                api.destroy.is_some(),
                api.reset.is_some(),
                api.put.is_some(),
                api.get.is_some(),
                api.advance.is_some(),
                api.observe.is_some(),
            ]
            .into_iter()
            .all(|present| present)
        {
            return Err(Error::InvalidDescriptor);
        }
        let options = Config {
            struct_size: size_of::<Config>() as u32,
            abi_version: 1,
            clock_rate_hz: rate,
            minimum_delay: rate * 160 / 1000,
            initial_delay: rate * 180 / 1000,
            maximum_delay: rate / 2,
            adjustment_step: rate / 100,
            concealment_span: loss_unit,
            maximum_packets: 64,
            maximum_payload_bytes: maximum_payload_bytes as u32,
        };
        let mut context = ptr::null_mut();
        if unsafe { api.create.unwrap()(&options, &mut context) } != 0 {
            return Err(Error::OperationFailed);
        }
        Ok(Self {
            api,
            context: NonNull::new(context).ok_or(Error::OperationFailed)?,
            release_span: (rate / 50).div_ceil(loss_unit) * loss_unit,
        })
    }

    /// Closest complete coding-unit request at least the ordinary 20 ms quantum.
    pub fn release_span(&self) -> u32 {
        self.release_span
    }

    /// Start a new timestamp/receive epoch, discarding old buffered media.
    pub fn reset(&mut self) -> Result<(), Error> {
        status(unsafe { self.api.reset.unwrap()(self.context.as_ptr()) })
    }

    /// Copy an encoded packet with its original receive timestamp, not handler time.
    pub fn put(
        &mut self,
        timestamp: u64,
        span: u32,
        arrival_ns: u64,
        payload: &[u8],
    ) -> Result<Admission, Error> {
        let packet = Packet {
            struct_size: size_of::<Packet>() as u32,
            timestamp,
            span,
            arrival_ns,
            payload: payload.as_ptr(),
            length: payload.len(),
        };
        match unsafe { self.api.put.unwrap()(self.context.as_ptr(), &packet) } {
            0 => Ok(Admission::Accepted),
            1 => Ok(Admission::Duplicate),
            2 => Ok(Admission::Late),
            3 => Ok(Admission::Full),
            _ => Err(Error::OperationFailed),
        }
    }

    /// Fetch a due decision; honor actual span/offset, including short missing spans.
    pub fn get(
        &mut self,
        now_ns: u64,
        desired_span: u32,
        payload: &mut [u8],
    ) -> Result<Decision, Error> {
        let mut result = RawDecision {
            struct_size: size_of::<RawDecision>() as u32,
            ..RawDecision::default()
        };
        status(unsafe {
            self.api.get.unwrap()(
                self.context.as_ptr(),
                now_ns,
                desired_span,
                payload.as_mut_ptr(),
                payload.len(),
                &mut result,
            )
        })?;
        let kind = match result.kind {
            0 => Kind::Packet,
            1 => Kind::Missing,
            2 => Kind::Insertion,
            3 => Kind::NotDue,
            _ => return Err(Error::OperationFailed),
        };
        if result.struct_size < size_of::<RawDecision>() as u32
            || result.length > payload.len()
            || (kind != Kind::Packet && result.length != 0)
            || (kind != Kind::NotDue
                && (result.span == 0
                    || i64::from(result.span) + i64::from(result.start_offset) <= 0))
        {
            return Err(Error::OperationFailed);
        }
        Ok(Decision {
            kind,
            timestamp: result.timestamp,
            span: result.span,
            start_offset: result.start_offset,
            length: result.length,
            deadline_ns: result.deadline_ns,
        })
    }

    /// Commit one decision. Pass zero when the whole output was published to the ring.
    pub fn advance(&mut self, now_ns: u64, retained_span: u32) -> Result<(), Error> {
        status(unsafe { self.api.advance.unwrap()(self.context.as_ptr(), now_ns, retained_span) })
    }

    /// Read diagnostics on the owning handler, never on an audio callback.
    pub fn observe(&self) -> Result<Observation, Error> {
        let mut result = Observation {
            struct_size: size_of::<Observation>() as u32,
            ..Observation::default()
        };
        status(unsafe { self.api.observe.unwrap()(self.context.as_ptr(), &mut result) })?;
        if result.struct_size < size_of::<Observation>() as u32 {
            return Err(Error::OperationFailed);
        }
        Ok(result)
    }
}

impl Drop for Jitter {
    fn drop(&mut self) {
        unsafe { self.api.destroy.unwrap()(self.context.as_ptr()) };
    }
}

fn status(value: i32) -> Result<(), Error> {
    if value == 0 {
        Ok(())
    } else {
        Err(Error::OperationFailed)
    }
}

#[cfg(test)]
mod tests;
