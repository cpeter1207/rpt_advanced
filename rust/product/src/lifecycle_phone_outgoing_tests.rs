//! Explicit administrative telephone commands, not numeric-node link aliases.
use super::*;

#[test]
fn product_descriptor_exposes_an_explicit_sized_telephone_operation() {
    assert!(
        descriptor().phone_command.is_some(),
        "telephone origination must use a typed product entry point, not node syntax"
    );
    assert_eq!(
        std::mem::offset_of!(abi::rptadv_product_descriptor_v1, phone_command),
        128
    );
    assert_eq!(size_of::<abi::rptadv_phone_command_v1>(), 56);
}

#[test]
fn outgoing_engine_reserves_before_factory_and_cancels_only_the_exact_call() {
    use rpt_advanced_core::link::PhoneAccountId;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    #[derive(Default)]
    struct Factory {
        entered: AtomicUsize,
        answer: AtomicBool,
        canceled: AtomicUsize,
        peers: Mutex<Vec<Arc<Mutex<crate::link::session::tests::State>>>>,
    }
    unsafe extern "C" fn dial(
        context: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        local: *const std::ffi::c_char,
        length: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        current: abi::rptadv_current_v1,
        current_context: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        let state = unsafe { &*context.cast::<Arc<Factory>>() };
        state.entered.fetch_add(1, Ordering::AcqRel);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if current.is_none_or(|check| unsafe { check(current_context) } == 0) {
                state.canceled.fetch_add(1, Ordering::AcqRel);
                return -1;
            }
            if state.answer.load(Ordering::Acquire) {
                break;
            }
            if Instant::now() >= deadline {
                return -1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let local =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(local.cast(), length) })
                .unwrap();
        let (handle, peer) = crate::link::session::tests::phone_dial_handle(local);
        state.peers.lock().unwrap().push(peer);
        unsafe {
            *output = handle;
        }
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let factory = Arc::<Factory>::default();
    let services = crate::services::tests::phone_factory_fixture(|table| {
        // Match the existing fixture table lifetime even if a test assertion unwinds.
        table.context = Box::into_raw(Box::new(Arc::clone(&factory))).cast();
        table.peer_dial_phone = Some(dial);
    });
    let configuration = active_configuration(&format!(
        "phone_allow_accounts=provider\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}\nprotocol=sip\nattach_local_nodes=1000\n",
        include_str!("tests/fixtures/phone_account.conf"),
    ));
    let engine = super::phone::engine(configuration, services, super::phone::speech());
    let account = PhoneAccountId::new("provider").unwrap();
    let first = engine
        .start_phone("1000".into(), account.clone(), "5550100".into())
        .expect("product must accept a bounded node-origin call job");
    let until = Instant::now() + Duration::from_secs(3);
    while factory.entered.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < until, "outgoing factory never entered");
        std::thread::yield_now();
    }
    assert!(
        engine
            .run(|host| Ok(crate::host::tests::phone::outgoing_reserved(host, "1000")))
            .unwrap()
    );
    assert!(
        engine
            .start_phone("1000".into(), account.clone(), "5550101".into())
            .is_err()
    );
    assert!(engine.hangup_phone("1000".into(), first + 1).is_err());
    engine.hangup_phone("1000".into(), first).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while factory.canceled.load(Ordering::Acquire) == 0 {
        engine.pump(engine.clock(), false).unwrap();
        assert!(Instant::now() < until, "canceled factory retained its call");
        std::thread::yield_now();
    }
    assert!(factory.peers.lock().unwrap().is_empty());
    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    let refused = engine.start_phone("1000".into(), account.clone(), "5550101".into());
    FAIL_DIAL_THREAD.store(false, Ordering::Release);
    assert!(
        refused.is_err(),
        "failed job creation cannot retain a call intent"
    );
    engine.pump(engine.clock(), false).unwrap();
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::phone::outgoing_reserved(host, "1000")))
            .unwrap()
    );
    assert_eq!(factory.entered.load(Ordering::Acquire), 1);
    factory.answer.store(true, Ordering::Release);
    let second = engine
        .start_phone("1000".into(), account, "5550102".into())
        .unwrap();
    assert_ne!(first, second);
    assert!(engine.hangup_phone("1000".into(), first).is_err());
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        if engine
            .run(|host| Ok(crate::host::tests::phone::outgoing_media_reply(host, "1000").is_some()))
            .unwrap()
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "answered call never attached its media"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let reply = engine
        .run(|host| Ok(crate::host::tests::phone::outgoing_media_reply(host, "1000").unwrap()))
        .unwrap();
    assert!(reply.phone_outgoing() && !reply.phone_authenticated());
    engine.hangup_phone("1000".into(), second).unwrap();
    assert!(reply.ended());
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::phone::outgoing_reserved(host, "1000")))
            .unwrap()
    );
    factory.answer.store(false, Ordering::Release);
    engine
        .start_phone(
            "1000".into(),
            PhoneAccountId::new("provider").unwrap(),
            "5550103".into(),
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while factory.entered.load(Ordering::Acquire) != 3 {
        assert!(
            Instant::now() < until,
            "final pending factory never entered"
        );
        std::thread::yield_now();
    }
    assert!(engine.stop());
    assert_eq!(factory.canceled.load(Ordering::Acquire), 2);
    assert_eq!(factory.peers.lock().unwrap()[0].lock().unwrap().drops, 1);
}

