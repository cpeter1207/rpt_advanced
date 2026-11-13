//! Real serial control/dial owners and native media callbacks; no radio hardware.
use super::*;
use crate::media::text::tests::SpeechFixture;
use rpt_advanced_core::{
    link::{PeerReply, PhoneSource},
    runtime::dtmf::DigitOutcome,
};

fn engine(
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

fn speech() -> Arc<SpeechFixture> {
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
                        .phone_digit(&source, digit, now_ms, reply.clone())
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
fn asynchronous_phone_link_commands_deliver_privately_without_public_connect_or_disconnect_notices()
{
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
            .any(|text| text == "Command failed")
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
                        .phone_digit(&source, digit, clock.now_ms, owner.clone())
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
                    now_ms: if expiry { 3001 } else { 4 },
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
