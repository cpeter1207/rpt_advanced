//! Real serial control/dial owners and native media callbacks; no radio hardware.
use super::*;
use crate::media::text::tests::SpeechFixture;
use rpt_advanced_core::{
    link::{PeerReply, PhoneSource},
    runtime::dtmf::DigitOutcome,
};

pub(super) fn engine(
    configuration: String,
    services: HostServices,
    speech: Arc<SpeechFixture>,
) -> Arc<Engine> {
    let document = document(&configuration).unwrap();
    let origin = Instant::now();
    let media = crate::media::text::tests::preparer(speech);
    let site_backends = Arc::new(SiteBackends::load(&document).unwrap());
    let engine = Arc::new(Engine {
        executor: unsafe {
            ControlClient::open(
                crate::fixture::control_descriptor(),
                "phone-control-test",
                512,
            )
        }
        .unwrap(),
        host: Mutex::new(None),
        services,
        origin,
        health: SchedulerHealth::default(),
        accepting: AtomicBool::new(true),
        reloading: AtomicBool::new(false),
        reload_status: AtomicI32::new(0),
        revision: AtomicU64::new(1),
        lost_digits: AtomicBool::new(false),
        event_busy: AtomicU64::new(0),
        calls: Mutex::new(0),
        drained: Condvar::new(),
        ticker: Mutex::new(None),
        jobs: Mutex::new(Vec::new()),
        site_backends,
    });
    let owner = Arc::clone(&engine);
    let (send, receive) = mpsc::sync_channel(1);
    engine
        .enqueue(move || {
            let result = Host::start(document, media, services, origin, capture(origin, services));
            let result = result.map(|host| *owner.host.lock().unwrap() = Some(host));
            send.send(result).unwrap();
        })
        .unwrap();
    receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    engine
}

fn configuration() -> String {
    active_configuration(&format!(
        "link_static_directory_file=fixture\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}{}",
        include_str!("tests/fixtures/phone_mailbox.conf"),
        include_str!("tests/fixtures/phone_account.conf"),
    ))
}

pub(super) fn speech() -> Arc<SpeechFixture> {
    let mut fixture = SpeechFixture::default();
    fixture.frames = Some(960);
    Arc::new(fixture)
}

fn admit(
    engine: &Arc<Engine>,
) -> (
    PhoneSource,
    PeerReply,
    Arc<Mutex<crate::link::session::tests::State>>,
) {
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    let clock = engine.clock();
    let (source, reply) = engine
        .run(move |host| {
            host.incoming_peer(phone, "203.0.113.9", clock)?;
            let source = host.phone_source(call).unwrap().clone();
            let reply = host.phone_reply(&source).unwrap();
            host.authenticate_phone(&source, &reply, "100", "1234", clock)?;
            Ok((source, reply))
        })
        .unwrap();
    (source, reply, state)
}

fn source_operation(
    engine: &Arc<Engine>,
    source: &PhoneSource,
    reply: &PeerReply,
    action: LinkAction,
    target: &str,
) -> DigitOperation {
    let source = source.clone();
    let reply = reply.clone();
    let map = rpt_advanced_core::command::DtmfCommandMap::standard();
    let prefix = &map
        .mappings()
        .iter()
        .find(|mapping| mapping.action == action)
        .unwrap()
        .digits;
    let text = format!("*{prefix}{target}#");
    let now_ms = engine.elapsed_ms();
    engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let operation = text
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .phone_digit(&source, digit, now_ms, reply.clone(), 3000)
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation)
                    | DigitOutcome::RemoteCandidate(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            Ok(operation)
        })
        .unwrap()
}

