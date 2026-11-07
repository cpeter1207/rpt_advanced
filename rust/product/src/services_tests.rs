use super::*;
use std::{
    ffi::{c_char, c_void},
    mem::size_of_val,
    ptr,
    sync::atomic::{AtomicU8, Ordering},
};

static LOCAL_TIME_MODE: AtomicU8 = AtomicU8::new(0);
static LOOKUP_MODE: AtomicU8 = AtomicU8::new(0);
static RADIO_OPEN_MODE: AtomicU8 = AtomicU8::new(0);
static PEER_DIAL_MODE: AtomicU8 = AtomicU8::new(0);
static DIRECTORY_NOTICE: AtomicU8 = AtomicU8::new(0);
#[path = "services_echo_tests.rs"]
mod echo;
#[path = "services_processing_tests.rs"]
mod processing;
#[path = "services_remote_tests.rs"]
mod remote;

unsafe extern "C" fn echo_info(
    _: *mut c_void,
    _: *const c_void,
    output: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    unsafe { crate::fixture::peer_info(ptr::null_mut(), ptr::null(), output) };
    let output = unsafe { &mut *output };
    output.protocol = 2;
    output.capabilities = 23;
    output.account_generation = 3;
    output.qso_generation = 4;
    output.remote = abi::rptadv_peer_bytes_v1 {
        data: b"123456".as_ptr(),
        length: 6,
    };
    output.callsign = abi::rptadv_peer_bytes_v1 {
        data: b"W1XYZ-R".as_ptr(),
        length: 7,
    };
    0
}

#[test]
fn mixed_protocol_host_dispatch_uses_each_peers_copied_capabilities() {
    unsafe extern "C" fn info(
        _: *mut c_void,
        _: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        let output = unsafe { &mut *output };
        *output = abi::rptadv_peer_info_v1 {
            struct_size: size_of::<abi::rptadv_peer_info_v1>() as u32,
            abi_version: 1,
            protocol: 1,
            capabilities: 0,
            account_generation: 0,
            qso_generation: 0,
            remote: abi::rptadv_peer_bytes_v1 {
                data: b"2000".as_ptr(),
                length: 4,
            },
            callsign: abi::rptadv_peer_bytes_v1 {
                data: ptr::null(),
                length: 0,
            },
            local: abi::rptadv_peer_bytes_v1 {
                data: b"1000".as_ptr(),
                length: 4,
            },
        };
        0
    }
    let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    assert!(prepared.async_owner().is_some());
    assert!(prepared.prepared_media());
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(info);
        table.peer_write = unsafe { (*crate::fixture::host_descriptor()).peer_write };
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    assert!(
        peer.async_owner().is_none(),
        "a PCM peer must not inherit another protocol's async callbacks"
    );
    assert!(
        !peer.prepared_media(),
        "a PCM peer must not use globally present codec callbacks"
    );
    assert_eq!(peer.identity().node(), "2000");
    assert_eq!(peer.callsign(), "");
    assert_eq!(peer.generations(), (0, 0));
    assert!(matches!(
        peer.read_source(SourceCompletion::Pending, |_| 0),
        Err(Error::Operation)
    ));
    assert!(matches!(
        peer.send_human_text("W1XYZ", "Hello"),
        Err(Error::Operation)
    ));
}

