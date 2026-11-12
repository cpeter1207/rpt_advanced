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

fn source_session_with_auth(
    authenticated: bool,
) -> (
    PrivatePhoneSession,
    PrivatePhoneControl,
    Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    PeerReply,
) {
    source_session_at_rate(8000, authenticated)
}

fn source_session_at_rate(
    rate: u32,
    authenticated: bool,
) -> (
    PrivatePhoneSession,
    PrivatePhoneControl,
    Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    PeerReply,
) {
    source_session_with_peer(
        crate::link::session::tests::phone_source_peer_at_rate(rate),
        authenticated,
    )
}

fn source_session_with_peer(
    (io, state): (
        PeerIo,
        Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    ),
    authenticated: bool,
) -> (
    PrivatePhoneSession,
    PrivatePhoneControl,
    Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    PeerReply,
) {
    let (account, call) = io.generations();
    let source = PhoneSource::new(
        io.phone_account().unwrap().clone(),
        account,
        io.identity().call_id().unwrap(),
        call,
        13,
    )
    .unwrap();
    let signals = PeerSignals::for_phone(source);
    if authenticated {
        assert!(signals.authenticate_phone());
    }
    let reply = signals.reply_target();
    let (session, control) = PrivatePhoneSession::prepare(io, reply.clone()).unwrap();
    (session, control, state, reply)
}

fn source_session() -> (
    PrivatePhoneSession,
    PrivatePhoneControl,
    Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    PeerReply,
) {
    source_session_with_auth(false)
}

#[test]
fn private_source_owner_reads_digits_once_without_legacy_polling_or_rf() {
    let (mut session, mut control, state, reply) = source_session();
    let symbols: Vec<_> = "0123456789ABCD*#".bytes().cycle().take(65).collect();
    state
        .lock()
        .unwrap()
        .input
        .extend(symbols.iter().copied().map(Input::Digit));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.75; 160]));
    // Source fixtures deliberately reject legacy readiness. No second reader is allowed.
    session.step(10).unwrap();
    assert_eq!(control.pending_digits(), 64);
    assert_eq!(state.lock().unwrap().input.len(), 2);
    session.step(11).unwrap();
    assert_eq!(control.pending_digits(), 64);
    assert_eq!(
        state.lock().unwrap().input.len(),
        2,
        "full native queue retains the next event"
    );
    for symbol in &symbols[..64] {
        assert_eq!(control.digit(), Some((char::from(*symbol), 10)));
    }
    session.step(20).unwrap();
    assert_eq!(control.digit(), Some((char::from(symbols[64]), 20)));
    assert!(control.digit().is_none());
    assert!(state.lock().unwrap().input.is_empty());
    assert_eq!(state.lock().unwrap().legacy_reads, 0);
    assert!(state.lock().unwrap().source_reads > 64);
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    let reads = state.lock().unwrap().source_reads;
    reply.retire();
    assert_eq!(session.step(30), Err(Error::Hangup));
    assert_eq!(state.lock().unwrap().source_reads, reads);
}

#[test]
fn private_source_owner_cancels_discarded_finite_media_and_ignores_public_text() {
    let (mut session, mut control, state, reply) = source_session();
    state.lock().unwrap().source_finish = true;
    state
        .lock()
        .unwrap()
        .source_text
        .push_back((b"caller", b"*41000#*70", 1000));
    let clock = SourceTimeClock::new(Instant::now()).unwrap();
    session.set_source_clock(clock);
    session.step(10).unwrap();
    assert_eq!(
        state.lock().unwrap().source_completions,
        [SourceCompletion::Cancelled as u32]
    );
    assert_eq!(
        state.lock().unwrap().source_reference.unwrap().0,
        clock.reference().host_clock_epoch
    );
    assert!(control.digit().is_none());
    assert!(!reply.ended());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    state.lock().unwrap().source_closed = true;
    assert_eq!(session.step(20), Err(Error::Hangup));
    assert!(reply.ended());
}

#[test]
fn prepared_phone_media_uses_the_shared_ring_with_original_age_and_exact_retirement() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    let clock = SourceTimeClock::new(Instant::now()).unwrap();
    session.set_source_clock(clock);
    let mut input = control.prepare_media(None).unwrap();
    assert!(
        control.prepare_media(None).is_err(),
        "no implicit replacement"
    );
    assert_eq!(input.source(), reply.phone_source().unwrap());
    assert_eq!(input.phone_reply(), Some(&reply));
    assert_eq!(input.available(), 0, "preparation alone cannot admit media");
    session.step(0).unwrap();
    let (epoch, birth) = state.lock().unwrap().source_reference.unwrap();
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.25; 1600]));
    for now in 20..40 {
        session.step(now).unwrap();
        if state.lock().unwrap().input.is_empty() {
            break;
        }
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert!(input.available() >= 1280);
    let mut seen = false;
    for _ in 0..9600 {
        let sample = input.render_timed_sample().unwrap();
        if sample.provenance.as_ref().is_some_and(|value| {
            value.real
                && !value.plc
                && !value.gap
                && !value.zero_extension
                && value.position.is_some_and(|position| !position.negative)
        }) {
            let bound = sample.age_bound.unwrap();
            assert_eq!(bound.host_clock_epoch, epoch);
            assert_eq!(bound.not_after_ns, birth);
            seen |= sample.sample.abs() > 0.1;
        }
    }
    assert!(seen, "real source PCM must traverse the existing ring");
    state.lock().unwrap().input.push_back(Input::Digit(b'5'));
    session.step(40).unwrap();
    assert_eq!(control.digit(), Some(('5', 40)));
    let mut output = [1.0; 960];
    assert_eq!(input.render(&mut output), None);
    assert!(output.iter().all(|sample| *sample == 0.0));
    assert!(input.render_timed_sample().is_none());
    assert!(!reply.ended(), "DTMF retires media, not the logical call");
    let mut replacement = control.prepare_media(None).unwrap();
    session.step(50).unwrap();
    // Original provider-held PCM cannot be re-admitted under a fresh generation.
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.75; 160]));
    session.step(60).unwrap();
    assert_eq!(replacement.available(), 0);
    reply.retire();
    assert!(replacement.render_timed_sample().is_none());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
}

