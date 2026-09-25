use super::*;
use std::{
    ffi::{c_char, c_void},
    ptr,
    sync::atomic::{AtomicU8, Ordering},
};

static LOCAL_TIME_MODE: AtomicU8 = AtomicU8::new(0);
static LOOKUP_MODE: AtomicU8 = AtomicU8::new(0);
static RADIO_OPEN_MODE: AtomicU8 = AtomicU8::new(0);
static PEER_DIAL_MODE: AtomicU8 = AtomicU8::new(0);

#[test]
fn obsolete_exchange_host_revision_is_rejected_before_callbacks() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.abi_version = 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

#[test]
fn host_without_radio_link_binding_is_rejected_before_callbacks() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.abi_version = 2;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.abi_version = 3;
    table.peer_bind_radio = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

#[test]
fn direct_table_requires_both_activation_and_destroy() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    assert!(unsafe { HostServices::open(crate::fixture::host_descriptor()) }.is_ok());
    table.radio_activate = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.radio_activate = unsafe { (*crate::fixture::host_descriptor()).radio_activate };
    table.radio_destroy = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

fn host_services(configure: impl FnOnce(&mut abi::rptadv_host_services_v3)) -> HostServices {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    configure(&mut table);
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}

unsafe extern "C" fn local_time_mode(
    _: *mut c_void,
    _: i64,
    output: *mut abi::rptadv_local_time_v1,
) -> i32 {
    if LOCAL_TIME_MODE.load(Ordering::Relaxed) == 1 {
        return -1;
    }
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    *output = abi::rptadv_local_time_v1 {
        struct_size: size_of::<abi::rptadv_local_time_v1>() as u32,
        valid: 1,
        year: 2030,
        month: 1,
        day: 1,
        weekday: 2,
        hour: 9,
        minute: 7,
        second: 3,
    };
    match LOCAL_TIME_MODE.load(Ordering::Relaxed) {
        2 => output.valid = 0,
        3 => output.weekday = 7,
        4 => output.second = 60,
        5 => output.month = 0,
        _ => {}
    }
    0
}

unsafe extern "C" fn lookup_mode(
    _: *mut c_void,
    _: u32,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    output: *mut c_char,
    _: usize,
    written: *mut usize,
) -> i32 {
    let Some(written) = (unsafe { written.as_mut() }) else {
        return -1;
    };
    match LOOKUP_MODE.load(Ordering::Relaxed) {
        1 => -1,
        2 => {
            *written = 1025;
            0
        }
        3 => {
            let Some(output) = (unsafe { output.cast::<u8>().as_mut() }) else {
                return -1;
            };
            *output = 0xff;
            *written = 1;
            0
        }
        _ => {
            if output.is_null() {
                return -1;
            }
            let output = output.cast::<u8>();
            unsafe {
                output.write(b'o');
                output.add(1).write(b'k');
            }
            *written = 2;
            0
        }
    }
}

unsafe extern "C" fn radio_open_mode(
    _: *mut c_void,
    _: *const c_char,
    _: usize,
    _: usize,
    output: *mut *mut c_void,
) -> i32 {
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    match RADIO_OPEN_MODE.load(Ordering::Relaxed) {
        1 => -1,
        2 => {
            *output = ptr::null_mut();
            0
        }
        _ => unsafe {
            (*crate::fixture::host_descriptor()).radio_open.unwrap()(
                ptr::null_mut(),
                c"usb".as_ptr(),
                3,
                8,
                output,
            )
        },
    }
}

unsafe extern "C" fn peer_dial_mode(
    _: *mut c_void,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    _: usize,
    _: abi::rptadv_current_v1,
    _: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    match PEER_DIAL_MODE.load(Ordering::Relaxed) {
        1 => -1,
        2 => {
            *output = ptr::null_mut();
            0
        }
        _ => {
            *output = crate::fixture::peer();
            0
        }
    }
}

unsafe extern "C" fn ready_true(_: *mut c_void, _: *mut c_void) -> i32 {
    1
}

unsafe extern "C" fn ready_hangup(_: *mut c_void, _: *mut c_void) -> i32 {
    -1
}

unsafe extern "C" fn peer_events(
    _: *mut c_void,
    _: *mut c_void,
    event: abi::rptadv_peer_event_v1,
    context: *mut c_void,
) -> i32 {
    let Some(event) = event else {
        return -1;
    };
    let text = *b"ok";
    let digit = b'7';
    let audio = [0.25_f32];
    unsafe {
        event(context, 1, text.as_ptr().cast(), text.len());
        event(context, 2, ptr::from_ref(&digit).cast(), 1);
        event(context, 3, audio.as_ptr().cast(), audio.len());
        event(context, 0, ptr::null(), 0);
        event(context, 1, ptr::null(), 1);
        event(context, 2, ptr::null(), 1);
        event(context, 2, ptr::from_ref(&digit).cast(), 2);
        event(context, 3, ptr::null(), 1);
    }
    0
}

unsafe extern "C" fn peer_read_failure(
    _: *mut c_void,
    _: *mut c_void,
    _: abi::rptadv_peer_event_v1,
    _: *mut c_void,
) -> i32 {
    -1
}

unsafe extern "C" fn send_text_failure(
    _: *mut c_void,
    _: *mut c_void,
    _: *const c_char,
    _: usize,
) -> i32 {
    -1
}

unsafe extern "C" fn send_digit_failure(_: *mut c_void, _: *mut c_void, _: u8) -> i32 {
    -1
}

