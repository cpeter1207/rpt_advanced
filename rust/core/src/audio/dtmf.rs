//! Borrowed DTMF events and finite PCM generation from the released ABI 1 provider.

use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

/// An unavailable or incompatible detector provider, or failed state creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfError;

#[repr(C)]
#[derive(Clone, Copy)]
struct DtmfDescriptor {
    struct_size: u32,
    abi_version: u32,
    capability_name: *const c_char,
    create: Option<unsafe extern "C" fn(u32, *mut *mut c_void) -> i32>,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    set_muting: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    suppressing: Option<unsafe extern "C" fn(*const c_void) -> u32>,
    process: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u32,
            *mut f32,
            u32,
            Option<extern "C" fn(*mut c_void, u8)>,
            *mut c_void,
        ),
    >,
}

#[derive(Clone, Copy)]
struct DtmfApi {
    create: unsafe extern "C" fn(u32, *mut *mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    set_muting: unsafe extern "C" fn(*mut c_void, u32),
    suppressing: unsafe extern "C" fn(*const c_void) -> u32,
    process: unsafe extern "C" fn(
        *mut c_void,
        u32,
        *mut f32,
        u32,
        Option<extern "C" fn(*mut c_void, u8)>,
        *mut c_void,
    ),
}

unsafe extern "C" {
    fn rptadv_dtmf_descriptor() -> *const DtmfDescriptor;
}

static API: OnceLock<Result<DtmfApi, DtmfError>> = OnceLock::new();

fn provider_api() -> Result<DtmfApi, DtmfError> {
    *API.get_or_init(|| unsafe { validate_descriptor(rptadv_dtmf_descriptor()) })
}

/// Check the size prefix before reading the complete immutable table.
///
/// # Safety
/// `pointer` must address a readable ABI prefix and, if its size suffices,
/// a complete process-lifetime descriptor.
unsafe fn validate_descriptor(pointer: *const DtmfDescriptor) -> Result<DtmfApi, DtmfError> {
    if pointer.is_null() {
        return Err(DtmfError);
    }
    let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read() };
    if size < std::mem::size_of::<DtmfDescriptor>() as u32 {
        return Err(DtmfError);
    }
    let abi = unsafe { std::ptr::addr_of!((*pointer).abi_version).read() };
    if abi != 1 {
        return Err(DtmfError);
    }
    let descriptor = unsafe { &*pointer };
    if descriptor.capability_name.is_null()
        || unsafe { CStr::from_ptr(descriptor.capability_name) } != c"rptadv.dtmf"
    {
        return Err(DtmfError);
    }
    Ok(DtmfApi {
        create: descriptor.create.ok_or(DtmfError)?,
        destroy: descriptor.destroy.ok_or(DtmfError)?,
        set_muting: descriptor.set_muting.ok_or(DtmfError)?,
        suppressing: descriptor.suppressing.ok_or(DtmfError)?,
        process: descriptor.process.ok_or(DtmfError)?,
    })
}

/// Fixed-state detector for one 48 kHz receive stream; state lives in the provider.
pub struct DtmfDetector {
    handle: NonNull<c_void>,
    api: DtmfApi,
}

// One receive worker uniquely owns the handle. Ownership may move between
// worker generations, but calls never overlap; the DSO remains loaded.
unsafe impl Send for DtmfDetector {}

impl DtmfDetector {
    /// Validate mandatory descriptor capability before publishing audio callbacks.
    pub fn provider_ready() -> Result<(), DtmfError> {
        provider_api().map(|_| ())
    }

    /// Allocate the detector before audio starts; a failed provider is a setup error.
    pub fn new(muting: bool) -> Result<Self, DtmfError> {
        Self::from_api(provider_api()?, muting)
    }

    fn from_api(api: DtmfApi, muting: bool) -> Result<Self, DtmfError> {
        let mut raw = std::ptr::null_mut();
        let status = unsafe { (api.create)(u32::from(muting), &mut raw) };
        if status != 0 {
            if let Some(handle) = NonNull::new(raw) {
                unsafe { (api.destroy)(handle.as_ptr()) };
            }
            return Err(DtmfError);
        }
        let handle = NonNull::new(raw).ok_or(DtmfError)?;
        Ok(Self { handle, api })
    }

    /// Change qualified-tone muting without resetting detector state.
    pub fn set_muting(&mut self, enabled: bool) {
        unsafe { (self.api.set_muting)(self.handle.as_ptr(), u32::from(enabled)) };
    }

    /// Whether the current qualified tone is subject to configured muting.
    #[must_use]
    pub fn suppressing(&self) -> bool {
        unsafe { (self.api.suppressing)(self.handle.as_ptr()) != 0 }
    }

    /// Process and possibly mute PCM; the completed ASCII digit callback is borrowed.
    /// The callback must not unwind, allocate, lock, block, or perform I/O.
    pub fn process(&mut self, receiving: bool, audio: &mut [f32], mut emit: impl FnMut(char)) {
        extern "C" fn callback(context: *mut c_void, digit: u8) {
            // SAFETY: `process` borrows this local callback reference for one call.
            let emit = unsafe { &mut *context.cast::<&mut dyn FnMut(char)>() };
            emit(char::from(digit));
        }
        let mut borrowed: &mut dyn FnMut(char) = &mut emit;
        let context = (&mut borrowed as *mut &mut dyn FnMut(char)).cast::<c_void>();
        if !receiving {
            // The original detector ignores PCM on carrier loss, but completes
            // a qualified digit even for an empty span.
            unsafe {
                (self.api.process)(
                    self.handle.as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    Some(callback),
                    context,
                )
            };
            return;
        }
        for chunk in audio.chunks_mut(u32::MAX as usize) {
            unsafe {
                (self.api.process)(
                    self.handle.as_ptr(),
                    1,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    Some(callback),
                    context,
                )
            };
        }
    }
}

