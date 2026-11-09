use super::*;
use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::dtmf::DigitOperation,
};
use std::{
    ffi::c_void,
    ptr,
    sync::atomic::Ordering,
    time::{Duration, Instant, UNIX_EPOCH},
};

fn descriptor() -> &'static abi::rptadv_product_descriptor_v1 {
    unsafe { &*rptadv_product_descriptor_v1() }
}

unsafe fn start(configuration: &str) -> i32 {
    let api = descriptor();
    unsafe {
        api.start.unwrap()(
            crate::fixture::host_descriptor(),
            crate::fixture::control_descriptor(),
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
            configuration.as_ptr().cast(),
            configuration.len(),
        )
    }
}

fn active_configuration(extra: &str) -> String {
    format!("[1000]\nradio_channel=usb\nduplex=full\n{extra}")
}

#[test]
fn startup_waits_for_the_radio_activation_acknowledgement() {
    use std::sync::atomic::AtomicUsize;
    static PENDING: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        i32::from(
            PENDING
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok(),
        )
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_ready = Some(ready);
    PENDING.store(10, Ordering::Release);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let source = active_configuration("");
    assert_eq!(
        unsafe {
            descriptor().start.unwrap()(
                &table,
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                source.as_ptr().cast(),
                source.len(),
            )
        },
        0
    );
    assert_eq!(PENDING.load(Ordering::Acquire), 0);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn activation_reload_poll_stays_admitted_until_commit_or_safe_rollback() {
    use std::sync::atomic::AtomicI32;
    static ACTIVATION: AtomicI32 = AtomicI32::new(0);
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        let status = ACTIVATION.load(Ordering::Acquire);
        if status < 0 {
            ACTIVATION.store(0, Ordering::Release);
        }
        status
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let api = descriptor();
    unsafe { api.stop.unwrap()() };
    ACTIVATION.store(0, Ordering::Release);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_ready = Some(ready);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let source = "[1000]\nradio_channel=old\nduplex=full\n";
    assert_eq!(
        unsafe {
            api.start.unwrap()(
                &table,
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                source.as_ptr().cast(),
                source.len(),
            )
        },
        0
    );
    let next = source.replace("old", "new");
    ACTIVATION.store(1, Ordering::Release);
    assert_eq!(
        unsafe { api.reload.unwrap()(next.as_ptr().cast(), next.len()) },
        1
    );
    assert_eq!(unsafe { api.reload_poll.unwrap()() }, 1);
    ACTIVATION.store(0, Ordering::Release);
    assert_eq!(unsafe { api.reload_poll.unwrap()() }, 0);
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(
        unsafe {
            api.start.unwrap()(
                &table,
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                source.as_ptr().cast(),
                source.len(),
            )
        },
        0
    );
    let rejected = source.replace("old", "rejected");
    ACTIVATION.store(1, Ordering::Release);
    assert_eq!(
        unsafe { api.reload.unwrap()(rejected.as_ptr().cast(), rejected.len()) },
        1
    );
    ACTIVATION.store(-123, Ordering::Release);
    assert_eq!(unsafe { api.reload_poll.unwrap()() }, -1);
    let engine = ENGINE.lock().unwrap().as_ref().unwrap().clone();
    assert_eq!(
        engine.run(|host| Ok(host.runtime.settings("1000").unwrap().channel.clone())),
        Ok("old".into())
    );
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
}

fn scheduler_notices() -> Vec<String> {
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clone()
}

fn clear_scheduler_notices() {
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
}

#[test]
fn scheduler_health_status_counts_submission_failure_not_stale_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    engine.reloading.store(true, Ordering::Release);
    assert_eq!(
        engine.reload_configuration(document(&active_configuration("")).unwrap()),
        Err(RuntimeError::Busy)
    );
    let stale = engine.run(|_| Ok(()));
    engine.reloading.store(false, Ordering::Release);
    assert_eq!(engine.poll_reload_configuration(), Ok(true));
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    let failed = engine.run(|_| Ok(()));
    let lost_digits = engine.lost_digits.load(Ordering::Acquire);
    let notices = scheduler_notices();
    engine
        .reload_configuration(document(&active_configuration("")).unwrap())
        .unwrap();
    let mut status = Vec::<u8>::new();
    unsafe extern "C" fn sink(context: *mut c_void, text: *const std::ffi::c_char, length: usize) {
        let status = unsafe { &mut *context.cast::<Vec<u8>>() };
        status.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast(), length) });
    }
    let result = unsafe {
        descriptor().link_status.unwrap()(
            c"1000".as_ptr(),
            4,
            Some(sink),
            ptr::from_mut(&mut status).cast(),
        )
    };
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(stale, Err(RuntimeError::Rejected));
    assert_eq!(failed, Err(RuntimeError::Rejected));
    assert!(lost_digits);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("task submission failed: Backend"));
    assert_eq!(result, 0);
    let status = String::from_utf8(status).unwrap();
    assert!(
        status.contains("scheduler submission_failures=1"),
        "{status}"
    );
}