#[test]
fn source_read_preserves_prefix_credit_provenance_and_finite_acknowledgement() {
    unsafe extern "C" fn read(
        _: *mut c_void,
        _: *mut c_void,
        drained: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        text: abi::rptadv_peer_source_text_v1,
        context: *mut c_void,
        output: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        assert!(drained <= 2);
        let credit = if drained == 0 { 2 } else { 0 };
        let sink = unsafe { &*sink };
        let pcm = [0.25; 4];
        assert_eq!(
            unsafe { sink.audio.unwrap()(sink.context, 12, pcm.as_ptr(), pcm.len(), 1) },
            credit
        );
        assert_eq!(
            unsafe { sink.gap.unwrap()(sink.context, 16, 3) },
            credit as u32
        );
        unsafe {
            text.unwrap()(
                context,
                abi::rptadv_peer_bytes_v1 {
                    data: b"W1XYZ-R".as_ptr(),
                    length: 7,
                },
                abi::rptadv_peer_bytes_v1 {
                    data: b"Hello".as_ptr(),
                    length: 5,
                },
            )
        };
        let output = unsafe { &mut *output };
        output.progress = drained;
        output.activity = 1;
        output.finish_pending = 1;
        output.state = 1;
        output.represented_node_number = 123456;
        0
    }
    let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(echo_info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    for (completion, code) in [
        (SourceCompletion::Pending, 0),
        (SourceCompletion::Drained, 1),
        (SourceCompletion::Cancelled, 2),
    ] {
        let mut seen = Vec::new();
        let result = peer
            .read_source(completion, |input| {
                match input {
                    SourceInput::Audio {
                        start,
                        samples,
                        concealed,
                    } => {
                        assert_eq!((start, samples, concealed), (12, &[0.25; 4][..], true));
                        seen.push("audio");
                    }
                    SourceInput::Gap { start, count } => {
                        assert_eq!((start, count), (16, 3));
                        seen.push("gap");
                    }
                    SourceInput::Text {
                        sender,
                        body,
                        represented_node_number,
                    } => {
                        assert_eq!((sender, body), ("W1XYZ-R", "Hello"));
                        assert_eq!(represented_node_number, 123456);
                        seen.push("text");
                    }
                }
                2
            })
            .unwrap();
        assert_eq!(
            seen,
            if code == 0 {
                vec!["audio", "gap", "text"]
            } else {
                vec!["text"]
            }
        );
        assert_eq!(
            (
                result.progress,
                result.activity,
                result.finish_pending,
                result.closed
            ),
            (code, Some(true), true, false)
        );
    }
}

#[test]
fn scheduler_notice_preserves_borrowed_utf8() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    services.scheduler_notice("scheduler queue 50% — Δ");
    assert_eq!(
        *crate::fixture::SCHEDULER_NOTICES.lock().unwrap(),
        ["scheduler queue 50% — Δ"]
    );
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
}

#[test]
fn scheduler_notice_is_required_for_host_admission() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.scheduler_notice = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

#[test]
fn site_bridge_rejects_a_host_without_the_site_contract() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.abi_version = 6;
    table.capability = *b"rptadv.hst6\0\0\0\0\0";
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table = unsafe { crate::fixture::host_descriptor().read() };
    for length in [
        std::mem::offset_of!(abi::rptadv_host_services_v10, radio_site_bind),
        std::mem::offset_of!(abi::rptadv_host_services_v10, radio_site_publish),
        std::mem::offset_of!(abi::rptadv_host_services_v10, radio_site_snapshot),
    ] {
        table.struct_size = length as u32;
        assert!(matches!(
            unsafe { HostServices::open(&table) },
            Err(Error::Admission)
        ));
    }
}

