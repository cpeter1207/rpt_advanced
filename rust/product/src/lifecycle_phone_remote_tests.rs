//! Actual phone grammar and serialized radio tickets; every physical callback is a fixture.
use super::*;
use crate::link::session::tests::Input;
use std::sync::atomic::AtomicUsize;

static READY: AtomicBool = AtomicBool::new(false);
static SUBMITTED: AtomicUsize = AtomicUsize::new(0);
static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static LOCAL: AtomicBool = AtomicBool::new(false);
static LONG: AtomicBool = AtomicBool::new(false);
static MODE: AtomicI32 = AtomicI32::new(0);
static RETIRE_ON_TAKE: Mutex<Option<PeerReply>> = Mutex::new(None);

const STATUS: &[u8] = br#"{"version":1,"kind":"status","requested":{"rx_hz":146530000,"tx_hz":146530000,"mode":1,"power_mw":5000},"applied":{"rx_hz":146520000,"tx_hz":146520000,"mode":1,"power_mw":5000,"bandwidth_hz":12500},"observed_values":null,"scan":{},"tx_inhibited":false,"receive_permitted":true,"serviced_ptt":false,"failure_code":0}"#;

unsafe extern "C" fn source(_: *mut c_void, _: u32) -> i32 {
    if MODE.load(Ordering::Acquire) == 3 {
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
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    if let Some(reply) = RETIRE_ON_TAKE.lock().unwrap().take() {
        reply.retire();
    }
    if MODE.load(Ordering::Acquire) == 5 {
        return -42;
    }
    let text = if MODE.load(Ordering::Acquire) == 4 {
        b"malformed".as_slice()
    } else {
        STATUS
    };
    // SAFETY: the product provides this synchronous copied-result sink.
    unsafe { sink.unwrap()(context, text.as_ptr(), text.len() as u32) };
    0
}
unsafe extern "C" fn destroy(handle: *mut c_void) {
    // SAFETY: submit transfers exactly one boxed fixture owner to the product.
    drop(unsafe { Box::from_raw(handle.cast::<u8>()) });
    DESTROYED.fetch_add(1, Ordering::AcqRel);
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
    _: *mut c_void,
    request: *const abi::UrpRemoteOperation,
    flags: u32,
    output: *mut abi::rptadv_remote_ticket_v1,
) -> i32 {
    // SAFETY: the product lends a complete wire request and writable ticket.
    let request = unsafe { &*request };
    assert_eq!(
        request.operation, 6,
        "only the existing read-only status operation"
    );
    assert!(request.flags <= 1);
    LONG.store(request.flags != 0, Ordering::Release);
    assert_eq!(
        flags & 1,
        u32::from(LOCAL.load(Ordering::Acquire)),
        "source identity must not change"
    );
    SUBMITTED.fetch_add(1, Ordering::AcqRel);
    match MODE.load(Ordering::Acquire) {
        1 => return 1,
        2 => return -42,
        _ => {}
    }
    unsafe {
        output.write(abi::rptadv_remote_ticket_v1 {
            struct_size: size_of::<abi::rptadv_remote_ticket_v1>() as u32,
            abi_version: 1,
            context: Box::into_raw(Box::new(0_u8)).cast(),
            source: Some(source),
            take: Some(take),
            destroy: Some(destroy),
        })
    };
    0
}
fn services() -> HostServices {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_remote_submit = Some(submit);
    table.radio_metadata_bind = Some(bind_metadata);
    table.radio_service = Some(service);
    table.radio_quiesce = Some(service);
    table.radio_ready = Some(service);
    // Existing fixture radio open/close never opens a device. Only the remote
    // ticket boundary is replaced, leaving real control ownership in place.
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}
fn configured(access: &str) -> String {
    format!(
        "{}\n[remote_base 1000]\nenabled=yes\nbackend=hamlib\nhamlib_model=RIG_MODEL_DUMMY\n\
        dtmf_access={access}\nstartup_memory=local\nallowed_modes=FM\n\
        allowed_rx_ranges_hz=144000000-148000000\nallowed_tx_ranges_hz=144000000-148000000\n\
        [remote_memory 1000 local]\nselector=1\nrx_frequency_hz=146520000\n\
        tx_frequency_hz=146520000\nmode=FM\n",
        configuration()
    )
}
fn radio_speech() -> Arc<crate::media::text::tests::SpeechFixture> {
    let fixture = super::speech();
    // Applied settings is ordinary output here, not the shared text fixture's
    // intentional A-prefixed cancellation stall.
    fixture.release();
    fixture
}

fn reset() {
    LOCAL.store(false, Ordering::Release);
    LONG.store(false, Ordering::Release);
    MODE.store(0, Ordering::Release);
    *RETIRE_ON_TAKE.lock().unwrap() = None;
    READY.store(false, Ordering::Release);
    SUBMITTED.store(0, Ordering::Release);
    DESTROYED.store(0, Ordering::Release);
}

#[test]
fn private_radio_status_existing_local_query_baseline_uses_same_radio_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    LOCAL.store(true, Ordering::Release);
    let voice = radio_speech();
    let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
    let clock = engine.clock();
    engine
        .run(move |host| {
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    host.runtime.digit(
                        "1000",
                        rpt_advanced_core::runtime::dtmf::DigitEvent::Digit {
                            digit,
                            now_ms: clock.now_ms,
                        },
                    )
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let effect = host.runtime.command("1000", operation, clock, false)?;
            assert!(matches!(effect, LinkEffect::RemoteBase(_)));
            host.immediate("1000", effect, clock)
        })
        .unwrap();
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
    READY.store(true, Ordering::Release);
    engine.pump(engine.clock(), false).unwrap();
    let public = engine
        .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
        .unwrap();
    let texts = voice.opened.lock().unwrap().clone();
    assert!(engine.stop());
    assert!(
        public,
        "ordinary local status keeps its existing RF audience"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Applied settings.")
                && text.contains("Receive hertz 146520000.")),
        "{texts:?}"
    );
    assert_eq!(DESTROYED.load(Ordering::Acquire), 1);
}