#[test]
fn scheduler_health_reports_live_cycle_failure_but_not_generation_retirement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    crate::fixture::CONTROL_REVISION.store(
        ptr::from_ref(&engine.revision).cast_mut(),
        Ordering::Release,
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(3, Ordering::Release);
    engine.pump_observed(engine.clock(), false);
    crate::fixture::CONTROL_REVISION.store(ptr::null_mut(), Ordering::Release);
    let mut retired_status = String::new();
    engine.health.status(&mut retired_status);
    let retired_notices = scheduler_notices();
    // An accepted cycle can fail independently of executor admission.
    let host = engine.host.lock().unwrap().take();
    engine.pump_observed(engine.clock(), false);
    *engine.host.lock().unwrap() = host;
    for _ in 0..2 {
        crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
        engine.pump_observed(engine.clock(), false);
    }
    let mut failed_status = String::new();
    engine.health.status(&mut failed_status);
    let failed_notices = scheduler_notices();
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert!(
        retired_status.contains("submission_failures=0 cycle_failures=0"),
        "{retired_status}"
    );
    assert!(retired_notices.is_empty(), "{retired_notices:?}");
    assert!(
        failed_status.contains("submission_failures=2 cycle_failures=3"),
        "{failed_status}"
    );
    assert_eq!(failed_notices.len(), 1, "{failed_notices:?}");
    assert!(failed_notices[0].contains("control cycle failed"));
}

#[test]
fn scheduler_health_queue_latency_has_its_own_warning_budget() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    // Inject enqueue times, not scheduling sleeps or performance assertions.
    for _ in 0..2 {
        engine.queue_started(Instant::now() - Duration::from_secs(2));
    }
    let mut status = String::new();
    engine.health.status(&mut status);
    let notices = scheduler_notices();
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert!(status.contains("delayed_tasks=2"), "{status}");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("control queue start delayed"));
}

#[test]
fn site_lifecycle_installs_configured_acquisition_before_station_polling() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=2\n",
    );
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    let readings = engine.run(|host| Ok(host.poll_sites(0))).unwrap();
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(
        readings["1000"].inputs["door"].quality,
        rpt_advanced_core::site::ReadingQuality::Unavailable
    );
}

#[test]
fn site_backends_reload_rejection_preserves_live_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let (engine, call) = selected().unwrap();
    let active = engine
        .run(|host| Ok(host.runtime.status(0)[0].1.active))
        .unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    let configuration = active_configuration(
        "[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=/no-device\nline=0\n",
    );
    let result =
        unsafe { descriptor().reload.unwrap()(configuration.as_ptr().cast(), configuration.len()) };
    let after = engine
        .run(|host| Ok(host.runtime.status(0)[0].1.active))
        .unwrap();
    let after_drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(result, -1);
    assert_eq!(after, active);
    assert_eq!(after_drops, drops);
}

#[test]
fn site_lifecycle_reload_replaces_bindings_without_reopening_same_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    let configuration = active_configuration(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=2\n",
    );
    let result =
        unsafe { descriptor().reload.unwrap()(configuration.as_ptr().cast(), configuration.len()) };
    let (engine, call) = selected().unwrap();
    let readings = engine.run(|host| Ok(host.poll_sites(0))).unwrap();
    let after_drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(result, 0);
    assert_eq!(after_drops, drops);
    assert!(
        readings
            .get("1000")
            .is_some_and(|node| node.inputs.contains_key("door"))
    );
}