unsafe extern "C" fn write_failure(_: *mut c_void, _: *mut c_void, _: *const f32, _: usize) -> i32 {
    -1
}

#[test]
fn host_table_validation_and_value_conversions_fail_closed() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(matches!(
        unsafe { HostServices::open(ptr::null()) },
        Err(Error::Admission)
    ));
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.struct_size -= 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size = size_of::<abi::rptadv_host_services_v3>() as u32;
    table.abi_version = 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.abi_version = 3;
    table.capability[0] = b'!';
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.capability = *b"rptadv.hst3\0";
    table.local_time = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let (civil, second) = services.local_time(0).unwrap();
    assert_eq!(
        civil,
        CivilTime::new(2030, 1, 1, Weekday::Tuesday, 9, 7).unwrap()
    );
    assert_eq!(second, 3);
    assert_eq!(
        services.lookup(0, "", "", "2000", None).unwrap(),
        "radio@fixture/2000"
    );
    services.command_notice("1000", true);
    unsafe {
        services.reaper_acquire()();
        services.reaper_release()();
    }

    let services = host_services(|table| table.local_time = Some(local_time_mode));
    for mode in [1, 2, 3, 4, 5] {
        LOCAL_TIME_MODE.store(mode, Ordering::Relaxed);
        assert_eq!(services.local_time(0), None);
    }
    LOCAL_TIME_MODE.store(0, Ordering::Relaxed);
}

#[test]
fn lookup_and_handle_admission_errors_are_rejected_without_ownership_transfer() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| table.directory_lookup = Some(lookup_mode));
    for mode in [1, 2, 3] {
        LOOKUP_MODE.store(mode, Ordering::Relaxed);
        assert_eq!(
            services.lookup(0, "static", "external", "remote", Some("source")),
            Err(Error::Operation)
        );
    }
    LOOKUP_MODE.store(0, Ordering::Relaxed);
    assert_eq!(
        services.lookup(0, "static", "external", "remote", Some("source")),
        Ok("ok".into())
    );
    assert!(matches!(
        unsafe { services.peer(ptr::null_mut()) },
        Err(Error::Admission)
    ));

    let services = host_services(|table| table.radio_open = Some(radio_open_mode));
    for mode in [1, 2] {
        RADIO_OPEN_MODE.store(mode, Ordering::Relaxed);
        assert!(matches!(services.radio("usb", 8), Err(Error::Operation)));
    }
    RADIO_OPEN_MODE.store(0, Ordering::Relaxed);
    drop(services.radio("usb", 8).unwrap());

    let services = host_services(|table| table.peer_dial = Some(peer_dial_mode));
    for mode in [1, 2] {
        PEER_DIAL_MODE.store(mode, Ordering::Relaxed);
        assert!(matches!(
            services.dial("remote", "1000", 8, || true),
            Err(Error::Operation | Error::Admission)
        ));
    }
    PEER_DIAL_MODE.store(0, Ordering::Relaxed);
    drop(services.dial("remote", "1000", 8, || true).unwrap());
}

#[test]
fn radio_and_peer_callbacks_contain_client_panics_and_release_once() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::fixture::RADIO_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let radio = services.radio("usb", 8).unwrap();
    drop(radio);
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), 1);

    assert!(
        services
            .dial("remote", "1000", 8, || panic!("current"))
            .is_err()
    );
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.rate(), 8000);
    assert!(!peer.ready().unwrap());
    assert_eq!(peer.read(|_| panic!("dispatch")), Err(Error::Hangup));
    peer.send_text(c"hello").unwrap();
    peer.send_digit('1').unwrap();
    peer.write(&[0.25]).unwrap();
    drop(peer);
    assert_eq!(crate::fixture::PEER_DROPS.load(Ordering::Relaxed), 1);
}

#[test]
fn peer_callbacks_dispatch_valid_events_and_reject_invalid_or_failed_operations() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    let services = host_services(|table| table.peer_ready = Some(ready_true));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert!(peer.ready().unwrap());
    drop(peer);

    let services = host_services(|table| table.peer_ready = Some(ready_hangup));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.ready(), Err(Error::Hangup));
    drop(peer);

    let services = host_services(|table| table.peer_read = Some(peer_events));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    let mut events = Vec::new();
    peer.read(|event| match event {
        PeerInput::Text(text) => events.push(format!("text:{:?}", text)),
        PeerInput::Digit(digit) => events.push(format!("digit:{digit}")),
        PeerInput::Audio(audio) => events.push(format!("audio:{audio:?}")),
    })
    .unwrap();
    assert_eq!(events, ["text:[111, 107]", "digit:7", "audio:[0.25]"]);
    drop(peer);

    let services = host_services(|table| table.peer_read = Some(peer_read_failure));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.read(|_| {}), Err(Error::Hangup));
    drop(peer);

    let services = host_services(|table| table.peer_send_text = Some(send_text_failure));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.send_text(c"fail"), Err(Error::Write));
    drop(peer);

    let services = host_services(|table| table.peer_send_digit = Some(send_digit_failure));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.send_digit('1'), Err(Error::Write));
    drop(peer);

    let services = host_services(|table| table.peer_write = Some(write_failure));
    let mut peer = services.dial("remote", "1000", 8, || true).unwrap();
    assert_eq!(peer.write(&[0.25]), Err(Error::Write));
    drop(peer);
}
