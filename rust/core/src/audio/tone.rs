//! Streaming courtesy tones supplied by the released ABI 1 tone library.

use super::AudioSource;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

/// A rejected tone sequence or unavailable/incompatible tone provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToneError;

#[repr(C)]
#[derive(Clone, Copy)]
struct ToneDescriptor {
    struct_size: u32,
    abi_version: u32,
    capability_name: *const c_char,
    sequence_create: Option<unsafe extern "C" fn(*const u8, usize, i8, *mut *mut c_void) -> i32>,
    sequence_rendered_samples: Option<unsafe extern "C" fn(*const c_void, *mut usize) -> i32>,
    sequence_render: Option<unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32>,
    sequence_destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    oscillator_create: Option<unsafe extern "C" fn(f32, f32, *mut *mut c_void) -> i32>,
    oscillator_reset: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    oscillator_render: Option<unsafe extern "C" fn(*mut c_void, *mut f32, usize) -> i32>,
    oscillator_destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[derive(Clone, Copy)]
struct ToneApi {
    create: unsafe extern "C" fn(*const u8, usize, i8, *mut *mut c_void) -> i32,
    rendered_samples: unsafe extern "C" fn(*const c_void, *mut usize) -> i32,
    render: unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}

unsafe extern "C" {
    fn rptadv_tone_descriptor() -> *const ToneDescriptor;
}

static API: OnceLock<Result<ToneApi, ToneError>> = OnceLock::new();

fn provider_api() -> Result<ToneApi, ToneError> {
    *API.get_or_init(|| unsafe { validate_descriptor(rptadv_tone_descriptor()) })
}

/// Check only the size prefix before reading the complete ABI table.
///
/// # Safety
/// `pointer` must address a readable ABI descriptor prefix and, when its reported
/// size is sufficient, a complete immutable descriptor during this call.
unsafe fn validate_descriptor(pointer: *const ToneDescriptor) -> Result<ToneApi, ToneError> {
    if pointer.is_null() {
        return Err(ToneError);
    }
    let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read() };
    if size < std::mem::size_of::<ToneDescriptor>() as u32 {
        return Err(ToneError);
    }
    let abi = unsafe { std::ptr::addr_of!((*pointer).abi_version).read() };
    if abi != 1 {
        return Err(ToneError);
    }
    let descriptor = unsafe { &*pointer };
    if descriptor.capability_name.is_null()
        || unsafe { CStr::from_ptr(descriptor.capability_name) } != c"rptadv.tone"
        || descriptor.oscillator_create.is_none()
        || descriptor.oscillator_reset.is_none()
        || descriptor.oscillator_render.is_none()
        || descriptor.oscillator_destroy.is_none()
    {
        return Err(ToneError);
    }
    Ok(ToneApi {
        create: descriptor.sequence_create.ok_or(ToneError)?,
        rendered_samples: descriptor.sequence_rendered_samples.ok_or(ToneError)?,
        render: descriptor.sequence_render.ok_or(ToneError)?,
        destroy: descriptor.sequence_destroy.ok_or(ToneError)?,
    })
}

/// A bounded tone sequence rendered incrementally without allocating PCM storage.
pub struct ToneSequence {
    handle: NonNull<c_void>,
    rendered_samples: usize,
    render: unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}

// Each handle has unique ownership; the producer transfers it between threads,
// never using it concurrently. Cached entrypoints remain valid for the DSO lifetime.
unsafe impl Send for ToneSequence {}

impl ToneSequence {
    /// Validate the released provider during startup, even without configured tones.
    pub fn provider_ready() -> Result<(), ToneError> {
        provider_api().map(|_| ())
    }

    /// Parse one bounded tone sequence at the fixed native rate.
    pub fn new(text: &str, default_level_db: i8) -> Result<Self, ToneError> {
        Self::from_api(provider_api()?, text, default_level_db)
    }

    fn from_api(api: ToneApi, text: &str, default_level_db: i8) -> Result<Self, ToneError> {
        let mut raw = std::ptr::null_mut();
        if unsafe { (api.create)(text.as_ptr(), text.len(), default_level_db, &mut raw) } != 0 {
            if let Some(handle) = NonNull::new(raw) {
                unsafe { (api.destroy)(handle.as_ptr()) };
            }
            return Err(ToneError);
        }
        let handle = NonNull::new(raw).ok_or(ToneError)?;
        let mut rendered_samples = 0;
        let status = unsafe { (api.rendered_samples)(handle.as_ptr(), &mut rendered_samples) };
        if status != 0 {
            unsafe { (api.destroy)(handle.as_ptr()) };
            return Err(ToneError);
        }
        Ok(Self {
            handle,
            rendered_samples,
            render: api.render,
            destroy: api.destroy,
        })
    }

    /// Return the sequence's total native-rate sample count.
    pub fn rendered_samples(&self) -> usize {
        self.rendered_samples
    }

    /// Render the next samples into the telemetry producer's bounded PCM chunk.
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        let mut written = 0;
        // The validated table and uniquely owned handle are cached at setup.
        let status = unsafe {
            (self.render)(
                self.handle.as_ptr(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        // A valid owned handle and Rust slice cannot be rejected by ABI 1. If a
        // broken provider nevertheless does so, return no samples without
        // allocating, logging, or unwinding on the producer path.
        if status != 0 {
            return 0;
        }
        written.min(output.len())
    }
}

impl Drop for ToneSequence {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

impl AudioSource for ToneSequence {
    fn render(&mut self, output: &mut [f32]) -> usize {
        self.render(output)
    }
}

#[cfg(test)]
#[path = "tone_tests.rs"]
mod tests;