#[test]
fn private_radio_status_copied_operation_retains_exact_collector_through_actual_host_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), radio_speech());
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    let clock = engine.clock();
    let (before, after) = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .phone_digit(&source, digit, clock.now_ms, reply.clone())
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let before = host
                .runtime
                .command("1000", operation.clone(), clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            host.pump(clock)?;
            let after = host
                .runtime
                .command("1000", operation, clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            Ok((before, after))
        })
        .unwrap();
    assert!(engine.stop());
    assert_eq!(
        before,
        Ok(true),
        "same exact private query is authorized before the control boundary"
    );
    assert_eq!(
        after,
        Ok(true),
        "actual pump must not reclaim a source required by copied pending command"
    );
    assert_eq!(
        SUBMITTED.load(Ordering::Acquire),
        0,
        "diagnostic exercises admission only, not hardware submission"
    );
}

#[test]
fn ordinary_peer_radio_status_copied_operation_survives_actual_host_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), radio_speech());
    let clock = engine.clock();
    let (before, after) = engine
        .run(move |host| {
            host.runtime.incoming("1000", "2000", true)?;
            let signals = rpt_advanced_core::link::PeerSignals::new();
            let reply = signals.reply_target();
            let node = host.runtime.node("1000").unwrap();
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .peer_digit("2000", digit, clock.now_ms, reply.clone())
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let before = host
                .runtime
                .command("1000", operation.clone(), clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            host.pump(clock)?;
            let after = host
                .runtime
                .command("1000", operation, clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            signals.end();
            host.runtime.node("1000").unwrap().links().ended("2000");
            host.runtime.peer_detached("1000", "2000", clock.now_ms);
            Ok((before, after))
        })
        .unwrap();
    assert!(engine.stop());
    assert_eq!(before, Ok(true));
    assert_eq!(
        after,
        Ok(true),
        "idle source reclamation must not invalidate a queued ordinary peer query"
    );
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 0);
}

