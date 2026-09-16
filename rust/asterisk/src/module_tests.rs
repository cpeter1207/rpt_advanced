use super::*;
use crate::bindings as ffi;
use std::{
    cell::RefCell,
    ffi::{c_char, c_int},
    ptr,
};

#[derive(Default)]
struct Host {
    failure: u8,
    module: usize,
    application: bool,
    entries: usize,
    unregisters: Vec<&'static str>,
}
thread_local! { static HOST: RefCell<Host> = RefCell::new(Host::default()); }
pub(crate) fn fail_registration(failure: u8) {
    HOST.with(|host| host.borrow_mut().failure = failure);
}
unsafe extern "C" fn incoming(_: *mut ffi::ast_channel, _: *const c_char) -> c_int {
    0
}
unsafe extern "C" fn cli(
    _: *mut ffi::ast_cli_entry,
    _: c_int,
    _: *mut ffi::ast_cli_args,
) -> *mut c_char {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ast_register_application2(
    _: *const c_char,
    _: Option<unsafe extern "C" fn(*mut ffi::ast_channel, *const c_char) -> c_int>,
    _: *const c_char,
    _: *const c_char,
    module: *mut ffi::ast_module,
) -> c_int {
    HOST.with(|host| {
        let mut h = host.borrow_mut();
        h.module = module as usize;
        if h.failure == 1 {
            -1
        } else {
            h.application = true;
            0
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_unregister_application(_: *const c_char) -> c_int {
    HOST.with(|host| {
        let mut h = host.borrow_mut();
        h.application = false;
        h.unregisters.push("application");
    });
    0
}
#[unsafe(no_mangle)]
unsafe extern "C" fn __ast_cli_register_multiple(
    entries: *mut ffi::ast_cli_entry,
    count: c_int,
    module: *mut ffi::ast_module,
) -> c_int {
    HOST.with(|host| {
        let mut h = host.borrow_mut();
        assert_eq!(h.module, module as usize);
        assert_eq!(count, 3);
        h.entries = entries as usize;
        if h.failure == 2 { -1 } else { 0 }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_cli_unregister_multiple(
    entries: *mut ffi::ast_cli_entry,
    count: c_int,
) -> c_int {
    HOST.with(|host| {
        let mut h = host.borrow_mut();
        assert_eq!(h.entries, entries as usize);
        assert_eq!(count, 3);
        h.entries = 0;
        h.unregisters.push("cli");
    });
    0
}
#[test]
fn registration_passes_real_module_identity_and_rolls_back_partial_cli_registration() {
    assert!(matches!(
        unsafe { Registration::register(ptr::null_mut(), incoming, [cli; 3]) },
        Err(crate::Error::Registration)
    ));
    for failure in 0..=2 {
        HOST.with(|host| {
            *host.borrow_mut() = Host {
                failure,
                ..Host::default()
            }
        });
        let registration = unsafe {
            Registration::register(0x1234_usize as *mut ffi::ast_module, incoming, [cli; 3])
        };
        assert_eq!(registration.is_ok(), failure == 0);
        HOST.with(|host| {
            let h = host.borrow();
            assert_eq!(h.module, 0x1234);
            if failure == 0 {
                assert!(h.application);
                assert_ne!(h.entries, 0);
            }
        });
        drop(registration);
        HOST.with(|host| {
            let h = host.borrow();
            assert!(!h.application);
            assert_eq!(h.entries, 0);
            assert_eq!(
                h.unregisters,
                if failure == 1 {
                    vec![]
                } else {
                    vec!["cli", "application"]
                }
            );
        });
    }
}
