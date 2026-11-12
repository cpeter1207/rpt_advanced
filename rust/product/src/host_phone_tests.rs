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
}
pub(crate) fn before_event() {
    let callback = BEFORE_EVENT.with_borrow_mut(Option::take);
    if let Some(callback) = callback {
        callback();
    }
}

fn driven_mailbox_host() -> (Host, Arc<crate::media::text::tests::SpeechFixture>) {
    let speech = Arc::new(crate::media::text::tests::SpeechFixture::default());
    let configuration = document(
        "usb",
        &format!(
            "[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}",
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
fn real_three_second_login_pause_resets_privately_before_the_next_digit() {
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
        .any(|text| text == "Login unsuccessful. Try again")
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
    assert!(host.private_phone_result(&reply, true).is_err());
    assert!(
        host.private_phone_result(&PeerSignals::new().reply_target(), false)
            .is_err()
    );
    assert!(host.phone_source(call).is_some());
    host.authenticate_phone(&source, &reply, "100", "1234", private_clock(&host))
        .unwrap();
    assert!(host.current_private_phone("9999", &reply).is_err());
    let foreign = PeerSignals::for_phone(source.clone());
    assert!(foreign.authenticate_phone());
    assert!(
        host.private_phone_result(&foreign.reply_target(), false)
            .is_err()
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
    assert!(host.private_phone_result(&reply, false).is_err());
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
    let mut host = mailbox_host();
    for input in ["100#9999#100#9999#100#9999#", "*41000#*70#*41000#"] {
        let (phone, state) = crate::link::session::tests::phone_peer();
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        state
            .lock()
            .unwrap()
            .input
            .extend(input.bytes().map(crate::link::session::tests::Input::Digit));
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
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone()))
            .next()
            .is_none(),
        "mailbox login never grants node unlock"
    );
    assert!(
        "*70#"
            .chars()
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone()))
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
    struct EmptyReply;
    impl rpt_advanced_core::audio::PcmStreamReader for EmptyReply {
        fn render(&mut self, _: &mut [f32]) -> rpt_advanced_core::audio::PcmRead {
            rpt_advanced_core::audio::PcmRead::Finished
        }
    }
    // The first unavailable report is queued. Fill the actual bounded mailbox.
    for _ in 1..16 {
        host.queue_phone_reply(&source, &reply, Box::new(EmptyReply))
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
    let mut retained = 0;
    while host.take_phone_reply(&source, &reply).unwrap().is_some() {
        retained += 1;
    }
    assert_eq!(
        retained, 16,
        "failed unavailable prompt must preserve queued replies"
    );
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