#[test]
fn remote_operation_custody_layout_and_lazy_allocation_probe() {
    use rpt_advanced_core::{
        access::AccessPolicy,
        command::DtmfCommandMap,
        link::PeerSignals,
        remote_base_command::RemoteDtmfAccess,
        runtime::{
            dtmf::{DigitEvent, DigitOperation, DtmfCommands},
            links::NodeLinkControl,
        },
    };
    fn collector() -> NodeLinkControl {
        let mut links = NodeLinkControl::new(
            "1000",
            AccessPolicy::new("", "").unwrap(),
            DtmfCommandMap::standard(),
            None,
        )
        .unwrap();
        links
            .configure_remote("6", RemoteDtmfAccess::Disabled)
            .unwrap();
        links.accept("2000", true).unwrap();
        links
    }
    fn peer_query(links: &mut NodeLinkControl, reply: &PeerReply) -> (DigitOperation, usize) {
        for digit in "*652".chars() {
            assert!(links.peer_digit("2000", digit, 1, reply.clone()).is_none());
        }
        let (outcome, allocations) =
            crate::fixture::allocations(|| links.peer_digit("2000", '#', 1, reply.clone()));
        let Some(DigitOutcome::Operation(operation)) = outcome else {
            panic!("real peer status completion")
        };
        (operation, allocations)
    }
    let mut links = collector();
    let reply = PeerSignals::new().reply_target();
    let (first, first_allocations) = peer_query(&mut links, &reply);
    let (second, shared_allocations) = peer_query(&mut links, &reply);
    let (_, clone_allocations) = crate::fixture::allocations(|| first.clone());
    drop((first, second));
    let (fresh, fresh_allocations) = peer_query(&mut links, &reply);
    drop(fresh);
    let mut local = DtmfCommands::new(DtmfCommandMap::standard());
    local
        .configure_remote("6", RemoteDtmfAccess::Disabled)
        .unwrap();
    for digit in "*652".chars() {
        assert!(local.feed(DigitEvent::Digit { digit, now_ms: 1 }).is_none());
    }
    let (outcome, local_allocations) = crate::fixture::allocations(|| {
        local.feed(DigitEvent::Digit {
            digit: '#',
            now_ms: 1,
        })
    });
    assert!(matches!(outcome, Some(DigitOutcome::Operation(_))));
    assert_eq!(
        (
            first_allocations,
            shared_allocations,
            fresh_allocations,
            clone_allocations,
            local_allocations
        ),
        (5, 4, 5, 1, 1),
        "one lazy custody allocation per original collector while work is pending; copies share it"
    );
    println!(
        "custody layout: DigitOperation={} DigitOutcome={} DtmfCommands={}; allocations: first={first_allocations} shared={shared_allocations} fresh={fresh_allocations} clone={clone_allocations} local={local_allocations}",
        std::mem::size_of::<DigitOperation>(),
        std::mem::size_of::<DigitOutcome>(),
        std::mem::size_of::<DtmfCommands>()
    );
}