#[track_caller]
fn wait_for(engine: &Arc<Engine>, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        if predicate() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "serial phone ownership receipt missing"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn asynchronous_phone_link_results_stay_private_without_rf_or_remote_fanout() {
    static PUBLIC_SAMPLES: AtomicU64 = AtomicU64::new(0);
    unsafe extern "C" fn write(
        _: *mut c_void,
        _: *mut c_void,
        input: *const f32,
        count: usize,
    ) -> i32 {
        let input = unsafe { std::slice::from_raw_parts(input, count) };
        PUBLIC_SAMPLES.fetch_add(
            input.iter().filter(|sample| **sample != 0.0).count() as u64,
            Ordering::Relaxed,
        );
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let voice = speech();
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_write = Some(write);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let (services, timed_peers) = crate::services::tests::timed_dial_fixture(services);
    let engine = engine(configuration(), services, Arc::clone(&voice));
    let (source, reply, state) = admit(&engine);
    let operation = source_operation(&engine, &source, &reply, LinkAction::Transceive, "2000");
    let effect = engine.operation("1000", operation).unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("source-owned dial")
    };
    assert_eq!(attempt.private_reply(), Some(&reply));
    engine
        .spawn_dial(
            "1000".into(),
            Dial::Connect(attempt),
            engine.revision.load(Ordering::Acquire),
            None,
            false,
        )
        .unwrap();
    wait_for(&engine, || {
        let connected = engine
            .run(|host| Ok(host.status_text("1000")?.contains("2000: transceive")))
            .unwrap();
        connected && state.lock().unwrap().audio.len() >= 480
    });
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Command completed")
    );
    PUBLIC_SAMPLES.store(0, Ordering::Release);
    let operation = source_operation(&engine, &source, &reply, LinkAction::Disconnect, "2000");
    let effect = engine.operation("1000", operation).unwrap();
    assert!(matches!(&effect, LinkEffect::PrivateDetach { reply: target, .. } if target == &reply));
    engine
        .effect(
            "1000".into(),
            effect,
            engine.revision.load(Ordering::Acquire),
        )
        .unwrap();
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 640);
    engine
        .digit_effect(
            "1000",
            DigitOutcome::MacroStopped {
                reply: Some(reply.clone()),
            },
        )
        .unwrap();
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 800);
    state.lock().unwrap().input.extend(
        "*4#*70"
            .bytes()
            .map(crate::link::session::tests::Input::Digit),
    );
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 960);
    let mut delivered = 960;
    for action in [
        LinkAction::Status,
        LinkAction::LastKeyed,
        LinkAction::FullStatus,
        LinkAction::Time,
    ] {
        let operation = source_operation(&engine, &source, &reply, action, "");
        engine.execute("1000".into(), operation).unwrap();
        delivered += 160;
        wait_for(&engine, || state.lock().unwrap().audio.len() >= delivered);
    }
    let loop_request = source_operation(&engine, &source, &reply, LinkAction::Transceive, "1000");
    assert!(matches!(
        engine.digit_effect("1000", DigitOutcome::Operation(loop_request)),
        Ok(LinkEffect::None)
    ));
    delivered += 160;
    wait_for(&engine, || state.lock().unwrap().audio.len() >= delivered);
    let unsupported = source_operation(&engine, &source, &reply, LinkAction::Command, "2000");
    assert!(engine.execute("1000".into(), unsupported).is_err());
    delivered += 160;
    wait_for(&engine, || state.lock().unwrap().audio.len() >= delivered);
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Command failed")
    );
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .all(|text| !text.contains("1234") && !text.contains('*') && !text.contains('#'))
    );
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert_eq!(PUBLIC_SAMPLES.load(Ordering::Acquire), 0);
    assert!(
        timed_peers
            .lock()
            .unwrap()
            .iter()
            .all(|peer| peer.lock().unwrap().prepared.is_empty()),
        "private command completion never reaches the actual timed remote egress"
    );
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().digits.is_empty());
    // Positive control uses the unchanged ordinary public event route and same audio owners.
    engine
        .run(|host| {
            host.runtime.queue_link_event("1000", "2000", true)?;
            Ok(())
        })
        .unwrap();
    let public_clock = engine.clock();
    engine
        .run(move |host| {
            host.immediate(
                "1000",
                LinkEffect::Digit(DigitOutcome::MacroStopped { reply: None }),
                public_clock,
            )?;
            host.immediate(
                "1000",
                LinkEffect::Digit(DigitOutcome::Regenerate('5')),
                public_clock,
            )
        })
        .unwrap();
    engine.pump(engine.clock(), false).unwrap();
    assert!(
        engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(engine.stop());
}

#[test]
fn retirement_during_async_dial_cannot_attach_or_reply_to_a_replacement_mailbox_login() {
    type Gate = (mpsc::SyncSender<()>, mpsc::Receiver<()>);
    static GATE: Mutex<Option<Gate>> = Mutex::new(None);
    static AFTER_RETIRE_CURRENT: AtomicBool = AtomicBool::new(true);
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
        unsafe {
            *output = crate::fixture::peer();
        }
        let (entered, release) = GATE.lock().unwrap().take().unwrap();
        entered.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(5)).unwrap();
        AFTER_RETIRE_CURRENT.store(unsafe { current.unwrap()(context) } != 0, Ordering::Release);
        // Deliberately return the answered handle: final publication must recheck the source.
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let voice = speech();
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_dial = Some(dial);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let (services, _timed_peers) = crate::services::tests::timed_dial_fixture(services);
    let engine = engine(configuration(), services, Arc::clone(&voice));
    let (source, reply, _old_state) = admit(&engine);
    let operation = source_operation(&engine, &source, &reply, LinkAction::Transceive, "2000");
    let LinkEffect::Connect(attempt) = engine.operation("1000", operation).unwrap() else {
        panic!("dial")
    };
    let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
    let (entered, receipt) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    *GATE.lock().unwrap() = Some((entered, released));
    engine
        .spawn_dial(
            "1000".into(),
            Dial::Connect(attempt),
            engine.revision.load(Ordering::Acquire),
            None,
            false,
        )
        .unwrap();
    receipt.recv_timeout(Duration::from_secs(5)).unwrap();
    let old_source = source.clone();
    engine
        .run(move |host| {
            assert!(host.retire_phone(&old_source));
            Ok(())
        })
        .unwrap();
    let (replacement, fresh_reply, fresh_state) = admit(&engine);
    assert_eq!(source.call_id(), replacement.call_id());
    assert_ne!(source.ingress(), replacement.ingress());
    release.send(()).unwrap();
    wait_for(&engine, || engine.jobs.lock().unwrap().is_empty());
    assert!(!AFTER_RETIRE_CURRENT.load(Ordering::Acquire));
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Acquire),
        before + 1
    );
    assert!(
        engine
            .run(|host| Ok(host.status_text("1000")?.contains("no active links")))
            .unwrap()
    );
    assert!(reply.ended() && fresh_reply.phone_authenticated());
    wait_for(&engine, || fresh_state.lock().unwrap().audio.len() >= 320);
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .all(|text| !text.starts_with("Command "))
    );
    assert!(engine.stop());
}