#[test]
fn site_bridge_requires_every_operation_and_preserves_owner_observations() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = unsafe { crate::fixture::host_descriptor().read() };
    for missing in 0..3 {
        let mut table = fixture;
        match missing {
            0 => table.radio_site_bind = None,
            1 => table.radio_site_publish = None,
            _ => table.radio_site_snapshot = None,
        }
        assert!(matches!(
            unsafe { HostServices::open(&table) },
            Err(Error::Admission)
        ));
    }
    unsafe extern "C" fn bind(
        _: *mut c_void,
        radio: *mut c_void,
        bank: u32,
        request: *const abi::UrpAstSiteBinding,
    ) -> i32 {
        assert!(!radio.is_null());
        assert!(bank <= 1);
        let request = unsafe { &*request };
        assert_eq!(
            (
                request.generation,
                request.output_mask,
                request.initial_high_mask,
                request.safe_high_mask
            ),
            (71, 4, 4, 0)
        );
        if bank == 1 { -3 } else { 0 }
    }
    unsafe extern "C" fn publish(
        _: *mut c_void,
        radio: *mut c_void,
        bank: u32,
        request: *const abi::UrpAstSiteRequest,
    ) -> i32 {
        assert!(!radio.is_null());
        assert!(bank <= 1);
        let request = unsafe { &*request };
        assert_eq!(
            (request.generation, request.output_mask, request.high_mask),
            (72, 4, 4)
        );
        if bank == 1 { 0 } else { -4 }
    }
    unsafe extern "C" fn snapshot(
        _: *mut c_void,
        radio: *mut c_void,
        bank: u32,
        output: *mut abi::UrpAstSiteSnapshot,
    ) -> i32 {
        assert!(!radio.is_null());
        let output = unsafe { &mut *output };
        assert_eq!(output.struct_size as usize, size_of_val(output));
        assert_eq!(output.abi_version, 1);
        output.generation = 72;
        output.serviced_generation = 71;
        output.observed_at_valid = 1;
        output.observed_at_ms = 100;
        output.snapshot_at_ms = 700;
        output.input_quality = 2;
        output.last_command_error = 2;
        match bank {
            1 => 0,
            2 => -5,
            3 => {
                output.struct_size -= 1;
                0
            }
            _ => {
                output.abi_version = 2;
                0
            }
        }
    }
    let services = host_services(|table| {
        table.radio_site_bind = Some(bind);
        table.radio_site_publish = Some(publish);
        table.radio_site_snapshot = Some(snapshot);
    });
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    let binding = abi::UrpAstSiteBinding {
        struct_size: size_of::<abi::UrpAstSiteBinding>() as u32,
        abi_version: 1,
        generation: 71,
        output_mask: 4,
        initial_high_mask: 4,
        safe_high_mask: 0,
    };
    let request = abi::UrpAstSiteRequest {
        struct_size: size_of::<abi::UrpAstSiteRequest>() as u32,
        abi_version: 1,
        generation: 72,
        output_mask: 4,
        high_mask: 4,
    };
    assert_eq!(radio.site_bind(1, &binding), Err(-3));
    assert_eq!(radio.site_bind(0, &binding), Ok(()));
    assert_eq!(radio.site_publish(1, &request), Ok(()));
    assert_eq!(radio.site_publish(0, &request), Err(-4));
    let observed = radio.site_snapshot(1).unwrap();
    assert_eq!(
        (observed.generation, observed.serviced_generation),
        (72, 71)
    );
    assert_eq!(
        (observed.observed_at_ms, observed.snapshot_at_ms),
        (100, 700)
    );
    assert_eq!(
        (
            observed.input_quality,
            observed.last_command_error,
            observed.readback_valid_mask
        ),
        (2, 2, 0)
    );
    for (bank, error) in [(2, -5), (3, -1), (4, -1)] {
        assert!(matches!(radio.site_snapshot(bank), Err(actual) if actual == error));
    }
}

#[test]
fn directory_host_revision_and_every_backend_callback_are_required() {
    let fixture = unsafe { crate::fixture::host_descriptor().read() };
    let mut old = fixture;
    old.abi_version = 4;
    assert!(matches!(
        unsafe { HostServices::open(&old) },
        Err(Error::Admission)
    ));
    for missing in 0..4 {
        let mut table = fixture;
        match missing {
            0 => table.directory_record = None,
            1 => table.directory_srv = None,
            2 => table.directory_addresses = None,
            _ => table.directory_notice = None,
        }
        assert!(matches!(
            unsafe { HostServices::open(&table) },
            Err(Error::Admission)
        ));
    }
}

unsafe extern "C" fn unavailable_srv(
    _: *mut c_void,
    _: *const c_char,
    _: usize,
    _: abi::rptadv_directory_srv_sink_v1,
    _: *mut c_void,
) -> i32 {
    1
}
unsafe extern "C" fn directory_notice(_: *mut c_void, reason: u32) {
    DIRECTORY_NOTICE.store(reason as u8, Ordering::Relaxed);
}