#[test]
fn site_alarm_link_action_is_dispatched_without_a_schedule_tick() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    let configuration = active_configuration(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=2\n\
         [alarm 1000 missing]\nenabled=yes\nsource=door\ncondition=unavailable\non_trip=macro:connect\n\
         [macro 1000 connect]\naction=connect\ntarget_node=2000\n",
    );
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    engine.pump(engine.clock(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        if engine.jobs.lock().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "site action dial did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        engine
            .run(|host| host.status_text("1000"))
            .unwrap()
            .contains("  2000: transceive")
    );
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

fn operation(action: LinkAction, node: &str) -> DigitOperation {
    DigitOperation::new(
        Command {
            action,
            node: node.into(),
        },
        None,
    )
}

#[test]
fn descriptor_rejects_bad_composition_and_preserves_reload_state() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration = "[general]\nenabled=no\n";
    assert_eq!(
        unsafe {
            descriptor().start.unwrap()(
                ptr::null(),
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                configuration.as_ptr().cast(),
                configuration.len(),
            )
        },
        1
    );
    assert_eq!(unsafe { start("invalid") }, 1);
    assert_eq!(unsafe { start(configuration) }, 0);
    assert_eq!(unsafe { start(configuration) }, 1);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"missing".as_ptr(),
                    7,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        -1
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );
    assert_eq!(
        unsafe { descriptor().reload.unwrap()(c"invalid".as_ptr(), 7) },
        -1
    );
    assert_eq!(
        unsafe { descriptor().reload.unwrap()(configuration.as_ptr().cast(), configuration.len()) },
        0
    );
    let mut completed = 0;
    assert_eq!(
        unsafe { descriptor().digit.unwrap()(c"missing".as_ptr(), 7, b'?', &mut completed) },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().link_command.unwrap()(c"missing".as_ptr(), 7, c"2000".as_ptr(), 4, 0)
        },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn active_host_owns_radio_and_incoming_peer_until_quiescent_stop() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::fixture::RADIO_OPENS.store(0, Ordering::Relaxed);
    crate::fixture::RADIO_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let radio_ready_before = crate::fixture::RADIO_READY.swap(1, Ordering::AcqRel);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration =
        "[1000]\nradio_channel=usb\nduplex=full\n[sensor 1000 battery]\nunit=volts\ndtmf_id=1\n";
    assert_eq!(unsafe { start(configuration) }, 0);
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    c"127.0.0.1".as_ptr(),
                    9,
                ),
                ptr::null_mut(),
            )
        },
        0
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    c"127.0.0.1".as_ptr(),
                    9,
                ),
                peer,
            )
        },
        0
    );
    let mut status = Vec::<u8>::new();
    unsafe extern "C" fn sink(context: *mut c_void, text: *const std::ffi::c_char, length: usize) {
        let status = unsafe { &mut *context.cast::<Vec<u8>>() };
        status.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast(), length) });
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        status.clear();
        assert_eq!(
            unsafe {
                descriptor().link_status.unwrap()(
                    c"1000".as_ptr(),
                    4,
                    Some(sink),
                    ptr::from_mut(&mut status).cast(),
                )
            },
            0
        );
        let text = std::str::from_utf8(&status).unwrap();
        assert!(
            text.contains("sensor battery value=None quality=Unavailable"),
            "{text}"
        );
        if text.contains("2000") {
            break;
        }
        assert!(Instant::now() < deadline, "unexpected link status: {text}");
        std::thread::sleep(Duration::from_millis(1));
    }
    let rejected = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2001".as_ptr(),
                    4,
                    c"192.0.2.2".as_ptr(),
                    9,
                ),
                rejected,
            )
        },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::RADIO_READY.store(radio_ready_before, Ordering::Release);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(Ordering::Relaxed),
        crate::fixture::RADIO_OPENS.load(Ordering::Relaxed)
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );

    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
}

#[test]
fn incoming_peer_is_rejected_and_released_when_refresh_cannot_prepare_audio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let configuration = "[1000]\nradio_channel=usb\nduplex=full\nkerchunk_max_ms=4294967296\n";
    assert_eq!(unsafe { start(configuration) }, 0);
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    c"127.0.0.1".as_ptr(),
                    9,
                ),
                ptr::null_mut(),
            )
        },
        0
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    c"127.0.0.1".as_ptr(),
                    9,
                ),
                peer,
            )
        },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );
}