#[test]
fn outgoing_claim_pauses_retry_admission_without_erasing_permanent_intent() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let configuration = active_configuration(&format!(
        "phone_allow_accounts=provider\nlink_static_directory_file=fixture\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}\nprotocol=sip\nattach_local_nodes=1000\n",
        include_str!("tests/fixtures/phone_account.conf"),
    ));
    let engine = super::phone::engine(configuration, services, super::phone::speech());
    let claim = engine
        .run(|host| {
            crate::host::tests::phone::retain_outgoing_test_retry(host);
            host.reserve_outgoing_phone(
                rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap(),
                "1000",
            )
        })
        .unwrap();
    let mut due = engine.clock();
    due.now_ms = 60_000;
    engine.pump(due, true).unwrap();
    assert!(
        engine.jobs.lock().unwrap().is_empty(),
        "reserved node must not dispatch an ordinary retry during its outgoing call"
    );
    assert!(
        engine
            .run(|host| Ok(host.status_text("1000")?.contains("(retrying)")))
            .unwrap()
    );
    engine.hangup_phone("1000".into(), claim.id()).unwrap();
    engine.pump(due, true).unwrap();
    assert_eq!(
        engine.jobs.lock().unwrap().len(),
        1,
        "ending the exact call must make retained ordinary retry intent eligible again"
    );
    assert!(engine.stop());
}

