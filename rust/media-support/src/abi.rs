//! Common ABI layouts with one capability-specific operation per provider build.
use std::ffi::{c_char, c_void};

/// Incremental media capability ABI; incompatible tables are rejected before creation.
pub const ABI_VERSION: u32 = 3;
/// Fixed-width capability identity, including NUL padding.
#[cfg(file_adapter)]
pub const CAPABILITY: [u8; 16] = *b"rptadv.file\0\0\0\0\0";
/// Fixed-width speech capability identity, including NUL padding.
#[cfg(speech_adapter)]
pub const CAPABILITY: [u8; 16] = *b"rptadv.speech\0\0\0";

/// Readable prefix of every versioned ABI structure.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct AbiHeader {
    pub(crate) struct_size: u32,
    pub(crate) abi_version: u32,
}

unsafe fn versioned<'a, T>(
    value: *const T,
    error: crate::MediaError,
) -> Result<&'a T, crate::MediaError> {
    if value.is_null() {
        return Err(error);
    }
    // SAFETY: ABI callers must provide the readable versioned prefix.
    let header = unsafe { value.cast::<AbiHeader>().read_unaligned() };
    if header.struct_size < size_of::<T>() as u32 || header.abi_version != ABI_VERSION {
        return Err(error);
    }
    // SAFETY: a validated full size makes the complete structure readable.
    Ok(unsafe { &*value })
}

/// Local process configuration borrowed only during create.
#[repr(C)]
pub struct RawConfig {
    /// Complete structure size.
    pub struct_size: u32,
    /// Required media ABI.
    pub abi_version: u32,
    /// NUL-terminated native path to this capability's executable.
    pub executable: *const c_char,
    /// Each subprocess's deadline in milliseconds; nonzero.
    pub timeout_ms: u32,
    /// Optional host reaper acquisition; must be paired with release.
    pub reaper_acquire: Option<extern "C" fn()>,
    /// Optional host reaper release, invoked after final reap.
    pub reaper_release: Option<extern "C" fn()>,
}

impl RawConfig {
    pub(crate) unsafe fn from_pointer<'a>(
        value: *const Self,
    ) -> Result<&'a Self, crate::MediaError> {
        // SAFETY: the caller provides the ABI prefix; versioned validates it first.
        unsafe { versioned(value, crate::MediaError::InvalidRequest) }
    }
}

/// Request-local cancellation callback, borrowed until preparation returns.
#[repr(C)]
pub struct RawCancellation {
    /// Opaque callback context, never interpreted by the adapter.
    pub context: *const c_void,
    /// Required nonblocking callback: nonzero means cancelled; must not unwind.
    pub is_cancelled: extern "C" fn(*const c_void) -> u32,
}

/// Literal speech request borrowed until preparation returns.
#[repr(C)]
pub struct RawSpeechRequest {
    /// NUL-terminated UTF-8 speech text.
    pub text: *const c_char,
    /// NUL-terminated native path to an existing model.
    pub model: *const c_char,
    /// Speaking speed in the inclusive range 1 through 1000.
    pub speed_percent: u32,
    /// Speech-only gain in the inclusive range -60 through 0 dB.
    pub level_db: i32,
}

/// No samples or format are available on this visit.
pub const STEP_PENDING: u32 = 0;
/// The source rate is ready. Exactly one such result precedes samples.
pub const STEP_FORMAT_READY: u32 = 1;
/// The output contains a nonzero number of samples.
pub const STEP_SAMPLES: u32 = 2;
/// The child exited successfully and all valid samples were returned.
pub const STEP_EOF: u32 = 3;

/// One result of a bounded stream visit; unused fields are zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawStep {
    /// One of the STEP_* event constants.
    pub kind: u32,
    /// Decoded source rate, used only for STEP_FORMAT_READY.
    pub sample_rate_hz: u32,
    /// Output count, used only for STEP_SAMPLES.
    pub samples: usize,
}

/// Versioned stream functions, used off the audio callback.
///
/// Status values: 0 success; -1 invalid request; -2 unavailable local resource;
/// -3 I/O/internal failure; -4 failed subprocess; -5 timeout; -6 cancelled;
/// -7 invalid output. Contexts allow concurrent independent streams. Open and
/// close may block on local I/O or process lifecycle. Step never waits for media.
/// Each handle may move between workers with exclusive, sequential ownership.
/// Successful setup always transfers the handle, even if cancellation races it.
/// The caller checks cancellation before use and closes every returned handle.
/// Only successful Samples results make output consumable. Destroy requires all
/// streams and calls to have ended; the host retains the provider and callbacks.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    /// Complete descriptor size.
    pub struct_size: u32,
    /// Version implemented by every function.
    pub abi_version: u32,
    /// Exact capability identifier.
    pub capability: [u8; 16],
    /// Create an opaque configuration owner; initializes output to null on error.
    pub create: Option<unsafe extern "C" fn(*const RawConfig, *mut *mut c_void) -> i32>,
    /// Destroy a context after its users stop; null is harmless.
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    /// Open one local file stream and initialize the handle to null on failure.
    #[cfg(file_adapter)]
    pub open_file: Option<
        unsafe extern "C" fn(
            *const c_void,
            *const c_char,
            *const RawCancellation,
            *mut *mut c_void,
        ) -> i32,
    >,
    /// Open one synthesized speech stream and initialize the handle to null on failure.
    #[cfg(speech_adapter)]
    pub open_speech: Option<
        unsafe extern "C" fn(
            *const c_void,
            *const RawSpeechRequest,
            *const RawCancellation,
            *mut *mut c_void,
        ) -> i32,
    >,
    /// Advance a stream once, returning its event separately from the status.
    pub step_stream: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const RawCancellation,
            *mut f32,
            usize,
            *mut RawStep,
        ) -> i32,
    >,
    /// Release a stream exactly once; null is harmless.
    pub close_stream: Option<unsafe extern "C" fn(*mut c_void)>,
}

impl Descriptor {
    /// Validate a versioned descriptor pointer before accessing its full table.
    ///
    /// # Safety
    /// `value` must be null or point to a readable `AbiHeader`; a non-null header
    /// claiming the complete descriptor size must truthfully back that allocation.
    pub unsafe fn from_pointer<'a>(value: *const Self) -> Result<&'a Self, crate::MediaError> {
        // SAFETY: the caller provides the ABI prefix; versioned validates it first.
        let descriptor = unsafe { versioned(value, crate::MediaError::IncompatibleAdapter) }?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Validate the entire current table before invoking any operation.
    pub fn validate(&self) -> Result<(), crate::MediaError> {
        if self.struct_size < std::mem::size_of::<Self>() as u32
            || self.abi_version != ABI_VERSION
            || self.capability != CAPABILITY
            || self.create.is_none()
            || self.destroy.is_none()
            || self.operation_missing()
            || self.step_stream.is_none()
            || self.close_stream.is_none()
        {
            return Err(crate::MediaError::IncompatibleAdapter);
        }
        Ok(())
    }
    fn operation_missing(&self) -> bool {
        #[cfg(file_adapter)]
        {
            self.open_file.is_none()
        }
        #[cfg(speech_adapter)]
        {
            self.open_speech.is_none()
        }
    }
}

#[cfg(test)]
#[path = "abi_tests.rs"]
mod tests;