#[test]
fn directory_backend_failures_keep_host_diagnostics_and_selected_fallback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = host_services(|table| {
        table.directory_srv = Some(unavailable_srv);
        table.directory_notice = Some(directory_notice);
    });
    assert_eq!(
        services.lookup(1, "", "external", "123", None),
        Err(Error::Operation)
    );
    assert_eq!(DIRECTORY_NOTICE.load(Ordering::Relaxed), 4);
    assert_eq!(
        services.lookup(0, "", "external", "123", None),
        Ok("radio@fixture/123".into())
    );
    assert_eq!(
        services.lookup(2, "static", "", "123", Some("")),
        Ok("radio@fixture/123".into())
    );
    assert_eq!(
        services.lookup(2, "static", "", "123", Some("192.0.2.2")),
        Err(Error::Operation)
    );
    assert_eq!(DIRECTORY_NOTICE.load(Ordering::Relaxed), 2);
    assert_eq!(
        services.lookup(2, "", "", "123", None),
        Err(Error::Operation)
    );
    assert_eq!(DIRECTORY_NOTICE.load(Ordering::Relaxed), 3);
}

#[test]
fn borrowed_directory_results_validate_strings_cardinality_and_status() {
    let mut results = DirectoryResults::new();
    unsafe { directory_text(ptr::from_mut(&mut results).cast(), ptr::null(), 0) };
    assert_eq!(
        results.finish(0).unwrap().single().unwrap(),
        Some(String::new())
    );
    assert!(matches!(
        DirectoryResults::new().finish(-1),
        Err(BackendError::Rejected)
    ));
    assert!(matches!(
        DirectoryResults::new().finish(1),
        Err(BackendError::Unavailable)
    ));
    let mut results = DirectoryResults::new();
    unsafe {
        directory_srv(
            ptr::from_mut(&mut results).cast(),
            c"srv.example".as_ptr(),
            11,
            4571,
        )
    };
    assert_eq!(results.port, 4571);
    assert_eq!(results.single().unwrap(), Some("srv.example".into()));
}

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
    table.abi_version = 7;
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

fn host_services(configure: impl FnOnce(&mut abi::rptadv_host_services_v10)) -> HostServices {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    configure(&mut table);
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}

#[test]
fn radio_update_is_optional_and_preserves_stage_results_and_finish_errors() {
    let services = host_services(|_| {});
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.configuration_update(0), Ok(false));
    assert_eq!(radio.configuration_update(1), Err(-7));
    thread_local! { static MODE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) }; }
    unsafe extern "C" fn update(_: *mut c_void, radio: *mut c_void, phase: u32) -> i32 {
        assert!(!radio.is_null());
        assert!(phase <= 2);
        MODE.get()
    }
    let services = host_services(|table| table.radio_update = Some(update));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    for phase in 0..=2 {
        for code in [0, 1, -5, -9] {
            MODE.set(code);
            assert_eq!(
                radio.configuration_update(phase),
                match code {
                    0 => Ok(true),
                    1 if phase == 0 => Ok(false),
                    _ => Err(code),
                }
            );
        }
    }
}

