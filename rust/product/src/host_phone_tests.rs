//! Pending phone calls have private control ownership, never a radio-peer slot.
use super::*;
use crate::phone::PhoneLogin;
use rpt_advanced_core::command::LinkAction;

fn account_configuration(local: &str) -> String {
    include_str!("tests/fixtures/phone_account.conf")
        .replace("local_node=1000", &format!("local_node={local}"))
}

fn host() -> Host {
    Host::start(
        document("usb", &account_configuration("1000")),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}

fn mailbox_configuration(local: &str) -> String {
    format!(
        "{}{}",
        include_str!("tests/fixtures/phone_mailbox.conf"),
        account_configuration(local),
    )
}

fn mailbox_host() -> Host {
    Host::start(
        document("usb", &mailbox_configuration("1000")),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}

thread_local! {
    static BEFORE_EVENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_OPERATION_RESULT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(crate) fn before_event() {
    let callback = BEFORE_EVENT.with_borrow_mut(Option::take);
    if let Some(callback) = callback {
        callback();
    }
}

pub(crate) fn before_operation_result() {
    if let Some(callback) = BEFORE_OPERATION_RESULT.with_borrow_mut(Option::take) {
        callback();
    }
}

// Keep real-time worker timeout fixtures short; default and overridden timing
// are independently checked with deterministic timestamps below.
fn driven_mailbox_host() -> (Host, Arc<crate::media::text::tests::SpeechFixture>) {
    let speech = Arc::new(crate::media::text::tests::SpeechFixture::default());
    let configuration = document(
        "usb",
        &format!(
            "[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}inter_digit_timeout_ms=3000\n",
            mailbox_configuration("1000")
        ),
    );
    let host = Host::start(
        configuration,
        crate::media::text::tests::preparer(Arc::clone(&speech)),
        services(),
        Instant::now(),
        RuntimeClock {
            now_ms: 0,
            ..clock()
        },
    )
    .unwrap();
    (host, speech)
}
fn private_clock(host: &Host) -> RuntimeClock {
    RuntimeClock {
        now_ms: host.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        ..clock()
    }
}
fn await_digits(host: &Host, source: &PhoneSource, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while crate::phone::tests::pending_digits(&host.phone_calls, source) < count {
        assert!(
            Instant::now() < deadline,
            "actual private input receipt missing"
        );
        std::thread::yield_now();
    }
}

#[test]
fn retirement_after_a_digit_is_copied_cannot_authenticate_or_dispatch_it() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _speech) = driven_mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
        .unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    state
        .lock()
        .unwrap()
        .input
        .push_back(crate::link::session::tests::Input::Digit(b'1'));
    await_digits(&host, &source, 1);
    let owner_reply = reply.clone();
    let (retire, request) = std::sync::mpsc::sync_channel(1);
    let (completed, receipt) = std::sync::mpsc::sync_channel(1);
    let owner = std::thread::spawn(move || {
        request.recv().unwrap();
        owner_reply.retire();
        completed.send(()).unwrap();
    });
    BEFORE_EVENT.with_borrow_mut(|callback| {
        *callback = Some(Box::new(move || {
            retire.send(()).unwrap();
            receipt.recv_timeout(Duration::from_secs(2)).unwrap();
        }))
    });
    host.pump(private_clock(&host)).unwrap();
    owner.join().unwrap();
    assert!(host.phone_source(call).is_none() && reply.ended());
    assert!(host.operations.is_empty() && host.peers.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(host.stop(20));
}

#[test]
fn private_event_and_timeout_reply_pressure_retire_only_the_original_call() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Empty;
    impl PcmStreamReader for Empty {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for kind in ["event", "login-timeout", "grammar-timeout"] {
        for full in [false, true] {
            if kind == "event" && !full {
                continue;
            }
            let (mut host, _speech) = driven_mailbox_host();
            let (phone, state) = crate::link::session::tests::phone_peer();
            let call = phone.identity().call_id().unwrap();
            host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
                .unwrap();
            let source = host.phone_source(call).unwrap().clone();
            let reply = host.phone_reply(&source).unwrap();
            if kind != "login-timeout" {
                host.authenticate_phone(&source, &reply, "100", "1234", private_clock(&host))
                    .unwrap();
            }
            while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
            if full {
                for _ in 0..16 {
                    host.queue_phone_reply(&source, &reply, Box::new(Empty))
                        .unwrap();
                }
            }
            let mut tick = private_clock(&host);
            match kind {
                "event" => {
                    state
                        .lock()
                        .unwrap()
                        .input
                        .extend("*3#".bytes().map(crate::link::session::tests::Input::Digit));
                    await_digits(&host, &source, 3);
                }
                "login-timeout" => {
                    host.phone_calls
                        .login_digit(&source, &reply, '1', 0)
                        .unwrap();
                    tick.now_ms = 3000;
                }
                "grammar-timeout" => {
                    for digit in "*3".chars() {
                        host.phone_calls.actions(&source, &reply, digit, 0).unwrap();
                    }
                    tick.now_ms = 3000;
                }
                _ => unreachable!(),
            }
            host.pump(tick).unwrap();
            assert_eq!(host.phone_source(call).is_none(), full, "{kind}");
            assert_eq!(reply.ended(), full, "{kind}");
            assert!(host.operations.is_empty() && host.peers.is_empty());
            assert!(state.lock().unwrap().bound_radio.is_none());
            assert!(host.stop(3020));
        }
    }
}

#[test]
fn configured_three_second_login_pause_resets_privately_before_the_next_digit() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, speech) = driven_mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
        .unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    state
        .lock()
        .unwrap()
        .input
        .push_back(crate::link::session::tests::Input::Digit(b'1'));
    await_digits(&host, &source, 1);
    host.pump(private_clock(&host)).unwrap();
    std::thread::sleep(Duration::from_millis(3010));
    state
        .lock()
        .unwrap()
        .input
        .push_back(crate::link::session::tests::Input::Digit(b'2'));
    await_digits(&host, &source, 1);
    host.pump(private_clock(&host)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !speech
        .opened
        .lock()
        .unwrap()
        .iter()
        .any(|text| text == "Login unsuccessful. Enter your mailbox number followed by pound")
    {
        host.pump(private_clock(&host)).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!reply.phone_authenticated() && !reply.ended());
    assert!(host.operations.is_empty() && host.peers.is_empty());
    assert!(state.lock().unwrap().digits.is_empty());
    assert!(host.stop(4000));
}

fn await_private_drop(
    state: &std::sync::Arc<std::sync::Mutex<crate::link::session::tests::State>>,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.lock().unwrap().drops == 0 {
        assert!(
            Instant::now() < deadline,
            "private I/O owner did not release its retired handle"
        );
        std::thread::yield_now();
    }
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn sequential_login_prompts_wait_for_each_field_without_public_audio_or_credentials() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, speech) = driven_mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
        .unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let await_prompt = |host: &mut Host, count: usize| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while speech.opened.lock().unwrap().len() < count {
            host.pump(private_clock(host)).unwrap();
            assert!(Instant::now() < deadline, "private prompt was not prepared");
            std::thread::yield_now();
        }
    };
    await_prompt(&mut host, 1);
    assert_eq!(
        speech.opened.lock().unwrap()[0],
        "Enter your mailbox number followed by pound"
    );
    state.lock().unwrap().input.extend(
        "100#"
            .bytes()
            .map(crate::link::session::tests::Input::Digit),
    );
    await_digits(&host, &source, 4);
    host.pump(private_clock(&host)).unwrap();
    await_prompt(&mut host, 2);
    assert_eq!(
        speech.opened.lock().unwrap()[1],
        "Enter your passcode followed by pound"
    );
    assert!(!reply.phone_authenticated());
    std::thread::sleep(Duration::from_millis(3010));
    host.pump(private_clock(&host)).unwrap();
    assert_eq!(
        speech.opened.lock().unwrap().len(),
        2,
        "field transition must not time out while hearing the password prompt"
    );
    state.lock().unwrap().input.extend(
        "1234#"
            .bytes()
            .map(crate::link::session::tests::Input::Digit),
    );
    await_digits(&host, &source, 5);
    host.pump(private_clock(&host)).unwrap();
    await_prompt(&mut host, 3);
    assert!(reply.phone_authenticated());
    assert_eq!(
        speech.opened.lock().unwrap().as_slice(),
        [
            "Enter your mailbox number followed by pound",
            "Enter your passcode followed by pound",
            "Mailbox authenticated. Protected commands still require node unlock",
        ]
    );
    assert!(host.peers.is_empty() && host.operations.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    {
        let state = state.lock().unwrap();
        assert!(state.texts.is_empty() && state.digits.is_empty());
    }
    assert!(host.stop(private_clock(&host).now_ms + 1));
}