#[test]
fn telephone_entry_rejects_invalid_sized_spans_without_starting_a_runtime() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let api = descriptor();
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    let command = api.phone_command.expect("explicit typed telephone entry");
    let request = abi::rptadv_phone_command_v1 {
        struct_size: size_of::<abi::rptadv_phone_command_v1>() as u32,
        abi_version: 1,
        operation: 1,
        account: c"provider".as_ptr(),
        account_length: 8,
        destination: c"5550100".as_ptr(),
        destination_length: 7,
        call_id: 0,
    };
    let mut output = 99;
    assert_eq!(
        unsafe { command(c"1000".as_ptr(), 4, &request, &mut output) },
        -1
    );
    assert_eq!(
        output, 0,
        "valid request without an owner remains unavailable"
    );
    for case in 0..12 {
        let mut invalid = request;
        let mut local = c"1000".as_ptr();
        let mut length = 4;
        match case {
            0 => invalid.struct_size -= 1,
            1 => invalid.abi_version = 2,
            2 => invalid.operation = 99,
            3 => invalid.account_length = 65,
            4 => invalid.account_length = 0,
            5 => invalid.destination_length = 256,
            6 => invalid.destination = ptr::null(),
            7 => invalid.call_id = 1,
            8 => length = 64,
            9 => local = ptr::null(),
            10 => {
                invalid.operation = 2;
                invalid.call_id = 0;
                invalid.account_length = 0;
                invalid.destination_length = 0;
            }
            11 => {
                invalid.operation = 2;
                invalid.call_id = 1;
            }
            _ => unreachable!(),
        }
        output = 99;
        assert_eq!(
            unsafe { command(local, length, &invalid, &mut output) },
            -2,
            "case {case}"
        );
        assert_eq!(output, 0, "case {case}");
    }
    assert_eq!(
        unsafe { command(c"1000".as_ptr(), 4, ptr::null(), &mut output) },
        -2
    );
    assert_eq!(
        unsafe { command(c"1000".as_ptr(), 4, &request, ptr::null_mut()) },
        -2
    );
}