#[test]
fn prepared_phone_media_handles_single_sample_partitions_without_metadata_deadlock() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, _reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let mut input = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .extend((0..1600).map(|_| Input::Audio(vec![0.25])));
    for now in 1..=26 {
        session.step(now).unwrap();
    }
    assert!(
        state.lock().unwrap().input.is_empty(),
        "small offers must not exhaust metadata before priming"
    );
    assert!(input.available() >= 1280);
    let mut output = [0.0_f32; 960];
    assert!(input.render(&mut output).is_some());
    assert!(output.iter().any(|sample| sample.abs() > 0.1));
}

#[test]
fn prepared_phone_media_requires_authentication_source_capability_and_free_control_slot() {
    let (_session, mut control, _state, _reply) = source_session();
    assert!(matches!(control.prepare_media(None), Err(Error::Admission)));
    let (_calls, _session, mut legacy, _state, _source, _reply) = private_session(false, false);
    assert!(matches!(legacy.prepare_media(None), Err(Error::Admission)));
    let (mut session, mut control, _state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    for _ in 0..16 {
        let input = control.prepare_media(None).unwrap();
        drop(input);
    }
    assert!(matches!(
        control.prepare_media(None),
        Err(Error::Allocation)
    ));
    session.step(0).unwrap();
    let input = control.prepare_media(None).unwrap();
    control.retire_media();
    session.step(1).unwrap();
    assert!(
        !input.active(),
        "control revocation precedes pending adoption"
    );
    assert!(!reply.ended());
    assert!(control.prepare_media(None).is_ok());
}

#[test]
fn prepared_phone_media_loss_retires_buffered_pcm_without_ending_private_control() {
    use rpt_advanced_core::link::PeerInput as _;
    for concealed in [false, true] {
        let (mut session, mut control, state, reply) = source_session_with_auth(true);
        session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
        let mut input = control.prepare_media(None).unwrap();
        session.step(0).unwrap();
        let birth = state.lock().unwrap().source_reference.unwrap().1;
        state.lock().unwrap().source_birth = Some(birth);
        state
            .lock()
            .unwrap()
            .input
            .push_back(Input::Audio(vec![0.5; 160]));
        session.step(1).unwrap();
        assert_eq!(input.available(), 160);
        state.lock().unwrap().input.push_back(if concealed {
            Input::Concealed(vec![0.5; 160])
        } else {
            Input::Gap(160)
        });
        session.step(2).unwrap();
        assert!(!input.active());
        assert!(input.render_timed_sample().is_none());
        assert!(!reply.ended());
        state.lock().unwrap().input.push_back(Input::Digit(b'7'));
        session.step(3).unwrap();
        assert_eq!(control.digit(), Some(('7', 3)));
        assert!(state.lock().unwrap().input.is_empty());
    }
}

#[test]
fn prepared_phone_media_reset_retires_the_old_source_range_before_explicit_readmission() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let mut old = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 1600]));
    session.step(1).unwrap();
    assert_eq!(old.available(), 160);
    state.lock().unwrap().source_reset = true;
    session.step(2).unwrap();
    assert!(old.render_timed_sample().is_none());
    assert!(!reply.ended());
    // Reset has no authority to reuse the previous clean source revision.
    state.lock().unwrap().input.push_back(Input::Digit(b'5'));
    session.step(3).unwrap();
    assert_eq!(control.digit(), Some(('5', 3)));
    assert!(control.collection_ended(&reply));
    let fresh = control.prepare_media(None).unwrap();
    session.step(4).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.75; 160]));
    session.step(5).unwrap();
    assert_eq!(fresh.available(), 160, "a reset starts a new source range");
    assert!(!old.active());
}

#[test]
fn prepared_phone_media_missing_birth_retires_only_media_before_later_source_audio() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let input = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 160]));
    session.step(1).unwrap();
    assert_eq!(input.available(), 160);
    state.lock().unwrap().source_birth = None;
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 160]));
    session.step(2).unwrap();
    assert!(
        !input.active(),
        "unknown original receipt cannot extend admission"
    );
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 160]));
    session.step(3).unwrap();
    assert!(
        !reply.ended(),
        "loss of timing cannot disconnect private control"
    );
    assert_eq!(input.available(), 0);
}

/// Native-event mode still detects onset and never dispatches its in-band completion.
fn assert_native_source_onset(rate: u32) {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let mut media = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
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
    for now in 1..10 {
        session.step(now).unwrap();
    }
    assert!(
        !media.active(),
        "onset must revoke buffered phone media before any completed digit"
    );
    assert_eq!(media.render_timed_sample(), None);
    assert!(
        control.digit().is_none(),
        "NativeEvents does not dispatch in-band digits"
    );
    assert!(
        !reply.ended(),
        "private call control survives media retirement"
    );
    state
        .lock()
        .unwrap()
        .input
        .extend([Input::Audio(vec![0.0; part * 3]), Input::Digit(b'5')]);
    for now in 10..20 {
        session.step(now).unwrap();
    }
    assert_eq!(control.digit().map(|(digit, _)| digit), Some('5'));
    assert!(
        control.digit().is_none(),
        "simultaneous native/tone event is exactly one command"
    );
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert_eq!(state.lock().unwrap().legacy_reads, 0);
}

#[test]
fn private_source_analysis_retires_onset_but_native_digit_remains_the_only_command_8000() {
    assert_native_source_onset(8000);
}

#[test]
fn private_source_analysis_retires_onset_but_native_digit_remains_the_only_command_48000() {
    assert_native_source_onset(48000);
}