#[test]
fn initial_private_retry_completes_privately_then_transport_loss_uses_ordinary_public_notice() {
    static FAIL_SOCKET: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        if FAIL_SOCKET.load(Ordering::Acquire) {
            -1
        } else {
            0
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    FAIL_SOCKET.store(false, Ordering::Release);
    let voice = speech();
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_ready = Some(ready);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let (services, _timed_peers) = crate::services::tests::timed_dial_fixture(services);
    let engine = engine(configuration(), services, Arc::clone(&voice));
    let (source, reply, state) = admit(&engine);
    let operation = source_operation(
        &engine,
        &source,
        &reply,
        LinkAction::PermanentTransceive,
        "2000",
    );
    let LinkEffect::Connect(attempt) = engine.operation("1000", operation).unwrap() else {
        panic!("initial dial")
    };
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    engine
        .spawn_dial(
            "1000".into(),
            Dial::Connect(attempt),
            engine.revision.load(Ordering::Acquire),
            None,
            false,
        )
        .unwrap();
    wait_for(&engine, || {
        state.lock().unwrap().audio.len() >= 480 && engine.jobs.lock().unwrap().is_empty()
    });
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Requested operation is unavailable")
    );
    let now_ms = engine.elapsed_ms();
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let work = node.work().unwrap();
            Ok(node.links().take_retry(now_ms, work).unwrap())
        })
        .unwrap();
    assert_eq!(retry.private_reply(), Some(&reply));
    engine
        .spawn_dial(
            "1000".into(),
            Dial::Retry(retry),
            engine.revision.load(Ordering::Acquire),
            None,
            false,
        )
        .unwrap();
    wait_for(&engine, || {
        state.lock().unwrap().audio.len() >= 640 && engine.jobs.lock().unwrap().is_empty()
    });
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Command completed")
    );
    let retired = source.clone();
    engine
        .run(move |host| {
            assert!(host.retire_phone(&retired));
            Ok(())
        })
        .unwrap();
    assert!(reply.ended());
    assert!(
        engine
            .run(|host| Ok(host.status_text("1000")?.contains("2000: transceive")))
            .unwrap()
    );
    FAIL_SOCKET.store(true, Ordering::Release);
    wait_for(&engine, || {
        engine
            .run(|host| {
                Ok(host
                    .runtime
                    .node("1000")
                    .unwrap()
                    .links()
                    .manager()
                    .snapshot()
                    .iter()
                    .any(|peer| peer.name == "2000" && peer.retrying))
            })
            .unwrap()
    });
    let now_ms = engine.elapsed_ms();
    let ordinary_retry = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let work = node.work().unwrap();
            Ok(node.links().take_retry(now_ms, work).unwrap())
        })
        .unwrap();
    assert!(
        ordinary_retry.private_reply().is_none(),
        "completed commands retain ordinary recovery after their caller leaves"
    );
    assert!(
        ordinary_retry.timed_g711(),
        "transport intent survives without private authorization"
    );
    drop(ordinary_retry);
    assert!(
        engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(engine.stop());
    FAIL_SOCKET.store(false, Ordering::Release);
}

