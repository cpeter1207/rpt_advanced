use super::*;
use rpt_advanced_core::remote_base_command::{DecimalValue, RemoteCommand};
use std::cell::Cell;

thread_local! { static READY: Cell<bool> = const { Cell::new(false) }; static SOURCE: Cell<u32> = const { Cell::new(0) }; static DESTROYED: Cell<u32> = const { Cell::new(0) }; static MODE: Cell<i32> = const { Cell::new(0) }; static EXPECTED_FLAGS: Cell<u32> = const { Cell::new(1) }; }
unsafe extern "C" fn source(_: *mut c_void, flags: u32) -> i32 {
    SOURCE.set(flags);
    if flags != 0 && MODE.get() == 3 {
        -42
    } else {
        0
    }
}
unsafe extern "C" fn take(
    _: *mut c_void,
    sink: abi::rptadv_remote_sink_v1,
    context: *mut c_void,
) -> i32 {
    if !READY.get() {
        return 1;
    }
    if MODE.get() == 4 {
        return -42;
    }
    let bytes: &[u8] = if MODE.get() == 5 {
        b"malformed"
    } else {
        br#"{"version":1,"kind":"applied"}"#
    };
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
    assert_eq!(flags, EXPECTED_FLAGS.get());
    if MODE.get() == 1 {
        return 1;
    }
    if MODE.get() == 2 {
        return -42;
    }
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

pub(super) fn remote_services() -> HostServices {
    let mut table = service_table();
    table.radio_remote_submit = Some(submit);
    table.radio_metadata_bind = Some(bind_metadata);
    table.radio_service = Some(service);
    table.radio_quiesce = Some(service);
    table.radio_ready = Some(service);
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}

pub(super) fn remote_configuration() -> PreparedConfiguration {
    configuration(
        "usb",
        &[
            (Kind::Radio, "", json!({"startup_memory":1})),
            (
                Kind::Control,
                "",
                json!({"enabled":true,"backend":"hamlib","access":"open","hamlib":{"model":"RIG_MODEL_DUMMY"}}),
            ),
            (
                Kind::Limits,
                "",
                json!({"rx_ranges_mhz":[[144,148]],"tx_ranges_mhz":[[144,148]],"modes":["FM"]}),
            ),
            (
                Kind::Memories,
                "",
                json!({"entries":[{"frequency":146.52,"offset":{"enabled":false,"direction":"plus","frequency":0.6},"mode":"FM","tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}}]}),
            ),
        ],
    )
}

fn active_device(host: &Host) -> &str {
    &host
        .runtime
        .modular_configuration()
        .unwrap()
        .radio
        .port(LOCAL.parse().unwrap())
        .unwrap()
        .interface
        .audio
        .device_identifier
}

#[test]
fn host_remote_completion_is_nonblocking_and_reload_revokes_pending_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    READY.set(false);
    SOURCE.set(0);
    DESTROYED.set(0);
    MODE.set(0);
    EXPECTED_FLAGS.set(1);
    let services = remote_services();
    let mut host = Host::fixture_start(
        remote_configuration(),
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
    host.immediate(LOCAL, LinkEffect::RemoteBase(operation()), clock())
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
    host.immediate(LOCAL, LinkEffect::RemoteBase(operation()), clock())
        .unwrap();
    host.fixture_reload(remote_configuration(), clock())
        .unwrap();
    host.pump_remote(clock());
    assert!(host.remote_pending.is_empty());
    assert_eq!(DESTROYED.get(), 2);
    assert_eq!(SOURCE.get(), 0);
    // A request with no remote operation fails shared authorization before any
    // physical callback. Synchronous failures never acquire a pending ticket.
    let mut denied = operation();
    denied.remote = None;
    assert_eq!(
        host.submit_remote(LOCAL, denied, clock()),
        Err(RuntimeError::Rejected)
    );
    for mode in [1, 2] {
        MODE.set(mode);
        host.submit_remote(LOCAL, operation(), clock()).unwrap();
        assert!(host.remote_pending.is_empty());
        assert_eq!(DESTROYED.get(), 2);
    }
    READY.set(true);
    for (offset, mode) in [3, 4, 5].into_iter().enumerate() {
        MODE.set(mode);
        host.submit_remote(LOCAL, operation(), clock()).unwrap();
        assert_eq!(host.remote_pending.len(), 1);
        host.pump_remote(clock());
        assert!(host.remote_pending.is_empty());
        assert_eq!(DESTROYED.get(), 3 + offset as u32);
        assert_eq!(SOURCE.get(), 0);
    }
    MODE.set(0);
    READY.set(false);
    EXPECTED_FLAGS.set(0);
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    let signals = rpt_advanced_core::link::PeerSignals::new();
    let reply = signals.reply_target();
    let links = host.runtime.node(LOCAL).unwrap().links();
    links
        .attach_ingress(PeerIdentity::from_operator("2000").unwrap(), reply.clone())
        .unwrap();
    links.peer_digit("2000", '*', clock().now_ms, reply.clone());
    let mut request = operation();
    request.origin = Some("2000".into());
    request.reply = Some(reply);
    host.submit_remote(LOCAL, request, clock()).unwrap();
    assert_eq!(host.remote_pending.len(), 1);
    signals.end();
    host.pump_remote(clock());
    assert!(host.remote_pending.is_empty());
    assert_eq!(DESTROYED.get(), 6);
    assert_eq!(SOURCE.get(), 0);
    host.runtime.node(LOCAL).unwrap().links().ended("2000");
    host.runtime.peer_detached(LOCAL, "2000", clock().now_ms);
    assert!(host.fixture_stop(1000));
}

#[test]
fn remote_configuration_requires_complete_host_operations_before_device_activation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(
        Host::fixture_start(
            remote_configuration(),
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
    let mut table = service_table();
    table.radio_ready = Some(ready);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let revision = host.runtime.revision();
    ACTIVATION.set(1);
    let mut adoption = host
        .fixture_begin_reload(configuration("replacement", &[]), clock())
        .unwrap();
    assert_eq!(
        host.fixture_begin_reload(configuration("second", &[]), clock())
            .err(),
        Some(RuntimeError::Busy)
    );
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(active_device(&host), "usb");
    assert!(!adoption.poll(&mut host, clock()).unwrap());
    assert_eq!(host.runtime.revision(), revision);
    ACTIVATION.set(0);
    pump_until(&mut host, |host| adoption.poll(host, clock()).unwrap());
    assert!(host.runtime.revision() > revision);
    assert_eq!(active_device(&host), "replacement");
    assert!(host.fixture_stop(1000));
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let revision = host.runtime.revision();
    ACTIVATION.set(1);
    let mut adoption = host
        .fixture_begin_reload(configuration("rejected", &[]), clock())
        .unwrap();
    ACTIVATION.set(-123);
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = loop {
        match adoption.poll(&mut host, clock()) {
            Ok(false) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            terminal => break terminal,
        }
    };
    assert_eq!(result, Err(RuntimeError::Device));
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(active_device(&host), "usb");
    assert!(
        crate::fixture::SCHEDULER_NOTICES
            .lock()
            .unwrap()
            .iter()
            .any(|notice| notice.contains(&format!("radio {LOCAL} activation failed: -123")))
    );
    host.stop(1000);
}