#[test]
fn decoded_private_call_digits_authenticate_on_the_serialized_host_without_rf() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    state.lock().unwrap().input.extend(
        "100#1234#"
            .bytes()
            .map(crate::link::session::tests::Input::Digit),
    );
    pump_until(&mut host, |_| reply.phone_authenticated());
    assert_eq!(
        host.phone_mailbox(&source, &reply).unwrap().callsign,
        "KG0BP"
    );
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().digits.is_empty());
    assert!(host.stop(20));
}

#[test]
fn actual_private_digits_select_the_bound_numeric_or_named_local_with_star_four_pound() {
    for local in ["1000", "local-repeater"] {
        let _serial = crate::fixture::LIFECYCLE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let configuration = ConfigDocument::parse(&format!(
            "[{local}]\nradio_channel=usb\nduplex=full\n{}",
            mailbox_configuration(local)
        ))
        .unwrap();
        let mut host =
            Host::start(configuration, media(), services(), Instant::now(), clock()).unwrap();
        let (phone, state) = crate::link::session::tests::phone_peer_named(local);
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        state.lock().unwrap().input.extend(
            "100#1234#*4#*70"
                .bytes()
                .map(crate::link::session::tests::Input::Digit),
        );
        pump_until(&mut host, |host| !host.operations.is_empty());
        let [(target, DigitOutcome::Operation(operation))] = host.operations.as_slice() else {
            panic!("one private status operation")
        };
        assert_eq!(target, local);
        assert_eq!(operation.reply, Some(reply.clone()));
        let operation = operation.clone();
        host.operations.clear();
        let effect = host
            .runtime
            .command(local, operation, clock(), false)
            .unwrap();
        host.immediate(local, effect, clock()).unwrap();
        assert!(reply.phone_authenticated());
        assert!(host.peers.is_empty());
        assert!(state.lock().unwrap().bound_radio.is_none());
        assert!(host.stop(20));
    }
}

#[test]
fn private_owner_preparation_failure_reclaims_only_the_new_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = Host::start(
        document(
            "usb",
            &format!("link_command_status=91\n{}", account_configuration("1000")),
        ),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let (phone, state) = crate::link::session::tests::phone_peer();
    assert_eq!(
        host.incoming_peer(phone, "203.0.113.9", clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(host.phone_calls.is_empty() && host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.stop(20));
}

#[test]
fn private_completion_rejects_foreign_or_unauthenticated_tokens_and_closes_only_its_full_sink() {
    use rpt_advanced_core::{
        audio::{PcmRead, PcmStreamReader},
        link::PeerSignals,
        phone_dtmf::PhoneDtmfAction,
    };
    struct Empty;
    impl PcmStreamReader for Empty {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _voice) = driven_mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
        .unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(host.current_private_phone("1000", &reply).is_err());
    assert!(host.private_phone_failure(&reply).is_err());
    assert!(
        host.node_operation_result(
            "1000",
            Some(&reply),
            rpt_advanced_core::messages::OperationResult::Completed
        )
        .is_err()
    );
    assert!(
        host.private_phone_failure(&PeerSignals::new().reply_target())
            .is_err()
    );
    assert!(host.phone_source(call).is_some());
    host.authenticate_phone(&source, &reply, "100", "1234", private_clock(&host))
        .unwrap();
    assert!(host.current_private_phone("9999", &reply).is_err());
    let foreign = PeerSignals::for_phone(source.clone());
    assert!(foreign.authenticate_phone());
    assert!(host.private_phone_failure(&foreign.reply_target()).is_err());
    assert!(
        host.node_operation_result(
            "1000",
            Some(&foreign.reply_target()),
            rpt_advanced_core::messages::OperationResult::Unavailable
        )
        .is_err()
    );
    assert!(
        !probe_public_phone_media(&mut host, "1000"),
        "unauthenticated or foreign sources cannot enqueue public results"
    );
    assert!(
        reply.phone_authenticated(),
        "foreign failure cannot retire the actual same-ID call"
    );
    while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
    host.private_phone_actions(
        &source,
        &reply,
        vec![PhoneDtmfAction::NodeDigit {
            target: "9999".into(),
            digit: '*',
        }],
        private_clock(&host),
    )
    .unwrap();
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
    for _ in 0..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Empty))
            .unwrap();
    }
    assert!(host.private_phone_failure(&reply).is_err());
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.stop(20));
}