#[test]
fn private_site_report_and_rejected_message_preserve_unrelated_rf_macro() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let voice = speech();
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let config = format!(
        "{}\n[dtmf_macro status]\ntrigger=*95\nprotected=no\ncommands=*70#,*73#\n",
        configuration()
    );
    let engine = engine(config, services, Arc::clone(&voice));
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    let clock = engine.clock();
    engine
        .run(move |host| {
            for digit in "*95".chars() {
                host.runtime.digit(
                    "1000",
                    rpt_advanced_core::runtime::dtmf::DigitEvent::Digit {
                        digit,
                        now_ms: clock.now_ms,
                    },
                );
            }
            assert_eq!(
                host.runtime
                    .node("1000")
                    .unwrap()
                    .links()
                    .advance_macros(clock.now_ms)
                    .len(),
                1
            );
            Ok(())
        })
        .unwrap();
    let map = rpt_advanced_core::command::DtmfCommandMap::standard();
    let mut delivered = 320;
    for (action, expected) in [
        (LinkAction::Message, "Requested operation is unavailable"),
        (
            LinkAction::SiteStatus,
            "The requested site reading is unavailable.",
        ),
    ] {
        let prefix = &map
            .mappings()
            .iter()
            .find(|mapping| mapping.action == action)
            .unwrap()
            .digits;
        let opened = voice.opened.lock().unwrap().len();
        // A quoted pound terminates the unchanged node parser; a bare pound
        // exits the previous addressed mode before selecting this bound node.
        state.lock().unwrap().input.extend(
            format!("#*4#*{prefix}1**#")
                .bytes()
                .map(crate::link::session::tests::Input::Digit),
        );
        delivered += 160;
        wait_for(&engine, || state.lock().unwrap().audio.len() >= delivered);
        let settle = Instant::now() + Duration::from_millis(80);
        while Instant::now() < settle {
            engine.pump(engine.clock(), false).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(state.lock().unwrap().audio.len(), delivered);
        let prompts = voice.opened.lock().unwrap();
        assert_eq!(prompts.len(), opened + 1, "one prepared result per command");
        assert_eq!(prompts.last().unwrap(), expected);
    }
    let protected = map
        .mappings()
        .iter()
        .find(|mapping| mapping.action == LinkAction::ParrotEnable)
        .unwrap()
        .digits
        .clone();
    let owner = reply.clone();
    engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let outcomes: Vec<_> = format!("*{protected}#")
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .phone_digit(&source, digit, clock.now_ms, owner.clone(), 3000)
                })
                .collect();
            assert!(
                outcomes.is_empty(),
                "read-only report must not confer node unlock"
            );
            Ok(())
        })
        .unwrap();
    let due_ms = clock.now_ms + 2000;
    let remaining = engine
        .run(move |host| {
            Ok(host
                .runtime
                .node("1000")
                .unwrap()
                .links()
                .advance_macros(due_ms))
        })
        .unwrap();
    assert!(
        matches!(remaining.as_slice(), [DigitOutcome::Operation(operation)] if operation.reply.is_none())
    );
    assert!(!reply.ended());
    assert!(engine.stop());
}

fn assert_actual_private_collection_consumer(rate: u32, pending_reload: bool) {
    use crate::link::session::{SerialSession, tests::Input};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let engine = engine(configuration(), services, speech());
    let (calls, mut session, state, _source, reply) =
        crate::phone::tests::controlled_source_session(rate);
    engine
        .run(move |host| {
            crate::host::tests::phone::install_controlled_source(host, calls);
            Ok(())
        })
        .unwrap();
    let part = rate as usize / 50;
    let tone: Vec<_> = (0..part * 4)
        .map(|n| {
            let phase = n as f32 * std::f32::consts::TAU / rate as f32;
            0.25 * ((phase * 770.0).sin() + (phase * 1336.0).sin())
        })
        .collect();
    state
        .lock()
        .unwrap()
        .input
        .extend(tone.chunks(part).map(|pcm| Input::Audio(pcm.to_vec())));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.0; part * 3]));
    for now in 1..10 {
        session.step(now).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert!(crate::link::private_phone::tests::collection_suppressed(
        &session
    ));
    let clock = engine.clock();
    if pending_reload {
        engine.reloading.store(true, Ordering::Release);
        engine
            .run_inner(true, move |host| host.pump(clock))
            .unwrap();
        session.step(10).unwrap();
        assert!(
            crate::link::private_phone::tests::collection_suppressed(&session),
            "reload servicing cannot release private admission"
        );
        engine.reloading.store(false, Ordering::Release);
    }
    engine.pump(clock, false).unwrap();
    session.step(11).unwrap();
    let released = !crate::link::private_phone::tests::collection_suppressed(&session);
    assert!(!reply.ended());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(engine.stop());
    assert!(
        released,
        "ordinary serialized control must release an actually idle private collection"
    );
}

#[test]
fn private_collection_real_engine_releases_idle_source_8000() {
    assert_actual_private_collection_consumer(8000, false);
}

#[test]
fn private_collection_real_engine_releases_idle_source_48000() {
    assert_actual_private_collection_consumer(48000, false);
}

#[test]
fn private_collection_real_engine_excludes_pending_reload_service() {
    assert_actual_private_collection_consumer(8000, true);
}