#[test]
fn ordinary_peer_radio_status_native_pending_keeps_custody_until_completion_or_retirement() {
    for boundary in ["completion", "retirement", "reload"] {
        let _serial = crate::fixture::LIFECYCLE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        reset();
        let engine = engine(configured("disabled"), services(), radio_speech());
        let clock = engine.clock();
        let signals = engine
            .run(move |host| {
                host.runtime.incoming("1000", "2000", true)?;
                let signals = rpt_advanced_core::link::PeerSignals::new();
                let reply = signals.reply_target();
                let node = host.runtime.node("1000").unwrap();
                let operation = "*652#"
                    .chars()
                    .filter_map(|digit| {
                        node.links()
                            .peer_digit("2000", digit, clock.now_ms, reply.clone())
                    })
                    .find_map(|outcome| match outcome {
                        DigitOutcome::Operation(operation) => Some(operation),
                        _ => None,
                    })
                    .unwrap();
                let effect = host.runtime.command("1000", operation, clock, false)?;
                host.immediate("1000", effect, clock)?;
                Ok(signals)
            })
            .unwrap();
        for _ in 0..3 {
            engine.pump(engine.clock(), false).unwrap();
            assert_eq!(
                DESTROYED.load(Ordering::Acquire),
                0,
                "native Pending alone must keep the collector discoverable"
            );
        }
        assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
        match boundary {
            "completion" => READY.store(true, Ordering::Release),
            "retirement" => signals.end(),
            "reload" => {
                let clock = engine.clock();
                engine
                    .run(move |host| host.reload(document(&configured("disabled")).unwrap(), clock))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        engine.pump(engine.clock(), false).unwrap();
        assert_eq!(DESTROYED.load(Ordering::Acquire), 1, "{boundary}");
        signals.end();
        engine
            .run(move |host| {
                host.runtime.node("1000").unwrap().links().ended("2000");
                host.runtime.peer_detached("1000", "2000", clock.now_ms);
                Ok(())
            })
            .unwrap();
        assert!(engine.stop());
        assert_eq!(
            DESTROYED.load(Ordering::Acquire),
            1,
            "native receipt is released exactly once"
        );
    }
}

#[test]
fn private_radio_status_real_grammar_preserves_public_query_permission_and_private_audience() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut failures = Vec::new();
    for access in ["disabled", "locked", "open"] {
        for long in [false, true] {
            reset();
            let voice = radio_speech();
            let engine = engine(configured(access), services(), Arc::clone(&voice));
            let (_source, reply, state) = admit(&engine);
            wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
            state.lock().unwrap().input.extend(
                if long { "*4#*651**#" } else { "*4#*652**#" }
                    .bytes()
                    .map(Input::Digit),
            );
            wait_for(&engine, || {
                SUBMITTED.load(Ordering::Acquire) != 0 || state.lock().unwrap().audio.len() >= 480
            });
            if SUBMITTED.load(Ordering::Acquire) != 0 {
                assert_eq!(
                    state.lock().unwrap().audio.len(),
                    320,
                    "pending status is not a fabricated success"
                );
                READY.store(true, Ordering::Release);
                wait_for_private_status_audio(&engine, &state, &voice, &reply);
            }
            let text = voice.opened.lock().unwrap().last().cloned().unwrap();
            if !text.contains("Applied settings.") || !text.contains("Receive hertz 146520000.") {
                failures.push(format!(
                    "{access}/{long}: missing actual status, got {text:?}"
                ));
            }
            if text.contains("Requested settings.") != long
                || text.contains("Passband hertz 12500.") != long
            {
                failures.push(format!(
                    "{access}/{long}: wrong short/long presentation {text:?}"
                ));
            }
            let submitted = SUBMITTED.load(Ordering::Acquire);
            assert!(
                !engine
                    .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
                    .unwrap()
            );
            assert!(state.lock().unwrap().bound_radio.is_none());
            assert!(reply.phone_authenticated());
            assert!(engine.stop());
            if submitted != 1
                || DESTROYED.load(Ordering::Acquire) != 1
                || LONG.load(Ordering::Acquire) != long
            {
                failures.push(format!(
                    "{access}/{long}: request/ticket ownership count is wrong ({submitted})"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn wait_for_private_status_audio(
    engine: &Arc<Engine>,
    state: &Arc<Mutex<crate::link::session::tests::State>>,
    voice: &Arc<crate::media::text::tests::SpeechFixture>,
    reply: &PeerReply,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        let current = state.lock().unwrap();
        if current.audio.len() >= 480 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "private Status output stalled: frames={}, writes={:?}, ended={}, submitted={}, destroyed={}, texts={:?}",
            current.audio.len(),
            current.writes,
            reply.ended(),
            SUBMITTED.load(Ordering::Acquire),
            DESTROYED.load(Ordering::Acquire),
            voice.opened.lock().unwrap()
        );
        drop(current);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn private_radio_operation(
    engine: &Arc<Engine>,
    source: &PhoneSource,
    reply: &PeerReply,
    selector: &str,
) -> DigitOperation {
    let source = source.clone();
    let reply = reply.clone();
    let text = format!("*6{selector}#");
    let now_ms = engine.elapsed_ms();
    engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            Ok(text
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .phone_digit(&source, digit, now_ms, reply.clone())
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap())
        })
        .unwrap()
}

fn submit_private_status(
    engine: &Arc<Engine>,
    source: &PhoneSource,
    reply: &PeerReply,
    selector: &str,
) -> Result<(), RuntimeError> {
    let operation = private_radio_operation(engine, source, reply, selector);
    let clock = engine.clock();
    engine.run(move |host| {
        let effect = host.runtime.command("1000", operation, clock, false)?;
        host.immediate("1000", effect, clock)
    })
}

#[test]
fn private_radio_status_errors_are_private_and_release_only_actual_native_receipts() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for mode in 1..=5 {
        reset();
        let voice = radio_speech();
        let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
        let (source, reply, state) = admit(&engine);
        wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
        MODE.store(mode, Ordering::Release);
        READY.store(true, Ordering::Release);
        submit_private_status(&engine, &source, &reply, "52").unwrap();
        wait_for(&engine, || state.lock().unwrap().audio.len() >= 480);
        let text = voice.opened.lock().unwrap().last().cloned().unwrap();
        assert!(
            text.contains(if mode == 1 {
                "Remote radio is busy."
            } else {
                "Remote radio command failed."
            }),
            "{mode}: {text}"
        );
        assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
        assert_eq!(DESTROYED.load(Ordering::Acquire), usize::from(mode >= 3));
        assert!(
            !engine
                .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
                .unwrap()
        );
        assert!(engine.stop());
        assert_eq!(DESTROYED.load(Ordering::Acquire), usize::from(mode >= 3));
    }
}

#[test]
fn private_radio_status_duplicate_busy_obeys_existing_reply_queue_and_fifo() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Tagged(usize, Arc<Mutex<Vec<usize>>>);
    impl PcmStreamReader for Tagged {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    impl Drop for Tagged {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let voice = radio_speech();
    let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    submit_private_status(&engine, &source, &reply, "52").unwrap();
    submit_private_status(&engine, &source, &reply, "51").unwrap();
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 480);
    assert_eq!(
        voice.opened.lock().unwrap().last().map(String::as_str),
        Some("Remote radio is busy. Try again.")
    );
    assert_eq!(
        SUBMITTED.load(Ordering::Acquire),
        1,
        "duplicate Busy never submits native work"
    );
    // Fill the actual reply queue without racing the separate four-entry
    // producer command queue. Its own backpressure is not private queue capacity.
    let dropped = Arc::new(Mutex::new(Vec::new()));
    let (saved_source, saved_reply, owners) = (source.clone(), reply.clone(), dropped.clone());
    engine
        .run(move |host| {
            for id in 0..16 {
                host.queue_phone_reply(
                    &saved_source,
                    &saved_reply,
                    Box::new(Tagged(id, owners.clone())),
                )?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        submit_private_status(&engine, &source, &reply, "52"),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
    assert!(dropped.lock().unwrap().is_empty());
    let (saved_source, saved_reply, owners) = (source.clone(), reply.clone(), dropped.clone());
    engine
        .run(move |host| {
            for id in 0..16 {
                drop(host.take_phone_reply(&saved_source, &saved_reply)?.unwrap());
                assert_eq!(*owners.lock().unwrap(), (0..=id).collect::<Vec<_>>());
            }
            assert!(
                host.take_phone_reply(&saved_source, &saved_reply)?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(
        engine
            .run(move |host| Ok(host.retire_phone(&source)))
            .unwrap()
    );
    engine.pump(engine.clock(), false).unwrap();
    assert_eq!(DESTROYED.load(Ordering::Acquire), 1);
    assert!(engine.stop());
}

#[test]
fn private_radio_status_revalidates_after_take_before_registering_a_producer() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let voice = radio_speech();
    let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    let prepared = voice.opened.lock().unwrap().len();
    submit_private_status(&engine, &source, &reply, "52").unwrap();
    *RETIRE_ON_TAKE.lock().unwrap() = Some(reply.clone());
    READY.store(true, Ordering::Release);
    wait_for(&engine, || DESTROYED.load(Ordering::Acquire) == 1);
    assert!(reply.ended());
    assert_eq!(
        voice.opened.lock().unwrap().len(),
        prepared,
        "retired callback cannot create a response producer"
    );
    assert_eq!(state.lock().unwrap().audio.len(), 320);
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    assert!(engine.stop());
}

#[test]
fn private_radio_status_cap_includes_retired_receipts_until_real_cleanup() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), radio_speech());
    for _ in 0..128 {
        let (source, reply, _) = admit(&engine);
        submit_private_status(&engine, &source, &reply, "52").unwrap();
        assert!(
            engine
                .run(move |host| Ok(host.retire_phone(&source)))
                .unwrap()
        );
    }
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 128);
    assert_eq!(
        DESTROYED.load(Ordering::Acquire),
        0,
        "retirement alone does not release native receipt ownership"
    );
    let (source, reply, _) = admit(&engine);
    submit_private_status(&engine, &source, &reply, "52").unwrap(); // Busy, not another ticket.
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 128);
    engine.pump(engine.clock(), false).unwrap();
    assert_eq!(DESTROYED.load(Ordering::Acquire), 128);
    submit_private_status(&engine, &source, &reply, "52").unwrap();
    assert_eq!(
        SUBMITTED.load(Ordering::Acquire),
        129,
        "actual receipt cleanup restores capacity for same-call-ID replacement"
    );
    assert!(engine.stop());
    assert_eq!(DESTROYED.load(Ordering::Acquire), 129);
}

#[test]
fn private_radio_status_other_families_and_foreign_or_retired_sources_never_submit() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("open"), services(), radio_speech());
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    for selector in ["998", "61"] {
        // Existing public CurrentStatus, then an administrative Mode command.
        assert_eq!(
            submit_private_status(&engine, &source, &reply, selector),
            Err(RuntimeError::Rejected)
        );
    }
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 0);
    let operation = private_radio_operation(&engine, &source, &reply, "52");
    let other = rpt_advanced_core::link::PeerSignals::for_phone(source.clone());
    assert!(other.authenticate_phone());
    let mut foreign = operation.clone();
    foreign.reply = Some(other.reply_target());
    let clock = engine.clock();
    assert_eq!(
        engine.run(move |host| host.immediate("1000", LinkEffect::RemoteBase(foreign), clock)),
        Err(RuntimeError::Rejected)
    );
    let old_source = source.clone();
    assert!(
        engine
            .run(move |host| Ok(host.retire_phone(&old_source)))
            .unwrap()
    );
    assert_eq!(
        engine.run(move |host| host.immediate("1000", LinkEffect::RemoteBase(operation), clock)),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 0);
    assert!(engine.stop());
}

#[test]
fn private_radio_status_reload_or_hangup_cancels_old_ticket_without_replying_to_replacement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for reload in [false, true] {
        reset();
        let voice = radio_speech();
        let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
        let (source, reply, old_state) = admit(&engine);
        wait_for(&engine, || old_state.lock().unwrap().audio.len() >= 320);
        submit_private_status(&engine, &source, &reply, "52").unwrap();
        let old_operation = private_radio_operation(&engine, &source, &reply, "51");
        let clock = engine.clock();
        engine
            .run(move |host| {
                if reload {
                    host.reload(document(&configured("disabled")).unwrap(), clock)
                } else {
                    assert!(host.retire_phone(&source));
                    Ok(())
                }
            })
            .unwrap();
        engine.pump(engine.clock(), false).unwrap();
        assert!(reply.ended());
        assert_eq!(DESTROYED.load(Ordering::Acquire), 1);
        let (fresh_source, fresh_reply, fresh_state) = admit(&engine);
        wait_for(&engine, || fresh_state.lock().unwrap().audio.len() >= 320);
        assert_ne!(reply, fresh_reply);
        assert_eq!(
            engine.run(move |host| host.immediate(
                "1000",
                LinkEffect::RemoteBase(old_operation),
                clock
            )),
            Err(RuntimeError::Rejected)
        );
        assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
        submit_private_status(&engine, &fresh_source, &fresh_reply, "52").unwrap();
        READY.store(true, Ordering::Release);
        wait_for_private_status_audio(&engine, &fresh_state, &voice, &fresh_reply);
        assert_eq!(old_state.lock().unwrap().audio.len(), 320);
        assert_eq!(DESTROYED.load(Ordering::Acquire), 2);
        assert!(engine.stop());
    }
}

