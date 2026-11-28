//! Physical maintenance retains its lease while controller generations drain.

use super::*;

#[derive(Default)]
pub(super) struct Calls {
    handle: Option<usize>,
    operations: Vec<u32>,
    unsupported: usize,
}

unsafe fn record(context: *mut c_void, radio: *mut c_void, operation: u32) -> bool {
    // SAFETY: the test retains this shared fixture until host destruction.
    let mut calls = unsafe { &*context.cast::<Mutex<Calls>>() }.lock().unwrap();
    if radio.is_null() || *calls.handle.get_or_insert(radio as usize) != radio as usize {
        return false;
    }
    calls.operations.push(operation);
    true
}

unsafe extern "C" fn update(context: *mut c_void, radio: *mut c_void, phase: u32) -> i32 {
    // SAFETY: the unique host control owner serializes these fixture calls.
    if matches!(phase, 1 | 2) && unsafe { record(context, radio, 10 + phase) } {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn maintenance(
    context: *mut c_void,
    radio: *mut c_void,
    request: *mut abi::rptadv_radio_control_v1,
    sink: abi::rptadv_text_sink_v1,
    sink_context: *mut c_void,
) -> i32 {
    // SAFETY: Host supplies its complete exclusive typed request.
    let request = unsafe { &mut *request };
    if request.operation == 7 {
        request.command.value = 0;
        request.command.flags = 0;
        return 0;
    }
    // SAFETY: this shared tracker remains retained through the synchronous call.
    if !unsafe { record(context, radio, request.operation) } {
        return -1;
    }
    if request.operation == 2 {
        request.command.value = 333;
    }
    if let Some(sink) = sink {
        // SAFETY: this static text is borrowed only during the callback.
        unsafe { sink(sink_context, c"physical=ready\n".as_ptr(), 15) };
    }
    0
}

unsafe extern "C" fn stage(context: *mut c_void, radio: *mut c_void, plan: *const c_void) -> i32 {
    // SAFETY: fixture::station creates a boxed port UUID string for each borrowed plan.
    let Some(owner) = (unsafe { plan.cast::<String>().as_ref() }) else {
        return -1;
    };
    if radio.is_null() || unsafe { crate::fixture::radio_name(radio) } != *owner {
        return -1;
    }
    if owner == SECOND_LOCAL {
        unsafe { &*context.cast::<Mutex<Calls>>() }
            .lock()
            .unwrap()
            .unsupported += 1;
        return 1;
    }
    if owner == LOCAL && unsafe { record(context, radio, 10) } {
        0
    } else {
        -1
    }
}

pub(super) fn tracked_services(calls: &Mutex<Calls>) -> HostServices {
    let mut table = service_table();
    table.context = ptr::from_ref(calls).cast_mut().cast();
    let mut binding = station::BINDING.0;
    binding.stage_update = Some(stage);
    table.station_binding = Box::leak(Box::new(binding));
    table.radio_update = Some(update);
    table.radio_control = Some(maintenance);
    // SAFETY: tests retain their boxed tracker beyond all radio owners; both tables are static.
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}

fn device_counts() -> (usize, usize) {
    use std::sync::atomic::Ordering;
    (
        crate::fixture::RADIO_OPENS.load(Ordering::Relaxed),
        crate::fixture::RADIO_DROPS.load(Ordering::Relaxed),
    )
}

fn physical_query(host: &Host, operation: u32) {
    // Exercise the serialized physical worker. The native-maintenance-session HTTP case
    // covers grant ownership, stale requests, and clearing keyed/tone state on disconnect.
    let lease = &host
        .leases
        .iter()
        .find(|(owner, _)| owner == LOCAL)
        .unwrap()
        .1;
    let mut lease = lease.lock().unwrap();
    assert!(!lease.quiesced && !lease.quiescing);
    // SAFETY: the complete initialized request contains scalar fields and nullable pointers.
    let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    request.struct_size = std::mem::size_of_val(&request) as u32;
    request.abi_version = 1;
    request.operation = operation;
    request.command.struct_size = std::mem::size_of_val(&request.command) as u32;
    request.command.abi_version = 1;
    request.command.command = abi::URP_AST_COMMAND_GET_MIXER;
    request.command.target = abi::URP_AST_MIXER_RECEIVE;
    assert_eq!(
        lease
            .worker
            .as_mut()
            .unwrap()
            .maintenance(&mut request)
            .unwrap(),
        "physical=ready\n"
    );
    if operation == 2 {
        assert_eq!(request.command.value, 333);
    }
}

#[test]
fn physical_status_mixer_and_cleanup_keep_the_live_lease_during_controller_retirement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let calls = Box::new(Mutex::new(Calls::default()));
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        tracked_services(&calls),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let held = host.runtime.node(LOCAL).unwrap().work().unwrap();
    let before = device_counts();
    let mut adoption = host
        .fixture_begin_reload(
            configuration("usb", &[(Kind::RxAudio, "", json!({"output_gain_db":-6}))]),
            clock(),
        )
        .unwrap();
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring == Some(held.id())
    });
    for op in [1, 2, 4] {
        physical_query(&host, op);
        assert_eq!(host.runtime.status(10)[0].1.retiring, Some(held.id()));
    }
    assert_eq!(calls.lock().unwrap().operations, [10, 11, 1, 2, 4]);
    assert_eq!(device_counts(), before);
    drop(held);
    pump_until(&mut host, |host| {
        adoption.poll(host, clock()).unwrap() && host.runtime.status(10)[0].1.retiring.is_none()
    });
    let lease = host
        .leases
        .iter()
        .find(|(owner, _)| owner == LOCAL)
        .unwrap()
        .1
        .clone();
    {
        let mut lease = lease.lock().unwrap();
        assert!(lease.quiesce());
        assert!(lease.worker.is_none());
    }
    assert!(host.fixture_stop(20));
}

#[test]
fn unsupported_physical_update_uses_handoff_and_later_failure_aborts_the_retained_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let calls = Box::new(Mutex::new(Calls::default()));
    let ports = [
        (LOCAL, "1000", "usb", json!({})),
        (SECOND_LOCAL, "2000", "usb2", json!({})),
    ];
    let source = stations(&ports, &[]);
    let previous = Arc::clone(source.runtime());
    let mut host = Host::fixture_start(
        source,
        media(),
        tracked_services(&calls),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let revision = host.runtime.revision();
    let (opens, drops) = device_counts();
    let changes = [
        (LOCAL, Kind::RxAudio, "", json!({"output_gain_db":-6})),
        (
            SECOND_LOCAL,
            Kind::RxAudio,
            "",
            json!({"output_gain_db":-9}),
        ),
    ];
    crate::fixture::RADIO_OPEN_RESULT.store(1, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        host.fixture_reload(stations(&ports, &changes), clock()),
        Err(RuntimeError::Device)
    );
    assert_eq!(host.runtime.revision(), revision);
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &previous
    ));
    physical_query(&host, 1);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.unsupported, 1);
    assert_eq!(calls.operations, [10, 12, 1]);
    drop(calls);
    // The unsupported station was closed and restored after its new open failed.
    // The first station used its staged reverse on the same live handle.
    assert_eq!(device_counts(), (opens + 1, drops + 1));
    assert!(host.fixture_stop(20));
}
