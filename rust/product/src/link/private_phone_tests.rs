use super::*;
use crate::{
    link::session::tests::{Input, phone_peer},
    phone::tests::private_session,
};
use rpt_advanced_core::{
    audio::{PcmRead, PcmStreamReader},
    link::{PeerSignals, PhoneAccountId, PhoneSource},
};

thread_local! {
    static BEFORE_SEND: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn before_send() {
    let callback = BEFORE_SEND.with_borrow_mut(Option::take);
    if let Some(callback) = callback {
        callback();
    }
}

#[test]
fn control_retirement_after_render_prevents_native_egress_and_pending_retry() {
    for blocked in [false, true] {
        let (mut calls, mut session, mut control, state, source, reply) =
            private_session(false, true);
        calls
            .queue_reply(&source, &reply, Box::new(Speech::new(1920)))
            .unwrap();
        control
            .send(calls.take_reply(&source, &reply).unwrap().unwrap())
            .ok()
            .unwrap();
        if blocked {
            state.lock().unwrap().prepared_result = 1;
            session.step(0).unwrap();
        }
        let owner_reply = reply.clone();
        let (retire, retired) = std::sync::mpsc::sync_channel(1);
        let (completed, receipt) = std::sync::mpsc::sync_channel(1);
        let owner = std::thread::spawn(move || {
            retired.recv().unwrap();
            owner_reply.retire();
            completed.send(()).unwrap();
        });
        BEFORE_SEND.with_borrow_mut(|callback| {
            *callback = Some(Box::new(move || {
                retire.send(()).unwrap();
                receipt
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            }))
        });
        assert_eq!(session.step(20), Err(Error::Hangup));
        owner.join().unwrap();
        assert_eq!(state.lock().unwrap().prepared.len(), usize::from(blocked));
        assert!(state.lock().unwrap().audio.is_empty());
    }
}

#[test]
fn requested_registry_stop_wins_a_simultaneous_provider_failure_without_error_logging() {
    let (_calls, mut session, _control, state, _source, reply) = private_session(false, false);
    let pause = Arc::new(std::sync::Barrier::new(2));
    {
        let mut state = state.lock().unwrap();
        state.pause = Some(Arc::clone(&pause));
        state.fail_ready = true;
    }
    let stop = session.stop_flag();
    let owner = std::thread::spawn(move || {
        crate::fixture::ERROR_LOGS.with_borrow_mut(Vec::clear);
        let result = session.step(0);
        let logged = crate::fixture::ERROR_LOGS.with_borrow(|logs| logs.len());
        (session, result, logged)
    });
    pause.wait();
    // This is the registry's existing stop capability. Delay its following reply
    // retirement until the already-admitted failing provider step has returned.
    stop.store(true, Ordering::Release);
    pause.wait();
    let (_session, result, logged) = owner.join().unwrap();
    assert_eq!(result, Err(Error::Hangup));
    assert_eq!(logged, 0);
    assert!(reply.ended());
}

#[test]
fn provider_local_metadata_cannot_put_nul_in_native_error_logging() {
    let (io, state) = crate::link::session::tests::phone_peer_named("local\0extra");
    let (account_generation, call_generation) = io.generations();
    let source = PhoneSource::new(
        io.phone_account().unwrap().clone(),
        account_generation,
        io.identity().call_id().unwrap(),
        call_generation,
        1,
    )
    .unwrap();
    let signals = PeerSignals::for_phone(source);
    let (mut session, _control) = PrivatePhoneSession::prepare(io, signals.reply_target()).unwrap();
    state.lock().unwrap().fail_ready = true;
    crate::fixture::ERROR_LOGS.with_borrow_mut(Vec::clear);
    assert_eq!(session.step(0), Err(Error::Hangup));
    crate::fixture::ERROR_LOGS.with_borrow(|logs| {
        assert_eq!(logs.len(), 1);
        assert!(!logs[0].1.contains('\0'));
        assert!(logs[0].1.contains("local\\u{0}extra"));
    });
}

#[test]
fn private_async_owner_uses_existing_registration_and_quiesces_before_destroy() {
    let (_calls, session, mut control, state, _source, reply) = private_session(true, true);
    state.lock().unwrap().continuous_async = true;
    state.lock().unwrap().input.push_back(Input::Digit(b'7'));
    let epoch = Instant::now() - std::time::Duration::from_millis(37);
    let mut worker = crate::link::session::PeerIoWorker::start(epoch).unwrap();
    worker.attach_private(session).ok().unwrap();
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    let mut digit = None;
    loop {
        digit = digit.or_else(|| control.digit());
        if digit.is_some() && !state.lock().unwrap().deadlines.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let (symbol, received_ms) = digit.unwrap();
    assert_eq!(symbol, '7');
    assert!(received_ms >= 37, "digits retain the registered host epoch");
    assert!(
        state
            .lock()
            .unwrap()
            .deadlines
            .iter()
            .all(|ns| *ns <= 10_000_000),
        "relative wake deadlines subtract elapsed time from that same epoch"
    );
    assert_eq!(state.lock().unwrap().drops, 0);
    drop(control);
    assert!(reply.ended());
    worker.reap();
    worker.stop();
    let state = state.lock().unwrap();
    assert_eq!(
        (
            state.async_attaches,
            state.async_stops,
            state.async_detaches,
            state.drops
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn rejected_private_registration_returns_the_unconsumed_owner() {
    for stopped in [false, true] {
        let (_calls, session, _control, state, _source, reply) = private_session(true, true);
        state.lock().unwrap().fail_async_attach = true;
        let mut worker = crate::link::session::PeerIoWorker::start(Instant::now()).unwrap();
        if stopped {
            worker.stop();
        }
        let session = worker.attach_private(session).err().unwrap();
        assert_eq!(state.lock().unwrap().drops, 0);
        drop(session);
        assert!(reply.ended());
        assert_eq!(state.lock().unwrap().drops, 1);
        worker.stop();
    }
}

struct Speech {
    remaining: usize,
    pending: bool,
    fail: bool,
    retire: Option<PeerReply>,
}
impl Speech {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            pending: false,
            fail: false,
            retire: None,
        }
    }
}
impl PcmStreamReader for Speech {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        if let Some(reply) = self.retire.take() {
            reply.retire();
        }
        if self.fail {
            return PcmRead::Failed;
        }
        if std::mem::take(&mut self.pending) {
            return PcmRead::Pending;
        }
        let count = self.remaining.min(output.len());
        output[..count].fill(0.25);
        self.remaining -= count;
        if self.remaining == 0 {
            PcmRead::FinalSamples(count)
        } else {
            PcmRead::Samples(count)
        }
    }
}

#[test]
fn private_media_reuses_existing_conversion_and_never_echoes_receive_or_asl() {
    for prepared in [false, true] {
        let (mut calls, mut session, mut control, state, source, reply) =
            private_session(false, prepared);
        state.lock().unwrap().input.extend([
            Input::Audio(vec![0.75; 160]),
            Input::Text(b"!NEWKEY!"),
            Input::RadioKey,
            Input::Gap(160),
            Input::RadioUnkey,
            Input::Digit(b'5'),
        ]);
        let mut speech = Speech::new(1920);
        speech.pending = true;
        calls
            .queue_reply(&source, &reply, Box::new(speech))
            .unwrap();
        control
            .send(calls.take_reply(&source, &reply).unwrap().unwrap())
            .ok()
            .unwrap();
        for now_ms in (0..160).step_by(10) {
            session.step(now_ms).unwrap();
        }
        assert_eq!(control.digit(), Some(('5', 0)));
        assert_eq!(control.digit(), None);
        let state = state.lock().unwrap();
        assert!(state.bound_radio.is_none() && state.texts.is_empty() && state.digits.is_empty());
        if prepared {
            assert!(state.audio.is_empty());
            assert!(state.prepared.iter().any(|(bytes, _)| !bytes.is_empty()));
        } else {
            assert!(state.audio.iter().any(|sample| *sample > 0.2));
            assert!(state.audio.iter().all(|sample| *sample < 0.5));
        }
    }
}

#[test]
fn private_digit_queue_retains_the_unread_provider_suffix_and_rejects_bad_reads() {
    let (_calls, mut session, mut control, state, _source, reply) = private_session(false, false);
    state
        .lock()
        .unwrap()
        .input
        .extend((0..80).map(|_| Input::Digit(b'1')));
    session.step(0).unwrap();
    assert_eq!(state.lock().unwrap().input.len(), 16);
    session.step(1).unwrap();
    assert_eq!(state.lock().unwrap().input.len(), 16);
    for _ in 0..64 {
        assert_eq!(control.digit(), Some(('1', 0)));
    }
    session.step(2).unwrap();
    for _ in 0..16 {
        assert_eq!(control.digit(), Some(('1', 2)));
    }
    assert!(state.lock().unwrap().input.is_empty());
    state.lock().unwrap().ready_without_input = true;
    session.step(3).unwrap();
    state.lock().unwrap().duplicate_digit = true;
    state.lock().unwrap().input.push_back(Input::Digit(b'2'));
    assert_eq!(session.step(4), Err(Error::InvalidFrame));
    assert!(reply.ended());
}

#[test]
fn private_failure_or_retirement_cannot_send_pending_or_new_prompt_samples() {
    for failure in [
        "ready",
        "read",
        "write",
        "speech",
        "retire-before",
        "retire-during",
    ] {
        let (mut calls, mut session, mut control, state, source, reply) =
            private_session(false, true);
        let mut speech = Speech::new(1920);
        match failure {
            "ready" => state.lock().unwrap().fail_ready = true,
            "read" => {
                let mut state = state.lock().unwrap();
                state.fail_read = true;
                state.input.push_back(Input::Digit(b'1'));
            }
            "write" => state.lock().unwrap().prepared_result = -1,
            "speech" => speech.fail = true,
            "retire-before" => {}
            "retire-during" => speech.retire = Some(reply.clone()),
            _ => unreachable!(),
        }
        calls
            .queue_reply(&source, &reply, Box::new(speech))
            .unwrap();
        control
            .send(calls.take_reply(&source, &reply).unwrap().unwrap())
            .ok()
            .unwrap();
        if failure == "retire-before" {
            reply.retire();
        }
        assert!(session.step(0).is_err(), "{failure}");
        assert!(reply.ended());
        let attempts = state.lock().unwrap().prepared.len();
        assert_eq!(attempts, usize::from(failure == "write"));
        assert!(session.step(20).is_err());
        assert_eq!(state.lock().unwrap().prepared.len(), attempts);
    }
}

#[test]
fn private_command_queue_is_bounded_and_rejects_other_actual_ingress() {
    let (mut calls, mut session, mut control, _state, source, reply) =
        private_session(false, false);
    for _ in 0..16 {
        calls
            .queue_reply(&source, &reply, Box::new(Speech::new(0)))
            .unwrap();
        control
            .send(calls.take_reply(&source, &reply).unwrap().unwrap())
            .ok()
            .unwrap();
    }
    calls
        .queue_reply(&source, &reply, Box::new(Speech::new(0)))
        .unwrap();
    let pending = control
        .send(calls.take_reply(&source, &reply).unwrap().unwrap())
        .err()
        .unwrap();
    session.step(0).unwrap();
    control.send(pending).ok().unwrap();
    let (mut other, _session, _control, _state, other_source, other_reply) =
        private_session(false, false);
    other
        .queue_reply(&other_source, &other_reply, Box::new(Speech::new(1)))
        .unwrap();
    assert!(
        control
            .send(
                other
                    .take_reply(&other_source, &other_reply)
                    .unwrap()
                    .unwrap()
            )
            .is_err()
    );
    reply.retire();
    calls
        .queue_reply(&source, &reply, Box::new(Speech::new(1)))
        .err()
        .unwrap();
}

#[test]
fn private_prepare_checks_all_provider_identity_fields_and_retirement() {
    for mismatch in [
        "node",
        "call",
        "account",
        "account-generation",
        "call-generation",
        "retired",
    ] {
        let (io, _) = phone_peer();
        let source = PhoneSource::new(
            PhoneAccountId::new(if mismatch == "account" {
                "other"
            } else {
                "provider"
            })
            .unwrap(),
            if mismatch == "account-generation" {
                2
            } else {
                1
            },
            if mismatch == "call" {
                "112233445566778899aabbccddeeff00"
            } else {
                "aabbccddeeff00112233445566778899"
            }
            .parse()
            .unwrap(),
            if mismatch == "call-generation" { 3 } else { 2 },
            1,
        )
        .unwrap();
        let signals = if mismatch == "node" {
            PeerSignals::new()
        } else {
            PeerSignals::for_phone(source)
        };
        let reply = signals.reply_target();
        if mismatch == "retired" {
            reply.retire();
        }
        assert!(
            PrivatePhoneSession::prepare(io, reply).is_err(),
            "{mismatch}"
        );
    }
}

#[test]
fn private_prepared_backpressure_expires_packets_without_advancing_unrendered_speech() {
    let (mut calls, mut session, mut control, state, source, reply) = private_session(false, true);
    state.lock().unwrap().prepared_result = 1;
    calls
        .queue_reply(&source, &reply, Box::new(Speech::new(4800)))
        .unwrap();
    control
        .send(calls.take_reply(&source, &reply).unwrap().unwrap())
        .ok()
        .unwrap();
    session.step(0).unwrap();
    session.step(20).unwrap();
    assert_eq!(session.source_position, 960);
    session.step(200).unwrap();
    state.lock().unwrap().prepared_result = 0;
    for now_ms in (210..450).step_by(10) {
        session.step(now_ms).unwrap();
    }
    assert_eq!(session.source_position, 4800);
    assert!(
        session
            .outbound
            .statistics
            .snapshot()
            .expired_native_samples
            > 0
    );
    assert!(
        state
            .lock()
            .unwrap()
            .prepared
            .iter()
            .any(|(bytes, _)| !bytes.is_empty())
    );
}
