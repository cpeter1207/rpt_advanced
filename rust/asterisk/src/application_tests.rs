use super::*;
use crate::fixture::{host, reset};
use std::{
    cell::RefCell,
    ffi::{c_char, c_int},
};
struct State {
    failure: u8,
    tech: Box<ffi::ast_channel_tech>,
    caller: Box<ffi::ast_party_caller>,
    unlocked: usize,
    moved: usize,
    answered: usize,
}
impl Default for State {
    fn default() -> Self {
        let mut tech: Box<ffi::ast_channel_tech> = Box::new(unsafe { std::mem::zeroed() });
        tech.type_ = c"IAX2".as_ptr();
        let mut caller: Box<ffi::ast_party_caller> = Box::new(unsafe { std::mem::zeroed() });
        caller.id.number.valid = 1;
        caller.id.number.str_ = c"2000".as_ptr().cast_mut();
        Self {
            failure: 0,
            tech,
            caller,
            unlocked: 0,
            moved: 0,
            answered: 0,
        }
    }
}
thread_local! { static STATE:RefCell<State>=RefCell::new(State::default()); }
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_tech(_: *const ffi::ast_channel) -> *const ffi::ast_channel_tech {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.failure == 8 {
            return ptr::null();
        }
        s.tech.type_ = if s.failure == 1 {
            c"SIP".as_ptr()
        } else if s.failure == 9 {
            ptr::null()
        } else {
            c"IAX2".as_ptr()
        };
        ptr::from_ref(&*s.tech)
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_caller(_: *mut ffi::ast_channel) -> *mut ffi::ast_party_caller {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.failure == 10 {
            return ptr::null_mut();
        }
        s.caller.id.number.valid = u8::from(s.failure != 2);
        s.caller.id.number.str_ = match s.failure {
            11 => ptr::null_mut(),
            12 => c"\xff".as_ptr().cast_mut(),
            _ => c"2000".as_ptr().cast_mut(),
        };
        ptr::from_mut(&mut *s.caller)
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_func_read(
    _: *mut ffi::ast_channel,
    name: *const c_char,
    out: *mut c_char,
    length: usize,
) -> c_int {
    assert_eq!(unsafe { CStr::from_ptr(name) }, c"CHANNEL(peerip)");
    STATE.with(|s| {
        let s = s.borrow();
        if s.failure == 3 {
            -1
        } else {
            unsafe {
                if s.failure == 7 {
                    ptr::write_bytes(out, b'x', length);
                } else if s.failure == 13 {
                    ptr::copy_nonoverlapping(c"\xff".as_ptr(), out, 2);
                } else {
                    ptr::copy_nonoverlapping(c"192.0.2.1".as_ptr(), out, 10);
                }
            }
            0
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_uniqueid(_: *const ffi::ast_channel) -> *const c_char {
    STATE.with(|s| {
        if s.borrow().failure == 14 {
            ptr::null()
        } else {
            c"unique".as_ptr()
        }
    })
}
// The public variadic allocator is called with exactly two pointer formatting arguments.
#[unsafe(no_mangle)]
unsafe extern "C" fn __ast_channel_alloc(
    queue: c_int,
    state: c_int,
    number: *const c_char,
    _name: *const c_char,
    _account: *const c_char,
    _extension: *const c_char,
    _context: *const c_char,
    _ids: *const c_void,
    source: *const ffi::ast_channel,
    ama: ffi::ama_flags,
    _endpoint: *mut c_void,
    _file: *const c_char,
    _line: c_int,
    _function: *const c_char,
    format: *const c_char,
    remote: *const c_char,
    unique: *const c_char,
) -> *mut ffi::ast_channel {
    assert_eq!(queue, 1);
    assert_eq!(state, ffi::AST_STATE_DOWN as i32);
    assert_eq!(source as usize, 1);
    assert_eq!(ama, ffi::AST_AMA_NONE);
    unsafe {
        assert_eq!(CStr::from_ptr(number), c"2000");
        assert_eq!(CStr::from_ptr(format), c"RptAdvanced/%s-%s");
        assert_eq!(CStr::from_ptr(remote), c"2000");
        assert_eq!(CStr::from_ptr(unique), c"unique");
    }
    if STATE.with(|s| s.borrow().failure == 4) {
        return ptr::null_mut();
    }
    let native = host(|s| std::mem::replace(&mut s.native, 0));
    let connection = crate::connection::Connection::open(c"usb").unwrap();
    host(|s| s.native = native);
    let pointer = connection.channel.pointer.as_ptr();
    std::mem::forget(connection.channel);
    pointer
}
#[unsafe(no_mangle)]
unsafe extern "C" fn __ao2_unlock(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
    _: c_int,
    _: *const c_char,
) -> c_int {
    STATE.with(|s| s.borrow_mut().unlocked += 1);
    0
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_move(_: *mut ffi::ast_channel, _: *mut ffi::ast_channel) -> c_int {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        assert_eq!(s.unlocked, 1);
        s.moved += 1;
        if s.failure == 5 { -1 } else { 0 }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_answer(_: *mut ffi::ast_channel) -> c_int {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.answered += 1;
        host(|state| match s.failure {
            15 => state.failure = 23,
            16 => state.failure = 26,
            17 => state.missing_linear = true,
            _ => (),
        });
        if s.failure == 6 { -1 } else { 0 }
    })
}

#[test]
fn missing_and_malformed_public_metadata_never_retains_a_channel() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    reset();
    // SAFETY: fixture recognizes the sentinel channel without dereferencing it.
    unsafe {
        STATE.with(|s| *s.borrow_mut() = State::default());
        assert_eq!(callback(ptr::null_mut(), ptr::null()), -1);
        assert_eq!(callback(ptr::null_mut(), c"1000".as_ptr()), -1);
        assert_eq!(
            callback(1_usize as *mut ffi::ast_channel, c"1000".as_ptr()),
            -1
        );
        assert!(IncomingIdentity::inspect(ptr::null_mut(), c"1000").is_err());
        for local in [c"", c"\xff"] {
            assert!(IncomingIdentity::inspect(1_usize as *mut c_void, local).is_err());
        }
        for failure in 8..=17 {
            STATE.with(|s| {
                *s.borrow_mut() = State {
                    failure,
                    ..State::default()
                }
            });
            let identity = IncomingIdentity::inspect(1_usize as *mut c_void, c"1000");
            if failure <= 13 {
                assert!(identity.is_err());
            } else {
                assert!(
                    identity
                        .unwrap()
                        .handoff(1_usize as *mut c_void, 960)
                        .is_err()
                );
            }
            host(|state| {
                state.clean();
                state.failure = 0;
                state.missing_linear = false;
            });
        }
        let mut identity = IncomingIdentity {
            local: "1000".into(),
            remote: "2000".into(),
            address: "192.0.2.1".into(),
        };
        assert!(identity.handoff(ptr::null_mut(), 960).is_err());
        identity.remote.push('\0');
        assert!(identity.handoff(1_usize as *mut c_void, 960).is_err());
    }
}

#[test]
fn loaded_product_does_not_receive_a_failed_channel_handoff() {
    crate::lifecycle::tests::with_running(|| {
        crate::link::directory::tests::valid_record();
        assert_eq!(
            unsafe { callback(1_usize as *mut ffi::ast_channel, c"missing".as_ptr()) },
            -1
        );
        STATE.with(|state| {
            assert_eq!(state.borrow().moved, 0);
            assert_eq!(state.borrow().answered, 0);
        });
        STATE.with(|s| {
            *s.borrow_mut() = State {
                failure: 4,
                ..State::default()
            }
        });
        // SAFETY: fixture recognizes the borrowed dialplan channel sentinel.
        assert_eq!(
            unsafe { callback(1_usize as *mut ffi::ast_channel, c"1000".as_ptr()) },
            -1
        );
    });
}

#[test]
fn incoming_wire_codec_is_converted_to_same_rate_linear() {
    reset();
    STATE.with(|s| *s.borrow_mut() = State::default());
    host(|s| s.native = 1);
    let identity = unsafe { IncomingIdentity::inspect(1_usize as *mut c_void, c"1000") }.unwrap();
    let peer = unsafe { identity.handoff(1_usize as *mut c_void, 960) }.unwrap();
    assert_eq!(peer.rate(), 16000);
    drop(peer);
    host(|s| s.clean());
}

#[test]
fn admission_return_code_releases_declined_peer_but_never_reclaims_transferred_peer() {
    crate::lifecycle::tests::with_running(|| {
        for result in [0, 1, -1] {
            reset();
            STATE.with(|state| *state.borrow_mut() = State::default());
            crate::lifecycle::tests::incoming_result(result);
            assert_eq!(
                unsafe { callback(1_usize as *mut ffi::ast_channel, c"1000".as_ptr()) },
                if result == 0 { 0 } else { -1 }
            );
            host(|state| state.clean());
        }
    });
}
#[test]
fn copied_identity_and_every_handoff_failure_preserve_exact_channel_ownership() {
    for failure in 0..=7 {
        reset();
        STATE.with(|s| {
            *s.borrow_mut() = State {
                failure,
                ..State::default()
            }
        });
        let identity = unsafe { IncomingIdentity::inspect(1_usize as *mut c_void, c"1000") };
        if matches!(failure, 1 | 2 | 3 | 7) {
            assert!(matches!(identity, Err(Error::Admission)));
            continue;
        }
        let identity = identity.unwrap();
        assert_eq!(
            (&*identity.local, &*identity.remote, &*identity.address),
            ("1000", "2000", "192.0.2.1")
        );
        let result = unsafe { identity.handoff(1_usize as *mut c_void, 960) };
        assert_eq!(result.is_ok(), failure == 0);
        drop(result);
        host(|s| s.clean());
        STATE.with(|s| {
            let s = s.borrow();
            assert_eq!(s.unlocked, usize::from(failure != 4));
            assert_eq!(s.moved, usize::from(failure != 4));
            assert_eq!(s.answered, usize::from(!matches!(failure, 4 | 5)));
        });
    }
}
