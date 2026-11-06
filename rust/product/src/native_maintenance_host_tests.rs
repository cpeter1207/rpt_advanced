//! Physical maintenance retains its lease while controller generations drain.

use super::*;

#[derive(Default)]
struct Calls {
    handle: Option<usize>,
    operations: Vec<u32>,
}

unsafe fn record(context: *mut c_void, radio: *mut c_void, operation: u32) {
    // SAFETY: the test retains this shared fixture until host destruction.
    let mut calls = unsafe { &*context.cast::<Mutex<Calls>>() }.lock().unwrap();
    assert!(!radio.is_null());
    assert_eq!(*calls.handle.get_or_insert(radio as usize), radio as usize);
    calls.operations.push(operation);
}

unsafe extern "C" fn update(context: *mut c_void, radio: *mut c_void, phase: u32) -> i32 {
    // SAFETY: the unique host control owner serializes these fixture calls.
    unsafe { record(context, radio, 10 + phase) };
    0
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
    unsafe { record(context, radio, request.operation) };
    if request.operation == 2 {
        request.command.value = 333;
    }
    if let Some(sink) = sink {
        // SAFETY: this static text is borrowed only during the callback.
        unsafe { sink(sink_context, c"physical=ready\n".as_ptr(), 15) };
    }
    0
}

#[test]
fn physical_status_mixer_and_cleanup_keep_the_live_lease_during_controller_retirement() {
    struct Resume(Arc<Barrier>);
    impl Drop for Resume {
        fn drop(&mut self) {
            self.0.wait();
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let calls = Box::new(Mutex::new(Calls::default()));
    // SAFETY: the released fixture supplies all required host operations.
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = std::ptr::from_ref(calls.as_ref()).cast_mut().cast();
    table.radio_update = Some(update);
    table.radio_control = Some(maintenance);
    // SAFETY: the immutable table and tracker outlive all host workers.
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::start(
        document("usb", ""),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    let opens = crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed);
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    host.reload(
        document("usb", "[radio 1000]\nreceive_output_gain_db=-6\n"),
        clock(),
    )
    .unwrap();
    assert!(host.runtime.status(10)[0].1.retiring.is_some());

    let backends =
        Arc::new(crate::site::backends::SiteBackends::load(host.runtime.document()).unwrap());
    let mut acquisition = Some(
        crate::site::acquisition::Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(crate::site::radio::RadioSite::default());
    host.begin_site_reload(
        host.runtime.revision(),
        &mut acquisition,
        &mut radio,
        &Default::default(),
    )
    .unwrap();
    assert!(host.site_transition.is_some());
    for operation in [1, 2, 4] {
        // SAFETY: the ABI request contains only scalar fields and nullable pointers.
        let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = operation;
        request.command.struct_size = std::mem::size_of_val(&request.command) as u32;
        request.command.abi_version = 1;
        request.command.command = abi::URP_AST_COMMAND_GET_MIXER;
        request.command.target = abi::URP_AST_MIXER_RECEIVE;
        let reply = host.radio_control("1000", request).unwrap();
        assert_eq!(reply.text, "physical=ready\n");
        if operation == 2 {
            assert_eq!(reply.request.command.value, 333);
        }
        assert!(host.runtime.status(10)[0].1.retiring.is_some());
    }
    assert_eq!(calls.lock().unwrap().operations, [10, 11, 1, 2, 4]);
    assert_eq!(
        crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
        opens
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    drop(host.abort_sites());
    drop(resume);
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    assert!(host.stop(20));
}

#[test]
fn unsupported_physical_update_uses_handoff_and_later_failure_aborts_the_retained_radio() {
    unsafe extern "C" fn selective_update(
        context: *mut c_void,
        radio: *mut c_void,
        phase: u32,
    ) -> i32 {
        // SAFETY: the fixture retains this radio while its serialized update runs.
        if unsafe { crate::fixture::radio_name(radio) }.starts_with("2000\0") {
            assert_eq!(phase, 0);
            1
        } else {
            // SAFETY: this is the existing first radio's shared tracker/lease.
            unsafe { update(context, radio, phase) }
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let calls = Box::new(Mutex::new(Calls::default()));
    // SAFETY: the fixture supplies the complete released host table.
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = std::ptr::from_ref(calls.as_ref()).cast_mut().cast();
    table.radio_update = Some(selective_update);
    table.radio_control = Some(maintenance);
    // SAFETY: the immutable table and tracker outlive all host workers.
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let source = document("usb", "[2000]\nradio_channel=usb2\n");
    let mut host = Host::start(source.clone(), media(), services, Instant::now(), clock()).unwrap();
    let opens = crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed);
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    crate::fixture::RADIO_OPEN_RESULT.store(1, std::sync::atomic::Ordering::Relaxed);
    let candidate = document(
        "usb",
        "[2000]\nradio_channel=usb2\n[radio 1000]\nreceive_output_gain_db=-6\n[radio 2000]\ndevice_selection=exact\ndevice_identifier=another-device\n",
    );
    assert_eq!(host.reload(candidate, clock()), Err(RuntimeError::Device));
    assert_eq!(host.runtime.revision(), 1);
    assert_eq!(host.runtime.document(), &source);
    // SAFETY: a complete initialized request is borrowed by the current serialized lease.
    let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    request.struct_size = std::mem::size_of_val(&request) as u32;
    request.abi_version = 1;
    request.operation = 1;
    assert_eq!(
        host.radio_control("1000", request).unwrap().text,
        "physical=ready\n"
    );
    assert_eq!(calls.lock().unwrap().operations, [10, 12, 1]);
    // Only the unsupported second radio closes and reopens; the accepted first
    // radio restores its prepared reverse without losing its original stream.
    assert_eq!(
        crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
        opens + 1
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops + 1
    );
    assert!(host.stop(20));
}
