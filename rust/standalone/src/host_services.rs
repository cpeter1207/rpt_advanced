//! Asterisk-free host callbacks for the portable product runtime.

use crate::{
    abi, directory,
    iax::{IaxClient, IaxError, IaxEvent, IaxPeer},
    radio_host::{RadioHostContext, peer_bind_radio, radio_activate, radio_destroy, radio_open},
    secrets::SecretsFile,
};
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
    net::{SocketAddr, ToSocketAddrs},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    ptr,
};

/// Context retained while the product uses the standalone host table.
pub struct StandaloneHostContext {
    radio: RadioHostContext,
    secrets: SecretsFile,
}

/// Owns a complete host-services table and the context it points to.
pub struct HostServicesOwner {
    context: Box<StandaloneHostContext>,
    descriptor: Box<abi::rptadv_host_services_v5>,
}

impl HostServicesOwner {
    /// Create the immutable host table around already-resolved node and provider owners.
    pub fn new(
        radios: Vec<crate::ResolvedRadioNode>,
        providers: Option<&'static crate::providers::ProviderSet>,
        secrets: SecretsFile,
    ) -> Self {
        let mut context = Box::new(StandaloneHostContext {
            radio: RadioHostContext::new(radios, providers),
            secrets,
        });
        let descriptor = Box::new(abi::rptadv_host_services_v5 {
            struct_size: size_of::<abi::rptadv_host_services_v5>() as u32,
            abi_version: 5,
            capability: *b"rptadv.hst5\0",
            context: ptr::from_mut(context.as_mut()).cast(),
            local_time: Some(local_time),
            command_notice: Some(command_notice),
            reaper_acquire: Some(noop),
            reaper_release: Some(noop),
            directory_record: Some(directory::directory_record),
            directory_srv: Some(directory::directory_srv),
            directory_addresses: Some(directory::directory_addresses),
            directory_notice: Some(directory::directory_notice),
            radio_open: Some(open_radio),
            radio_activate: Some(activate_radio),
            radio_destroy: Some(destroy_radio),
            peer_dial: Some(peer_dial),
            peer_bind_radio: Some(peer_bind_radio),
            peer_rate: Some(peer_rate),
            peer_ready: Some(peer_ready),
            peer_read: Some(peer_read),
            peer_send_text: Some(peer_send_text),
            peer_send_digit: Some(peer_send_digit),
            peer_write: Some(peer_write),
            peer_destroy: Some(peer_destroy),
        });
        Self {
            context,
            descriptor,
        }
    }

    /// Borrow the descriptor; this owner and its provider libraries must remain live through stop.
    pub fn descriptor(&self) -> &abi::rptadv_host_services_v5 {
        let _keep_context_alive = &self.context;
        self.descriptor.as_ref()
    }

    /// Put one peer handle into the representation used by the host-services callbacks.
    pub fn peer_handle(&self, peer: IaxPeer) -> *mut c_void {
        Box::into_raw(Box::new(PeerHandle(peer))).cast()
    }

    /// Destroy an unconsumed peer handle with the host's normal ownership rule.
    pub fn destroy_peer(&self, peer: *mut c_void) {
        if !peer.is_null() {
            unsafe { drop(Box::from_raw(peer.cast::<PeerHandle>())) };
        }
    }

    /// Stage radio settings while keeping the active set available for rollback.
    pub fn stage_radios(&self, radios: Vec<crate::ResolvedRadioNode>) -> bool {
        self.context.radio.stage_radios(radios)
    }

    /// Make successfully opened candidate radios active after product reload succeeds.
    pub fn commit_radios(&self) {
        self.context.radio.commit_radios();
    }

    /// Discard candidate radio settings after reload failure.
    pub fn discard_staged_radios(&self) {
        self.context.radio.discard_staged_radios();
    }
}

struct PeerHandle(IaxPeer);

pub(crate) fn boundary<T>(fallback: T, operation: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(fallback)
}

pub(crate) unsafe fn input<'a>(pointer: *const c_char, length: usize) -> Option<&'a str> {
    if pointer.is_null() && length != 0 {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer.cast(), length) }
    };
    std::str::from_utf8(bytes).ok()
}

unsafe extern "C" fn noop() {}