#[test]
fn private_grammar_dispatches_only_the_bound_node_and_reports_unavailable_media() {
    use rpt_advanced_core::phone_dtmf::PhoneDtmfAction as Action;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
    for digit in "*41000#*70".chars() {
        let actions = host.phone_calls.actions(&source, &reply, digit, 0).unwrap();
        host.private_phone_actions(&source, &reply, actions, clock())
            .unwrap();
    }
    let [(local, DigitOutcome::Operation(operation))] = host.operations.as_slice() else {
        panic!("one source-tagged current-local command")
    };
    assert_eq!(local, "1000");
    assert_eq!(operation.reply, Some(reply.clone()));
    assert_eq!(
        operation.origin.as_deref(),
        Some(source.call_id().to_string().as_str())
    );
    let operation = operation.clone();
    host.operations.clear();
    let effect = host
        .runtime
        .command("1000", operation, clock(), false)
        .unwrap();
    host.immediate("1000", effect, clock()).unwrap();
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
    for action in [Action::ExitCommandMode, Action::Attach(None)] {
        let unavailable = !matches!(action, Action::ExitCommandMode);
        host.private_phone_actions(&source, &reply, vec![action], clock())
            .unwrap();
        assert_eq!(
            host.take_phone_reply(&source, &reply).unwrap().is_some(),
            unavailable
        );
    }
    assert!(host.operations.is_empty() && host.peers.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    {
        let state = state.lock().unwrap();
        assert!(state.texts.is_empty() && state.digits.is_empty());
    }
    host.retire_phone(&source);
    assert!(
        host.private_phone_actions(&source, &reply, vec![Action::Attach(None)], clock())
            .is_err()
    );
    assert!(host.stop(20));

    // Each independently prepared call keeps this test inside the real producer's
    // four-job bound; cancellation is asynchronous and must not be bypassed.
    for route in ["*49999#", "*41000#*91", "*41000#*42000**#"] {
        let mut host = mailbox_host();
        let (phone, state) = crate::link::session::tests::phone_peer();
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
        for digit in route.chars() {
            let actions = host.phone_calls.actions(&source, &reply, digit, 0).unwrap();
            host.private_phone_actions(&source, &reply, actions, clock())
                .unwrap();
        }
        assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
        if route == "*49999#" {
            assert!(
                host.phone_calls
                    .actions(&source, &reply, '5', 0)
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(host.operations.is_empty() && host.peers.is_empty());
        assert!(state.lock().unwrap().bound_radio.is_none());
        assert!(host.stop(20));
    }
}

#[test]
fn private_worker_login_failure_and_provider_hangup_revoke_only_that_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, speech) = driven_mailbox_host();
    for input in ["100#9999#100#9999#100#9999#", "*41000#*70#*41000#"] {
        let (phone, state) = crate::link::session::tests::phone_peer();
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        // Exercise each actual field/prompt in sequence instead of filling the
        // four-entry station preparation queue with several logins at once.
        for field in input.split_inclusive('#') {
            let before = speech.opened.lock().unwrap().len();
            state
                .lock()
                .unwrap()
                .input
                .extend(field.bytes().map(crate::link::session::tests::Input::Digit));
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                host.pump(private_clock(&host)).unwrap();
                if reply.ended() || speech.opened.lock().unwrap().len() > before {
                    break;
                }
                assert!(Instant::now() < deadline, "private login response missing");
                std::thread::yield_now();
            }
        }
        pump_until(&mut host, |host| host.phone_source(call).is_none());
        assert!(reply.ended() && !reply.phone_authenticated());
        await_private_drop(&state);
        assert!(host.operations.is_empty());
    }
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    state.lock().unwrap().fail_ready = true;
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    await_private_drop(&state);
    assert!(host.stop(20));
}

#[test]
fn provider_admission_prepares_an_unauthenticated_private_login_prompt() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(!reply.phone_authenticated());
    assert!(
        host.take_phone_reply(&source, &reply).unwrap().is_some(),
        "login must prepare privately before any command or attachment"
    );
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(host.stop(20));
}

