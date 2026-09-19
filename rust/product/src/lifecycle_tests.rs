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

#[link(name = "rptadv_file_adapter")]
unsafe extern "C" {
    fn rptadv_file_adapter_descriptor() -> *const FileDescriptor;
}
#[link(name = "rptadv_speech_adapter")]
unsafe extern "C" {
    fn rptadv_speech_adapter_descriptor() -> *const SpeechDescriptor;
}

fn descriptor() -> &'static abi::rptadv_product_descriptor_v1 {
    unsafe { &*rptadv_product_descriptor_v1() }
}

unsafe fn start(configuration: &str) -> i32 {
    let api = descriptor();
    unsafe {
        api.start.unwrap()(
            crate::fixture::host_descriptor(),
            crate::fixture::control_descriptor(),
            rptadv_file_adapter_descriptor(),
            rptadv_speech_adapter_descriptor(),
            configuration.as_ptr().cast(),
            configuration.len(),
        )
    }
}

fn active_configuration(extra: &str) -> String {
    format!("[1000]\nradio_channel=usb\nduplex=full\n{extra}")
}

fn operation(action: LinkAction, node: &str) -> DigitOperation {
    DigitOperation {
        command: Command {
            action,
            node: node.into(),
        },
        digit: None,
    }
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
                rptadv_file_adapter_descriptor(),
                rptadv_speech_adapter_descriptor(),
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
                c"missing".as_ptr(),
                7,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
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
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    let configuration = "[1000]\nradio_channel=usb\nduplex=full\n";
    assert_eq!(unsafe { start(configuration) }, 0);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                c"1000".as_ptr(),
                4,
                c"2000".as_ptr(),
                4,
                c"192.0.2.1".as_ptr(),
                9,
            )
        },
        0
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                c"1000".as_ptr(),
                4,
                c"2000".as_ptr(),
                4,
                c"192.0.2.1".as_ptr(),
                9,
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
    assert!(String::from_utf8(status).unwrap().contains("2000"));
    let rejected = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                c"1000".as_ptr(),
                4,
                c"2001".as_ptr(),
                4,
                c"192.0.2.2".as_ptr(),
                9,
                rejected,
            )
        },
        -1
    );
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
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
                c"1000".as_ptr(),
                4,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
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
                DigitOperation {
                    command: Command {
                        action: LinkAction::Transceive,
                        node: "2000".into(),
                    },
                    digit: None,
                },
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
        unsafe { incoming_identity(ptr::null(), 1, c"2".as_ptr(), 1, ptr::null(), 0) }.is_err()
    );
    assert!(document("invalid").is_err());
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
    let configuration = active_configuration("");
    assert_eq!(unsafe { start(&configuration) }, 0);

    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                ptr::null(),
                1,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
            )
        },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                c"missing".as_ptr(),
                7,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
            )
        },
        -1
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                ptr::null(),
                1,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                c"1000".as_ptr(),
                4,
                c"2000".as_ptr(),
                4,
                ptr::null(),
                0,
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
    for digit in b"*722" {
        completed = 99;
        assert_eq!(
            unsafe { descriptor().digit.unwrap()(c"1000".as_ptr(), 4, *digit, &mut completed) },
            0
        );
        assert!(completed <= 1);
    }
    assert_eq!(completed, 1);
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