unsafe extern "C" fn command_notice(
    _: *mut c_void,
    local: *const c_char,
    length: usize,
    completed: u32,
) {
    boundary((), || {
        if let Some(local) = unsafe { input(local, length) } {
            eprintln!(
                "node {local}: link command {}",
                if completed != 0 {
                    "completed"
                } else {
                    "rejected"
                }
            );
        }
    });
}

unsafe extern "C" fn local_time(
    _: *mut c_void,
    unix_seconds: i64,
    result: *mut abi::rptadv_local_time_v1,
) -> i32 {
    boundary(-1, || {
        let Some(result) = (unsafe { result.as_mut() }) else {
            return -1;
        };
        if result.struct_size < size_of::<abi::rptadv_local_time_v1>() as u32 {
            return -1;
        }
        let timestamp = unix_seconds as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        if unsafe { libc::localtime_r(&timestamp, local.as_mut_ptr()) }.is_null() {
            result.valid = 0;
            return 0;
        }
        let local = unsafe { local.assume_init() };
        result.year = (local.tm_year + 1900) as u16;
        result.month = (local.tm_mon + 1) as u8;
        result.day = local.tm_mday as u8;
        result.weekday = local.tm_wday as u8;
        result.hour = local.tm_hour as u8;
        result.minute = local.tm_min as u8;
        result.second = local.tm_sec as u8;
        result.valid = 1;
        0
    })
}

unsafe extern "C" fn open_radio(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    maximum_frames: usize,
    radio: *mut *mut c_void,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    // Access only the radio field; other host callbacks may concurrently read secrets.
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe { radio_open(radio_context, name, name_length, maximum_frames, radio) }
}

unsafe extern "C" fn activate_radio(
    context: *mut c_void,
    radio: *mut c_void,
    receive: abi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe {
        radio_activate(
            radio_context,
            radio,
            receive,
            receive_context,
            transmit,
            transmit_context,
        )
    }
}

unsafe extern "C" fn destroy_radio(context: *mut c_void, radio: *mut c_void) {
    if context.is_null() {
        return;
    }
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe { radio_destroy(radio_context, radio) };
}

unsafe extern "C" fn peer_dial(
    context: *mut c_void,
    destination: *const c_char,
    destination_length: usize,
    local: *const c_char,
    local_length: usize,
    _: usize,
    current: abi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(context) = (unsafe { context.cast::<StandaloneHostContext>().as_ref() }) else {
            return -1;
        };
        let (Some(destination), Some(local), Some(current), Some(output)) = (
            unsafe { input(destination, destination_length) },
            unsafe { input(local, local_length) },
            current,
            unsafe { output.as_mut() },
        ) else {
            return -1;
        };
        *output = ptr::null_mut();
        if unsafe { current(current_context) } == 0 {
            return -1;
        }
        let Some((address, remote)) = parse_destination(destination) else {
            eprintln!("rpt-advanced: IAX2 destination validation or address resolution failed");
            return -1;
        };
        let peer = match dial_peer(
            Path::new("librptadviax2.so.1"),
            address,
            local,
            &remote,
            &context.secrets,
            5000,
        ) {
            Ok(peer) => peer,
            Err(error) => {
                eprintln!("rpt-advanced: IAX2 outbound dial failed: {error}");
                return -1;
            }
        };
        if unsafe { current(current_context) } == 0 {
            drop(peer);
            return -1;
        }
        *output = Box::into_raw(Box::new(PeerHandle(peer))).cast();
        0
    })
}

fn dial_peer(
    library: &Path,
    address: SocketAddr,
    local: &str,
    remote: &str,
    secrets: &SecretsFile,
    timeout_ms: u32,
) -> Result<IaxPeer, IaxError> {
    IaxClient::load(library)?.dial_configured(address, local, remote, secrets, timeout_ms)
}

fn parse_destination(value: &str) -> Option<(SocketAddr, String)> {
    let (address, remote) = value.strip_prefix("radio@")?.rsplit_once('/')?;
    if remote.is_empty() || remote.len() > 63 || !remote.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let address: SocketAddr = if let Ok(address) = address.parse() {
        address
    } else if let Ok(ip) = address
        .strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
        .unwrap_or(address)
        .parse::<std::net::IpAddr>()
    {
        SocketAddr::new(ip, 4569)
    } else if address.contains(':') {
        address.to_socket_addrs().ok()?.next()?
    } else {
        (address, 4569).to_socket_addrs().ok()?.next()?
    };
    (address.port() != 0).then(|| (address, remote.to_owned()))
}