#[test]
fn real_mailbox_verification_keeps_prompts_private_and_does_not_unlock_node_commands() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "9999", clock())
            .unwrap(),
        PhoneLogin::Retry
    );
    assert!(!reply.phone_authenticated());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap(),
        PhoneLogin::Authenticated
    );
    assert_eq!(
        host.phone_mailbox(&source, &reply).unwrap().callsign,
        "KG0BP"
    );
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    let mut prompts = 0;
    while let Some(prepared) = host.take_phone_reply(&source, &reply).unwrap() {
        assert_eq!(prepared.source(), &source);
        assert!(prepared.current());
        prompts += 1;
    }
    assert_eq!(
        prompts, 3,
        "login, generic retry and success all use the exact call outbox"
    );
    let links = host.runtime.node("1000").unwrap().links();
    assert!(
        "*804#"
            .chars()
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone(), 3000))
            .next()
            .is_none(),
        "mailbox login never grants node unlock"
    );
    assert!(
        "*70#"
            .chars()
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone(), 3000))
            .any(|outcome| matches!(outcome, DigitOutcome::Operation(_))),
        "unprotected commands are available after login"
    );
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    host.reload(document("usb", ""), clock()).unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn exhausted_or_abandoned_login_retires_the_actual_provider_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for expected in [PhoneLogin::Retry, PhoneLogin::Retry, PhoneLogin::Closed] {
        assert_eq!(
            host.authenticate_phone(&source, &reply, "unknown", "1234", clock())
                .unwrap(),
            expected
        );
    }
    assert!(reply.ended() && host.phone_source(call).is_none());
    await_private_drop(&state);
    assert_eq!(state.lock().unwrap().drops, 1);
    let (phone, fresh_state) = crate::link::session::tests::phone_peer();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(source.ingress(), fresh.ingress());
    let new_reply = host.phone_reply(&fresh).unwrap();
    let mut expired = clock();
    expired.now_ms = 60_010;
    host.pump(expired).unwrap();
    assert!(new_reply.ended() && host.phone_source(call).is_none());
    await_private_drop(&fresh_state);
    assert_eq!(fresh_state.lock().unwrap().drops, 1);
    assert!(host.stop(60_020));
}

#[test]
fn private_login_preparation_rejects_node_and_retired_tokens_without_rf_fallback() {
    use rpt_advanced_core::{link::PeerSignals, messages::PhoneAuthenticationPrompt as Prompt};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let regular = PeerSignals::default().reply_target();
    assert!(
        host.runtime
            .prepare_private_phone_prompt("1000", Prompt::Login, &regular)
            .is_err()
    );
    assert!(
        host.runtime
            .prepare_phone_status("1000", LinkAction::Status, None, clock(), &regular)
            .is_err()
    );
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(matches!(
        host.runtime
            .prepare_private_phone_prompt("missing", Prompt::Login, &reply),
        Err(RuntimeError::MissingNode)
    ));
    assert!(host.retire_phone(&source));
    assert!(
        host.runtime
            .prepare_private_phone_prompt("1000", Prompt::Login, &reply)
            .is_err()
    );
    assert!(
        host.prepare_login_prompt(&source, &reply, Prompt::Login)
            .is_err()
    );
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(reply)),
            clock()
        )
        .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn failed_private_success_prompt_retires_login_authority_and_transport() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Empty;
    impl PcmStreamReader for Empty {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for _ in 1..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Empty))
            .unwrap();
    }
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    await_private_drop(&state);
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.stop(20));
}

#[test]
fn incoming_prompt_backpressure_reclaims_the_provider_call_without_attachment() {
    use crate::media::SpeechDescriptor;
    use rpt_advanced_core::messages::PhoneAuthenticationPrompt as Prompt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    struct Gate {
        entered: mpsc::SyncSender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
        once: AtomicBool,
    }
    static GATE: Mutex<Option<Arc<Gate>>> = Mutex::new(None);
    struct Resume(Option<mpsc::SyncSender<()>>);
    impl Drop for Resume {
        fn drop(&mut self) {
            if let Some(resume) = self.0.take() {
                let _ = resume.try_send(());
            }
        }
    }
    unsafe extern "C" fn open(
        _: *const c_void,
        _: *const abi::rptadv_media_speech_request,
        _: *const abi::rptadv_media_cancellation,
        output: *mut abi::rptadv_media_stream,
    ) -> i32 {
        // SAFETY: the media owner lends its initialized writable stream record.
        unsafe {
            *output = abi::rptadv_media_stream {
                handle: ptr::null_mut(),
                sample_rate_hz: 0,
            }
        };
        let gate = GATE.lock().unwrap().as_ref().unwrap().clone();
        if !gate.once.swap(true, Ordering::AcqRel) {
            let _ = gate.entered.try_send(());
            let _ = gate
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }
        -6
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let (resume_tx, resume) = mpsc::sync_channel(1);
    *GATE.lock().unwrap() = Some(Arc::new(Gate {
        entered: entered_tx,
        resume: Mutex::new(resume),
        once: AtomicBool::new(false),
    }));
    // SAFETY: the real complete speech table is copied; only its replaceable
    // open boundary is controlled, with context destruction still delegated.
    let mut speech = unsafe {
        rptadv_speech_adapter::rptadv_speech_adapter_descriptor()
            .cast::<SpeechDescriptor>()
            .read()
    };
    speech.open_speech = Some(open);
    // SAFETY: both process-lifetime descriptor tables and reaper callbacks outlive Host.
    let media = unsafe {
        NativeMediaPreparer::from_descriptors(
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            Box::leak(Box::new(speech)),
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            1000,
            (reaper, reaper),
        )
    }
    .unwrap();
    let mut host = Host::start(
        document("usb", &mailbox_configuration("1000")),
        media,
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    // Declared after Host: unwind releases the provider before Host joins it.
    let resume = Resume(Some(resume_tx));
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let mut initial = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    initial.render(false, &mut [0.0; 64]);
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("actual speech provider entered");
    let mut pending = Vec::new();
    for _ in 0..4 {
        pending.push(
            host.runtime
                .prepare_private_phone_prompt("1000", Prompt::Login, &reply)
                .unwrap(),
        );
    }
    assert!(host.retire_phone(&source));
    let (phone, rejected) = crate::link::session::tests::phone_peer();
    assert_eq!(
        host.incoming_peer(phone, "203.0.113.9", clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(host.phone_source(call).is_none());
    assert_eq!(rejected.lock().unwrap().drops, 1);
    assert!(host.peers.is_empty());
    drop(pending);
    drop(initial);
    drop(resume);
    assert!(host.stop(20));
    drop(host);
    *GATE.lock().unwrap() = None;
}

#[test]
fn pending_phone_has_no_handshake_topology_or_radio_attachment_and_queues_only_private_status() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .links()
            .manager()
            .snapshot()
            .is_empty()
    );
    let source = host.phone_source(call).unwrap().clone();
    assert_eq!(source.account_generation(), 1);
    assert_eq!(source.call_generation(), 2);
    assert_ne!(source.ingress(), 0);
    let reply = host.phone_reply(&source).unwrap();
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
            clock()
        )
        .is_err()
    );
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap(),
        PhoneLogin::Authenticated
    );
    while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
    host.immediate(
        "1000",
        LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
        clock(),
    )
    .unwrap();
    let prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(prepared.source(), &source);
    assert!(prepared.current());
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Message {
                selector: "1".into(),
                reply: Some(reply.clone())
            },
            clock()
        )
        .is_err(),
        "unimplemented private message preparation must fail explicitly"
    );
    struct TaggedReply(usize, Arc<Mutex<Vec<usize>>>);
    impl Drop for TaggedReply {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }
    impl rpt_advanced_core::audio::PcmStreamReader for TaggedReply {
        fn render(&mut self, _: &mut [f32]) -> rpt_advanced_core::audio::PcmRead {
            rpt_advanced_core::audio::PcmRead::Finished
        }
    }
    // Remove the prior report, then fill every slot with an identifiable owner.
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
    let released = Arc::new(Mutex::new(Vec::new()));
    for id in 0..16 {
        host.queue_phone_reply(
            &source,
            &reply,
            Box::new(TaggedReply(id, Arc::clone(&released))),
        )
        .unwrap();
    }
    assert_eq!(
        host.immediate(
            "1000",
            LinkEffect::Message {
                selector: "1".into(),
                reply: Some(reply.clone())
            },
            clock()
        ),
        Err(RuntimeError::Rejected)
    );
    assert!(
        released.lock().unwrap().is_empty(),
        "failed enqueue must not replace any owner"
    );
    for id in 0..16 {
        let queued = host.take_phone_reply(&source, &reply).unwrap().unwrap();
        assert_eq!(queued.source(), &source);
        drop(queued);
        assert_eq!(
            *released.lock().unwrap(),
            (0..=id).collect::<Vec<_>>(),
            "queued owner identity/order changed"
        );
    }
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    let (other, rejected) = crate::link::session::tests::phone_peer();
    assert!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, other, clock())
            .is_err()
    );
    assert!(rejected.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(reply.ended());
    assert!(!prepared.current());
    assert!(host.phone_source(call).is_none());
    assert!(!host.retire_phone(&source));
    await_private_drop(&state);
    let state = state.lock().unwrap();
    assert!(
        state.texts.is_empty(),
        "no ASL protocol text may reach a phone"
    );
    assert!(state.audio.is_empty());
    assert!(state.bound_radio.is_none());
    assert_eq!(state.drops, 1);
    drop(state);
    assert!(host.stop(20));
}