#[test]
fn private_collection_real_engine_respects_quotes_cancellation_and_actual_timeout() {
    use crate::link::session::{SerialSession, tests::Input};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (prefix, completion, expiry) in [
        ("*4#*7", "", true),
        ("*4#*7", "#", false),
        ("*4#*7", "**#", false),
        ("*4#**", "#", false),
    ] {
        let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
        let engine = engine(configuration(), services, speech());
        let (calls, mut session, state, source, reply) =
            crate::phone::tests::controlled_source_session_mode(8000, false);
        engine
            .run(move |host| {
                crate::host::tests::phone::install_controlled_source(host, calls);
                Ok(())
            })
            .unwrap();
        state
            .lock()
            .unwrap()
            .input
            .extend(prefix.bytes().map(Input::Digit));
        session.step(1).unwrap();
        let clock = RuntimeClock {
            now_ms: 1,
            ..engine.clock()
        };
        engine.pump(clock, false).unwrap();
        session.step(2).unwrap();
        assert!(
            crate::link::private_phone::tests::collection_suppressed(&session),
            "{prefix}"
        );
        // An unchanged control pass must not invent a timeout or a completion.
        engine
            .pump(
                RuntimeClock {
                    now_ms: 100,
                    ..clock
                },
                false,
            )
            .unwrap();
        session.step(3).unwrap();
        assert!(crate::link::private_phone::tests::collection_suppressed(
            &session
        ));
        state
            .lock()
            .unwrap()
            .input
            .extend(completion.bytes().map(Input::Digit));
        session.step(4).unwrap();
        engine
            .pump(
                RuntimeClock {
                    now_ms: if expiry { 10001 } else { 4 },
                    ..clock
                },
                false,
            )
            .unwrap();
        for now in 5..10 {
            session.step(now).unwrap();
        }
        let released = !crate::link::private_phone::tests::collection_suppressed(&session);
        let proof_source = source.clone();
        let proof_reply = reply.clone();
        assert_eq!(
            engine
                .run(move |host| {
                    Ok(host
                        .runtime
                        .node("1000")
                        .unwrap()
                        .links()
                        .phone_collection_pending(&proof_source, &proof_reply))
                })
                .unwrap(),
            Some(false)
        );
        assert!(engine.stop());
        assert!(released, "completion={completion:?}, expiry={expiry}");
    }
}

#[test]
fn private_collection_real_engine_reconciles_loss_without_clearing_new_input() {
    use crate::link::session::{SerialSession, tests::Input};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for newer in [true, false] {
        let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
        let engine = engine(configuration(), services, speech());
        let (calls, mut session, state, source, reply) =
            crate::phone::tests::controlled_source_session_mode(8000, false);
        engine
            .run(move |host| {
                crate::host::tests::phone::install_controlled_source(host, calls);
                Ok(())
            })
            .unwrap();
        state
            .lock()
            .unwrap()
            .input
            .extend("*4#*7".bytes().map(Input::Digit));
        session.step(1).unwrap();
        let clock = RuntimeClock {
            now_ms: 1,
            ..engine.clock()
        };
        engine.pump(clock, false).unwrap();
        session.step(2).unwrap();
        assert!(crate::link::private_phone::tests::collection_suppressed(
            &session
        ));
        // A normal reconciliation already observed the pending inner collector
        // and cleared this call's dirty bit.
        engine
            .pump(
                RuntimeClock {
                    now_ms: 100,
                    ..clock
                },
                false,
            )
            .unwrap();
        engine.lost_digits.store(true, Ordering::Release);
        if newer {
            state
                .lock()
                .unwrap()
                .input
                .extend("*7".bytes().map(Input::Digit));
        }
        session.step(3).unwrap();
        engine
            .pump(
                RuntimeClock {
                    now_ms: 200,
                    ..clock
                },
                false,
            )
            .unwrap();
        session.step(4).unwrap();
        let suppressed = crate::link::private_phone::tests::collection_suppressed(&session);
        let proof_source = source.clone();
        let proof_reply = reply.clone();
        let pending = engine
            .run(move |host| {
                Ok(host
                    .runtime
                    .node("1000")
                    .unwrap()
                    .links()
                    .phone_collection_pending(&proof_source, &proof_reply))
            })
            .unwrap();
        assert!(!reply.ended());
        assert!(state.lock().unwrap().bound_radio.is_none());
        assert!(engine.stop());
        assert_eq!(
            pending,
            Some(newer),
            "the real cancellation precedes later source input"
        );
        assert_eq!(
            suppressed, newer,
            "lost input must release only an actually idle collection, without waiting for another digit"
        );
    }
}

#[path = "lifecycle_phone_remote_tests.rs"]
mod radio_status;