unsafe extern "C" fn peer_rate(_: *mut c_void, peer: *const c_void) -> u32 {
    unsafe { peer.cast::<PeerHandle>().as_ref() }.map_or(0, |peer| peer.0.sample_rate_hz())
}

unsafe extern "C" fn peer_ready(_: *mut c_void, peer: *mut c_void) -> i32 {
    if unsafe { peer.cast::<PeerHandle>().as_ref() }.is_none() {
        return -1;
    }
    1
}

unsafe extern "C" fn peer_read(
    _: *mut c_void,
    peer: *mut c_void,
    event: abi::rptadv_peer_event_v1,
    event_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(event)) = (unsafe { peer.cast::<PeerHandle>().as_mut() }, event)
        else {
            return -1;
        };
        let mut samples = [0.0_f32; 4096];
        let mut text = [0_u8; 1024];
        match peer.0.poll(&mut samples, &mut text) {
            Ok(IaxEvent::None) => 0,
            Ok(IaxEvent::Audio(length)) => unsafe {
                event(event_context, 3, samples.as_ptr().cast(), length);
                0
            },
            Ok(IaxEvent::Text(length)) => unsafe {
                event(event_context, 1, text.as_ptr().cast(), length);
                0
            },
            Ok(IaxEvent::Digit(digit)) => unsafe {
                event(event_context, 2, ptr::from_ref(&digit).cast(), 1);
                0
            },
            Ok(IaxEvent::RadioKey) => unsafe {
                event(event_context, 4, ptr::null(), 0);
                0
            },
            Ok(IaxEvent::RadioUnkey) => unsafe {
                event(event_context, 5, ptr::null(), 0);
                0
            },
            Ok(IaxEvent::Hangup) | Err(_) => -1,
        }
    })
}

unsafe extern "C" fn peer_send_text(
    _: *mut c_void,
    peer: *mut c_void,
    text: *const c_char,
    length: usize,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(text)) = (unsafe { peer.cast::<PeerHandle>().as_mut() }, unsafe {
            input(text, length)
        }) else {
            return -1;
        };
        if peer.0.send_text(text.as_bytes()).is_ok() {
            0
        } else {
            -1
        }
    })
}

unsafe extern "C" fn peer_send_digit(_: *mut c_void, peer: *mut c_void, digit: u8) -> i32 {
    unsafe { peer.cast::<PeerHandle>().as_mut() }.map_or(-1, |peer| {
        if peer.0.send_digit(digit).is_ok() {
            0
        } else {
            -1
        }
    })
}

unsafe extern "C" fn peer_write(
    _: *mut c_void,
    peer: *mut c_void,
    samples: *const f32,
    count: usize,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { peer.cast::<PeerHandle>().as_mut() }) else {
            return -1;
        };
        if samples.is_null() && count != 0 {
            return -1;
        }
        let samples = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(samples, count) }
        };
        if peer.0.send_audio(samples).is_ok() {
            0
        } else {
            -1
        }
    })
}

unsafe extern "C" fn peer_destroy(_: *mut c_void, peer: *mut c_void) {
    if !peer.is_null() {
        unsafe { drop(Box::from_raw(peer.cast::<PeerHandle>())) };
    }
}