#[test]
fn failed_dial_thread_runs_reserved_attempt_synchronously() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration = "[1000]\nradio_channel=usb\nduplex=full\n";
    assert_eq!(unsafe { start(configuration) }, 0);
    {
        let (engine, _call) = selected().unwrap();
        let effect = engine
            .operation(
                "1000",
                DigitOperation::new(
                    Command {
                        action: LinkAction::Transceive,
                        node: "2000".into(),
                    },
                    None,
                ),
            )
            .unwrap();
        let LinkEffect::Connect(attempt) = effect else {
            panic!("connect operation must reserve a dial");
        };
        FAIL_DIAL_THREAD.store(true, Ordering::Release);
        engine
            .spawn_dial(
                "1000".into(),
                Dial::Connect(attempt),
                engine.revision.load(Ordering::Acquire),
                None,
                false,
            )
            .unwrap();
        assert!(
            engine
                .run(|host| host.status_text("1000"))
                .unwrap()
                .contains("2000")
        );
    }
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn local_rf_loop_rejection_is_a_completed_command_with_queued_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);

    for (index, digit) in b"*31000#".iter().enumerate() {
        let mut completed = 99;
        assert_eq!(
            unsafe { descriptor().digit.unwrap()(c"1000".as_ptr(), 4, *digit, &mut completed) },
            0,
            "digit index {index}"
        );
        assert_eq!(completed, u32::from(index == 6));
    }
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn rejected_dtmf_routes_keep_local_or_originating_peer_telemetry() {
    use rpt_advanced_core::{link::PeerSignals, runtime::dtmf::DigitOutcome};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let (engine, call) = selected().unwrap();
    assert!(matches!(
        engine.execute("1000".into(), operation(LinkAction::Command, "3002000")),
        Err(RuntimeError::Link(
            rpt_advanced_core::link::AdmissionError::UnsupportedRoute
        ))
    ));
    for (action, remote) in [
        (LinkAction::Transceive, "1000"),
        (LinkAction::Command, "3002000"),
    ] {
        for linked_source in [false, true] {
            let source = PeerSignals::new();
            let mut operation = operation(action, remote);
            operation.reply = linked_source.then(|| source.reply_target());
            assert_eq!(
                engine.execute_digit("1000".into(), DigitOutcome::Operation(operation)),
                Ok(())
            );
        }
    }
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn incoming_preauthorization_never_adopts_a_foreign_or_already_owned_peer() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    for protocol in [1, 2] {
        let peer = crate::fixture::peer();
        let mut request = crate::fixture::incoming_request(
            c"1000".as_ptr(),
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        request.protocol = protocol;
        assert_eq!(
            unsafe { descriptor().authorize_incoming.unwrap()(&request, peer) },
            -1
        );
        unsafe { crate::fixture::release_peer(peer) };
    }
    for wrong_protocol in [false, true] {
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        let peer = crate::fixture::peer();
        let mut request = crate::fixture::incoming_request(
            if wrong_protocol {
                c"1000".as_ptr()
            } else {
                c"9999".as_ptr()
            },
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        if wrong_protocol {
            request.protocol = 2;
        }
        assert_eq!(
            unsafe { descriptor().incoming.unwrap()(&request, peer) },
            -1
        );
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire),
            before + 1
        );
    }
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn pad_test_dispatch_queues_local_telemetry() {
    use rpt_advanced_core::runtime::dtmf::{DigitOutcome, PadTestResult};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    assert!(
        selected()
            .unwrap()
            .0
            .execute_digit(
                "1000".into(),
                DigitOutcome::PadTest(PadTestResult {
                    digits: "*#A".into(),
                    overflowed: false
                })
            )
            .is_ok()
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn dtmf_macro_stopped_dispatch_queues_only_the_existing_source_reply() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let (engine, call) = selected().unwrap();
    assert!(
        engine
            .execute_digit("1000".into(), DigitOutcome::MacroStopped { reply: None })
            .is_ok()
    );
    assert!(
        engine
            .execute_digit("missing".into(), DigitOutcome::MacroStopped { reply: None })
            .is_err()
    );
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn dtmf_macro_pending_dial_checks_cancellation_before_dial_and_after_answer() {
    use rpt_advanced_core::runtime::dtmf::DigitEvent;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    static DIALS: AtomicUsize = AtomicUsize::new(0);
    static CANCEL_DURING: AtomicBool = AtomicBool::new(false);
    static RELOAD_DURING: AtomicBool = AtomicBool::new(false);
    static STILL_CURRENT: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn dial(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        current: abi::rptadv_current_v1,
        context: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        DIALS.fetch_add(1, Ordering::AcqRel);
        if CANCEL_DURING.load(Ordering::Acquire) {
            let (engine, _call) = selected().unwrap();
            engine
                .run(|host| {
                    host.runtime.digit(
                        "1000",
                        DigitEvent::Digit {
                            digit: '*',
                            now_ms: 2,
                        },
                    );
                    Ok(())
                })
                .unwrap();
        }
        if RELOAD_DURING.load(Ordering::Acquire) {
            let (engine, _call) = selected().unwrap();
            engine
                .reload_configuration(document(&active_configuration("")).unwrap())
                .unwrap();
            while !engine.poll_reload_configuration().unwrap() {
                std::thread::yield_now();
            }
        }
        STILL_CURRENT.store(
            current.is_some_and(|callback| unsafe { callback(context) } != 0),
            Ordering::Release,
        );
        // A provider may answer despite cancellation; final publication must still reject it.
        unsafe { *output = crate::fixture::peer() };
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for phase in ["before_dial", "during_dial", "during_reload", "ordinary"] {
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
        DIALS.store(0, Ordering::Release);
        STILL_CURRENT.store(false, Ordering::Release);
        CANCEL_DURING.store(
            matches!(phase, "during_dial" | "ordinary"),
            Ordering::Release,
        );
        RELOAD_DURING.store(phase == "during_reload", Ordering::Release);
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        let source = active_configuration(
            "[dtmf_macro connect]\ntrigger=*95\nprotected=no\ncommands=*33000#\n",
        );
        let mut table = unsafe { crate::fixture::host_descriptor().read() };
        table.peer_dial = Some(dial);
        assert_eq!(
            unsafe {
                descriptor().start.unwrap()(
                    &table,
                    crate::fixture::control_descriptor(),
                    rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                    rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                    source.as_ptr().cast(),
                    source.len(),
                )
            },
            0
        );
        let (engine, call) = selected().unwrap();
        let effect = if phase == "ordinary" {
            engine
                .operation("1000", operation(LinkAction::Transceive, "3000"))
                .unwrap()
        } else {
            dtmf_macro_connect(&engine)
        };
        let LinkEffect::Connect(attempt) = effect else {
            panic!("pending connect")
        };
        if phase == "before_dial" {
            engine
                .run(|host| {
                    host.runtime.digit(
                        "1000",
                        DigitEvent::Digit {
                            digit: '*',
                            now_ms: 2,
                        },
                    );
                    Ok(())
                })
                .unwrap();
        }
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        let result = engine.dial(
            "1000".into(),
            Dial::Connect(attempt),
            engine.revision.load(Ordering::Acquire),
        );
        let attached = engine
            .run(|host| {
                Ok(host
                    .runtime
                    .node("1000")
                    .unwrap()
                    .links()
                    .manager()
                    .snapshot()
                    .iter()
                    .any(|peer| peer.name == "3000"))
            })
            .unwrap();
        assert_eq!(attached, phase == "ordinary", "{phase}");
        assert_eq!(result.is_ok(), phase == "ordinary", "{phase}");
        assert_eq!(
            DIALS.load(Ordering::Acquire),
            usize::from(phase != "before_dial")
        );
        if matches!(phase, "during_dial" | "during_reload") {
            assert!(!STILL_CURRENT.load(Ordering::Acquire));
            assert_eq!(
                crate::fixture::PEER_DROPS.load(Ordering::Acquire),
                before + 1
            );
        }
        if phase == "ordinary" {
            assert!(STILL_CURRENT.load(Ordering::Acquire));
        }
        drop(call);
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    }
}

fn dtmf_macro_connect(engine: &Arc<Engine>) -> LinkEffect {
    use rpt_advanced_core::runtime::dtmf::{DigitEvent, DigitOutcome};
    let clock = engine.clock();
    engine
        .run(move |host| {
            for digit in "*95".chars() {
                host.runtime.digit(
                    "1000",
                    DigitEvent::Digit {
                        digit,
                        now_ms: clock.now_ms,
                    },
                );
            }
            let node = host.runtime.node("1000").unwrap();
            let DigitOutcome::Operation(operation) =
                node.links().advance_macros(clock.now_ms).remove(0)
            else {
                panic!("macro connect")
            };
            host.runtime.command("1000", operation, clock, false)
        })
        .unwrap()
}

#[test]
fn dtmf_macro_stop_during_directory_lookup_prevents_provider_dial() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    type PendingStop = (Arc<Engine>, u64, mpsc::SyncSender<()>);
    static STOP_LOOKUP: Mutex<Option<PendingStop>> = Mutex::new(None);
    static STOP_OBSERVED: AtomicBool = AtomicBool::new(false);
    static DIALS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn record(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        let pending = STOP_LOOKUP
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some((engine, revision, request)) = pending else {
            return -1;
        };
        if request.send(()).is_err() {
            return -1;
        }
        // Stop closes admission before waiting for this command's held Call.
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.current(revision) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let stopped = !engine.current(revision);
        STOP_OBSERVED.store(stopped, Ordering::Release);
        let Some(sink) = sink.filter(|_| stopped) else {
            return -1;
        };
        let destination = "radio@fixture/3000,127.0.0.1";
        unsafe { sink(context, destination.as_ptr().cast(), destination.len()) };
        0
    }
    unsafe extern "C" fn dial(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        _: abi::rptadv_current_v1,
        _: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        DIALS.fetch_add(1, Ordering::AcqRel);
        unsafe { *output = crate::fixture::peer() };
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    STOP_OBSERVED.store(false, Ordering::Release);
    DIALS.store(0, Ordering::Release);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let source = active_configuration(
        "link_lookup_method=file\nlink_static_directory_file=fixture\n\
         [dtmf_macro connect]\ntrigger=*95\nprotected=no\ncommands=*33000#\n",
    );
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.directory_record = Some(record);
    table.peer_dial = Some(dial);
    assert_eq!(
        unsafe {
            descriptor().start.unwrap()(
                &table,
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                source.as_ptr().cast(),
                source.len(),
            )
        },
        0
    );
    let (engine, call) = selected().unwrap();
    let LinkEffect::Connect(attempt) = dtmf_macro_connect(&engine) else {
        panic!("pending macro connection")
    };
    let revision = engine.revision.load(Ordering::Acquire);
    let (request, receive) = mpsc::sync_channel(1);
    *STOP_LOOKUP.lock().unwrap() = Some((Arc::clone(&engine), revision, request));
    let stop = std::thread::spawn(move || {
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { descriptor().stop.unwrap()() }
    });
    let result = engine.dial("1000".into(), Dial::Connect(attempt), revision);
    drop(call);
    assert_eq!(stop.join().unwrap(), 0);
    assert!(STOP_OBSERVED.load(Ordering::Acquire));
    assert_eq!(result, Err(RuntimeError::Rejected));
    assert_eq!(DIALS.load(Ordering::Acquire), 0);
}

#[test]
fn remote_candidate_dispatch_ignores_denied_command_but_keeps_accepted_effect() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let (engine, call) = selected().unwrap();

    // An unconnected peer's candidate D command must not reject local digit handling.
    assert!(matches!(
        engine.digit_effect(
            "1000",
            DigitOutcome::RemoteCandidate(operation(LinkAction::Command, "2000"))
        ),
        Ok(LinkEffect::None)
    ));
    engine
        .execute("1000".into(), operation(LinkAction::Transceive, "2000"))
        .unwrap();
    assert!(matches!(
        engine.digit_effect(
            "1000",
            DigitOutcome::RemoteCandidate(operation(LinkAction::Command, "2000"))
        ),
        Ok(LinkEffect::SelectedRemote)
    ));

    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn regeneration_dispatch_obeys_node_configuration_and_reload() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    for enabled in ["no", "yes"] {
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        assert_eq!(
            unsafe {
                start(&active_configuration(&format!(
                    "dtmf_regeneration_enabled={enabled}\n"
                )))
            },
            0
        );
        let (engine, call) = selected().unwrap();
        assert!(
            engine
                .execute_digit("1000".into(), DigitOutcome::Regenerate('5'))
                .is_ok()
        );
        assert!(
            engine
                .execute_digit("missing".into(), DigitOutcome::Regenerate('5'))
                .is_err()
        );
        assert!(
            engine
                .run(|host| host.status_text("1000"))
                .unwrap()
                .contains("regeneration-dropped=0")
        );
        drop(call);
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    }
}

#[test]
fn non_rf_loop_rejection_is_returned_without_local_rf_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);

    assert_eq!(
        unsafe { descriptor().link_command.unwrap()(c"1000".as_ptr(), 4, c"1000".as_ptr(), 4, 1,) },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn failed_start_drains_or_retains_the_executor_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration = "[1000]\nradio_channel=usb\nduplex=full\n";

    crate::fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration) }, 1);
    assert_eq!(unsafe { start("[general]\nenabled=no\n") }, 0);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    crate::fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
    crate::fixture::CONTROL_STOP_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration) }, 0);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn helpers_reject_malformed_inputs_and_capture_bounded_wall_time() {
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let origin = Instant::now();
    let before_epoch = UNIX_EPOCH.checked_sub(Duration::from_secs(1)).unwrap();
    let clock = capture_at(origin, before_epoch, services);
    assert_eq!(clock.wall_seconds, -1);
    assert!(clock.civil.is_none());

    crate::fixture::LOCAL_TIME_RESULT.store(1, Ordering::Release);
    let clock = capture_at(origin, UNIX_EPOCH + Duration::from_secs(1), services);
    assert_eq!(clock.wall_seconds, 1);
    assert!(clock.civil.is_none());

    assert_eq!(unsafe { input(ptr::null(), 0) }, Ok(""));
    assert!(unsafe { input(ptr::null(), 1) }.is_err());
    let invalid = [0xff_u8];
    assert!(unsafe { input(invalid.as_ptr().cast(), invalid.len()) }.is_err());
    assert!(
        unsafe {
            incoming_identity(&crate::fixture::incoming_request(
                ptr::null(),
                1,
                c"2".as_ptr(),
                1,
                ptr::null(),
                0,
            ))
        }
        .is_err()
    );
    assert!(document("invalid").is_err());
}