/// RF initiation is retained through the existing unfinished-attempt retry record.
#[test]
fn rf_dial_results_wait_for_actual_outcome_and_survive_initial_retry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let voice = speech();
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let engine = engine(configuration(), services, Arc::clone(&voice));
    let LinkEffect::Connect(cli) = engine
        .operation("1000", operation(LinkAction::Transceive, "2000"))
        .unwrap()
    else {
        panic!("CLI dial");
    };
    assert!(
        !cli.local_operation_result(),
        "CLI work has no RF result provenance"
    );
    engine
        .run(move |host| {
            host.runtime
                .node("1000")
                .unwrap()
                .links()
                .cancel_connect(cli);
            Ok(())
        })
        .unwrap();
    let effect = engine
        .digit_effect(
            "1000",
            DigitOutcome::Operation(operation(LinkAction::PermanentTransceive, "2000")),
        )
        .unwrap();
    assert!(
        voice.opened.lock().unwrap().is_empty(),
        "request acceptance is not completion"
    );
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    engine
        .background(
            "1000".into(),
            effect,
            engine.revision.load(Ordering::Acquire),
            true,
        )
        .unwrap();
    wait_for(&engine, || engine.jobs.lock().unwrap().is_empty());
    let now_ms = engine.elapsed_ms();
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let work = node.work().unwrap();
            Ok(node.links().take_retry(now_ms, work).unwrap())
        })
        .unwrap();
    engine
        .spawn_dial(
            "1000".into(),
            Dial::Retry(retry),
            engine.revision.load(Ordering::Acquire),
            None,
            false,
        )
        .unwrap();
    wait_for(&engine, || engine.jobs.lock().unwrap().is_empty());
    assert!(
        engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Requested operation is unavailable"),
        "a failed RF dial needs a generic local outcome"
    );
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Command completed"),
        "initial retry success retains the RF operation result"
    );
    assert!(engine.stop());
}

#[test]
fn initial_permanent_result_source_survives_host_publication_failures() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (phone_origin, retire_after_failure) in [(false, false), (true, false), (true, true)] {
        let voice = speech();
        let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
        let (services, peers) =
            crate::services::tests::timed_dial_fixture_with_bind_failures(services, 2);
        let engine = engine(configuration(), services, voice);
        let caller = phone_origin.then(|| admit(&engine));
        let request = if let Some((source, reply, _)) = &caller {
            source_operation(
                &engine,
                source,
                reply,
                LinkAction::PermanentTransceive,
                "2000",
            )
        } else {
            operation(LinkAction::PermanentTransceive, "2000")
        };
        let effect = engine
            .digit_effect("1000", DigitOutcome::Operation(request))
            .unwrap();
        let revision = engine.revision.load(Ordering::Acquire);
        assert!(engine.effect("1000".into(), effect, revision).is_err());
        assert_eq!(
            peers.lock().unwrap().len(),
            1,
            "provider dial actually succeeded"
        );
        if retire_after_failure {
            let source = caller.as_ref().unwrap().0.clone();
            let now_ms = engine.elapsed_ms();
            engine
                .run(move |host| {
                    assert!(host.retire_phone(&source));
                    let node = host.runtime.node("1000").unwrap();
                    let work = node.work().unwrap();
                    assert!(
                        node.links().take_retry(now_ms, work).is_none(),
                        "caller retirement cancels unfinished publication recovery"
                    );
                    Ok(())
                })
                .unwrap();
            assert!(engine.stop());
            continue;
        }
        for succeeds in [false, true] {
            let now_ms = engine.elapsed_ms();
            let retry = engine
                .run(move |host| {
                    let node = host.runtime.node("1000").unwrap();
                    let work = node.work().unwrap();
                    Ok(node.links().take_retry(now_ms, work).unwrap())
                })
                .unwrap();
            assert_eq!(
                retry.private_reply(),
                caller.as_ref().map(|(_, reply, _)| reply),
                "unfinished phone work retains the exact source after failed host publication"
            );
            assert_eq!(
                retry.local_operation_result(),
                !phone_origin,
                "unfinished RF work retains its result after failed host publication"
            );
            assert_eq!(
                engine
                    .dial("1000".into(), Dial::Retry(retry), revision)
                    .is_ok(),
                succeeds
            );
        }
        let clock = engine.clock();
        engine
            .run(move |host| {
                host.reject_peer("1000", "2000", clock.now_ms);
                Ok(())
            })
            .unwrap();
        let now_ms = engine.elapsed_ms();
        let recovery = engine
            .run(move |host| {
                let node = host.runtime.node("1000").unwrap();
                let work = node.work().unwrap();
                Ok(node.links().take_retry(now_ms, work).unwrap())
            })
            .unwrap();
        assert!(recovery.private_reply().is_none());
        assert!(
            !recovery.local_operation_result(),
            "completed work never grants authority to recovery"
        );
        drop(recovery);
        assert!(engine.stop());
    }
}