#[test]
fn phone_reload_and_unload_retire_the_actual_call_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let next = document(
        "usb",
        &format!("ctcss_encode=no\n{}", account_configuration("1000")),
    );
    let mut acquisition = Some(
        crate::site::acquisition::Acquisition::prepare_reload(
            &Default::default(),
            Arc::new(crate::site::backends::SiteBackends::load(&next).unwrap()),
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
    assert_eq!(host.site_barrier(clock().now_ms), Ok(true));
    // The ownership-boundary assertion must observe retirement, not just its
    // eventual completion after new site bindings have been installed.
    assert!(!host.phone_calls.is_empty());
    host.reload(next, clock()).unwrap();
    assert!(host.site_transition.is_none());
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    let (phone, _) = crate::link::session::tests::phone_peer();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(fresh.ingress(), source.ingress());
    let reply = host.phone_reply(&fresh).unwrap();
    assert!(host.stop(20));
    assert!(reply.ended());
}

#[test]
fn private_prompt_queue_requires_the_exact_live_token_and_never_uses_radio_media() {
    struct Prompt;
    impl rpt_advanced_core::audio::PcmStreamReader for Prompt {
        fn render(&mut self, output: &mut [f32]) -> rpt_advanced_core::audio::PcmRead {
            let count = output.len().min(8);
            output[..count].fill(0.25);
            rpt_advanced_core::audio::PcmRead::FinalSamples(count)
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let foreign = rpt_advanced_core::link::PeerSignals::for_phone(source.clone()).reply_target();
    assert!(matches!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(foreign.clone())),
            clock()
        ),
        Err(RuntimeError::Rejected)
    ));
    assert!(
        host.queue_phone_reply(&source, &foreign, Box::new(Prompt))
            .is_err()
    );
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    for _ in 0..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .unwrap();
    }
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    let mut prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    let mut output = [0.0; 16];
    assert_eq!(prepared.render(false, &mut output), 8);
    assert_eq!(&output[..8], &[0.25; 8]);
    assert_eq!(&output[8..], &[0.0; 8]);
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(!prepared.current());
    assert!(host.take_phone_reply(&source, &reply).is_err());
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn removed_node_reload_retires_its_pending_private_call_before_reclaiming_the_node() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(host.runtime.node("1000").is_none());
    assert!(reply.ended());
    assert!(host.phone_source(call).is_none());
    await_private_drop(&state);
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    pump_until(&mut host, |host| host.runtime.status(20).is_empty());
    assert!(host.stop(20));
}

#[test]
fn rejected_text_start_preserves_live_calls_and_text_executor() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for failed_worker in 0..2 {
        let (mut host, speech) = driven_mailbox_host();
        let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
        let (phone, _) = crate::link::session::tests::phone_peer();
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "127.0.0.1", private_clock(&host))
            .unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        let revision = host.runtime.revision();
        let active = host.runtime.status(0)[0].1.active;
        let configuration = host.runtime.document().clone();
        let outcome = crate::media::text::tests::failing_start(failed_worker, || {
            host.reload(configuration.clone(), clock())
        });
        assert_eq!(outcome, Err(RuntimeError::Preparation));
        assert_eq!(host.runtime.revision(), revision);
        assert_eq!(host.runtime.status(0)[0].1.active, active);
        assert_eq!(host.runtime.document(), &configuration);
        assert!(!host.runtime.reload_pending());
        assert_eq!(host.phone_source(call), Some(&source));
        assert!(!reply.ended());
        ingress
            .lock()
            .unwrap()
            .input
            .push_back(Input::Text(b"M 4000 0 B still live"));
        pump_until(&mut host, |_| {
            speech
                .opened
                .lock()
                .unwrap()
                .iter()
                .any(|text| text.contains("still live"))
        });
        assert!(host.stop(20));
    }
}