#[test]
fn private_source_analysis_rejects_unsupported_detector_rate_without_legacy_regression() {
    let (io, _state) = crate::link::session::tests::phone_source_peer_at_rate(16000);
    let (account, call) = io.generations();
    let source = PhoneSource::new(
        io.phone_account().unwrap().clone(),
        account,
        io.identity().call_id().unwrap(),
        call,
        13,
    )
    .unwrap();
    let signals = PeerSignals::for_phone(source);
    assert!(matches!(
        PrivatePhoneSession::prepare(io, signals.reply_target()),
        Err(Error::UnsupportedFormat)
    ));
    // Legacy command-only readers still retain their established preparation.
    let (_session, _control, _state, _reply) = source_session_with_peer(
        crate::link::session::tests::phone_peer_at_rate(16000),
        false,
    );
}

#[test]
fn private_source_analysis_retry_survives_prompt_overwrite_without_renewing_birth() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let mut media = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let (epoch, birth) = state.lock().unwrap().source_reference.unwrap();
    state.lock().unwrap().source_birth = Some(birth);
    // The real 8 kHz phone ring has 2400 source slots. The next offer accepts one
    // sample, then refuses its retained 159-sample suffix until the consumer runs.
    state.lock().unwrap().input.extend([
        Input::Audio(vec![0.75; 2399]),
        Input::Audio(vec![0.75; 160]),
    ]);
    for now in 1..30 {
        session.step(now).unwrap();
        if state
            .lock()
            .unwrap()
            .source_credits
            .iter()
            .map(|(_, n)| n)
            .sum::<usize>()
            == 2400
        {
            break;
        }
    }
    assert!(state.lock().unwrap().source_credits.contains(&(2399, 1)));
    let prompt = crate::phone::tests::private_prompt(&reply, Box::new(Speech::new(960)));
    control.send(prompt).ok().unwrap();
    session.step(40).unwrap();
    assert_eq!(
        state.lock().unwrap().source_credits.last(),
        Some(&(2400, 0))
    );
    assert_eq!(
        session.pcm, [0.25; 960],
        "real outgoing prompt overwrote the shared scratch"
    );
    assert!(
        matches!(state.lock().unwrap().input.front(), Some(Input::Audio(pcm)) if pcm == &[0.75; 159])
    );
    assert!(control.digit().is_none());
    for _ in 0..4800 {
        media.render_timed_sample().unwrap();
    }
    session.step(41).unwrap();
    assert!(state.lock().unwrap().input.is_empty());
    assert!(state.lock().unwrap().source_credits.contains(&(2400, 159)));
    let mut checked = 0;
    for _ in 0..14000 {
        let value = media.render_timed_sample().unwrap();
        if value.provenance.is_some_and(|p| {
            p.real
                && !p.plc
                && p.position
                    .is_some_and(|at| !at.negative && (2420..2500).contains(&at.frame))
        }) {
            checked += 1;
            assert!(
                value.sample > 0.6,
                "retry must use original caller PCM, not prompt scratch"
            );
            assert_eq!(
                value.age_bound,
                Some(rpt_advanced_core::link::SourceTimeBound {
                    host_clock_epoch: epoch,
                    not_after_ns: birth,
                })
            );
        }
    }
    assert!(
        checked > 0,
        "inspect the actual accepted retry, not earlier queued audio"
    );
    assert!(control.digit().is_none());
    assert_eq!(state.lock().unwrap().legacy_reads, 0);
    assert!(!reply.ended());
}

fn assert_inband_source_queue(rate: u32) {
    let (mut session, mut control, state, _reply) = source_session_with_peer(
        crate::link::session::tests::phone_source_peer_inband(rate),
        false,
    );
    assert_eq!(session.io.phone_digit_mode(), Some(PhoneDigitMode::InBand));
    for _ in 0..64 {
        session
            .digits
            .push(PrivateDigit {
                symbol: '1',
                received_ms: 0,
                revision: NonZeroU32::new(1).unwrap(),
            })
            .unwrap_or_else(|_| unreachable!());
    }
    let part = rate as usize / 50;
    let tone: Vec<_> = (0..part * 4)
        .map(|n| {
            let phase = n as f32 * std::f32::consts::TAU / rate as f32;
            0.25 * ((phase * 770.0).sin() + (phase * 1336.0).sin())
        })
        .collect();
    state.lock().unwrap().input.push_back(Input::Digit(b'9'));
    state
        .lock()
        .unwrap()
        .input
        .extend(tone.chunks(part).map(|pcm| Input::Audio(pcm.to_vec())));
    state
        .lock()
        .unwrap()
        .input
        .extend([Input::Digit(b'5'), Input::Audio(vec![0.0; part * 3])]);
    session.step(1).unwrap();
    assert!(matches!(
        state.lock().unwrap().input.front(),
        Some(Input::Audio(_))
    ));
    assert_eq!(
        control.pending_digits(),
        64,
        "ignored native symbol must not wedge the stream"
    );
    for _ in 0..64 {
        assert_eq!(control.digit(), Some(('1', 0)));
    }
    for now in 2..20 {
        session.step(now).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert_eq!(control.digit().map(|(digit, _)| digit), Some('5'));
    assert!(
        control.digit().is_none(),
        "native event and in-band completion are not double-dispatched"
    );
    assert_eq!(state.lock().unwrap().legacy_reads, 0);
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
}

#[test]
fn private_source_analysis_inband_discards_native_events_even_with_a_full_command_queue_8000() {
    assert_inband_source_queue(8000);
}

#[test]
fn private_source_analysis_inband_discards_native_events_even_with_a_full_command_queue_48000() {
    assert_inband_source_queue(48000);
}

/// Source reset ends analysis without ending this call's authoritative control.
fn assert_source_reset_completion(rate: u32, blocked: bool, inband: bool) {
    let peer = if inband {
        crate::link::session::tests::phone_source_peer_inband(rate)
    } else {
        crate::link::session::tests::phone_source_peer_at_rate(rate)
    };
    let (mut session, mut control, state, reply) = source_session_with_peer(peer, false);
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
    session.step(1).unwrap();
    assert!(state.lock().unwrap().input.is_empty());
    assert_eq!(session.source_end, (part * 4) as u64);
    assert!(control.digit().is_none(), "tone completion remains pending");
    if blocked {
        for _ in 0..64 {
            session
                .digits
                .push(PrivateDigit {
                    symbol: '1',
                    received_ms: 0,
                    revision: NonZeroU32::new(1).unwrap(),
                })
                .unwrap_or_else(|_| unreachable!());
        }
    }
    state.lock().unwrap().source_reset = true;
    session.step(2).unwrap();
    if blocked && inband {
        assert!(
            state.lock().unwrap().source_reset,
            "a full command queue must retain reset for retry"
        );
        assert_eq!(session.source_end, (part * 4) as u64);
        session.step(3).unwrap();
        assert!(state.lock().unwrap().source_reset);
        assert_eq!(control.pending_digits(), 64);
        assert_eq!(control.digit(), Some(('1', 0)));
        session.step(4).unwrap();
        // The one available slot belongs to the pending completion, after the
        // existing commands. A retry must not consume a second slot.
        for _ in 0..63 {
            assert_eq!(control.digit(), Some(('1', 0)));
        }
    } else if blocked {
        for _ in 0..64 {
            assert_eq!(control.digit(), Some(('1', 0)));
        }
    }
    assert!(!state.lock().unwrap().source_reset);
    assert_eq!(session.source_end, 0);
    assert_eq!(
        control.digit(),
        inband.then_some(('5', if blocked { 4 } else { 2 })),
        "acknowledge reset only after its qualified authoritative digit"
    );
    // A repeated reset and subsequent silence cannot duplicate completion.
    state.lock().unwrap().source_reset = true;
    session.step(5).unwrap();
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.0; part * 3]));
    session.step(6).unwrap();
    assert!(control.digit().is_none());
    assert!(
        !reply.ended(),
        "source reset must preserve private call control"
    );
    assert_eq!(state.lock().unwrap().legacy_reads, 0);
}

