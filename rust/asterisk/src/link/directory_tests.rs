use super::*;
use std::{cell::Cell, ffi::c_char};

thread_local! {
    static MODE: Cell<u8> = const { Cell::new(0) };
    static DESTROYED: Cell<usize> = const { Cell::new(0) };
    static CLEANED: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn valid_record() {
    MODE.set(5);
}

#[test]
fn native_directory_copies_owned_results_and_cleans_all_resolver_outcomes() {
    let directory = AsteriskDirectory;
    for operation in [0, 1, 2] {
        let error = match operation {
            0 => directory.record("bad\0path", "123").map(|_| ()),
            1 => directory.srv("bad\0service").map(|_| ()),
            _ => directory.addresses("bad\0host").map(|_| ()),
        };
        assert_eq!(error, Err(DirectoryError::Rejected));
    }
    assert_eq!(
        directory.record("valid", "bad\0node"),
        Err(DirectoryError::Rejected)
    );
    DESTROYED.set(0);
    for mode in 0..=5 {
        MODE.set(mode);
        let result = directory.record("static", "123");
        match mode {
            0..=3 => assert_eq!(result, Ok(None)),
            4 => assert_eq!(result, Err(DirectoryError::Rejected)),
            _ => assert_eq!(result, Ok(Some("radio@host/123,192.0.2.1".into()))),
        }
    }
    assert_eq!(DESTROYED.get(), 3);
    CLEANED.set(0);
    for mode in 0..=3 {
        MODE.set(mode);
        let result = directory.srv("_iax._udp.example");
        match mode {
            0 => assert_eq!(result, Ok(None)),
            1 | 2 => assert_eq!(result, Err(DirectoryError::Rejected)),
            _ => assert_eq!(result, Ok(Some(("srv.example".into(), 4570)))),
        }
    }
    assert_eq!(CLEANED.get(), 4);
    MODE.set(0);
    assert_eq!(directory.addresses("absent"), Ok(vec![]));
    MODE.set(1);
    assert_eq!(directory.addresses("empty"), Ok(vec![]));
    MODE.set(2);
    assert_eq!(
        directory.addresses("mixed"),
        Ok(vec![
            "192.0.2.1".parse().unwrap(),
            "2001:db8::1".parse().unwrap()
        ])
    );
}

#[unsafe(no_mangle)]
extern "C" fn ast_config_load2(
    _: *const c_char,
    _: *const c_char,
    _: ffi::ast_flags,
) -> *mut ffi::ast_config {
    match MODE.get() {
        0 => ptr::null_mut(),
        1 => usize::MAX as *mut _,
        2 => (usize::MAX - 1) as *mut _,
        _ => ptr::dangling_mut(),
    }
}
#[unsafe(no_mangle)]
extern "C" fn ast_config_destroy(_: *mut ffi::ast_config) {
    DESTROYED.set(DESTROYED.get() + 1);
}
#[unsafe(no_mangle)]
extern "C" fn ast_variable_retrieve(
    _: *mut ffi::ast_config,
    _: *const c_char,
    _: *const c_char,
) -> *const c_char {
    match MODE.get() {
        3 => ptr::null(),
        4 => c"\xff".as_ptr(),
        _ => c"radio@host/123,192.0.2.1".as_ptr(),
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_srv_lookup(
    _: *mut *mut ffi::srv_context,
    _: *const c_char,
    host: *mut *const c_char,
    port: *mut u16,
) -> i32 {
    // SAFETY: the production caller provides both writable output slots.
    unsafe {
        *host = match MODE.get() {
            0 | 1 => ptr::null(),
            2 => c"\xff".as_ptr(),
            _ => c"srv.example".as_ptr(),
        };
        *port = 4570;
    }
    i32::from(MODE.get() == 0)
}
#[unsafe(no_mangle)]
extern "C" fn ast_srv_cleanup(_: *mut *mut ffi::srv_context) {
    CLEANED.set(CLEANED.get() + 1);
}
unsafe extern "C" {
    fn calloc(count: usize, bytes: usize) -> *mut std::ffi::c_void;
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_sockaddr_resolve(
    addresses: *mut *mut ffi::ast_sockaddr,
    _: *const c_char,
    _: i32,
    _: i32,
) -> i32 {
    let count = match MODE.get() {
        0 => -1,
        1 => 0,
        _ => 4,
    };
    if count > 0 {
        // SAFETY: calloc creates the exact array freed by the production caller.
        unsafe {
            *addresses = calloc(count as usize, size_of::<ffi::ast_sockaddr>()).cast();
            assert!(!(*addresses).is_null());
            for index in 0..count as usize {
                (*(*addresses).add(index)).len = index as u32;
            }
        }
    }
    count
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_sockaddr_stringify_fmt(
    address: *const ffi::ast_sockaddr,
    _: i32,
) -> *mut c_char {
    // SAFETY: address is one live element from the fixture array above; strings are read-only.
    match unsafe { (*address).len } {
        0 => ptr::null_mut(),
        1 => c"invalid".as_ptr().cast_mut(),
        2 => c"192.0.2.1".as_ptr().cast_mut(),
        _ => c"[2001:db8::1]".as_ptr().cast_mut(),
    }
}