#[test]
fn private_radio_status_current_route_rejects_wrong_node_and_committed_account_changes() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for change in ["wrong node", "removed", "disabled", "rebound"] {
        reset();
        let voice = radio_speech();
        let initial = if change == "wrong node" {
            format!(
                "{}\n[2000]\nradio_channel=second\nduplex=full\n",
                configured("disabled")
            )
        } else {
            configured("disabled")
        };
        let engine = engine(initial, services(), Arc::clone(&voice));
        let (source, reply, state) = admit(&engine);
        wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
        let operation = private_radio_operation(&engine, &source, &reply, "52");
        let clock = engine.clock();
        if change == "wrong node" {
            let wrong = operation.clone();
            assert_eq!(
                engine.run(move |host| host.immediate(
                    "2000",
                    LinkEffect::RemoteBase(wrong),
                    clock
                )),
                Err(RuntimeError::Rejected)
            );
            assert_eq!(SUBMITTED.load(Ordering::Acquire), 0);
            assert_eq!(voice.opened.lock().unwrap().len(), 2);
        } else {
            submit_private_status(&engine, &source, &reply, "52").unwrap();
            let account = include_str!("tests/fixtures/phone_account.conf");
            let modified = match change {
                "removed" => configured("disabled").replace(account, ""),
                "disabled" => configured("disabled")
                    .replace(account, &account.replace("enabled=yes", "enabled=no")),
                _ => format!(
                    "{}\n[2000]\nradio_channel=second\nduplex=full\n",
                    configured("disabled").replace(
                        account,
                        &account.replace("local_node=1000", "local_node=2000")
                    )
                ),
            };
            engine
                .run(move |host| host.reload(document(&modified).unwrap(), clock))
                .unwrap();
            wait_for(&engine, || {
                reply.ended() && DESTROYED.load(Ordering::Acquire) == 1
            });
            READY.store(true, Ordering::Release);
            let before = voice.opened.lock().unwrap().len();
            assert_eq!(
                engine.run(move |host| host.immediate(
                    "1000",
                    LinkEffect::RemoteBase(operation),
                    clock
                )),
                Err(RuntimeError::Rejected)
            );
            engine.pump(engine.clock(), false).unwrap();
            assert_eq!(voice.opened.lock().unwrap().len(), before);
            assert_eq!(state.lock().unwrap().audio.len(), 320);
            assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
        }
        assert!(
            !engine
                .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
                .unwrap()
        );
        assert!(engine.stop());
    }
}

