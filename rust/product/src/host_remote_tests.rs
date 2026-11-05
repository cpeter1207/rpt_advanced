use super::*;
use rpt_advanced_core::remote_base_command::{DecimalValue, RemoteCommand};
use std::cell::Cell;

thread_local! { static READY: Cell<bool> = const { Cell::new(false) }; static SOURCE: Cell<u32> = const { Cell::new(0) }; static DESTROYED: Cell<u32> = const { Cell::new(0) }; }
unsafe extern "C" fn source(_: *mut c_void, flags: u32) -> i32 {
    SOURCE.set(flags);
    0
}
unsafe extern "C" fn take(
    _: *mut c_void,
    sink: abi::rptadv_remote_sink_v1,
    context: *mut c_void,
) -> i32 {
    if !READY.get() {
        return 1;
    }
    let bytes = br#"{"version":1,"kind":"applied"}"#;
    // SAFETY: the host lends a synchronous output collector through this call.
    unsafe { sink.unwrap()(context, bytes.as_ptr(), bytes.len() as u32) };
    0
}
unsafe extern "C" fn destroy(_: *mut c_void) {
    DESTROYED.set(DESTROYED.get() + 1);
}
unsafe extern "C" fn service(_: *mut c_void, _: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn bind_metadata(
    _: *mut c_void,
    _: *mut c_void,
    _: abi::rptadv_radio_metadata_v1,
) -> i32 {
    0
}
unsafe extern "C" fn submit(
    _: *mut c_void,
    radio: *mut c_void,
    request: *const abi::UrpRemoteOperation,
    flags: u32,
    out: *mut abi::rptadv_remote_ticket_v1,
) -> i32 {
    // SAFETY: the host lends a complete operation and writable ticket result.
    let request = unsafe { &*request };
    assert_eq!(
        (request.operation, request.selector, request.magnitude),
        (12, 9, 125)
    );
    assert_eq!(flags, 1);
    // SAFETY: the test functions and radio reservation remain live until destroy.
    unsafe {
        out.write(abi::rptadv_remote_ticket_v1 {
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
fn host_remote_completion_is_nonblocking_and_reload_revokes_pending_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    READY.set(false);
    SOURCE.set(0);
    DESTROYED.set(0);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_remote_submit = Some(submit);
    table.radio_metadata_bind = Some(bind_metadata);
    table.radio_service = Some(service);
    table.radio_quiesce = Some(service);
    table.radio_ready = Some(service);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let config = "[remote_base 1000]\nenabled=yes\nbackend=hamlib\nhamlib_model=RIG_MODEL_DUMMY\ndtmf_access=open\nstartup_memory=local\nallowed_modes=FM\nallowed_rx_ranges_hz=144000000-148000000\nallowed_tx_ranges_hz=144000000-148000000\n[remote_memory 1000 local]\nselector=1\nrx_frequency_hz=146520000\ntx_frequency_hz=146520000\nmode=FM\n";
    let mut host = Host::start(
        document("usb", config),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let operation = || {
        let mut operation = DigitOperation::new(
            Command {
                action: LinkAction::RemoteBase,
                node: String::new(),
            },
            None,
        );
        operation.remote = Some(RemoteCommand::SetSetting {
            selector: 9,
            value: DecimalValue {
                negative: false,
                magnitude: 125,
                decimal_places: 0,
            },
        });
        operation
    };
    host.immediate("1000", LinkEffect::RemoteBase(operation()), clock())
        .unwrap();
    assert_eq!(host.remote_pending.len(), 1);
    host.pump(clock()).unwrap();
    assert_eq!(host.remote_pending.len(), 1);
    assert_eq!(SOURCE.get(), 2);
    READY.set(true);
    host.pump(clock()).unwrap();
    assert!(host.remote_pending.is_empty());
    assert_eq!(DESTROYED.get(), 1);
    assert_eq!(SOURCE.get(), 0);
    READY.set(false);
    host.immediate("1000", LinkEffect::RemoteBase(operation()), clock())
        .unwrap();
    host.reload(document("usb", config), clock()).unwrap();
    assert!(host.remote_pending.is_empty());
    assert_eq!(DESTROYED.get(), 2);
    assert_eq!(SOURCE.get(), 0);
    host.stop(1000);
}

#[test]
fn remote_configuration_requires_complete_host_operations_before_device_activation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let config = "[remote_base 1000]\nenabled=yes\nbackend=hamlib\nhamlib_model=RIG_MODEL_DUMMY\nstartup_memory=local\nallowed_modes=FM\nallowed_rx_ranges_hz=144000000-148000000\nallowed_tx_ranges_hz=144000000-148000000\n[remote_memory 1000 local]\nselector=1\nrx_frequency_hz=146520000\ntx_frequency_hz=146520000\nmode=FM\n";
    assert!(
        Host::start(
            document("usb", config),
            media(),
            services(),
            Instant::now(),
            clock()
        )
        .is_err()
    );
}

#[test]
fn host_keeps_reload_configuration_until_radio_activation_is_ready() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    thread_local! { static ACTIVATION: Cell<i32> = const { Cell::new(0) }; }
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        let status = ACTIVATION.get();
        if status < 0 {
            ACTIVATION.set(0);
        }
        status
    }
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_ready = Some(ready);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::start(
        document("usb", ""),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let revision = host.runtime.revision();
    ACTIVATION.set(1);
    host.reload(document("replacement", ""), clock()).unwrap();
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(host.runtime.settings("1000").unwrap().channel, "usb");
    host.pump(clock()).unwrap();
    assert_eq!(host.runtime.revision(), revision);
    ACTIVATION.set(0);
    host.pump(clock()).unwrap();
    assert!(host.runtime.revision() > revision);
    assert_eq!(
        host.runtime.settings("1000").unwrap().channel,
        "replacement"
    );
    assert!(host.stop(1000));
    let mut host = Host::start(
        document("usb", ""),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let revision = host.runtime.revision();
    ACTIVATION.set(1);
    host.reload(document("rejected", ""), clock()).unwrap();
    ACTIVATION.set(-123);
    host.pump(clock()).unwrap();
    assert_eq!(host.poll_reload(clock()), Err(RuntimeError::Device));
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(host.runtime.settings("1000").unwrap().channel, "usb");
    assert!(
        crate::fixture::SCHEDULER_NOTICES
            .lock()
            .unwrap()
            .iter()
            .any(|notice| notice.contains("radio 1000 activation failed: -123"))
    );
    host.stop(1000);
}
