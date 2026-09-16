//! Common ABI layouts with one capability-specific operation per provider build.
use std::ffi::{c_char, c_void};

/// Initial media capability ABI; incompatible tables are rejected before creation.
pub const ABI_VERSION: u32 = 1;
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
    /// NUL-terminated temporary directory path.
    pub temporary_directory: *const c_char,
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

/// Owned result and immutable PCM view; released only by its originating adapter.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawAudio {
    /// Opaque ownership handle passed to release_audio exactly once.
    pub handle: *mut c_void,
    /// Mono normalized F32 samples valid until release_audio.
    pub samples: *const f32,
    /// Nonzero number of mono samples.
    pub sample_count: usize,
    /// Original decoded source rate; no native-rate conversion occurs here.
    pub sample_rate_hz: u32,
}

/// Versioned preparation functions. All operations are control-plane-only.
///
/// Status values: 0 success; -1 invalid request; -2 unavailable local resource;
/// -3 I/O/internal failure; -4 failed subprocess; -5 timeout; -6 cancelled;
/// -7 invalid output. A context supports concurrent independent preparations.
/// Destroy requires all calls to have returned. Cancellation callbacks and
/// host reaper callbacks must remain loaded and valid until their calls end.
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
    /// Decode one opened local file. Failure leaves an empty result.
    #[cfg(file_adapter)]
    pub prepare_file: Option<
        unsafe extern "C" fn(
            *const c_void,
            *const c_char,
            *const RawCancellation,
            *mut RawAudio,
        ) -> i32,
    >,
    /// Synthesize then decode one request. Failure leaves an empty result.
    #[cfg(speech_adapter)]
    pub prepare_speech: Option<
        unsafe extern "C" fn(
            *const c_void,
            *const RawSpeechRequest,
            *const RawCancellation,
            *mut RawAudio,
        ) -> i32,
    >,
    /// Free prepared PCM after its consumers stop; null is harmless.
    pub release_audio: Option<unsafe extern "C" fn(*mut c_void)>,
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
            || self.release_audio.is_none()
        {
            return Err(crate::MediaError::IncompatibleAdapter);
        }
        Ok(())
    }
    fn operation_missing(&self) -> bool {
        #[cfg(file_adapter)]
        {
            self.prepare_file.is_none()
        }
        #[cfg(speech_adapter)]
        {
            self.prepare_speech.is_none()
        }
    }
}

#[cfg(test)]
#[path = "abi_tests.rs"]
mod tests;