#[test]
fn private_radio_status_completed_result_drops_on_real_queue_pressure_without_rf_fallback() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Tagged(usize, Arc<Mutex<Vec<usize>>>);
    impl PcmStreamReader for Tagged {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    impl Drop for Tagged {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }
    struct Held(Arc<AtomicBool>);
    impl PcmStreamReader for Held {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            self.0.store(true, Ordering::Release);
            PcmRead::Pending
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), radio_speech());
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    let entered = Arc::new(AtomicBool::new(false));
    let (saved_source, saved_reply, held) = (source.clone(), reply.clone(), entered.clone());
    engine
        .run(move |host| host.queue_phone_reply(&saved_source, &saved_reply, Box::new(Held(held))))
        .unwrap();
    wait_for(&engine, || entered.load(Ordering::Acquire));
    let dropped = Arc::new(Mutex::new(Vec::new()));
    for batch in 0..2 {
        let (saved_source, saved_reply, dropped) = (source.clone(), reply.clone(), dropped.clone());
        engine
            .run(move |host| {
                for id in batch * 16..(batch + 1) * 16 {
                    host.queue_phone_reply(
                        &saved_source,
                        &saved_reply,
                        Box::new(Tagged(id, dropped.clone())),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        if batch == 0 {
            // Current Held playback prevents consumption of the first16 queued commands.
            engine.pump(engine.clock(), false).unwrap();
        }
    }
    submit_private_status(&engine, &source, &reply, "52").unwrap();
    READY.store(true, Ordering::Release);
    wait_for(&engine, || DESTROYED.load(Ordering::Acquire) == 1);
    assert!(
        dropped.lock().unwrap().is_empty(),
        "successful result cannot replace prior reply owners"
    );
    assert_eq!(state.lock().unwrap().audio.len(), 320);
    assert!(
        !engine
            .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
            .unwrap()
    );
    let (saved_source, saved_reply, observed) = (source.clone(), reply.clone(), dropped.clone());
    engine
        .run(move |host| {
            for id in 16..32 {
                drop(host.take_phone_reply(&saved_source, &saved_reply)?.unwrap());
                assert_eq!(*observed.lock().unwrap(), (16..=id).collect::<Vec<_>>());
            }
            assert!(
                host.take_phone_reply(&saved_source, &saved_reply)?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
    assert!(
        engine
            .run(move |host| Ok(host.retire_phone(&source)))
            .unwrap()
    );
    assert!(engine.stop());
    let mut all = dropped.lock().unwrap().clone();
    all.sort_unstable();
    assert_eq!(
        all,
        (0..32).collect::<Vec<_>>(),
        "every prior owner is reclaimed exactly once"
    );
}