impl Drop for DtmfDetector {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.handle.as_ptr()) };
    }
}

type GeneratorCreate = unsafe extern "C" fn(u8, u32, u32, i32, *mut *mut c_void) -> i32;
type GeneratorRender = unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32;
type GeneratorDestroy = unsafe extern "C" fn(*mut c_void);

/// Optional descriptor extension, checked separately from the detector prefix.
#[repr(C)]
#[derive(Clone, Copy)]
struct GeneratorDescriptor {
    detector: DtmfDescriptor,
    create: Option<GeneratorCreate>,
    render: Option<GeneratorRender>,
    destroy: Option<GeneratorDestroy>,
}

unsafe fn generator_api(
    pointer: *const DtmfDescriptor,
) -> Result<(GeneratorCreate, GeneratorRender, GeneratorDestroy), DtmfError> {
    unsafe {
        validate_descriptor(pointer)?;
    }
    if unsafe { (*pointer).struct_size } < std::mem::size_of::<GeneratorDescriptor>() as u32 {
        return Err(DtmfError);
    }
    // SAFETY: size covers the extension; the provider table is immutable.
    let descriptor = unsafe { *pointer.cast::<GeneratorDescriptor>() };
    Ok((
        descriptor.create.ok_or(DtmfError)?,
        descriptor.render.ok_or(DtmfError)?,
        descriptor.destroy.ok_or(DtmfError)?,
    ))
}

/// Finite 48 kHz mono digit plus silence. Allocate and release on the producer worker.
pub struct DtmfGenerator {
    handle: NonNull<c_void>,
    render: GeneratorRender,
    destroy: GeneratorDestroy,
}

// The generator has one owner, moves between workers, and is never shared concurrently.
unsafe impl Send for DtmfGenerator {}

impl DtmfGenerator {
    /// Reject a provider missing the finite-generator tail before starting the product.
    pub fn provider_ready() -> Result<(), DtmfError> {
        unsafe { generator_api(rptadv_dtmf_descriptor()) }.map(|_| ())
    }
    /// Validate settings and allocate provider state without involving audio callbacks.
    pub fn new(
        digit: char,
        duration_ms: u32,
        gap_ms: u32,
        level_dbfs: i8,
    ) -> Result<Self, DtmfError> {
        let digit = u8::try_from(u32::from(digit)).map_err(|_| DtmfError)?;
        let (create, render, destroy) = unsafe { generator_api(rptadv_dtmf_descriptor())? };
        let mut handle = std::ptr::null_mut();
        if unsafe {
            create(
                digit,
                duration_ms,
                gap_ms,
                i32::from(level_dbfs),
                &mut handle,
            )
        } != 0
        {
            return Err(DtmfError);
        }
        Ok(Self {
            handle: NonNull::new(handle).ok_or(DtmfError)?,
            render,
            destroy,
        })
    }

    /// Render a bounded partition; zero returned samples means finished for nonempty output.
    pub fn render(&mut self, output: &mut [f32]) -> Result<usize, DtmfError> {
        let mut written = 0;
        let status = unsafe {
            (self.render)(
                self.handle.as_ptr(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        };
        if status != 0 || written > output.len() {
            return Err(DtmfError);
        }
        Ok(written)
    }
}

impl Drop for DtmfGenerator {
    fn drop(&mut self) {
        unsafe {
            (self.destroy)(self.handle.as_ptr());
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTROYS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn create_error(_: u32, out: *mut *mut c_void) -> i32 {
        unsafe { *out = NonNull::<c_void>::dangling().as_ptr() };
        -1
    }
    unsafe extern "C" fn create_null(_: u32, _: *mut *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn create_error_null(_: u32, _: *mut *mut c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn set_muting(_: *mut c_void, _: u32) {}
    unsafe extern "C" fn suppressing(_: *const c_void) -> u32 {
        0
    }
    unsafe extern "C" fn process(
        _: *mut c_void,
        _: u32,
        _: *mut f32,
        _: u32,
        _: Option<extern "C" fn(*mut c_void, u8)>,
        _: *mut c_void,
    ) {
    }

    #[test]
    fn foreign_create_failures_release_any_returned_handle() {
        let api = DtmfApi {
            create: create_error,
            destroy,
            set_muting,
            suppressing,
            process,
        };
        DESTROYS.store(0, Ordering::SeqCst);
        assert!(DtmfDetector::from_api(api, false).is_err());
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
        assert!(
            DtmfDetector::from_api(
                DtmfApi {
                    create: create_error_null,
                    ..api
                },
                false,
            )
            .is_err()
        );
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
        assert!(
            DtmfDetector::from_api(
                DtmfApi {
                    create: create_null,
                    ..api
                },
                true,
            )
            .is_err()
        );
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
#[path = "dtmf_tests.rs"]
mod tests;