#[test]
fn private_source_reset_preserves_qualified_inband_digit_8000() {
    assert_source_reset_completion(8000, false, true);
}

#[test]
fn private_source_reset_preserves_qualified_inband_digit_48000() {
    assert_source_reset_completion(48000, false, true);
}

#[test]
fn private_source_reset_retries_full_inband_queue_8000() {
    assert_source_reset_completion(8000, true, true);
}

#[test]
fn private_source_reset_retries_full_inband_queue_48000() {
    assert_source_reset_completion(48000, true, true);
}

#[test]
fn private_source_reset_never_dispatches_non_authoritative_tone() {
    for rate in [8000, 48000] {
        assert_source_reset_completion(rate, true, false);
    }
}

fn assert_private_pound_waits_for_control(rate: u32, inband: bool) {
    let peer = if inband {
        crate::link::session::tests::phone_source_peer_inband(rate)
    } else {
        crate::link::session::tests::phone_source_peer_at_rate(rate)
    };
    let (mut session, mut control, state, reply) = source_session_with_peer(peer, false);
    let part = rate as usize / 50;
    let tone: Vec<_> = (0..part * 4)
        .map(|n| {
            let phase = n as f32 * std::f32::consts::TAU / rate as f32;
            0.25 * ((phase * 941.0).sin() + (phase * 1477.0).sin())
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
    if !inband {
        state.lock().unwrap().input.push_back(Input::Digit(b'#'));
    }
    for now in 1..10 {
        session.step(now).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert_eq!(control.digit().map(|(digit, _)| digit), Some('#'));
    assert!(control.digit().is_none());
    assert!(
        session.source.as_ref().unwrap().muted(),
        "a private pound cannot release collection before the authoritative grammar"
    );
    assert!(!reply.ended());
}

#[test]
fn private_collection_pound_waits_for_authoritative_control_8000() {
    assert_private_pound_waits_for_control(8000, false);
}

#[test]
fn private_collection_pound_waits_for_authoritative_control_48000() {
    assert_private_pound_waits_for_control(48000, false);
}

#[test]
fn private_collection_pound_waits_for_authoritative_inband_control_8000() {
    assert_private_pound_waits_for_control(8000, true);
}

#[test]
fn private_collection_pound_waits_for_authoritative_inband_control_48000() {
    assert_private_pound_waits_for_control(48000, true);
}

/// Read the actual analyzer latch, without changing source ownership or timing.
pub(crate) fn collection_suppressed(session: &PrivatePhoneSession) -> bool {
    session.source.as_ref().is_some_and(SourceIngress::muted)
}

#[test]
fn private_collection_revision_keeps_the_digit_slot_small_and_exhaustion_terminal() {
    assert_eq!(std::mem::size_of::<PrivateDigit>(), 16);
    assert_eq!(std::mem::size_of::<CollectionRevision>(), 4);
    for reset in [false, true] {
        let (mut session, mut control, state, reply) = source_session_with_auth(true);
        session.revision.0.set(NonZeroU32::new(u32::MAX));
        if reset {
            state.lock().unwrap().source_reset = true;
        } else {
            state.lock().unwrap().input.push_back(Input::Digit(b'5'));
        }
        assert!(session.step(1).is_err());
        assert!(reply.ended());
        assert!(control.digit().is_none());
        assert!(session.step(2).is_err());
        assert!(session.revision.0.get().is_none());
    }
}

#[test]
fn private_collection_release_matches_exact_token_and_latest_source_revision() {
    for change in ["none", "digit", "reset", "tone"] {
        let (mut session, mut control, state, reply) = source_session_with_auth(true);
        state.lock().unwrap().input.push_back(Input::Digit(b'5'));
        session.step(1).unwrap();
        assert_eq!(control.digit(), Some(('5', 1)));
        assert!(collection_suppressed(&session));
        let (_, _, _, other) = source_session_with_auth(true);
        assert!(!control.collection_ended(&other));
        assert!(control.collection_ended(&reply));
        match change {
            "digit" => state.lock().unwrap().input.push_back(Input::Digit(b'6')),
            "reset" => {
                state.lock().unwrap().source_reset = true;
                state.lock().unwrap().input.push_back(Input::Digit(b'6'));
            }
            "tone" => {
                let tone: Vec<_> = (0..640)
                    .map(|n| {
                        let phase = n as f32 * std::f32::consts::TAU / 8000.0;
                        0.25 * ((phase * 770.0).sin() + (phase * 1477.0).sin())
                    })
                    .collect();
                state
                    .lock()
                    .unwrap()
                    .input
                    .extend(tone.chunks(160).map(|pcm| Input::Audio(pcm.to_vec())));
            }
            _ => {}
        }
        {
            let mut fixture = state.lock().unwrap();
            fixture.source_credits.reserve(16);
            fixture.source_completions.reserve(64);
            fixture.deadlines.reserve(16);
        }
        let (result, allocations) = crate::fixture::allocations(|| session.step(2));
        result.unwrap();
        assert_eq!(allocations, 0, "{change}");
        assert_eq!(
            collection_suppressed(&session),
            change != "none",
            "{change}"
        );
        assert!(
            session.media.is_none(),
            "release never prepares a media generation"
        );
        assert!(!reply.ended());
    }
}

#[test]
fn private_collection_release_waits_for_the_real_qualified_tone_tail() {
    for rate in [8000, 48000] {
        let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
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
        state.lock().unwrap().input.extend([
            Input::Audio(vec![0.0; rate as usize * 1275 / 100_000]),
            Input::Digit(b'5'),
        ]);
        session.step(1).unwrap();
        assert_eq!(control.digit(), Some(('5', 1)));
        assert!(control.collection_ended(&reply));
        session.step(2).unwrap();
        assert!(
            collection_suppressed(&session),
            "pending quiet tail at {rate}"
        );
        state
            .lock()
            .unwrap()
            .input
            .push_back(Input::Audio(vec![0.0; part * 3]));
        for now in 3..8 {
            session.step(now).unwrap();
        }
        assert!(
            !collection_suppressed(&session),
            "completed quiet tail at {rate}"
        );
        assert!(
            control.digit().is_none(),
            "native mode never duplicates the tone"
        );
    }
}

#[test]
fn private_collection_control_queue_refusal_preserves_the_same_revision() {
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    state.lock().unwrap().input.push_back(Input::Digit(b'5'));
    session.step(1).unwrap();
    control.digit().unwrap();
    for _ in 0..16 {
        assert!(control.collection_ended(&reply));
    }
    assert!(!control.collection_ended(&reply));
    session.step(2).unwrap();
    assert!(control.collection_ended(&reply));
    session.step(3).unwrap();
    assert!(!collection_suppressed(&session));
    reply.retire();
    assert!(!control.collection_ended(&reply));
}

fn assert_fresh_media_rejects_reset_revision(rate: u32) {
    for reset_after_enqueue in [true, false] {
        let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
        session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
        state.lock().unwrap().input.push_back(Input::Digit(b'5'));
        session.step(1).unwrap();
        assert_eq!(control.digit(), Some(('5', 1)));
        assert!(control.collection_ended(&reply));
        session.step(2).unwrap();
        assert!(
            !collection_suppressed(&session),
            "real collection release completed"
        );
        let prepared = if reset_after_enqueue {
            Some(control.prepare_media(None).unwrap())
        } else {
            None
        };
        state.lock().unwrap().source_reset = true;
        session.step(3).unwrap();
        assert!(!state.lock().unwrap().source_reset);
        let candidate = prepared.unwrap_or_else(|| control.prepare_media(None).unwrap());
        session.step(4).unwrap();
        assert!(!reply.ended(), "reset preserves this logical call");
        assert!(state.lock().unwrap().bound_radio.is_none());
        assert!(
            !candidate.active(),
            "an older completed revision cannot authorize media after reset; enqueued_before_reset={reset_after_enqueue}"
        );
    }
}

#[test]
fn prepared_phone_media_rejects_old_completion_after_reset_8000() {
    assert_fresh_media_rejects_reset_revision(8000);
}

#[test]
fn prepared_phone_media_rejects_old_completion_after_reset_48000() {
    assert_fresh_media_rejects_reset_revision(48000);
}

#[test]
fn prepared_phone_media_initial_revision_cannot_be_recreated_after_reset() {
    for rate in [8000, 48000] {
        for reset in [false, true] {
            let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
            session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
            if reset {
                state.lock().unwrap().source_reset = true;
                session.step(0).unwrap();
            }
            let input = control.prepare_media(None).unwrap();
            session.step(1).unwrap();
            assert_eq!(input.active(), !reset);
            assert!(!reply.ended());
        }
    }
}

fn assert_prepared_media_waits_for_provider_suffix(rate: u32) {
    let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let part = rate as usize / 50;
    let mut raw = vec![0.0; part];
    raw.extend((0..part * 3).map(|n| {
        let phase = n as f32 * std::f32::consts::TAU / rate as f32;
        0.25 * ((phase * 770.0).sin() + (phase * 1336.0).sin())
    }));
    assert!(
        raw.len() <= 4096,
        "provider offer respects the source ABI bound"
    );
    let input = control.prepare_media(None).unwrap();
    control
        .send(crate::phone::tests::private_prompt(
            &reply,
            Box::new(Speech::new(960)),
        ))
        .ok()
        .unwrap();
    state.lock().unwrap().input.push_back(Input::Audio(raw));
    session.step(1).unwrap();
    assert_eq!(state.lock().unwrap().source_credits, [(0, part)]);
    assert_eq!(session.pcm, [0.25; 960], "prompt replaced analysis scratch");
    assert!(
        !input.active(),
        "unanalysed provider suffix is not a clean boundary"
    );
    assert!(matches!(
        control.prepare_media(None),
        Err(Error::Reservation)
    ));
    // The later qualified tone must invalidate the waiting candidate before it
    // ever activates, even though the first bounded prefix contained only silence.
    for now in 2..10 {
        session.step(now).unwrap();
        assert!(!input.active());
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert_eq!(
        session.revision.0.get().unwrap().get(),
        2,
        "one actual onset"
    );
    assert!(
        control.digit().is_none(),
        "native mode never invents an in-band command"
    );
    assert!(!reply.ended());
}

#[test]
fn prepared_phone_media_waits_for_unanalysed_provider_suffix_8000() {
    assert_prepared_media_waits_for_provider_suffix(8000);
}

#[test]
fn prepared_phone_media_waits_for_unanalysed_provider_suffix_48000() {
    assert_prepared_media_waits_for_provider_suffix(48000);
}

#[derive(Clone, Copy)]
enum TailCompletion {
    Quiet,
    NewDigit,
    Reset,
}

fn assert_prepared_media_matching_tone_tail(rate: u32, completion: TailCompletion) {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_at_rate(rate, true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
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
    state.lock().unwrap().input.extend([
        Input::Audio(vec![0.0; rate as usize * 1275 / 100_000]),
        Input::Digit(b'5'),
    ]);
    session.step(1).unwrap();
    assert_eq!(control.digit(), Some(('5', 1)));
    assert!(control.collection_ended(&reply));
    let input = control.prepare_media(None).unwrap();
    session.step(2).unwrap();
    assert!(!input.active(), "qualified tail still pending");
    assert!(control.media.as_ref().unwrap().occupied());
    let expected = session.revision.0.get();
    match completion {
        TailCompletion::Quiet => {}
        TailCompletion::NewDigit => state.lock().unwrap().input.push_back(Input::Digit(b'7')),
        TailCompletion::Reset => state.lock().unwrap().source_reset = true,
    }
    if !matches!(completion, TailCompletion::Quiet) {
        session.step(3).unwrap();
        assert_ne!(session.revision.0.get(), expected);
        assert!(!input.active());
        assert!(!control.media.as_ref().unwrap().occupied());
        assert!(!reply.ended());
        return;
    }
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.0; part * 3]));
    session.step(3).unwrap();
    assert!(!input.active());
    session.step(4).unwrap();
    assert!(
        !input.active(),
        "provider still retains an unanalysed suffix"
    );
    session.step(5).unwrap();
    assert!(
        input.active(),
        "actual matching release and drained source qualify one adoption"
    );
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    state.lock().unwrap().source_birth = Some(birth);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.75; part]));
    session.step(6).unwrap();
    assert_eq!(input.available(), part as u64);
    assert!(!reply.ended());
}