/// Install a real already-authenticated call fixture without a second source owner.
pub(crate) fn install_controlled_source(host: &mut Host, calls: crate::phone::PhoneCalls) {
    assert!(host.phone_calls.is_empty());
    host.phone_calls = calls;
}

#[path = "host_phone_attachment_policy_tests.rs"]
mod attachment_policy;

#[path = "host_phone_site_tests.rs"]
mod site_reports;

/// Generic node outcomes stay private to their exact authenticated caller.
#[test]
fn node_operation_results_reach_exact_phone_without_local_rf() {
    use rpt_advanced_core::phone_dtmf::PhoneDtmfAction;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for completed in [true, false] {
        let (mut host, speech) = driven_mailbox_host();
        let (phone, state) = crate::link::session::tests::phone_peer();
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", private_clock(&host))
            .unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        host.authenticate_phone(&source, &reply, "100", "1234", private_clock(&host))
            .unwrap();
        while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
        assert!(
            !probe_public_phone_media(&mut host, "1000"),
            "authentication stays private"
        );
        if completed {
            host.immediate(
                "1000",
                LinkEffect::PrivateDetach {
                    remotes: Vec::new(),
                    reply: reply.clone(),
                },
                private_clock(&host),
            )
            .unwrap();
        } else {
            host.private_phone_actions(
                &source,
                &reply,
                vec![PhoneDtmfAction::SelectTarget("unsupported".into())],
                private_clock(&host),
            )
            .unwrap();
        }
        attachment_policy::finish_prompt(&mut host, &source, &reply);
        assert!(
            !probe_public_phone_media(&mut host, "1000"),
            "phone completion/unavailability must not reach local RF"
        );
        let expected = if completed {
            "Command completed"
        } else {
            "Requested operation is unavailable"
        };
        assert!(
            speech
                .opened
                .lock()
                .unwrap()
                .iter()
                .any(|text| text == expected)
        );
        assert!(state.lock().unwrap().texts.is_empty());
        assert!(state.lock().unwrap().digits.is_empty());
        let foreign =
            rpt_advanced_core::link::PeerSignals::for_phone(source.clone()).reply_target();
        assert!(
            host.node_operation_result(
                "1000",
                Some(&foreign),
                rpt_advanced_core::messages::OperationResult::Completed,
            )
            .is_err()
        );
        assert!(!probe_public_phone_media(&mut host, "1000"));
        assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
        assert!(host.retire_phone(&source));
        assert!(
            host.node_operation_result(
                "1000",
                Some(&reply),
                rpt_advanced_core::messages::OperationResult::Completed,
            )
            .is_err()
        );
        assert!(!probe_public_phone_media(&mut host, "1000"));
        assert!(host.stop(private_clock(&host).now_ms + 1));
    }
}

#[test]
fn account_interdigit_timing_reaches_private_login_and_command_grammar() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = Host::start(
        document(
            "usb",
            &format!(
                "{}inter_digit_timeout_ms=15000\n",
                mailbox_configuration("1000")
            ),
        ),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert_eq!(
        host.phone_calls
            .inter_digit_timeout_ms(&source, &reply)
            .unwrap(),
        15000
    );
    host.phone_calls
        .login_digit(&source, &reply, '7', 0)
        .unwrap();
    assert!(host.phone_calls.login_timeouts(14999).is_empty());
    assert_eq!(host.phone_calls.login_timeouts(15000).len(), 1);
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    for (index, digit) in "*312".chars().enumerate() {
        assert!(
            host.phone_calls
                .actions(&source, &reply, digit, index as u64 * 11000)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        host.phone_calls
            .actions(&source, &reply, '#', 44000)
            .unwrap(),
        [rpt_advanced_core::phone_dtmf::PhoneDtmfAction::Attach(
            Some("12".into())
        )]
    );
    assert!(!reply.ended());
    assert!(host.stop(20));
}

#[test]
fn outgoing_isolation_admits_pending_ordinary_publication_without_releasing_claim() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = mailbox_host();
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("pending ordinary dial")
    };
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    let claim = host.phone_calls.reserve_outgoing(account, "1000").unwrap();
    assert!(
        host.runtime
            .finish_connect("1000", attempt, true, clock())
            .unwrap()
    );
    let (io, state) = peer(48000);
    host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
        .expect("ordinary control transport must remain available during a call");
    assert_eq!(host.peers.len(), 1);
    assert!(state.lock().unwrap().bound_radio.is_some());
    assert_eq!(state.lock().unwrap().drops, 0);
    assert!(claim.current());
    host.reject_peer("1000", "2000", clock().now_ms);
    host.phone_calls
        .cancel_outgoing("1000", claim.id())
        .unwrap();
    let ordinary = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    assert!(ordinary.lock().unwrap().bound_radio.is_some());
    assert!(host.stop(20));
}

#[test]
fn outgoing_isolation_reservation_preserves_existing_ordinary_owners() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let configuration = format!(
        "phone_allow_accounts=provider\n{}\nprotocol=sip\nattach_local_nodes=1000 2000\n[2000]\nradio_channel=second\nduplex=full\nphone_allow_accounts=provider\n",
        account_configuration("1000")
    );
    let mut host = Host::start(
        document("usb", &configuration),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    let _ordinary = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let first = host
        .reserve_outgoing_phone(account.clone(), "1000")
        .expect("ordinary peers must be isolated, not disconnected or rejected");
    assert!(first.current());
    assert_eq!(host.peers.len(), 1);
    assert_eq!(_ordinary.lock().unwrap().drops, 0);
    let claim = host
        .reserve_outgoing_phone(account.clone(), "2000")
        .expect("eligible explicit local is not restricted to account's incoming default");
    assert_eq!(claim.local(), "2000");
    assert!(claim.current());
    assert!(
        host.reserve_outgoing_phone(account.clone(), "missing")
            .is_err()
    );
    assert!(
        host.reserve_outgoing_phone(
            rpt_advanced_core::link::PhoneAccountId::new("unknown").unwrap(),
            "2000"
        )
        .is_err()
    );
    assert!(host.stop(20));
    assert!(!claim.current());
}