#[test]
fn radio_maintenance_validates_bounded_text_and_keeps_sink_errors() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    thread_local! { static MODE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
    unsafe extern "C" fn control(
        _: *mut c_void,
        radio: *mut c_void,
        _: *mut abi::rptadv_radio_control_v1,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        assert!(!radio.is_null());
        let sink = sink.unwrap();
        let mode = MODE.get();
        match mode {
            0 => unsafe {
                sink(context, ptr::null(), 0);
                sink(context, c"ok".as_ptr(), 2);
            },
            1 => unsafe { sink(context, c"ignored".as_ptr(), 0) },
            2 => unsafe { sink(context, ptr::null(), 1) },
            3 => unsafe {
                sink(context, c"\xff".as_ptr(), 1);
                sink(context, c"ok".as_ptr(), 2);
            },
            4..=6 => {
                let text = vec![b'a'; if mode == 4 { 65537 } else { 65536 }];
                unsafe { sink(context, text.as_ptr().cast(), text.len()) };
                if mode == 5 {
                    unsafe { sink(context, c"!".as_ptr(), 1) };
                }
            }
            _ => return -9,
        }
        0
    }
    let services = host_services(|table| table.radio_control = Some(control));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    for (mode, expected) in [
        (0, Ok(2)),
        (1, Ok(0)),
        (2, Err(-1)),
        (3, Err(-1)),
        (4, Err(-1)),
        (5, Err(-1)),
        (6, Ok(65536)),
        (7, Err(-9)),
    ] {
        MODE.set(mode);
        assert_eq!(
            radio.maintenance(&mut request).map(|text| text.len()),
            expected,
            "maintenance text mode {mode}"
        );
    }
    let services = host_services(|table| table.radio_control = None);
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.maintenance(&mut request), Err(-7));
}

#[test]
fn optional_peer_processor_tail_accepts_the_released_complete_host_prefix() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.struct_size =
        std::mem::offset_of!(abi::rptadv_host_services_v10, peer_processing_prepare) as u32;
    assert!(unsafe { HostServices::open(&table) }.is_ok());
}

#[test]
fn complete_pre_remote_host_v10_prefix_has_no_remote_callbacks() {
    unsafe extern "C" fn bind(
        _: *mut c_void,
        _: *mut c_void,
        _: abi::rptadv_radio_metadata_v1,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn service(_: *mut c_void, _: *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn submit(
        _: *mut c_void,
        _: *mut c_void,
        _: *const abi::UrpRemoteOperation,
        _: u32,
        _: *mut abi::rptadv_remote_ticket_v1,
    ) -> i32 {
        0
    }
    // SAFETY: the fixture retains this immutable descriptor for the test process.
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_metadata_bind = Some(bind);
    table.radio_service = Some(service);
    table.radio_quiesce = Some(service);
    table.radio_remote_submit = Some(submit);
    table.radio_ready = Some(service);
    assert_eq!(size_of_val(&table), 400);
    table.struct_size =
        std::mem::offset_of!(abi::rptadv_host_services_v10, radio_metadata_bind) as u32;
    assert_eq!(table.struct_size, 360);
    let mut prefix = [0_u64; 45];
    // SAFETY: the aligned destination contains exactly the declared complete old prefix.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::from_ref(&table).cast::<u8>(),
            prefix.as_mut_ptr().cast(),
            table.struct_size as usize,
        );
    }
    // SAFETY: the aligned prefix remains readable for its declared size throughout admission.
    let services = unsafe { HostServices::open(prefix.as_ptr().cast()) }.unwrap();
    assert!(services.0.radio_metadata_bind.is_none());
    assert!(services.0.radio_service.is_none());
    assert!(services.0.radio_quiesce.is_none());
    assert!(services.0.radio_remote_submit.is_none());
    assert!(services.0.radio_ready.is_none());
}

#[test]
fn radio_metadata_binding_preserves_optional_host_prefix_and_failures() {
    unsafe extern "C" fn bind(
        _: *mut c_void,
        _: *mut c_void,
        metadata: abi::rptadv_radio_metadata_v1,
    ) -> i32 {
        if metadata.is_some() { 0 } else { -9 }
    }
    unsafe extern "C" fn metadata(_: *mut c_void, _: u32, _: *mut u32) -> i32 {
        0
    }
    let services = host_services(|table| table.radio_metadata_bind = Some(bind));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.bind_metadata(Some(metadata)), Ok(()));
    assert_eq!(radio.bind_metadata(None), Err(Error::Operation));
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.struct_size =
        std::mem::offset_of!(abi::rptadv_host_services_v10, radio_metadata_bind) as u32;
    let services = unsafe { HostServices::open(&table) }.unwrap();
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    assert_eq!(radio.bind_metadata(Some(metadata)), Ok(()));
    table.struct_size += 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