#[test]
fn prepared_phone_media_keeps_matching_tone_tail_candidate_8000() {
    assert_prepared_media_matching_tone_tail(8000, TailCompletion::Quiet);
}

#[test]
fn prepared_phone_media_keeps_matching_tone_tail_candidate_48000() {
    assert_prepared_media_matching_tone_tail(48000, TailCompletion::Quiet);
}

#[test]
fn prepared_phone_media_matching_release_cannot_survive_newer_digit_or_reset() {
    for rate in [8000, 48000] {
        for completion in [TailCompletion::NewDigit, TailCompletion::Reset] {
            assert_prepared_media_matching_tone_tail(rate, completion);
        }
    }
}

#[test]
fn prepared_phone_media_rejects_equal_revision_from_another_actual_owner() {
    let (mut session, mut control, _state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let (mut other, mut other_control, _other_state, other_reply) = source_session_with_auth(true);
    assert_eq!(reply.phone_source(), other_reply.phone_source());
    assert_ne!(reply, other_reply);
    let input = other_control.prepare_media(None).unwrap();
    let command = other.replies.pop().ok().unwrap();
    assert!(control.replies.push(command).is_ok());
    session.step(0).unwrap();
    assert!(!input.active());
    assert!(!reply.ended());
    assert!(!other_reply.ended());
}

#[test]
fn prepared_phone_media_cancelled_queue_entry_cannot_replace_newer_candidate() {
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let old = control.prepare_media(None).unwrap();
    let delayed = session.replies.pop().ok().unwrap();
    control.retire_media();
    let fresh = control.prepare_media(None).unwrap();
    assert!(control.replies.push(delayed).is_ok());
    {
        let mut fixture = state.lock().unwrap();
        fixture.deadlines.reserve(16);
        fixture.source_completions.reserve(16);
    }
    let (result, allocations) = crate::fixture::allocations(|| session.step(0));
    result.unwrap();
    assert_eq!(allocations, 0);
    assert!(fresh.active());
    assert!(!old.active());
    assert!(!reply.ended());
}

#[test]
fn prepared_phone_media_owner_layout_has_no_second_candidate_or_pcm_buffer() {
    eprintln!(
        "prepared_owner_bytes session={} control={} command={} digit={}",
        std::mem::size_of::<PrivatePhoneSession>(),
        std::mem::size_of::<PrivatePhoneControl>(),
        std::mem::size_of::<PrivateCommand>(),
        std::mem::size_of::<PrivateDigit>(),
    );
    assert_eq!(std::mem::size_of::<PrivateDigit>(), 16);
    assert_eq!(std::mem::size_of::<SourceIngress<4>>(), 264);
}

#[test]
fn private_source_empty_audio_never_grants_credit_or_ends_control() {
    let (mut session, mut control, state, reply) = source_session();
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(Vec::new()));
    session.step(0).unwrap();
    assert_eq!(state.lock().unwrap().source_credits, [(0, 0)]);
    assert!(state.lock().unwrap().input.is_empty());
    assert!(control.digit().is_none());
    assert!(!reply.ended());
}