#[cfg(test)]
mod tests {
    use super::{HostServicesOwner, boundary, dial_peer, input, parse_destination, peer_dial};
    use crate::{iax::IaxError, secrets::SecretsFile};
    use std::{
        ffi::c_void,
        mem::size_of,
        net::{SocketAddr, UdpSocket},
        path::Path,
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    unsafe extern "C" fn current_no(_: *mut c_void) -> u32 {
        0
    }

    unsafe extern "C" fn current_yes(_: *mut c_void) -> u32 {
        1
    }

    unsafe extern "C" fn current_yes_then_no(context: *mut c_void) -> u32 {
        let Some(calls) = (unsafe { context.cast::<AtomicUsize>().as_ref() }) else {
            return 0;
        };
        u32::from(calls.fetch_add(1, Ordering::AcqRel) == 0)
    }

    unsafe extern "C" fn record_peer_event(
        context: *mut c_void,
        kind: u32,
        _: *const c_void,
        _: usize,
    ) {
        unsafe { context.cast::<Vec<u32>>().as_mut() }
            .unwrap()
            .push(kind);
    }

    unsafe extern "C" fn allow_test_peer(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn reject_test_peer(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        1
    }

    unsafe extern "C" fn capture_test_peer(
        context: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        peer: *mut c_void,
    ) -> i32 {
        let Some(context) = (unsafe { context.cast::<AtomicUsize>().as_ref() }) else {
            return -1;
        };
        if peer.is_null() {
            return -1;
        }
        context.store(peer as usize, Ordering::Release);
        0
    }

    #[test]
    fn host_services_table_is_complete_and_uses_its_owned_context() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let api = owner.descriptor();
        assert_eq!(
            api.struct_size,
            size_of::<crate::abi::rptadv_host_services_v5>() as u32
        );
        assert_eq!(api.abi_version, 5);
        assert_eq!(api.capability, *b"rptadv.hst5\0");
        assert!(!api.context.is_null());
        assert!(api.local_time.is_some());
        assert!(api.peer_dial.is_some());
        assert!(api.radio_activate.is_some());
        let _context: *mut c_void = api.context;
    }

    #[test]
    fn descriptor_address_survives_owner_move() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let before = ptr::from_ref(owner.descriptor());
        let owner = Box::new(owner);
        let after = ptr::from_ref(owner.descriptor());

        assert_eq!(before, after);
    }

    #[test]
    fn callback_input_and_panic_boundary_return_safe_fallbacks() {
        let invalid_utf8 = [0xff_u8];
        assert_eq!(unsafe { input(ptr::null(), 0) }, Some(""));
        assert_eq!(unsafe { input(ptr::null(), 1) }, None);
        assert_eq!(unsafe { input(invalid_utf8.as_ptr().cast(), 1) }, None);
        assert_eq!(boundary(-1, || panic!("test callback panic")), -1);
    }

