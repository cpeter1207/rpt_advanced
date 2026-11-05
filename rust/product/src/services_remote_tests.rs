use super::*;
use rpt_advanced_core::remote_base_command::{DecimalValue, RemoteCommand};

thread_local! {
    static FLAGS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DESTROYED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static MODE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static RADIO_CODE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

unsafe extern "C" fn source(_: *mut c_void, flags: u32) -> i32 {
    FLAGS.set(flags);
    if MODE.get() == 9 { -29 } else { 0 }
}
unsafe extern "C" fn take(
    _: *mut c_void,
    sink: abi::rptadv_remote_sink_v1,
    context: *mut c_void,
) -> i32 {
    match MODE.get() {
        10 => return -39,
        11 => {
            unsafe { sink.unwrap()(context, std::ptr::null(), 0) };
            return 0;
        }
        12 => {
            unsafe { sink.unwrap()(context, std::ptr::null(), 1) };
            return 0;
        }
        13 => {
            unsafe { sink.unwrap()(context, b"x".as_ptr(), 65_537) };
            return 0;
        }
        14 => {
            unsafe { sink.unwrap()(context, [255].as_ptr(), 1) };
            unsafe { sink.unwrap()(context, b"valid".as_ptr(), 5) };
            return 0;
        }
        15 => {
            let bytes = [b'x'; 65_536];
            unsafe { sink.unwrap()(context, bytes.as_ptr(), bytes.len() as u32) };
            return 0;
        }
        _ => {}
    }
    if FLAGS.get() & 4 != 0 {
        let bytes = [b'x'; 40_000];
        for _ in 0..3 {
            unsafe { sink.unwrap()(context, bytes.as_ptr(), bytes.len() as u32) };
        }
        return 0;
    }
    if FLAGS.get() & 1 != 0 {
        return 1;
    }
    let bytes = br#"{"version":1,"kind":"applied"}"#;
    unsafe { sink.unwrap()(context, bytes.as_ptr(), bytes.len() as u32) };
    0
}
unsafe extern "C" fn destroy(_: *mut c_void) {
    DESTROYED.set(DESTROYED.get() + 1);
}
unsafe extern "C" fn submit(
    _: *mut c_void,
    radio: *mut c_void,
    operation: *const abi::UrpRemoteOperation,
    flags: u32,
    output: *mut abi::rptadv_remote_ticket_v1,
) -> i32 {
    assert!(!radio.is_null());
    let operation = unsafe { &*operation };
    assert_eq!(
        (operation.operation, operation.selector, operation.magnitude),
        (12, 9, 125)
    );
    FLAGS.set((flags >> 1) | 2);
    unsafe {
        output.write(abi::rptadv_remote_ticket_v1 {
            struct_size: std::mem::size_of::<abi::rptadv_remote_ticket_v1>() as u32,
            abi_version: 1,
            context: radio,
            source: Some(source),
            take: Some(take),
            destroy: Some(destroy),
        })
    };
    let ticket = unsafe { &mut *output };
    match MODE.get() {
        1 => ticket.struct_size -= 1,
        2 => ticket.abi_version += 1,
        3 | 8 => ticket.context = std::ptr::null_mut(),
        4 => ticket.source = None,
        5 => ticket.take = None,
        6 => ticket.destroy = None,
        _ => {}
    }
    if matches!(MODE.get(), 7 | 8) { -31 } else { 0 }
}

fn operation() -> rpt_advanced_core::remote_base_command::RemoteOperation {
    RemoteCommand::SetSetting {
        selector: 9,
        value: DecimalValue {
            negative: false,
            magnitude: 125,
            decimal_places: 0,
        },
    }
    .wire()
}

#[test]
fn remote_tickets_preserve_pending_source_and_completion_ownership() {
    let _guard = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| table.radio_remote_submit = Some(submit));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    MODE.set(0);
    let operation = operation();
    let mut ticket = radio.remote_submit(&operation, false, true).unwrap();
    assert_eq!(ticket.take(), Ok(None));
    ticket.source_state(false, true).unwrap();
    assert_eq!(
        ticket.take().unwrap().unwrap(),
        r#"{"version":1,"kind":"applied"}"#
    );
    FLAGS.set(4);
    assert_eq!(ticket.take(), Err(-1));
    let before = DESTROYED.get();
    drop(ticket);
    assert_eq!(FLAGS.get(), 0);
    assert_eq!(DESTROYED.get(), before + 1);
}

#[test]
fn remote_ticket_admission_releases_only_transferred_contexts_with_destructors() {
    let _guard = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| table.radio_remote_submit = Some(submit));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    for case in 1..9 {
        MODE.set(case);
        let before = DESTROYED.get();
        let error = radio.remote_submit(&operation(), true, false).err();
        assert_eq!(error, Some(if case >= 7 { -31 } else { -7 }));
        assert_eq!(
            DESTROYED.get() - before,
            u32::from(!matches!(case, 3 | 6 | 8))
        );
    }
    MODE.set(0);
    let services = host_services(|table| table.radio_remote_submit = None);
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(
        radio.remote_submit(&operation(), true, false).err(),
        Some(-7)
    );
}

#[test]
fn remote_ticket_copy_validates_provider_spans_and_retains_original_errors() {
    let _guard = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    MODE.set(0);
    let services = host_services(|table| table.radio_remote_submit = Some(submit));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    let mut ticket = radio.remote_submit(&operation(), true, false).unwrap();
    MODE.set(9);
    assert_eq!(ticket.source_state(false, true), Err(-29));
    for (case, error) in [(10, -39), (12, -1), (13, -1), (14, -1)] {
        MODE.set(case);
        assert_eq!(ticket.take(), Err(error));
    }
    MODE.set(11);
    assert_eq!(ticket.take(), Ok(Some(String::new())));
    MODE.set(15);
    assert_eq!(ticket.take().unwrap().unwrap().len(), 65_536);
    MODE.set(0);
}

unsafe extern "C" fn radio_response(_: *mut c_void, _: *mut c_void) -> i32 {
    RADIO_CODE.get()
}

#[test]
fn remote_radio_lifecycle_maps_optional_operations_and_original_failures() {
    let _guard = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| {
        table.radio_service = None;
        table.radio_ready = None;
        table.radio_quiesce = None;
    });
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.service(), Ok(()));
    assert_eq!(radio.activation_ready(), Ok(true));
    assert_eq!(radio.poll_quiesce(), Ok(true));
    let services = host_services(|table| {
        table.radio_service = None;
        table.radio_ready = Some(radio_response);
        table.radio_quiesce = Some(radio_response);
    });
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    for (code, expected) in [(0, Ok(true)), (1, Ok(false)), (-47, Err(-47))] {
        RADIO_CODE.set(code);
        assert_eq!(radio.activation_ready(), expected);
        assert_eq!(radio.poll_quiesce(), expected);
    }
    let services = host_services(|table| table.radio_service = Some(radio_response));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.activation_ready(), Err(-47));
    RADIO_CODE.set(0);
    assert_eq!(radio.service(), Ok(()));
}
