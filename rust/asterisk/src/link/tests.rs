use super::directory::{Backend, DirectoryError, DirectoryResolver, Method};
use std::net::IpAddr;

struct Directory {
    static_record: Option<String>,
    external: Option<String>,
    addresses: Vec<IpAddr>,
    srv: Option<(String, u16)>,
}
impl Backend for Directory {
    fn record(&self, path: &str, node: &str) -> Result<Option<String>, DirectoryError> {
        assert_eq!(node, "123");
        Ok(if path == "static" {
            self.static_record.clone()
        } else {
            self.external.clone()
        })
    }
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, DirectoryError> {
        assert_eq!(service, "_iax._udp.123.nodes.allstarlink.org");
        Ok(self.srv.clone())
    }
    fn addresses(&self, host: &str) -> Result<Vec<IpAddr>, DirectoryError> {
        assert!(host == "123.nodes.allstarlink.org" || host == "srv.example");
        Ok(self.addresses.clone())
    }
}

#[test]
fn directory_priorities_fail_closed_and_dns_matches_any_address() {
    let backend = Directory {
        static_record: None,
        external: Some("radio@external/123,192.0.2.1".into()),
        addresses: vec!["192.0.2.2".parse().unwrap(), "192.0.2.1".parse().unwrap()],
        srv: Some(("srv.example".into(), 4570)),
    };
    let mut directory = DirectoryResolver::new(backend, Method::Both, "static", "external");
    assert_eq!(
        directory.lookup("123", Some("192.0.2.1")),
        Ok("radio@192.0.2.1:4570/123".into())
    );
    assert!(directory.lookup("123", Some("192.0.2.3")).is_err());
    directory.backend.static_record = Some("radio@static/999,192.0.2.1".into());
    assert!(directory.lookup("123", Some("192.0.2.1")).is_err());
    directory.backend.static_record = Some("radio@static/123,192.0.2.1".into());
    assert_eq!(
        directory.lookup("123", Some("192.0.2.99")),
        Err(DirectoryError::Rejected)
    );
    assert_eq!(
        directory.lookup("123", Some("192.0.2.1")),
        Ok("radio@static/123".into())
    );
    for record in [
        "missing-separator",
        "wrong@host/123,192.0.2.1",
        "radio@/123,192.0.2.1",
        "radio@ho st/123,192.0.2.1",
        "radio@host\0/123,192.0.2.1",
        "radio@host/123,hostname",
        "radio@host/123,192.0.2.1 ",
        "radio@host/123,192.0.2.1,192.0.2.2",
    ] {
        directory.backend.static_record = Some(record.into());
        assert!(directory.lookup("123", None).is_err(), "{record}");
    }
    for node in ["", "a", "123/1", "123\0"] {
        assert!(directory.lookup(node, None).is_err());
    }
    assert!(directory.lookup(&"1".repeat(64), None).is_err());
}

#[test]
fn directory_absence_falls_through_but_ipv6_and_modes_remain_exact() {
    let backend = Directory {
        static_record: None,
        external: Some("radio@external/123,192.0.2.1".into()),
        addresses: vec![],
        srv: None,
    };
    let mut directory = DirectoryResolver::new(backend, Method::Both, "static", "external");
    assert_eq!(
        directory.lookup("123", None),
        Ok("radio@external/123".into())
    );
    directory.backend.addresses = vec!["2001:db8::1".parse().unwrap()];
    assert_eq!(
        directory.lookup("123", Some("2001:db8::1")),
        Ok("radio@[2001:db8::1]:4569/123".into())
    );
    assert!(directory.lookup("123", Some("host.example")).is_err());
}

