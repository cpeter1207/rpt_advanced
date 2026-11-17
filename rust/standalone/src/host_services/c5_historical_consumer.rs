//! Retain one independently compiled old-header callback through actual native destroy.
use super::{BindingV2, HISTORICAL_BINDING, NATIVE_LIVE};
use std::{ffi::CString, sync::atomic::Ordering};

pub(super) struct Consumer {
    library: *mut libc::c_void,
    calls: unsafe extern "C" fn() -> u32,
    invalid: unsafe extern "C" fn() -> u32,
}
impl Consumer {
    pub(super) fn load(size: u32) -> Self {
        let path = CString::new(
            std::env::var("C5_OLD_CONSUMER").expect("explicit old-header consumer DSO"),
        )
        .unwrap();
        let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        assert!(!library.is_null(), "old-header callback must load");
        let symbol = |name: &std::ffi::CStr| {
            let value = unsafe { libc::dlsym(library, name.as_ptr()) };
            assert!(!value.is_null(), "missing old-header callback export");
            value
        };
        let initialize: unsafe extern "C" fn(u32) -> *mut BindingV2 =
            unsafe { std::mem::transmute(symbol(c"c5_old_binding")) };
        let binding = unsafe { initialize(size) };
        assert!(!binding.is_null());
        HISTORICAL_BINDING.store(binding as usize, Ordering::Release);
        Self {
            library,
            calls: unsafe {
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn() -> u32>(symbol(
                    c"c5_old_calls",
                ))
            },
            invalid: unsafe {
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn() -> u32>(symbol(
                    c"c5_old_invalid",
                ))
            },
        }
    }
    pub(super) fn verify(&self, size: u32) {
        let calls = unsafe { (self.calls)() };
        let invalid = unsafe { (self.invalid)() };
        eprintln!(
            "C5_OLD_CONSUMER known_size=96 reported_size={size} callbacks={calls} invalid={invalid} clock=Unknown physical=Unknown"
        );
        assert!(calls > 0, "old C callback must actually execute");
        assert_eq!(invalid, 0, "old C callback prefix validation");
    }
}
impl Drop for Consumer {
    fn drop(&mut self) {
        // An unexpected failed teardown must retain callback code/context, not cause a UAF.
        if NATIVE_LIVE.load(Ordering::Acquire) != 0 {
            return;
        }
        HISTORICAL_BINDING.store(0, Ordering::Release);
        assert_eq!(unsafe { libc::dlclose(self.library) }, 0);
    }
}