#[test]
fn phone_macro_dial_outcome_is_generic_but_macro_stopped_stays_private() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let voice = speech();
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let (services, peers) = crate::services::tests::timed_dial_fixture(services);
    let config = format!(
        "{}\n[dtmf_macro connect]\ntrigger=*95\nprotected=no\ncommands=*32000#\n",
        configuration()
    );
    let engine = engine(config, services, voice);
    let now_ms = engine.elapsed_ms();
    let rf = engine
        .run(move |host| {
            for digit in "*95".chars() {
                host.runtime.digit(
                    "1000",
                    rpt_advanced_core::runtime::dtmf::DigitEvent::Digit { digit, now_ms },
                );
            }
            Ok(host
                .runtime
                .node("1000")
                .unwrap()
                .links()
                .advance_macros(now_ms)
                .remove(0))
        })
        .unwrap();
    let LinkEffect::Connect(rf) = engine.digit_effect("1000", rf).unwrap() else {
        panic!("RF macro dial");
    };
    assert!(
        !rf.local_operation_result(),
        "RF macro announcements remain out of scope"
    );
    engine
        .run(move |host| {
            host.runtime
                .node("1000")
                .unwrap()
                .links()
                .cancel_connect(rf);
            Ok(())
        })
        .unwrap();
    let (source, reply, state) = admit(&engine);
    let origin = source.clone();
    let target = reply.clone();
    let now_ms = engine.elapsed_ms();
    let outcome = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            for digit in "*95".chars() {
                assert!(
                    node.links()
                        .phone_digit(&origin, digit, now_ms, target.clone(), 3000)
                        .is_none()
                );
            }
            Ok(node.links().advance_macros(now_ms).remove(0))
        })
        .unwrap();
    let effect = engine.digit_effect("1000", outcome).unwrap();
    engine
        .background(
            "1000".into(),
            effect,
            engine.revision.load(Ordering::Acquire),
            false,
        )
        .unwrap();
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 480);
    assert!(
        engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(
        peers
            .lock()
            .unwrap()
            .iter()
            .all(|peer| peer.lock().unwrap().prepared.is_empty()),
        "generic LOCAL outcome never reaches an ordinary peer"
    );
    engine
        .digit_effect("1000", DigitOutcome::MacroStopped { reply: Some(reply) })
        .unwrap();
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 640);
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap(),
        "macro-stopped details remain private after generic outcome drains"
    );
    assert!(engine.stop());
}

#[test]
fn attached_phone_preserves_ordinary_ota_connect_and_permanent_retry() {
    use crate::link::session::SerialSession;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (action, target, retry) in [
        (LinkAction::Transceive, "2000", false),
        (LinkAction::PermanentTransceive, "3000", true),
    ] {
        // Independent attached-call cases keep this deterministically stepped
        // source out of an unrelated preceding graph's asynchronous retirement.
        let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
        let engine = engine(configuration(), services, speech());
        let (mut calls, mut session, _state, source, reply) =
            crate::phone::tests::controlled_source_session_mode(8000, false);
        calls.attach(&source, &reply, "1000").unwrap();
        engine
            .run(move |host| {
                crate::host::tests::phone::install_controlled_source(host, calls);
                Ok(())
            })
            .unwrap();
        let map = rpt_advanced_core::command::DtmfCommandMap::standard();
        let prefix = &map
            .mappings()
            .iter()
            .find(|mapping| mapping.action == action)
            .unwrap()
            .digits;
        let digits = format!("*{prefix}{target}#");
        let now_ms = engine.elapsed_ms();
        let operation = engine
            .run(move |host| {
                let operation = digits
                    .chars()
                    .filter_map(|digit| {
                        host.runtime.digit(
                            "1000",
                            rpt_advanced_core::runtime::dtmf::DigitEvent::Digit { digit, now_ms },
                        )
                    })
                    .find_map(|outcome| match outcome {
                        DigitOutcome::Operation(operation) => Some(operation),
                        _ => None,
                    })
                    .expect("ordinary OTA collector completes independently");
                assert!(
                    operation.reply.is_none(),
                    "OTA does not acquire the private caller token"
                );
                Ok(operation)
            })
            .unwrap();
        let LinkEffect::Connect(attempt) = engine.operation("1000", operation).unwrap() else {
            panic!("ordinary OTA dial");
        };
        let attempt = Dial::Connect(attempt);
        assert!(
            !attempt.timed_g711(),
            "ordinary negotiated codec is preserved"
        );
        crate::fixture::PEER_DIAL_RESULT.store(usize::from(retry), Ordering::Release);
        let result = engine.dial(
            "1000".into(),
            attempt,
            engine.revision.load(Ordering::Acquire),
        );
        if retry {
            assert!(result.is_err());
            let now_ms = engine.elapsed_ms();
            let retry = engine
                .run(move |host| {
                    let node = host.runtime.node("1000").unwrap();
                    let work = node.work().unwrap();
                    Ok(node.links().take_retry(now_ms, work).unwrap())
                })
                .unwrap();
            assert!(retry.private_reply().is_none() && !retry.timed_g711());
            engine
                .dial(
                    "1000".into(),
                    Dial::Retry(retry),
                    engine.revision.load(Ordering::Acquire),
                )
                .unwrap();
        } else {
            result.unwrap();
        }
        assert!(
            engine
                .run(move |host| Ok(host
                    .status_text("1000")?
                    .contains(&format!("{target}: transceive"))))
                .unwrap()
        );
        session.step(engine.elapsed_ms()).unwrap();
        engine.pump(engine.clock(), false).unwrap();
        assert!(reply.phone_authenticated() && !reply.ended());
        let current = source.clone();
        let owner = reply.clone();
        assert!(
            engine
                .run(move |host| Ok(host.phone_reply(&current).as_ref() == Some(&owner)))
                .unwrap()
        );
        drop(session);
        assert!(engine.stop());
    }
}