fn outgoing_host() -> Host {
    let configuration = format!(
        "phone_allow_accounts=provider\n{}\nprotocol=sip\nattach_local_nodes=1000\n",
        account_configuration("1000")
    );
    Host::start(
        document("usb", &configuration),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}

#[test]
fn outgoing_actual_host_adopts_media_without_login_or_callee_control() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = outgoing_host();
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    let claim = host.reserve_outgoing_phone(account, "1000").unwrap();
    let (io, state) = crate::link::session::tests::phone_source_peer();
    // The source fixture defaults to one inline asynchronous turn; this case
    // feeds digits after adoption and therefore uses its existing live pump.
    state.lock().unwrap().continuous_async = true;
    let source = host
        .adopt_outgoing_phone(&claim, io, private_clock(&host))
        .expect("answered outgoing call must enter the existing private media owner");
    let reply = host.phone_reply(&source).unwrap();
    assert!(reply.phone_outgoing() && reply.phone_media_authorized());
    assert!(!reply.phone_authenticated() && host.phone_mailbox(&source, &reply).is_none());
    assert_eq!(
        host.phone_calls.attachments("1000"),
        [(source.clone(), reply.clone())]
    );
    state
        .lock()
        .unwrap()
        .input
        .extend("100#1234#*4#*32000**#".bytes().map(Input::Digit));
    await_digits(&host, &source, "100#1234#*4#*32000**#".len());
    host.pump(private_clock(&host)).unwrap();
    assert!(!reply.phone_authenticated() && !reply.ended());
    assert!(host.operations.is_empty() && host.peers.is_empty());
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    let mut late = private_clock(&host);
    late.now_ms = 600_000;
    host.pump(late).unwrap();
    assert!(!reply.ended() && claim.current());
    state.lock().unwrap().source_closed = true;
    pump_until(&mut host, |host| {
        host.phone_source(source.call_id()).is_none()
    });
    assert!(reply.ended() && !claim.current());
    assert!(!host.phone_calls.outgoing_reserved("1000"));
    assert!(host.stop(late.now_ms));
}

#[test]
fn outgoing_pending_claim_survives_failed_preparation_but_not_committed_reload() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = outgoing_host();
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    let claim = host.reserve_outgoing_phone(account, "1000").unwrap();
    let configuration = host.runtime.document().clone();
    assert_eq!(
        crate::media::text::tests::failing_start(0, || host.reload(configuration, clock())),
        Err(RuntimeError::Preparation)
    );
    assert!(claim.current() && host.phone_calls.outgoing_reserved("1000"));
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(!claim.current() && !host.phone_calls.outgoing_reserved("1000"));
    assert!(host.stop(20));
}

/// Read the real serialized reservation without exposing test policy in production.
pub(crate) fn outgoing_reserved(host: &Host, local: &str) -> bool {
    host.phone_calls.outgoing_reserved(local)
}
pub(crate) fn outgoing_media_reply(host: &Host, local: &str) -> Option<PeerReply> {
    host.phone_calls
        .attachments(local)
        .into_iter()
        .next()
        .map(|(_, reply)| reply)
}

pub(crate) fn retain_outgoing_test_retry(host: &mut Host) {
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::PermanentTransceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("permanent intent")
    };
    assert!(
        !host
            .runtime
            .finish_connect("1000", attempt, false, clock())
            .unwrap()
    );
}