#[test]
fn directory_modes_do_not_fall_back_to_disabled_sources_and_mapped_ips_match() {
    for (method, external, expected) in [
        (
            Method::Dns,
            Some("radio@external/123,192.0.2.1"),
            Err(DirectoryError::Absent),
        ),
        (Method::File, None, Err(DirectoryError::Absent)),
        (
            Method::File,
            Some("radio@external/123,::ffff:192.0.2.1"),
            Ok("radio@external/123".into()),
        ),
    ] {
        let directory = DirectoryResolver::new(
            Directory {
                static_record: None,
                external: external.map(str::to_owned),
                addresses: vec![],
                srv: None,
            },
            method,
            "",
            "external",
        );
        assert_eq!(directory.lookup("123", Some("::ffff:192.0.2.1")), expected);
    }
}

#[test]
fn peer_frame_owner_publishes_voice_and_frees_every_frame_and_channel() {
    use super::peer_io::{Input, PeerIo};
    use crate::{
        connection::Connection,
        fixture::{host, reset},
    };
    reset();
    // The live registry owns each source format; retain those references in this fixture.
    let registry = crate::codec::candidates().unwrap();
    let connection = Connection::open(c"usb").unwrap();
    let pointer = connection.channel.pointer.as_ptr().cast();
    std::mem::forget(connection.channel);
    let format = connection.linear;
    // The fixture grants one owned channel exactly as answered IAX ingress does.
    let mut peer = unsafe { PeerIo::from_owned_channel(pointer, format, 960) }.unwrap();
    host(|state| state.voice(vec![8192; 960], 0));
    let mut accepted = 0;
    peer.read(|input| {
        if let Input::Audio(samples) = input {
            accepted = samples.len();
            assert!(samples.iter().all(|sample| *sample == 0.25));
        }
    })
    .unwrap();
    assert_eq!(accepted, 960);
    host(|state| state.voice(vec![], 0));
    peer.read(|_| panic!("empty voice must not publish activity"))
        .unwrap();
    peer.write(&[-2.0, 0.5, 2.0]).unwrap();
    host(|state| assert_eq!(state.writes.last().unwrap(), &[-32768, 16384, 32767]));
    peer.write(&[]).unwrap();
    host(|state| {
        assert_eq!(
            state.write_types.last(),
            Some(&crate::bindings::AST_FRAME_CNG)
        )
    });
    assert!(peer.write(&[0.0; 961]).is_err());
    host(|state| {
        state.buffered = true;
        state.voice(vec![1; 2], 5);
    });
    peer.read(|_| panic!("buffered codec input is not activity"))
        .unwrap();
    host(|state| {
        state.buffered = false;
        state.voice(vec![1; 2], 5);
    });
    assert!(peer.read(|_| {}).is_err());
    host(|state| state.voice(vec![0; 2], 4));
    assert!(peer.read(|_| {}).is_err());
    drop(peer);
    drop(registry);
    host(|state| {
        assert_eq!(state.freed, 5);
        state.clean();
    });
}

#[test]
fn dial_rechecks_cancellation_and_owns_answered_channel() {
    use super::peer_io::PeerIo;
    use crate::fixture::{host, reset};
    reset();
    assert!(PeerIo::dial(c"radio@host/200", c"100", 960, || false).is_err());
    let peer = PeerIo::dial(c"radio@host/200", c"100", 960, || true).unwrap();
    drop(peer);
    host(|state| state.clean());
    for failure in [5, 6, 10, 11, 12, 20, 21, 39] {
        reset();
        host(|state| state.failure = failure);
        assert!(PeerIo::dial(c"radio@host/200", c"100", 960, || true).is_err());
        host(|state| state.clean());
    }
    reset();
    let mut checks = 0;
    assert!(
        PeerIo::dial(c"radio@host/200", c"100", 960, || {
            checks += 1;
            checks == 1
        })
        .is_err()
    );
    host(|state| state.clean());
    reset();
    let mut checks = 0;
    assert!(
        PeerIo::dial(c"radio@host/200", c"100", 960, || {
            checks += 1;
            if checks == 2 {
                host(|state| state.missing_linear = true);
            }
            true
        })
        .is_err()
    );
    host(|state| state.clean());
}

