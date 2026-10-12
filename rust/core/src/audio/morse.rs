//! Streaming Morse audio supplied by the released ABI 1 Morse library.

use super::AudioSource;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

/// A rejected Morse configuration or unavailable/incompatible provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MorseError;

#[repr(C)]
#[derive(Clone, Copy)]
struct MorseDescriptor {
    struct_size: u32,
    abi_version: u32,
    capability_name: *const c_char,
    create: Option<unsafe extern "C" fn(*const u8, usize, u32, f32, i8, *mut *mut c_void) -> i32>,
    render: Option<unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32>,
    restart: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[derive(Clone, Copy)]
struct MorseApi {
    create: unsafe extern "C" fn(*const u8, usize, u32, f32, i8, *mut *mut c_void) -> i32,
    render: unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32,
    restart: unsafe extern "C" fn(*mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}

unsafe extern "C" {
    fn rptadv_morse_descriptor() -> *const MorseDescriptor;
}

static API: OnceLock<Result<MorseApi, MorseError>> = OnceLock::new();

fn provider_api() -> Result<MorseApi, MorseError> {
    *API.get_or_init(|| unsafe { validate_descriptor(rptadv_morse_descriptor()) })
}

/// Check the size prefix before reading the complete ABI table.
///
/// # Safety
/// `pointer` must address a readable prefix and, when its reported size is
/// sufficient, a complete immutable descriptor for this call.
unsafe fn validate_descriptor(pointer: *const MorseDescriptor) -> Result<MorseApi, MorseError> {
    if pointer.is_null() {
        return Err(MorseError);
    }
    let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read() };
    if size < std::mem::size_of::<MorseDescriptor>() as u32 {
        return Err(MorseError);
    }
    let abi = unsafe { std::ptr::addr_of!((*pointer).abi_version).read() };
    if abi != 1 {
        return Err(MorseError);
    }
    let descriptor = unsafe { &*pointer };
    if descriptor.capability_name.is_null()
        || unsafe { CStr::from_ptr(descriptor.capability_name) } != c"rptadv.morse"
    {
        return Err(MorseError);
    }
    Ok(MorseApi {
        create: descriptor.create.ok_or(MorseError)?,
        render: descriptor.render.ok_or(MorseError)?,
        restart: descriptor.restart.ok_or(MorseError)?,
        destroy: descriptor.destroy.ok_or(MorseError)?,
    })
}

/// Prevalidated Morse source with provider operations cached before rendering.
pub struct MorseRenderer {
    handle: NonNull<c_void>,
    render: unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32,
    restart: unsafe extern "C" fn(*mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}

// The uniquely owned handle can move between threads, but is never used by
// concurrent producers. Cached entrypoints remain valid for the DSO lifetime.
unsafe impl Send for MorseRenderer {}

impl MorseRenderer {
    /// Validate the mandatory released provider even without configured Morse.
    pub fn provider_ready() -> Result<(), MorseError> {
        provider_api().map(|_| ())
    }

    /// Validate and prepare one ASCII Morse message for fixed 48 kHz rendering.
    pub fn new(
        text: &str,
        speed_wpm: u32,
        frequency_hz: f32,
        level_db: i8,
    ) -> Result<Self, MorseError> {
        Self::from_api(provider_api()?, text, speed_wpm, frequency_hz, level_db)
    }

    fn from_api(
        api: MorseApi,
        text: &str,
        speed_wpm: u32,
        frequency_hz: f32,
        level_db: i8,
    ) -> Result<Self, MorseError> {
        let mut raw = std::ptr::null_mut();
        let status = unsafe {
            (api.create)(
                text.as_ptr(),
                text.len(),
                speed_wpm,
                frequency_hz,
                level_db,
                &mut raw,
            )
        };
        if status != 0 {
            if let Some(handle) = NonNull::new(raw) {
                unsafe { (api.destroy)(handle.as_ptr()) };
            }
            return Err(MorseError);
        }
        let handle = NonNull::new(raw).ok_or(MorseError)?;
        Ok(Self {
            handle,
            render: api.render,
            restart: api.restart,
            destroy: api.destroy,
        })
    }

    /// Render as many samples as fit without setup work on the producer path.
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        let mut written = 0;
        let status = unsafe {
            (self.render)(
                self.handle.as_ptr(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        if status != 0 {
            return 0;
        }
        written.min(output.len())
    }

    /// Reset this validated renderer to the start without allocating.
    pub fn restart(&mut self) {
        // The owned handle is valid; ABI 1 reports no other restart failure.
        let _ = unsafe { (self.restart)(self.handle.as_ptr()) };
    }
}

impl Drop for MorseRenderer {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

impl AudioSource for MorseRenderer {
    fn render(&mut self, output: &mut [f32]) -> usize {
        self.render(output)
    }
}

#[cfg(test)]
#[path = "morse_tests.rs"]
mod tests;