    #[test]
    fn local_time_validates_output_and_returns_local_calendar_fields() {
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, ptr::null_mut()) },
            -1
        );
        let mut short = crate::abi::rptadv_local_time_v1 {
            struct_size: 0,
            valid: 7,
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, &mut short) },
            -1
        );
        assert_eq!(short.valid, 7);

        let mut result = crate::abi::rptadv_local_time_v1 {
            struct_size: size_of::<crate::abi::rptadv_local_time_v1>() as u32,
            valid: 0,
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, &mut result) },
            0
        );
        assert_eq!(result.valid, 1);
        assert!((1..=12).contains(&result.month));
        assert!((1..=31).contains(&result.day));

        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), i64::MAX, &mut result) },
            0
        );
        assert_eq!(result.valid, 0);
    }

    #[test]
    fn destination_parser_checks_scheme_node_and_address() {
        let (address, node) = parse_destination("radio@localhost:4571/506316")
            .expect("explicit hostname port must resolve");
        assert!(address.ip().is_loopback());
        assert_eq!(address.port(), 4571);
        assert_eq!(node, "506316");
        assert_eq!(
            parse_destination("radio@127.0.0.1:4569/506315"),
            Some(("127.0.0.1:4569".parse().unwrap(), "506315".into()))
        );
        assert!(parse_destination("radio@127.0.0.1/506315").is_some());
        for (target, expected) in [
            ("radio@[::1]:4571/506315", "[::1]:4571"),
            ("radio@[::1]/506315", "[::1]:4569"),
            ("radio@::1/506315", "[::1]:4569"),
        ] {
            assert_eq!(
                parse_destination(target).unwrap().0,
                expected.parse::<SocketAddr>().unwrap()
            );
        }
        for invalid in [
            "",
            "127.0.0.1/506315",
            "radio@127.0.0.1",
            "radio@127.0.0.1/",
            "radio@127.0.0.1/not-a-node",
            "radio@localhost:65536/506315",
            "radio@localhost:invalid/506315",
            "radio@127.0.0.1:0/506315",
        ] {
            assert_eq!(parse_destination(invalid), None, "accepted {invalid:?}");
        }
    }

    #[test]
    fn outbound_dial_reports_library_and_network_failures() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let secrets = SecretsFile::parse("").unwrap();
        assert!(matches!(
            dial_peer(
                Path::new("/no/such/librptadviax2.so.1"),
                address,
                "524950",
                "506315",
                &secrets,
                50,
            ),
            Err(IaxError::Load)
        ));
        assert!(matches!(
            dial_peer(
                Path::new("librptadviax2.so.1"),
                address,
                "524950",
                "506315",
                &secrets,
                50,
            ),
            Err(IaxError::Timeout | IaxError::Network)
        ));
    }

    #[test]
    fn outbound_dial_callback_rejects_bad_inputs_and_stale_calls() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let descriptor = owner.descriptor();
        let local = c"524950";
        let destination = c"not-a-radio-destination";
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    ptr::null(),
                    1,
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert!(output.is_null());
    }

    #[test]
    fn outbound_dial_callback_returns_failure_when_remote_rejects() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let library = Path::new("librptadviax2.so.1");
        let mut server =
            crate::iax::IaxServer::bind(library, address, &["506315".to_owned()]).unwrap();
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let context = owner.descriptor().context as usize;
        let destination = format!("radio@{address}/506315");
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524950";
            let mut output = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            };
            sender.send((status, output as usize)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(now, reject_test_peer, capture_test_peer, ptr::null_mut())
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, -1);
        assert_eq!(peer, 0);
    }

    #[test]
    fn host_callbacks_reject_missing_context_handles_and_outputs() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let api = owner.descriptor();
        let destination = c"radio@127.0.0.1/506315";
        let local = c"524950";
        let mut radio = ptr::null_mut();
        assert_eq!(
            unsafe { super::open_radio(ptr::null_mut(), c"x".as_ptr(), 1, 960, &mut radio) },
            -1
        );
        assert_eq!(
            unsafe { super::open_radio(api.context, c"x".as_ptr(), 1, 960, &mut radio) },
            -1
        );
        assert_eq!(radio, ptr::null_mut());
        assert_eq!(
            unsafe {
                super::activate_radio(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::activate_radio(
                    api.context,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                )
            },
            -1
        );
        unsafe { super::destroy_radio(ptr::null_mut(), ptr::null_mut()) };
        unsafe { super::destroy_radio(api.context, ptr::null_mut()) };

        let mut peer = ptr::null_mut();
        assert_eq!(
            unsafe {
                super::peer_dial(
                    ptr::null_mut(),
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::peer_dial(
                    api.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );
        assert_eq!(peer, ptr::null_mut());

        let invalid_destination = c"not-an-iax-destination";
        assert_eq!(
            unsafe {
                super::peer_dial(
                    api.context,
                    invalid_destination.as_ptr(),
                    invalid_destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );

        assert_eq!(unsafe { super::peer_rate(ptr::null_mut(), ptr::null()) }, 0);
        assert_eq!(
            unsafe { super::peer_ready(ptr::null_mut(), ptr::null_mut()) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_read(ptr::null_mut(), ptr::null_mut(), None, ptr::null_mut()) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), ptr::null_mut(), c"x".as_ptr(), 1) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), ptr::null_mut(), b'1') },
            -1
        );
        assert_eq!(
            unsafe { super::peer_write(ptr::null_mut(), ptr::null_mut(), ptr::null(), 1) },
            -1
        );
        unsafe { super::peer_destroy(ptr::null_mut(), ptr::null_mut()) };
        owner.destroy_peer(ptr::null_mut());
    }

    #[test]
    fn host_callback_notice_and_radio_staging_preserve_safe_state() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        unsafe { super::noop() };
        unsafe { super::command_notice(ptr::null_mut(), c"524950".as_ptr(), 6, 1) };
        unsafe { super::command_notice(ptr::null_mut(), c"524950".as_ptr(), 6, 0) };
        unsafe { super::command_notice(ptr::null_mut(), ptr::null(), 1, 1) };

        assert!(owner.stage_radios(Vec::new()));
        assert!(!owner.stage_radios(Vec::new()));
        owner.commit_radios();
        assert!(owner.stage_radios(Vec::new()));
        owner.discard_staged_radios();
        assert!(owner.stage_radios(Vec::new()));
        owner.discard_staged_radios();
    }

    #[test]
    fn peer_callbacks_forward_events_and_report_send_results() {
        let owner = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let events = [
            (0, 0, None),
            (1, 0, Some(3)),
            (2, 0, Some(1)),
            (4, 0, Some(2)),
            (5, 0, Some(4)),
            (6, 0, Some(5)),
            (3, -1, None),
        ];
        for (event, result, forwarded) in events {
            let peer = crate::iax::tests::peer_for_host_callback(event, false, false);
            let handle = owner.peer_handle(peer);
            let mut observed = Vec::new();
            assert_eq!(
                unsafe {
                    super::peer_read(
                        ptr::null_mut(),
                        handle,
                        Some(record_peer_event),
                        ptr::from_mut(&mut observed).cast(),
                    )
                },
                result
            );
            assert_eq!(observed.first().copied(), forwarded);
            owner.destroy_peer(handle);
        }

        let peer = crate::iax::tests::peer_for_host_callback(0, false, false);
        let handle = owner.peer_handle(peer);
        assert_eq!(unsafe { super::peer_rate(ptr::null_mut(), handle) }, 8000);
        assert_eq!(unsafe { super::peer_ready(ptr::null_mut(), handle) }, 1);
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, c"status".as_ptr(), 6) },
            0
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, ptr::null(), 1) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), handle, b'7') },
            0
        );
        assert_eq!(
            unsafe { super::peer_write(ptr::null_mut(), handle, ptr::null(), 0) },
            0
        );
        assert_eq!(
            unsafe { super::peer_write(ptr::null_mut(), handle, ptr::null(), 1) },
            -1
        );
        owner.destroy_peer(handle);

        let peer = crate::iax::tests::peer_for_host_callback(0, true, true);
        let handle = owner.peer_handle(peer);
        assert_eq!(
            unsafe {
                super::peer_read(
                    ptr::null_mut(),
                    handle,
                    Some(record_peer_event),
                    ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, c"status".as_ptr(), 6) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), handle, b'7') },
            -1
        );
        let samples = [0.0_f32; 160];
        assert_eq!(
            unsafe { super::peer_write(ptr::null_mut(), handle, samples.as_ptr(), samples.len()) },
            -1
        );
        unsafe { super::peer_destroy(ptr::null_mut(), handle) };
    }

    #[test]
    fn outbound_host_dial_connects_to_local_iax_server() {
        let library = Path::new("librptadviax2.so.1");
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut server =
            crate::iax::IaxServer::bind(library, address, &["506315".to_owned()]).unwrap();
        let accepted = AtomicUsize::new(0);
        let host = HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap());
        let host_context = host.descriptor().context as usize;
        let destination = format!("radio@{address}/506315");
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524950";
            let mut peer = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    host_context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut peer,
                )
            };
            sender.send((status, peer as usize)).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        allow_test_peer,
                        capture_test_peer,
                        ptr::from_ref(&accepted).cast_mut().cast(),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, 0);
        assert_ne!(peer, 0);
        assert_ne!(accepted.load(Ordering::Acquire), 0);
        unsafe { super::peer_destroy(ptr::null_mut(), peer as *mut c_void) };
        let accepted_peer = accepted.swap(0, Ordering::AcqRel) as *mut c_void;
        let server_client = crate::iax::IaxClient::load(library).unwrap();
        let inbound = unsafe { server_client.adopt_inbound(accepted_peer) }.unwrap();
        drop(inbound);

        let canceled_calls = AtomicUsize::new(0);
        let canceled_context = ptr::from_ref(&canceled_calls) as usize;
        let destination = format!("radio@{address}/506315");
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524951";
            let mut peer = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    host_context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes_then_no),
                    canceled_context as *mut c_void,
                    &mut peer,
                )
            };
            sender.send((status, peer as usize)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        allow_test_peer,
                        capture_test_peer,
                        ptr::from_ref(&accepted).cast_mut().cast(),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, -1);
        assert_eq!(peer, 0);
        let accepted_peer = accepted.swap(0, Ordering::AcqRel) as *mut c_void;
        let server_client = crate::iax::IaxClient::load(library).unwrap();
        let inbound = unsafe { server_client.adopt_inbound(accepted_peer) }.unwrap();
        drop(inbound);
    }
}
