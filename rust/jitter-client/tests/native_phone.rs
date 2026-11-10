#![cfg(target_os = "linux")]

use rptadv_jitter_client::{Admission, Jitter, Kind};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    path::PathBuf,
};

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

#[test]
fn phone_profile_interoperates_with_the_versioned_native_jitter_provider() {
    let directory = std::env::var_os("RPT_ADVANCED_LIBDIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let path = CString::new(
        directory
            .join("librptadv_jitter_adapter.so.1")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    // SAFETY: the path and symbol strings remain live; this setup-only test
    // retains the library until both exclusive contexts have been destroyed.
    let library = unsafe { dlopen(path.as_ptr(), 2) };
    assert!(!library.is_null(), "load required native provider {path:?}");
    let symbol = unsafe { dlsym(library, c"rptadv_jitter_adapter_descriptor_v1".as_ptr()) };
    assert!(!symbol.is_null());
    let descriptor: unsafe extern "C" fn() -> *const c_void =
        unsafe { std::mem::transmute(symbol) };
    {
        let descriptor = unsafe { descriptor() };
        let mut phone = unsafe { Jitter::phone(descriptor) }.unwrap();
        let ordinary = unsafe { Jitter::new_at_rate(descriptor, 8000, 160, 160) }.unwrap();
        assert_eq!(phone.observe().unwrap().delay, 320);
        assert_eq!(ordinary.observe().unwrap().delay, 1440);
        assert_eq!(phone.release_span(), 160);
        assert!(phone.put(1000, 160, 20_000_000, &[0; 161]).is_err());
        let packet = [0x51; 160];
        assert_eq!(
            phone.put(1000, 160, 20_000_000, &packet),
            Ok(Admission::Accepted)
        );
        let mut payload = [0; 160];
        let waiting = phone.get(20_000_000, 160, &mut payload).unwrap();
        assert_eq!(waiting.kind, Kind::NotDue);
        let due = phone.get(60_000_000, 160, &mut payload).unwrap();
        assert_eq!(due.kind, Kind::Packet);
        assert_eq!(
            (due.timestamp, due.span, due.start_offset, due.length),
            (1000, 160, 0, 160)
        );
        assert_eq!(payload, packet);
        phone.advance(60_000_000, 0).unwrap();
    }
    assert_eq!(unsafe { dlclose(library) }, 0);
}