#[test]
fn incoming_identity_rejects_foreign_headers_and_limits_before_borrowing_spans() {
    assert!(unsafe { incoming_identity(ptr::null()) }.is_err());
    for mode in 0..=5 {
        let mut request = crate::fixture::incoming_request(
            c"1000".as_ptr(),
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        match mode {
            0 => request.struct_size = 0,
            1 => request.abi_version = 0,
            2 => request.local.length = 33,
            3 => request.remote.length = 33,
            4 => request.source.length = 65,
            _ => request.protocol = 3,
        }
        assert!(
            unsafe { incoming_identity(&request) }.is_err(),
            "case {mode}"
        );
    }
}

#[test]
fn serialized_engine_rejects_stale_missing_and_unaccepted_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration("");
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();

    engine.reloading.store(true, Ordering::Release);
    assert_eq!(engine.enter().err(), Some(RuntimeError::Rejected));
    assert_eq!(engine.run(|_| Ok(1)).unwrap_err(), RuntimeError::Rejected);
    assert_eq!(engine.run_inner(true, |_| Ok(2)).unwrap(), 2);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire)));
    engine.reloading.store(false, Ordering::Release);

    engine.accepting.store(false, Ordering::Release);
    assert_eq!(engine.enter().err(), Some(RuntimeError::Rejected));
    assert_eq!(engine.run(|_| Ok(3)).unwrap_err(), RuntimeError::Rejected);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire)));
    engine.accepting.store(true, Ordering::Release);

    let host = engine.host.lock().unwrap().take();
    assert_eq!(engine.run(|_| Ok(4)).unwrap_err(), RuntimeError::Rejected);
    *engine.host.lock().unwrap() = host;

    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(engine.run(|_| Ok(5)).unwrap_err(), RuntimeError::Rejected);
    assert!(engine.lost_digits.load(Ordering::Acquire));

    crate::fixture::CONTROL_REVISION.store(
        ptr::from_ref(&engine.revision).cast_mut(),
        Ordering::Release,
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(3, Ordering::Release);
    assert_eq!(engine.run(|_| Ok(7)).unwrap_err(), RuntimeError::Rejected);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire) - 1));
    assert!(engine.current(engine.revision.load(Ordering::Acquire)));

    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn dial_retry_background_and_remote_command_paths_are_serialized() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    let configuration = active_configuration("");
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();

    engine
        .execute("1000".into(), operation(LinkAction::Transceive, "2000"))
        .unwrap();
    let selected = engine
        .operation("1000", operation(LinkAction::Command, "2000"))
        .unwrap();
    engine
        .effect(
            "1000".into(),
            selected,
            engine.revision.load(Ordering::Acquire),
        )
        .unwrap();
    let mut forwarded = operation(LinkAction::Command, "2000");
    forwarded.digit = Some('5');
    let effect = engine.operation("1000", forwarded).unwrap();
    engine
        .effect(
            "1000".into(),
            effect,
            engine.revision.load(Ordering::Acquire),
        )
        .unwrap();
    engine
        .execute("1000".into(), operation(LinkAction::Disconnect, "2000"))
        .unwrap();

    let revision = engine.revision.load(Ordering::Acquire);
    assert_eq!(
        engine.effect("1000".into(), LinkEffect::None, revision - 1),
        Err(RuntimeError::Rejected)
    );
    let LinkEffect::Connect(stale) = engine
        .operation("1000", operation(LinkAction::Transceive, "2001"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    engine.revision.fetch_add(1, Ordering::AcqRel);
    assert_eq!(
        engine.dial("1000".into(), Dial::Connect(stale), revision),
        Err(RuntimeError::Rejected)
    );

    let LinkEffect::Connect(permanent) = engine
        .operation("1000", operation(LinkAction::PermanentTransceive, "3000"))
        .unwrap()
    else {
        panic!("permanent operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    let revision = engine.revision.load(Ordering::Acquire);
    assert_eq!(
        engine.dial("1000".into(), Dial::Connect(permanent), revision),
        Err(RuntimeError::Rejected)
    );
    let mut retry_clock = engine.clock();
    retry_clock.now_ms = u64::MAX;
    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    assert_eq!(engine.pump(retry_clock, true), Err(RuntimeError::Rejected));
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node("1000").ok_or(RuntimeError::MissingNode)?;
            let work = node
                .work()
                .expect("configured node retains an active generation");
            node.links()
                .take_retry(retry_clock.now_ms, work)
                .ok_or(RuntimeError::Rejected)
        })
        .unwrap();
    engine
        .dial("1000".into(), Dial::Retry(retry), revision)
        .unwrap();

    let LinkEffect::Connect(rejected) = engine
        .operation("1000", operation(LinkAction::Transceive, "3500"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    crate::fixture::PEER_PREPARE_TEXT_RESULT.store(1, Ordering::Release);
    assert_eq!(
        engine.dial("1000".into(), Dial::Connect(rejected), revision),
        Err(RuntimeError::Preparation)
    );

    crate::fixture::NOTICE_COUNT.store(0, Ordering::Release);
    engine
        .background("1000".into(), LinkEffect::None, revision, true)
        .unwrap();
    engine
        .background("1000".into(), LinkEffect::None, revision, false)
        .unwrap();
    assert_eq!(crate::fixture::NOTICE_COUNT.load(Ordering::Acquire), 1);

    let LinkEffect::Connect(attempt) = engine
        .operation("1000", operation(LinkAction::Transceive, "4000"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_DELAY_MS.store(20, Ordering::Release);
    engine
        .background("1000".into(), LinkEffect::Connect(attempt), revision, true)
        .unwrap();
    engine.pump(engine.clock(), false).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    engine.pump(engine.clock(), false).unwrap();
    assert_eq!(crate::fixture::NOTICE_COUNT.load(Ordering::Acquire), 2);

    let stale_revision = engine.revision.load(Ordering::Acquire);
    let LinkEffect::Connect(permanent) = engine
        .operation("1000", operation(LinkAction::PermanentTransceive, "3600"))
        .unwrap()
    else {
        panic!("permanent operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    assert_eq!(
        engine.dial("1000".into(), Dial::Connect(permanent), stale_revision),
        Err(RuntimeError::Rejected)
    );
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node("1000").ok_or(RuntimeError::MissingNode)?;
            let work = node
                .work()
                .expect("configured node retains an active generation");
            node.links()
                .take_retry(u64::MAX, work)
                .ok_or(RuntimeError::Rejected)
        })
        .unwrap();
    engine.revision.fetch_add(1, Ordering::AcqRel);
    assert_eq!(
        engine.dial("1000".into(), Dial::Retry(retry), stale_revision),
        Err(RuntimeError::Rejected)
    );

    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

#[test]
fn scheduled_connect_failures_busy_gates_and_immediate_events_complete() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    let configuration = active_configuration(
        "[2000]\n\
         node_enabled=no\n\
         [macro 1000 a_fail]\n\
         action=connect\n\
         target_node=3000\n\
         [event 1000 a_fail]\n\
         at=daily 09:07\n\
         macro=a_fail\n\
         [macro 1000 b_connect]\n\
         action=connect\n\
         target_node=3001\n\
         [event 1000 b_connect]\n\
         at=daily 09:07\n\
         macro=b_connect\n\
         [macro 1000 c_clear]\n\
         action=disconnect_all\n\
         [event 1000 c_clear]\n\
         at=daily 09:07\n\
         macro=c_clear\n\
         [macro 1000 d_loop]\n\
         action=connect\n\
         target_node=1000\n\
         [event 1000 d_loop]\n\
         at=daily 09:07\n\
         macro=d_loop\n",
    );
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    let clock = engine.clock();

    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    engine.pump(clock, true).unwrap();
    assert_eq!(engine.event_busy.load(Ordering::Acquire), 0);

    crate::fixture::PEER_DIAL_DELAY_MS.store(20, Ordering::Release);
    engine.pump(clock, true).unwrap();
    assert_eq!(
        engine.event_busy.load(Ordering::Acquire),
        engine.revision.load(Ordering::Acquire)
    );
    engine.pump(clock, true).unwrap();

    let deadline = Instant::now() + Duration::from_secs(1);
    while engine.event_busy.load(Ordering::Acquire) != 0 || !engine.jobs.lock().unwrap().is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
        engine.pump(clock, true).unwrap();
    }
    engine.pump(clock, true).unwrap();

    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

#[test]
fn public_commands_cover_link_modes_status_digits_and_admission_failures() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration("[sensor 1000 battery]\nunit=volts\ndtmf_id=1\n");
    assert_eq!(unsafe { start(&configuration) }, 0);

    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    ptr::null(),
                    1,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"missing".as_ptr(),
                    7,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        -1
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    ptr::null(),
                    1,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        1
    );
    assert_eq!(
        unsafe { descriptor().link_status.unwrap()(c"1000".as_ptr(), 4, None, ptr::null_mut(),) },
        -1
    );
    let mut rejected = 0;
    assert_eq!(
        unsafe { descriptor().digit.unwrap()(c"1000".as_ptr(), 4, b'1', ptr::null_mut()) },
        -1
    );
    assert_eq!(
        unsafe { descriptor().digit.unwrap()(c"1000".as_ptr(), 4, b'!', &mut rejected) },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    for (action, remote) in [(1, "2001"), (2, "2002"), (3, "2003")] {
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        assert_eq!(unsafe { start(&configuration) }, 0);
        assert_eq!(
            unsafe {
                descriptor().link_command.unwrap()(
                    c"1000".as_ptr(),
                    4,
                    remote.as_ptr().cast(),
                    remote.len(),
                    action,
                )
            },
            0,
            "action {action} remote {remote}"
        );
        assert_eq!(
            unsafe {
                descriptor().link_command.unwrap()(
                    c"1000".as_ptr(),
                    4,
                    remote.as_ptr().cast(),
                    remote.len(),
                    4,
                )
            },
            0
        );
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    }

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    let mut completed = 99;
    for command in [b"*722".as_slice(), b"*761#"] {
        for digit in command {
            completed = 99;
            assert_eq!(
                unsafe { descriptor().digit.unwrap()(c"1000".as_ptr(), 4, *digit, &mut completed) },
                0
            );
            assert!(completed <= 1);
        }
        assert_eq!(completed, 1);
    }
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn lifecycle_thread_and_drain_failures_remain_recoverable() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration = active_configuration("");

    FAIL_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 1);

    FAIL_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::CONTROL_STOP_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    PANIC_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    let stopper = std::thread::spawn(|| unsafe { descriptor().stop.unwrap()() });
    std::thread::sleep(Duration::from_millis(2));
    drop(call);
    assert_eq!(stopper.join().unwrap(), 0);
    drop(engine);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    engine.jobs.lock().unwrap().push(std::thread::spawn(|| {}));
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    engine.jobs.lock().unwrap().push(std::thread::spawn(|| {
        panic!("injected dial job failure");
    }));
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start("[general]\nenabled=no\n") }, 0);
    HOST_STOP_MODE.store(1, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    HOST_STOP_MODE.store(2, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn stale_removed_node_and_failed_reload_preserve_lifecycle_ownership() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration("");
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    let revision = engine.revision.load(Ordering::Acquire);
    let LinkEffect::Connect(attempt) = engine
        .operation("1000", operation(LinkAction::Transceive, "2000"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    let disabled = "[general]\nenabled=no\n";
    assert_eq!(
        unsafe { descriptor().reload.unwrap()(disabled.as_ptr().cast(), disabled.len()) },
        0
    );
    assert_eq!(
        engine.dial("1000".into(), Dial::Connect(attempt), revision),
        Err(RuntimeError::Rejected)
    );
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    crate::fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
    let replacement = format!("{configuration}[2000]\nradio_channel=usb2\nduplex=full\n");
    assert_eq!(
        unsafe { descriptor().reload.unwrap()(replacement.as_ptr().cast(), replacement.len()) },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
}

#[test]
fn ticker_route_and_peer_digits_drive_serialized_background_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    crate::fixture::LOCAL_TIME_RESULT.store(6, Ordering::Release);
    assert_eq!(unsafe { start(&active_configuration("")) }, 0);
    let (wall_engine, wall_call) = selected().unwrap();
    wall_engine.reloading.store(true, Ordering::Release);
    std::thread::sleep(Duration::from_millis(35));
    wall_engine.reloading.store(false, Ordering::Release);
    drop(wall_call);
    drop(wall_engine);
    crate::fixture::LOCAL_TIME_RESULT.store(0, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration(
        "[permanent 1000 primary]\n\
         remote_node=2000\n",
    );
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    assert_eq!(unsafe { start(&configuration) }, 0);
    let (engine, call) = selected().unwrap();
    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    engine.pump(engine.clock(), true).unwrap();
    assert!(
        engine
            .run(|host| host.status_text("1000"))
            .unwrap()
            .contains("2000")
    );

    crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(b"*722");
    let deadline = Instant::now() + Duration::from_secs(1);
    while !crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(5));
    engine.pump(engine.clock(), false).unwrap();

    crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(b"*31000#");
    while !crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(5));
    engine.pump(engine.clock(), false).unwrap();
    assert!(engine.lost_digits.load(Ordering::Acquire));

    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}