#[test]
fn radio_phone_actions_use_the_existing_pending_call_and_exact_cancel() {
    use std::sync::atomic::AtomicUsize;
    #[derive(Default)]
    struct Factory {
        request: Mutex<Option<(String, String, String)>>,
        canceled: AtomicUsize,
    }
    unsafe extern "C" fn dial(
        context: *mut c_void,
        account: *const std::ffi::c_char,
        account_length: usize,
        local: *const std::ffi::c_char,
        local_length: usize,
        destination: *const std::ffi::c_char,
        destination_length: usize,
        _: usize,
        current: abi::rptadv_current_v1,
        current_context: *mut c_void,
        _: *mut *mut c_void,
    ) -> i32 {
        let state = unsafe { &*context.cast::<Arc<Factory>>() };
        let text = |data: *const std::ffi::c_char, length: usize| {
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(data.cast(), length) })
                .unwrap()
                .to_owned()
        };
        *state.request.lock().unwrap() = Some((
            text(account, account_length),
            text(local, local_length),
            text(destination, destination_length),
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while current.is_some_and(|check| unsafe { check(current_context) } != 0) {
            if Instant::now() >= deadline {
                return -1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        state.canceled.fetch_add(1, Ordering::AcqRel);
        -1
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let factory = Arc::<Factory>::default();
    let services = crate::services::tests::phone_factory_fixture(|table| {
        table.context = Box::into_raw(Box::new(Arc::clone(&factory))).cast();
        table.peer_dial_phone = Some(dial);
    });
    let configuration = active_configuration(&format!(
        "phone_allow_accounts=provider\nphone_outgoing_account=provider\nlink_command_phone_dial=6\nlink_command_phone_hangup=0\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}\nprotocol=sip\nserver=fixture.invalid:5060\nattach_local_nodes=1000\n",
        include_str!("tests/fixtures/phone_account.conf"),
    ));
    let document = rpt_advanced_core::config::ConfigDocument::parse(&configuration).unwrap();
    let map = rpt_advanced_core::config::ResolvedNodeSettings::resolve(
        &document,
        &rpt_advanced_core::config::NodeId::new("1000").unwrap(),
    )
    .unwrap()
    .value
    .command_map()
    .unwrap();
    let engine = super::phone::engine(configuration, services, super::phone::speech());
    let command = |text: &str| {
        DigitOutcome::Operation(DigitOperation::new(
            map.parse(text).expect("configured radio telephone action"),
            None,
        ))
    };
    engine
        .execute_digit("1000".into(), command("600123"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while factory.request.lock().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "radio command never reached the existing factory"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        factory.request.lock().unwrap().as_ref(),
        Some(&(
            "provider".into(),
            "1000".into(),
            "sip:00123@fixture.invalid:5060".into(),
        ))
    );
    assert!(
        engine
            .run(|host| Ok(host.outgoing_phone_reserved("1000")))
            .unwrap()
    );
    assert!(
        engine
            .execute_digit("1000".into(), command("65550100"))
            .is_err()
    );
    engine.execute_digit("1000".into(), command("0")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while factory.canceled.load(Ordering::Acquire) == 0 {
        engine.pump(engine.clock(), false).unwrap();
        assert!(
            Instant::now() < deadline,
            "radio hangup did not cancel its pending call"
        );
        std::thread::yield_now();
    }
    assert!(
        !engine
            .run(|host| Ok(host.outgoing_phone_reserved("1000")))
            .unwrap()
    );
    assert!(engine.execute_digit("1000".into(), command("0")).is_err());
    assert!(engine.stop());
}

#[test]
fn radio_phone_queued_macro_hangup_expires_before_mutating_its_call() {
    use rpt_advanced_core::runtime::dtmf::DigitEvent;
    static TRACK_CLOCK: AtomicBool = AtomicBool::new(false);
    static CLOCK_SEEN: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn local_time(
        _: *mut c_void,
        _: i64,
        _: *mut abi::rptadv_local_time_v1,
    ) -> i32 {
        if TRACK_CLOCK.load(Ordering::Acquire) {
            CLOCK_SEEN.store(true, Ordering::Release);
        }
        -1
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    TRACK_CLOCK.store(false, Ordering::Release);
    CLOCK_SEEN.store(false, Ordering::Release);
    let services = crate::services::tests::phone_factory_fixture(|table| {
        table.local_time = Some(local_time);
    });
    let configuration = active_configuration(&format!(
        "phone_allow_accounts=provider\nlink_command_phone_hangup=0\ndtmf_macro_max_duration_ms=200\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}\nprotocol=sip\nattach_local_nodes=1000\n[dtmf_macro finish]\ntrigger=*95\nprotected=no\ncommands=*0#\n",
        include_str!("tests/fixtures/phone_account.conf"),
    ));
    let engine = super::phone::engine(configuration, services, super::phone::speech());
    let clock = engine.clock();
    let (claim, operation) = engine
        .run(move |host| {
            let claim = host.reserve_outgoing_phone(
                rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap(),
                "1000",
            )?;
            for digit in "*95".chars() {
                host.runtime.digit(
                    "1000",
                    DigitEvent::Digit {
                        digit,
                        now_ms: clock.now_ms,
                    },
                );
            }
            let DigitOutcome::Operation(operation) = host
                .runtime
                .node("1000")
                .unwrap()
                .links()
                .advance_macros(clock.now_ms)
                .remove(0)
            else {
                panic!("macro hangup")
            };
            Ok((claim, operation))
        })
        .unwrap();
    // This fixture executes tasks synchronously, so hold the actual serialized
    // Host mutex instead of pretending that its control descriptor queues work.
    let held = engine.host.lock().unwrap();
    TRACK_CLOCK.store(true, Ordering::Release);
    let owner = Arc::clone(&engine);
    let queued = std::thread::spawn(move || {
        owner.execute_digit("1000".into(), DigitOutcome::Operation(operation))
    });
    // The real serialized owner holds admission beyond the macro deadline.
    // No clock is advanced or permit timestamp rewritten by the fixture.
    while engine.origin.elapsed().as_millis() <= u128::from(clock.now_ms + 300) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let captured_before_release = CLOCK_SEEN.load(Ordering::Acquire);
    drop(held);
    let outcome = queued.join().unwrap();
    TRACK_CLOCK.store(false, Ordering::Release);
    let retained = claim.current();
    let id = claim.id();
    let _ = engine.run(move |host| host.cancel_outgoing_phone("1000", id));
    assert!(engine.stop());
    assert!(
        outcome.is_err() && retained,
        "expired queued hangup changed its call: outcome={outcome:?}, retained={retained}, captured_before_release={captured_before_release}"
    );
}