#[test]
fn peer_processor_preparation_uses_the_selected_node_configuration() {
    unsafe extern "C" fn process(_: *mut c_void, _: *mut f32, _: usize) -> i32 {
        0
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {}
    unsafe extern "C" fn prepare(
        _: *mut c_void,
        configuration: *const c_char,
        length: usize,
        local: *const c_char,
        local_length: usize,
        maximum: usize,
        output: *mut abi::rptadv_pcm_processor_v1,
    ) -> i32 {
        assert_eq!(
            unsafe { std::slice::from_raw_parts(configuration.cast::<u8>(), length) },
            b"[link]\nlimiter_enabled=yes\n"
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) },
            b"1000"
        );
        assert_eq!(maximum, 4096);
        let output = unsafe { &mut *output };
        assert_eq!(
            output.struct_size as usize,
            size_of::<abi::rptadv_pcm_processor_v1>()
        );
        assert_eq!(output.abi_version, 1);
        output.context = ptr::without_provenance_mut(1);
        output.process = Some(process);
        output.destroy = Some(destroy);
        0
    }
    let services = host_services(|table| table.peer_processing_prepare = Some(prepare));
    assert!(
        services
            .prepare_peer_processing("[link]\nlimiter_enabled=yes\n", "1000", 4096)
            .unwrap()
            .is_some()
    );
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
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
) -> i32 {
    let sink = sink.unwrap();
    match LOOKUP_MODE.load(Ordering::Relaxed) {
        1 => -1,
        2 => {
            let record = format!("radio@{}/123,192.0.2.1", "x".repeat(1024));
            unsafe { sink(context, record.as_ptr().cast(), record.len()) };
            0
        }
        3 => {
            unsafe { sink(context, c"\xff".as_ptr(), 1) };
            0
        }
        4 => {
            unsafe { sink(context, ptr::null(), 1) };
            0
        }
        5 => {
            unsafe {
                sink(context, c"a".as_ptr(), 1);
                sink(context, c"b".as_ptr(), 1);
            };
            0
        }
        _ => {
            let record = "radio@ok/123,192.0.2.1";
            unsafe { sink(context, record.as_ptr().cast(), record.len()) };
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
        event(context, 4, ptr::null(), 0);
        event(context, 5, ptr::null(), 0);
        event(context, 6, ptr::null(), 80);
        event(context, 4, ptr::from_ref(&digit).cast(), 1);
        event(context, 4, ptr::null(), 1);
        event(context, 5, ptr::from_ref(&digit).cast(), 1);
        event(context, 5, ptr::null(), 1);
        event(context, 0, ptr::null(), 0);
        event(context, 1, ptr::null(), 1);
        event(context, 2, ptr::null(), 1);
        event(context, 2, ptr::from_ref(&digit).cast(), 2);
        event(context, 3, ptr::null(), 1);
        event(context, 6, ptr::null(), 0);
        event(context, 6, ptr::from_ref(&digit).cast(), 80);
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

unsafe extern "C" fn stop_only(_: *mut c_void, _: *mut c_void) {}

#[test]
fn peer_wake_retains_registration_until_closed() {
    use crate::link::dispatcher::PeerWake;
    use std::sync::atomic::AtomicUsize;

    unsafe extern "C" fn wake(_: *mut c_void, registration: *mut c_void) {
        let calls = unsafe { &*registration.cast::<AtomicUsize>() };
        calls.fetch_add(1, Ordering::Relaxed);
    }

    let calls = AtomicUsize::new(0);
    let (peer, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let mut services = peer.services;
    services.0.peer_step_wake = Some(wake);
    let notification = PeerWake::default();
    services.attach_wake(&notification, NonNull::from(&calls).cast());
    notification.notify();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    notification.close();
    notification.notify();
    assert!(notification.is_quiescent());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn prepared_peer_send_reports_transport_failure() {
    let (mut peer, state) = crate::link::session::tests::peer_with_egress(8000, true, true);
    state.lock().unwrap().prepared_result = -1;
    assert_eq!(peer.send_prepared(&[0xff], 1, 20), Err(Error::Write));
    assert_eq!(peer.send_prepared(&[], 0, 21), Err(Error::Write));
    assert_eq!(
        state.lock().unwrap().prepared,
        [(vec![0xff], 20), (vec![], 21)]
    );
}

#[test]
fn prepared_codec_results_reject_each_invalid_provider_field() {
    thread_local! { static CASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    unsafe extern "C" fn encode(
        _: *mut c_void,
        _: *mut c_void,
        _: *const f32,
        _: usize,
        _: *mut u8,
        _: usize,
        _: u32,
        result: *mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        let result = unsafe { &mut *result };
        result.status = 0;
        result.consumed_frames = 1;
        result.encoded_bytes = 1;
        result.source_frames = 1;
        result.packet_frames = 1;
        match CASE.get() {
            0 => return -1,
            1 => result.status = 1,
            2 => result.struct_size = 0,
            3 => result.consumed_frames = 2,
            4 => result.encoded_bytes = 2,
            5 => result.source_frames = 2,
            _ => {
                result.source_frames = 0;
                result.packet_frames = 0;
            }
        }
        0
    }
    let (mut peer, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    peer.services.0.peer_prepare_audio = Some(encode);
    for case in 0..7 {
        CASE.set(case);
        assert!(matches!(
            peer.prepare_audio(&[0.25], &mut [0], false),
            Err(Error::Translation)
        ));
    }
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
    table.struct_size =
        std::mem::offset_of!(abi::rptadv_host_services_v10, peer_step_attach) as u32;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size -= 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size = std::mem::offset_of!(abi::rptadv_host_services_v10, peer_observe) as u32;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size += 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size = (std::mem::offset_of!(abi::rptadv_host_services_v10, peer_destroy)
        + size_of::<usize>() * 2) as u32;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.struct_size = size_of::<abi::rptadv_host_services_v10>() as u32;
    table.peer_step_stop = Some(stop_only);
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.peer_step_stop = None;
    table.abi_version = 1;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.abi_version = 10;
    table.capability[0] = b'!';
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table.capability = *b"rptadv.hst10\0\0\0\0";
    table.local_time = None;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
    table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_write = None;
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
        services.lookup(0, "static", "", "2000", None).unwrap(),
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
    let services = host_services(|table| table.directory_record = Some(lookup_mode));
    for mode in [1, 2, 3, 4, 5] {
        LOOKUP_MODE.store(mode, Ordering::Relaxed);
        assert_eq!(
            services.lookup(0, "static", "external", "123", Some("192.0.2.1")),
            Err(Error::Operation)
        );
    }
    LOOKUP_MODE.store(0, Ordering::Relaxed);
    assert_eq!(
        services.lookup(0, "static", "external", "123", Some("192.0.2.1")),
        Ok("radio@ok/123".into())
    );
    assert!(matches!(
        unsafe { services.peer(ptr::null_mut()) },
        Err(Error::Admission)
    ));

    let services = host_services(|table| table.radio_open = Some(radio_open_mode));
    for mode in [1, 2] {
        RADIO_OPEN_MODE.store(mode, Ordering::Relaxed);
        assert!(matches!(
            services.radio("1000", "usb", 8),
            Err(Error::Operation)
        ));
    }
    RADIO_OPEN_MODE.store(0, Ordering::Relaxed);
    drop(services.radio("1000", "usb", 8).unwrap());

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
    let radio = services.radio("1000", "usb", 8).unwrap();
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
        PeerInput::Gap(samples) => events.push(format!("gap:{samples}")),
        PeerInput::RadioKey => events.push("radio-key".into()),
        PeerInput::RadioUnkey => events.push("radio-unkey".into()),
    })
    .unwrap();
    assert_eq!(
        events,
        [
            "text:[111, 107]",
            "digit:7",
            "audio:[0.25]",
            "radio-key",
            "radio-unkey",
            "gap:80"
        ]
    );
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

#[test]
fn receive_policy_query_requires_exact_response_and_rejects_unknown_success() {
    use std::cell::Cell;
    thread_local! {static CASE:Cell<u8>=const {Cell::new(0)};}
    unsafe extern "C" fn query(
        _: *mut c_void,
        _: *mut c_void,
        p: *mut abi::rptadv_radio_control_v1,
        _: abi::rptadv_text_sink_v1,
        _: *mut c_void,
    ) -> i32 {
        // SAFETY: the typed client owns a complete writable request through this call.
        let p = unsafe { &mut *p };
        assert_eq!(p.operation, 7);
        assert_eq!(p.command.value, -1);
        assert_eq!(p.command.flags, u32::MAX);
        let case = CASE.get();
        if case == 9 {
            return -9;
        }
        if case == 10 {
            return 0;
        }
        p.command.value = if case <= 2 { i64::from(case) } else { 2 };
        p.command.flags = if case == 0 { 0 } else { 7680 };
        match case {
            3 => p.command.value = 3,
            4 => {
                p.command.value = 0;
                p.command.flags = 7680
            }
            5 => p.command.flags = 0,
            6 => p.command.abi_version = 2,
            7 => p.operation = 6,
            8 => p.command.struct_size = 4,
            11 => p.abi_version = 2,
            12 => p.struct_size = 4,
            13 => p.command.command = 1,
            14 => p.command.target = 1,
            _ => {}
        }
        0
    }
    let services = host_services(|table| table.radio_control = Some(query));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    for case in 0..15 {
        CASE.set(case);
        assert_eq!(
            radio.receive_policy(),
            match case {
                0..=2 => Ok(ReceivePolicy {
                    mode: u32::from(case),
                    preroll_frames: if case == 0 { 0 } else { 7680 }
                }),
                9 => Err(-9),
                _ => Err(-1),
            },
            "case{case}"
        );
    }
    let services = host_services(|table| table.radio_control = None);
    assert_eq!(
        services.radio("1000", "usb", 8).unwrap().receive_policy(),
        Err(-7)
    );
}

#[test]
fn maintenance_text_rejects_invalid_spans_and_bounds_the_complete_reply() {
    thread_local! { static CASE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
    unsafe extern "C" fn reply(
        _: *mut c_void,
        _: *mut c_void,
        _: *mut abi::rptadv_radio_control_v1,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        let sink = sink.expect("maintenance client supplies its bounded text collector");
        let data = vec![b'x'; 65_537];
        // SAFETY: every nonnull pointer covers its declared length. The deliberately
        // malformed null/nonzero case must be rejected without dereferencing it.
        unsafe {
            match CASE.get() {
                0 => sink(context, ptr::null(), 1),
                1 => sink(context, ptr::null(), 0),
                2 => sink(context, data.as_ptr().cast(), 65_536),
                3 => sink(context, [0xff_u8].as_ptr().cast(), 1),
                4 => sink(context, data.as_ptr().cast(), data.len()),
                5 => {
                    sink(context, data.as_ptr().cast(), 65_536);
                    sink(context, c"!".as_ptr(), 1);
                }
                6 => {
                    sink(context, ptr::null(), 1);
                    sink(context, c"ok".as_ptr(), 2);
                }
                _ => sink(context, c"ok".as_ptr(), 2),
            }
        }
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let services = host_services(|table| table.radio_control = Some(reply));
    let mut radio = services.radio("1000", "usb", 8).unwrap();
    // SAFETY: this record contains only scalars and arrays; all-zero is valid storage.
    let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    request.struct_size = size_of_val(&request) as u32;
    request.abi_version = 1;
    request.operation = 1;
    for case in 0..8 {
        CASE.set(case);
        let result = radio.maintenance(&mut request);
        match case {
            1 => assert_eq!(result, Ok(String::new())),
            2 => assert_eq!(result.as_ref().map(String::len), Ok(65_536)),
            7 => assert_eq!(result, Ok("ok".to_owned())),
            _ => assert_eq!(result, Err(-1), "malformed or oversized reply case {case}"),
        }
    }
}