#[test]
fn private_source_retry_birth_change_fails_closed_before_new_media_credit() {
    use rpt_advanced_core::link::PeerInput as _;
    let (mut session, mut control, state, reply) = source_session_with_auth(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    let input = control.prepare_media(None).unwrap();
    session.step(0).unwrap();
    let birth = state.lock().unwrap().source_reference.unwrap().1;
    {
        let mut state = state.lock().unwrap();
        state.source_birth = Some(birth);
        state.input.extend([
            Input::Audio(vec![0.25; 2399]),
            Input::Audio(vec![0.25; 160]),
        ]);
    }
    for now in 1..30 {
        session.step(now).unwrap();
        if state.lock().unwrap().source_credits.contains(&(2399, 1)) {
            break;
        }
    }
    assert_eq!(input.available(), 2400);
    assert!(state.lock().unwrap().source_credits.contains(&(2399, 1)));
    // The provider changes original receipt metadata on its real retained suffix.
    // The analyzer must reject that new identity rather than renew source age.
    state.lock().unwrap().source_birth = Some(birth + 1);
    assert_eq!(session.step(40), Err(Error::InvalidFrame));
    assert!(reply.ended());
    assert!(!input.active());
    assert_eq!(input.available(), 0);
    assert_eq!(
        state.lock().unwrap().source_credits.last(),
        Some(&(2400, 0))
    );
    assert!(control.digit().is_none());
}

#[test]
fn private_source_gap_and_finish_complete_only_authoritative_pending_tones() {
    for inband in [false, true] {
        for finish in [false, true] {
            let peer = if inband {
                crate::link::session::tests::phone_source_peer_inband(8000)
            } else {
                crate::link::session::tests::phone_source_peer_at_rate(8000)
            };
            let (mut session, mut control, state, reply) = source_session_with_peer(peer, true);
            session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
            let input = control.prepare_media(None).unwrap();
            session.step(0).unwrap();
            assert!(input.active());
            state
                .lock()
                .unwrap()
                .input
                .push_back(Input::Audio(crate::link::session::tests::five_tone(640)));
            for now in 1..=4 {
                session.step(now).unwrap();
            }
            assert!(control.digit().is_none(), "tone completion remains pending");
            if finish {
                state.lock().unwrap().source_finish = true;
            } else {
                state.lock().unwrap().input.push_back(Input::Gap(160));
            }
            session.step(10).unwrap();
            assert_eq!(control.digit(), inband.then_some(('5', 10)));
            assert!(control.digit().is_none());
            assert!(!input.active());
            assert!(!reply.ended());
            session.step(11).unwrap();
            assert!(control.digit().is_none(), "completion is never replayed");
            assert_eq!(
                state.lock().unwrap().source_completions,
                if finish {
                    vec![SourceCompletion::Cancelled as u32]
                } else {
                    vec![]
                }
            );
        }
    }
}

#[test]
fn private_source_inband_saturation_backpressures_loss_and_finite_completion() {
    for finish in [false, true] {
        let (mut session, mut control, state, reply) = source_session_with_peer(
            crate::link::session::tests::phone_source_peer_inband(8000),
            true,
        );
        // Reach the actual bound through provider callbacks. Short offers need only
        // one reserved digit slot; no synthetic queue/revision state is installed.
        for digit in 0..64 {
            let chunk = if digit < 61 { 160 } else { 1 };
            let tone = crate::link::session::tests::five_tone(640);
            for pcm in tone.chunks(chunk) {
                state
                    .lock()
                    .unwrap()
                    .input
                    .push_back(Input::Audio(pcm.to_vec()));
                session.step(1).unwrap();
                assert!(state.lock().unwrap().input.is_empty());
            }
            for _ in 0..640 {
                if control.pending_digits() == digit + 1 {
                    break;
                }
                state
                    .lock()
                    .unwrap()
                    .input
                    .push_back(Input::Audio(vec![0.0]));
                session.step(1).unwrap();
            }
            assert_eq!(control.pending_digits(), digit + 1);
        }
        if finish {
            state.lock().unwrap().source_finish = true;
        } else {
            state.lock().unwrap().input.push_back(Input::Gap(160));
        }
        for now in 2..=3 {
            session.step(now).unwrap();
            assert_eq!(control.pending_digits(), 64);
            assert!(state.lock().unwrap().source_completions.is_empty());
            if !finish {
                assert!(matches!(
                    state.lock().unwrap().input.front(),
                    Some(Input::Gap(160))
                ));
            }
        }
        assert_eq!(control.digit(), Some(('5', 1)));
        session.step(4).unwrap();
        assert_eq!(
            state.lock().unwrap().source_completions,
            if finish {
                vec![SourceCompletion::Cancelled as u32]
            } else {
                vec![]
            }
        );
        assert!(state.lock().unwrap().input.is_empty());
        for _ in 0..63 {
            assert_eq!(control.digit(), Some(('5', 1)));
        }
        assert!(control.digit().is_none());
        assert!(!reply.ended());
    }
}

#[test]
fn provider_ring_failures_preserve_private_media_boundaries() {
    use rpt_advanced_core::link::{PeerInput as _, SourceTimeBound};
    use std::{
        ffi::{CStr, c_void},
        process::{Child, Command, Output, Stdio},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    const CHILD: &str = "RPTADV_TEST_PHONE_RING_CHILD";
    const TEST: &str =
        "link::private_phone::tests::provider_ring_failures_preserve_private_media_boundaries";
    #[repr(C)]
    #[derive(Debug, Default)]
    struct Counts {
        creates: u64,
        destroys: u64,
        timed_calls: u64,
        real_nonzero_forwarded: u64,
        injected_failures: u64,
    }
    if let Ok(mode) = std::env::var(CHILD) {
        let symbol = |name: &CStr| {
            // SAFETY: this child alone preloads the owned test DSO for its lifetime.
            let pointer = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            assert!(!pointer.is_null(), "missing fault control {name:?}");
            pointer
        };
        // SAFETY: these exact signatures and the C-layout record match the fixture.
        let snapshot = unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut Counts)>(symbol(
                c"rptadv_test_ring_snapshot",
            ))
        };
        // SAFETY: the fixture exports this exact scalar-only control signature.
        let arm = unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u64) -> i32>(symbol(
                c"rptadv_test_ring_arm_after",
            ))
        };
        let counts = || {
            let mut result = Counts::default();
            // SAFETY: the complete writable record lives through the synchronous call.
            unsafe { snapshot(&mut result) };
            result
        };
        let (mut session, mut control, state, reply) = source_session_at_rate(48000, true);
        session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
        assert_eq!(counts().creates, 0);
        let expected_creates = if mode == "missing-tail" {
            let slots = control.replies.slots();
            for attempt in 1..=2 {
                assert!(matches!(
                    control.prepare_media(None),
                    Err(Error::UnsupportedFormat)
                ));
                assert!(control.media.is_none());
                assert!(session.media.is_none());
                assert_eq!(control.replies.slots(), slots);
                let observed = counts();
                assert_eq!((observed.creates, observed.destroys), (attempt, attempt));
                assert_eq!(observed.timed_calls, 0);
                assert!(reply.phone_authenticated());
                assert!(!reply.ended());
            }
            session.step(0).unwrap();
            assert!(
                session.media.is_none(),
                "failed preparation queued no candidate"
            );
            2
        } else {
            assert_eq!(mode, "timed-error");
            let mut input = control.prepare_media(None).unwrap();
            assert_eq!(input.source(), reply.phone_source().unwrap());
            assert_eq!(input.phone_reply(), Some(&reply));
            assert!(!input.active());
            let generation = input.signals().reply_target();
            session.step(0).unwrap();
            assert!(input.active());
            let (epoch, birth) = state.lock().unwrap().source_reference.unwrap();
            {
                let mut state = state.lock().unwrap();
                state.source_birth = Some(birth);
                state
                    .input
                    .extend((0..12).map(|_| Input::Audio(vec![0.25; 960])));
            }
            for now in 1..=16 {
                session.step(now).unwrap();
                if state.lock().unwrap().input.is_empty() {
                    break;
                }
            }
            assert!(state.lock().unwrap().input.is_empty());
            assert!(input.available() >= 7680);
            // The released 48 kHz PLC has 180 samples of legitimate startup delay.
            let mut warmup = [0.0; 480];
            assert!(input.render_timed_block(&mut warmup).unwrap().real > 0);
            assert!(warmup.iter().any(|sample| sample.abs() > 0.1));
            let sample = input.render_timed_sample().unwrap();
            let provenance = sample.provenance.unwrap();
            assert!(provenance.real && !provenance.plc && !provenance.gap);
            assert!(!provenance.zero_extension && !provenance.muted);
            assert!(
                provenance
                    .position
                    .is_some_and(|position| !position.negative)
            );
            assert!((sample.sample - 0.25).abs() < 0.01);
            assert_eq!(
                sample.age_bound,
                Some(SourceTimeBound {
                    host_clock_epoch: epoch,
                    not_after_ns: birth,
                })
            );
            let before = counts();
            assert_eq!(
                (before.creates, before.destroys, before.injected_failures),
                (1, 0, 0)
            );
            // SAFETY: the sole consumer arms its third subsequent call, outside rendering.
            assert_eq!(unsafe { arm(3) }, 0);
            let mut output = [1.0; 8];
            assert_eq!(input.render_timed_block(&mut output), None);
            assert_eq!(
                output, [0.0; 8],
                "erase genuine partial PCM on provider error"
            );
            let failed = counts();
            assert_eq!(failed.timed_calls - before.timed_calls, 3);
            assert_eq!(
                failed.real_nonzero_forwarded - before.real_nonzero_forwarded,
                3
            );
            assert_eq!(failed.injected_failures, 1);
            assert_eq!(input.render_timed_sample(), None);
            let repeated = counts();
            assert_eq!(repeated.timed_calls - failed.timed_calls, 1);
            assert_eq!(repeated.injected_failures, 2);
            assert!(
                input.active(),
                "render failure is not automatic call retirement"
            );
            assert!(!generation.ended());
            assert!(!reply.ended());
            drop(input);
            assert!(
                generation.ended(),
                "dropping the consumer retires its generation"
            );
            assert!(
                !reply.ended(),
                "private control remains live after media retirement"
            );
            assert_eq!(
                counts().destroys,
                0,
                "serial/control owners still retain the ring"
            );
            1
        };
        assert!(state.lock().unwrap().audio.is_empty());
        assert!(state.lock().unwrap().bound_radio.is_none());
        drop(control);
        assert!(reply.ended());
        drop(session);
        assert_eq!(state.lock().unwrap().drops, 1);
        let observed = counts();
        assert_eq!(
            (observed.creates, observed.destroys),
            (expected_creates, expected_creates)
        );
        return;
    }

    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Running(Option<Child>);
    impl Drop for Running {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn run(command: &mut Command, label: &str) -> (u32, Output) {
        let mut running = Running(Some(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        let child = running.0.as_mut().unwrap();
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                timed_out = true;
                child.kill().unwrap();
                child.wait().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = running.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            !timed_out && output.status.success(),
            "{label}: timeout={timed_out}, {}\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (pid, output)
    }
    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(format!("rptadv-phone-ring-{unique}"));
    std::fs::create_dir(&path).unwrap();
    let directory = Directory(path);
    let library = directory.0.join("phone_ring_fault.so");
    run(
        Command::new("timeout")
            .args([
                "--signal=TERM",
                "--kill-after=2s",
                "15",
                "cc",
                "-std=c11",
                "-shared",
                "-fPIC",
                "-Wall",
                "-Wextra",
                "-Werror",
            ])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/phone_ring_fault.c"
            ))
            .args(["-pthread", "-ldl", "-lm", "-o"])
            .arg(&library),
        "compile ring fault fixture",
    );
    for mode in ["missing-tail", "timed-error"] {
        let mut preload = library.as_os_str().to_os_string();
        if let Some(inherited) = std::env::var_os("LD_PRELOAD").filter(|value| !value.is_empty()) {
            preload.push(":");
            preload.push(inherited);
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .env("LD_PRELOAD", preload);
        let template = std::env::var_os("LLVM_PROFILE_FILE");
        assert!(std::env::var_os("CARGO_LLVM_COV").is_none() || template.is_some());
        let profile = template.map(|template| {
            let directory = std::path::Path::new(&template).parent().unwrap().to_owned();
            let prefix = format!("phone-ring-{unique}-{mode}-");
            command.env(
                "LLVM_PROFILE_FILE",
                directory.join(format!("{prefix}%p.profraw")),
            );
            (directory, prefix)
        });
        let (pid, output) = run(&mut command, mode);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("1 passed; 0 failed"), "{mode}: {stdout}");
        if let Some((directory, prefix)) = profile {
            assert!(
                std::fs::metadata(directory.join(format!("{prefix}{pid}.profraw")))
                    .unwrap()
                    .len()
                    > 0
            );
        }
    }
}