#[test]
fn peer_preparation_failures_release_transferred_channel_and_scratch() {
    use super::peer_io::PeerIo;
    use crate::{
        Error,
        connection::Connection,
        fixture::{host, reset},
    };
    reset();
    for case in 0..8 {
        let connection = Connection::open(c"usb").unwrap();
        let maximum = match case {
            0 => 0,
            1 => usize::MAX,
            _ => 7,
        };
        host(|state| {
            state.missing_linear = case == 2;
            state.failure = match case {
                3 => 5,
                4 => 6,
                7 => 3,
                _ => 0,
            };
        });
        let pointer = connection.channel.pointer.as_ptr().cast();
        std::mem::forget(connection.channel);
        // SAFETY: transfers exactly one fixture channel and its retained linear format.
        let prepare = || unsafe { PeerIo::from_owned_channel(pointer, connection.linear, maximum) };
        let result = match case {
            5 => crate::fixture::fail_allocation(7 * size_of::<f32>(), prepare),
            6 => crate::fixture::fail_allocation(7 * size_of::<i16>(), prepare),
            _ => prepare(),
        };
        assert!(result.is_err(), "case {case}");
        host(|state| {
            state.clean();
            state.failure = 0;
            state.missing_linear = false;
        });
    }
    let connection = Connection::open(c"usb").unwrap();
    // SAFETY: null explicitly exercises rejected ownership; format remains an owned argument.
    assert!(matches!(
        unsafe { PeerIo::from_owned_channel(std::ptr::null_mut(), connection.linear, 7) },
        Err(Error::Reservation)
    ));
    drop(connection.channel);
    host(|state| state.clean());
}