#[test]
fn slow_phone_quote_keys_refresh_the_exact_destination_collection() {
    use crate::link::session::{SerialSession, tests::Input};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let engine = engine(configuration(), services, speech());
    let (calls, mut session, state, source, reply) =
        crate::phone::tests::controlled_source_session_mode(8000, false);
    engine
        .run(move |host| {
            crate::host::tests::phone::install_controlled_source(host, calls);
            Ok(())
        })
        .unwrap();
    for (now_ms, text) in [(1, "*4#*7"), (4001, "*"), (8001, "*"), (10001, "")] {
        state
            .lock()
            .unwrap()
            .input
            .extend(text.bytes().map(Input::Digit));
        session.step(now_ms).unwrap();
        engine
            .pump(
                RuntimeClock {
                    now_ms,
                    ..engine.clock()
                },
                false,
            )
            .unwrap();
    }
    let proof_source = source.clone();
    let proof_reply = reply.clone();
    assert_eq!(
        engine
            .run(move |host| Ok(host
                .runtime
                .node("1000")
                .unwrap()
                .links()
                .phone_collection_pending(&proof_source, &proof_reply)))
            .unwrap(),
        Some(true),
        "quote controls are actual phone input, not an idle destination"
    );
    state.lock().unwrap().input.push_back(Input::Digit(b'#'));
    session.step(12001).unwrap();
    engine
        .pump(
            RuntimeClock {
                now_ms: 12001,
                ..engine.clock()
            },
            false,
        )
        .unwrap();
    assert_eq!(
        engine
            .run(move |host| Ok(host
                .runtime
                .node("1000")
                .unwrap()
                .links()
                .phone_collection_pending(&source, &reply)))
            .unwrap(),
        Some(false)
    );
    assert!(engine.stop());
}

#[test]
fn phone_grammar_connect_disconnect_reconnect_keeps_exact_source() {
    use crate::link::session::tests::Input;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let (services, _peers) = crate::services::tests::timed_dial_fixture(services);
    let voice = speech();
    let configuration = configuration()
        .replace("[1000]", "[1000]\nphone_allow_accounts=provider")
        .replace(
            "[phone_account provider]",
            "[phone_account provider]\nattach_local_nodes=1000",
        );
    let engine = engine(configuration, services, Arc::clone(&voice));
    let (phone, state) = crate::link::session::tests::phone_source_peer();
    state.lock().unwrap().continuous_async = true;
    let call = phone.identity().call_id().unwrap();
    let clock = engine.clock();
    let (source, reply) = engine
        .run(move |host| {
            host.incoming_peer(phone, "203.0.113.9", clock)?;
            let source = host.phone_source(call).unwrap().clone();
            let reply = host.phone_reply(&source).unwrap();
            host.authenticate_phone(&source, &reply, "100", "1234", clock)?;
            Ok((source, reply))
        })
        .unwrap();
    state
        .lock()
        .unwrap()
        .input
        .extend("*3#".bytes().map(Input::Digit));
    wait_for(&engine, || {
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .any(|text| text == "Command completed")
    });
    let exact_source = source.clone();
    let exact_reply = reply.clone();
    assert!(
        engine
            .run(move |host| {
                Ok(crate::host::tests::phone::has_phone_attachment(
                    host,
                    &exact_source,
                    &exact_reply,
                ))
            })
            .unwrap()
    );
    assert!(reply.phone_authenticated(), "authorized after attachment");
    // Space commands while the real private owner consumes each finite reply.
    // This does not claim physical RF/audio callback validation.
    let pace = || {
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until {
            engine.pump(engine.clock(), false).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    pace();
    for (digits, expected) in [
        ("*4#*32000**#", "2000: transceive"),
        ("*12000**#", "no active links"),
        ("*33000**#", "3000: transceive"),
    ] {
        state
            .lock()
            .unwrap()
            .input
            .extend(digits.bytes().map(Input::Digit));
        wait_for(&engine, || {
            engine
                .run(move |host| Ok(host.status_text("1000")?.contains(expected)))
                .unwrap()
        });
        pace();
        assert!(reply.phone_authenticated(), "authorized after {expected}");
    }
    assert!(
        engine
            .run(move |host| {
                Ok(host.phone_reply(&source).as_ref() == Some(&reply)
                    && crate::host::tests::phone::has_phone_attachment(host, &source, &reply))
            })
            .unwrap()
    );
    assert!(state.lock().unwrap().digits.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(
        voice
            .opened
            .lock()
            .unwrap()
            .iter()
            .all(|text| !text.contains("1234") && !text.contains('*') && !text.contains('#'))
    );
    assert!(engine.stop());
}
