//! Test-only independent consumer of the released 8 kHz DTMF detector ABI.
use std::{
    ffi::{CStr, c_char, c_void},
    mem::size_of,
    ptr,
};

type Digit = Option<extern "C" fn(*mut c_void, u8)>;
#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    size: u32,
    version: u32,
    capability: *const c_char,
    create: Option<unsafe extern "C" fn(u32, *mut *mut c_void) -> i32>,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    set_muting: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    suppressing: Option<unsafe extern "C" fn(*const c_void) -> u32>,
    process: Option<unsafe extern "C" fn(*mut c_void, u32, *mut f32, u32, Digit, *mut c_void)>,
    generator_create: Option<unsafe extern "C" fn(u8, u32, u32, i32, *mut *mut c_void) -> i32>,
    generator_render: Option<unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut usize) -> i32>,
    generator_destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    create_at_rate: Option<unsafe extern "C" fn(u32, u32, *mut *mut c_void) -> i32>,
}

pub(super) struct Decoder {
    api: Descriptor,
    handle: *mut c_void,
    _library: libloading::Library,
}
impl Decoder {
    pub(super) fn new() -> Self {
        // SAFETY: the same released ABI-1 library is already required by product.
        let library = unsafe { libloading::Library::new("librptadvdtmf.so.1") }.unwrap();
        let api = unsafe {
            let entry = library
                .get::<unsafe extern "C" fn() -> *const Descriptor>(b"rptadv_dtmf_descriptor\0")
                .unwrap();
            let pointer = entry();
            assert!(!pointer.is_null());
            assert!(ptr::read(pointer.cast::<u32>()) >= size_of::<Descriptor>() as u32);
            pointer.read()
        };
        assert_eq!(api.version, 1);
        assert!(!api.capability.is_null());
        // SAFETY: released immutable capability is retained with this library.
        assert_eq!(unsafe { CStr::from_ptr(api.capability) }, c"rptadv.dtmf");
        let mut owner = Self {
            api,
            handle: ptr::null_mut(),
            _library: library,
        };
        assert!(owner.api.destroy.is_some() && owner.api.process.is_some());
        // SAFETY: the selected rate creates one unique borrowed-callback owner.
        assert_eq!(
            unsafe { owner.api.create_at_rate.unwrap()(0, 8000, &mut owner.handle) },
            0
        );
        assert!(!owner.handle.is_null());
        owner
    }
    pub(super) fn process(&mut self, pcm: &mut [f32], digits: &mut Vec<u8>) {
        extern "C" fn digit(context: *mut c_void, symbol: u8) {
            // SAFETY: caller retains this preallocated vector synchronously.
            let output = unsafe { &mut *context.cast::<Vec<u8>>() };
            if output.len() < output.capacity() {
                output.push(symbol);
            }
        }
        // SAFETY: no callback retains the borrowed PCM or preallocated output.
        unsafe {
            self.api.process.unwrap()(
                self.handle,
                1,
                pcm.as_mut_ptr(),
                pcm.len() as u32,
                Some(digit),
                ptr::from_mut(digits).cast(),
            );
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this unique test owner still retains the function library.
            unsafe { self.api.destroy.unwrap()(self.handle) };
        }
    }
}