#[test]
fn peer_frame_validation_controls_and_output_errors_keep_one_serial_owner() {
    use super::peer_io::{Input, PeerIo};
    use crate::{
        Error, bindings as ffi,
        fixture::{host, reset},
    };
    reset();
    let mut peer = PeerIo::dial(c"radio@host/200", c"100", 8, || true).unwrap();
    assert_eq!(peer.ready(), Ok(false));
    host(|state| state.failure = 34);
    assert_eq!(peer.ready(), Ok(true));
    host(|state| state.failure = 31);
    assert_eq!(peer.ready(), Err(Error::Hangup));
    host(|state| state.failure = 0);
    assert_eq!(peer.read(|_| {}), Err(Error::Hangup));
    assert_eq!(peer.send_text(c"fail"), Err(Error::Write));
    assert_eq!(peer.send_digit('x'), Err(Error::InvalidFrame));
    assert_eq!(peer.send_digit('1'), Ok(()));
    host(|state| state.failure = 33);
    assert_eq!(peer.send_digit('1'), Err(Error::Write));
    host(|state| state.failure = 8);
    assert_eq!(peer.write(&[0.5]), Err(Error::Write));
    host(|state| state.failure = 0);
    for malformed in [1, 2, 3, 4, 6] {
        host(|state| state.voice(vec![1; 2], malformed));
        assert_eq!(
            peer.read(|_| panic!("invalid audio")),
            Err(Error::InvalidFrame)
        );
    }
    host(|state| state.voice(vec![1; 9], 0));
    assert_eq!(peer.read(|_| {}), Err(Error::InvalidFrame));
    for failure in [13, 35, 36, 37] {
        host(|state| {
            state.failure = failure;
            state.voice(vec![1; 2], 5);
        });
        let mut accepted = 0;
        let result = peer.read(|input| {
            if let Input::Audio(samples) = input {
                accepted = samples.len();
            }
        });
        match failure {
            13 => assert_eq!(result, Err(Error::Translation)),
            36 => assert_eq!(result, Err(Error::InvalidFrame)),
            _ => assert_eq!(result, Ok(())),
        }
        assert_eq!(accepted, if failure == 37 { 2 } else { 0 });
    }
    host(|state| state.failure = 0);
    host(|state| {
        state.voice(vec![1; 2], 0);
        state.edit_frame(|frame| frame.subclass.__bindgen_anon_1.format = std::ptr::null_mut());
    });
    assert_eq!(peer.read(|_| {}), Err(Error::InvalidFrame));
    for digit in [-1, 256, i32::from(b'x'), i32::from(b'#')] {
        host(|state| {
            state.control(digit);
            state.edit_frame(|frame| frame.frametype = ffi::AST_FRAME_DTMF_END);
        });
        let mut received = None;
        peer.read(|input| {
            if let Input::Digit(value) = input {
                received = Some(value)
            }
        })
        .unwrap();
        assert_eq!(received, (digit == i32::from(b'#')).then_some('#'));
    }
    for case in 0..3 {
        host(|state| {
            state.voice(vec![0; 2], 0);
            state.edit_frame(|frame| {
                frame.frametype = ffi::AST_FRAME_TEXT;
                if case == 0 {
                    frame.datalen = 0;
                }
                if case == 1 {
                    frame.data.ptr = std::ptr::null_mut();
                }
            });
        });
        let mut length = 0;
        peer.read(|input| {
            if let Input::Text(bytes) = input {
                length = bytes.len();
            }
        })
        .unwrap();
        assert_eq!(length, if case == 2 { 4 } else { 0 });
    }
    host(|state| state.control(ffi::AST_CONTROL_RADIO_KEY as i32));
    peer.read(|_| panic!("ignored control")).unwrap();
    host(|state| state.control(ffi::AST_CONTROL_HANGUP as i32));
    assert_eq!(peer.read(|_| {}), Err(Error::Hangup));
    drop(peer);
    host(|state| state.clean());
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ast_request_and_dial(
    kind: *const std::ffi::c_char,
    _cap: *mut crate::bindings::ast_format_cap,
    _ids: *const crate::bindings::ast_assigned_ids,
    _requestor: *const crate::bindings::ast_channel,
    destination: *const std::ffi::c_char,
    timeout: i32,
    _reason: *mut i32,
    local: *const std::ffi::c_char,
    _name: *const std::ffi::c_char,
) -> *mut crate::bindings::ast_channel {
    assert_eq!(unsafe { std::ffi::CStr::from_ptr(kind) }, c"IAX2");
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(destination) },
        c"radio@host/200"
    );
    assert_eq!(unsafe { std::ffi::CStr::from_ptr(local) }, c"100");
    assert!((1..=20000).contains(&timeout));
    if crate::fixture::host(|state| state.failure == 39) {
        // Exercise the real shared dial deadline after one call consumes its allowance.
        std::thread::sleep(std::time::Duration::from_millis(timeout as u64 + 5));
        return std::ptr::null_mut();
    }
    crate::fixture::host(|state| {
        if state.failure == 20 {
            return std::ptr::null_mut();
        }
        state.channels += 1;
        std::ptr::NonNull::dangling().as_ptr()
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_state(
    _: *const crate::bindings::ast_channel,
) -> crate::bindings::ast_channel_state {
    crate::fixture::host(|state| {
        if state.failure == 21 {
            crate::bindings::AST_STATE_DOWN
        } else {
            crate::bindings::AST_STATE_UP
        }
    })
}

thread_local! { static SENT_TEXT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) }; }
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_sendtext(
    _: *mut crate::bindings::ast_channel,
    text: *const std::ffi::c_char,
) -> i32 {
    if unsafe { std::ffi::CStr::from_ptr(text) } == c"fail"
        || crate::fixture::host(|state| state.failure == 40)
    {
        return -1;
    }
    SENT_TEXT.with(|texts| {
        texts.borrow_mut().push(
            unsafe { std::ffi::CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned(),
        )
    });
    0
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_waitfor(channel: *mut crate::bindings::ast_channel, _: i32) -> i32 {
    if crate::fixture::FAILED_READY_CHANNEL.load(std::sync::atomic::Ordering::Acquire)
        == channel as usize
    {
        return -1;
    }
    crate::fixture::host(|state| match state.failure {
        31 => -1,
        34 => 1,
        _ => 0,
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_senddigit(
    _: *mut crate::bindings::ast_channel,
    _: std::ffi::c_char,
    _: u32,
) -> i32 {
    crate::fixture::host(|state| if state.failure == 33 { -1 } else { 0 })
}
