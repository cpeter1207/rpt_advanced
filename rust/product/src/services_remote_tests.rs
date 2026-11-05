use super::*;
use rpt_advanced_core::remote_base_command::{DecimalValue, RemoteCommand};

thread_local! { static FLAGS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; static DESTROYED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }

unsafe extern "C" fn source(_: *mut c_void, flags: u32) -> i32 {
    FLAGS.set(flags);
    0
}
unsafe extern "C" fn take(
    _: *mut c_void,
    sink: abi::rptadv_remote_sink_v1,
    context: *mut c_void,
) -> i32 {
    if FLAGS.get() & 4 != 0 {
        let bytes = [b'x'; 40_000];
        for _ in 0..2 {
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
    0
}

#[test]
fn remote_tickets_preserve_pending_source_and_completion_ownership() {
    let _guard = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| table.radio_remote_submit = Some(submit));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    let operation = RemoteCommand::SetSetting {
        selector: 9,
        value: DecimalValue {
            negative: false,
            magnitude: 125,
            decimal_places: 0,
        },
    }
    .wire();
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