#[test]
fn rf_operation_results_do_not_retire_or_privately_prompt_an_outgoing_callee() {
    use rpt_advanced_core::messages::OperationResult;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let speech = Arc::new(crate::media::text::tests::SpeechFixture::default());
    let configuration = format!(
        "phone_allow_accounts=provider\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}\nprotocol=sip\nattach_local_nodes=1000\n",
        account_configuration("1000")
    );
    let mut host = Host::start(
        document("usb", &configuration),
        crate::media::text::tests::preparer(Arc::clone(&speech)),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    let claim = host.reserve_outgoing_phone(account, "1000").unwrap();
    let (io, state) = crate::link::session::tests::phone_source_peer();
    state.lock().unwrap().continuous_async = true;
    let source = host
        .adopt_outgoing_phone(&claim, io, private_clock(&host))
        .unwrap();
    let reply = host.phone_reply(&source).unwrap();
    assert!(reply.phone_media_authorized() && !reply.phone_authenticated());
    for outcome in [OperationResult::Completed, OperationResult::Unavailable] {
        assert!(
            host.node_operation_result("1000", Some(&reply), outcome)
                .is_err()
        );
        host.node_operation_result("1000", None, outcome).unwrap();
        assert!(
            claim.current() && !reply.ended(),
            "RF result must not retire a media-only callee"
        );
        assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
        assert!(
            probe_public_phone_media(&mut host, "1000"),
            "LOCAL RF result still renders"
        );
        assert!(reply.phone_media_authorized() && !reply.phone_authenticated());
    }
    let opened = speech.opened.lock().unwrap();
    assert!(opened.iter().any(|text| text == "Command completed"));
    assert!(
        opened
            .iter()
            .any(|text| text == "Requested operation is unavailable")
    );
    drop(opened);
    host.cancel_outgoing_phone("1000", claim.id()).unwrap();
    assert!(reply.ended() && !claim.current());
    assert!(host.stop(20));
}

/// Inspect exact attachment custody without exposing the private call registry.
pub(crate) fn has_phone_attachment(host: &Host, source: &PhoneSource, reply: &PeerReply) -> bool {
    host.phone_calls
        .attachments("1000")
        .iter()
        .any(|(actual, owner)| actual == source && owner == reply)
}

/// Test-only topology cut using the same production graph replacement.
pub(crate) fn refresh_outgoing_test_graph(
    host: &mut Host,
    clock: RuntimeClock,
) -> Result<(), RuntimeError> {
    host.refresh("1000", clock)?;
    // The finite RF probe deliberately holds these callback owners. A real
    // device adopts asynchronously; perform that exact handoff in this fixture.
    let lease = Arc::clone(
        &host
            .leases
            .iter()
            .find(|(local, _)| local == "1000")
            .unwrap()
            .1,
    );
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let active = host
        .runtime
        .node("1000")
        .unwrap()
        .status(clock.now_ms)
        .active
        .unwrap();
    assert_eq!(owners.0.acquire().unwrap().id(), active);
    assert_eq!(owners.1.acquire().unwrap().id(), active);
    lease.lock().unwrap().owners = Some(owners);
    pump_until(host, |host| {
        host.runtime
            .node("1000")
            .unwrap()
            .status(clock.now_ms)
            .retiring
            .is_none()
    });
    Ok(())
}

#[test]
fn outgoing_isolation_waits_for_the_existing_serial_redirect_ack() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = outgoing_host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let old = host.peers[0].control.inbound_observer();
    let account = rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap();
    // Reserve directly so this test isolates ACK readiness from Host policy admission.
    let claim = host.phone_calls.reserve_outgoing(account, "1000").unwrap();
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(Arc::clone(&pause));
    host.peers[0]
        .control
        .send(PeerCommand::Text {
            text: std::ffi::CString::new("fixture control").unwrap(),
            advisory: true,
        })
        .ok()
        .unwrap();
    pause.wait();
    let before_ack = host.prepare_outgoing_isolation(&claim, clock());
    let replacement_before_ack = host.refresh("1000", clock());
    pause.wait();
    assert_eq!(replacement_before_ack, Err(RuntimeError::Busy));
    assert_eq!(
        before_ack,
        Ok(false),
        "queued redirect is not serial media retirement"
    );
    pump_until(&mut host, |host| {
        host.prepare_outgoing_isolation(&claim, clock()) == Ok(true)
    });
    assert!(old.signals().ended(), "old inbound history must be revoked");
    assert!(!old.same_generation(&host.peers[0].control.inbound_observer()));
    assert_eq!(
        state.lock().unwrap().drops,
        0,
        "control transport remains connected"
    );
    assert!(claim.current());
    host.peers[0]
        .control
        .send(PeerCommand::Text {
            text: std::ffi::CString::new("isolated control").unwrap(),
            advisory: true,
        })
        .ok()
        .unwrap();
    pump_until(&mut host, |_| {
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"isolated control")
    });
    let isolated_input = host.peers[0].control.inbound_observer();
    let lease = Arc::clone(
        &host
            .leases
            .iter()
            .find(|(local, _)| local == "1000")
            .unwrap()
            .1,
    );
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let isolated_generation = host
        .runtime
        .node("1000")
        .unwrap()
        .status(10)
        .active
        .unwrap();
    let held = owners.1.acquire().unwrap();
    host.cancel_outgoing_phone("1000", claim.id()).unwrap();
    host.reconcile_phone_isolation(clock());
    assert_eq!(
        host.runtime.node("1000").unwrap().status(10).active,
        Some(isolated_generation),
        "callback Busy must retain the old isolated graph until safe replacement"
    );
    drop(held);
    host.reconcile_phone_isolation(clock());
    pump_until(&mut host, |host| {
        drop(owners.0.acquire());
        drop(owners.1.acquire());
        let status = host.runtime.node("1000").unwrap().status(10);
        status.active != Some(isolated_generation) && status.retiring.is_none()
    });
    lease.lock().unwrap().owners = Some(owners);
    assert!(
        isolated_input.signals().ended(),
        "release must revoke isolated input history"
    );
    assert!(!isolated_input.same_generation(&host.peers[0].control.inbound_observer()));
    assert_eq!(
        state.lock().unwrap().drops,
        0,
        "ordinary owner survives call release"
    );
    assert!(host.stop(30));
}

#[test]
fn outgoing_isolation_failed_reset_cannot_become_vacuous_after_owner_reaping() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = outgoing_host();
    let LinkEffect::Connect(attempt) = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap()
    else {
        panic!("ordinary dial");
    };
    assert!(
        host.runtime
            .finish_connect("1000", attempt, true, clock())
            .unwrap()
    );
    let (io, state) = crate::link::session::tests::peer_with_egress(48000, true, true);
    state.lock().unwrap().continuous_async = true;
    host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    pump_until(&mut host, |host| {
        named_peer_ready(host, "2000")
            && host
                .runtime
                .node("1000")
                .unwrap()
                .status(clock().now_ms)
                .retiring
                .is_none()
    });
    let claim = host
        .reserve_outgoing_phone(
            rpt_advanced_core::link::PhoneAccountId::new("provider").unwrap(),
            "1000",
        )
        .unwrap();
    state.lock().unwrap().encoder_reset_result = -1;
    let _ = host.prepare_outgoing_isolation(&claim, clock());
    pump_until(&mut host, |host| host.peers.is_empty());
    let readiness = host.prepare_outgoing_isolation(&claim, clock());
    // A same-claim topology refresh must not erase the failed required-owner cut.
    let replacement = host.refresh("1000", clock());
    let after_replacement = host.prepare_outgoing_isolation(&claim, clock());
    // Always shut down before asserting a fixture failure.
    let reset_attempted = state.lock().unwrap().encoder_resets;
    host.cancel_outgoing_phone("1000", claim.id()).unwrap();
    assert!(host.stop(40));
    assert!(reset_attempted != 0);
    assert!(
        readiness.is_err() && after_replacement.is_err(),
        "failed exact cut cannot become ready by pruning/replacing: before={readiness:?}, replacement={replacement:?}, after={after_replacement:?}"
    );
}
