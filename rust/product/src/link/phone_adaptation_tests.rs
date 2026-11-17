//! Bounded composed-seam proof, not a socket/SIP failing-call replay.
use super::*;
use crate::link::session::tests::State;
use rpt_advanced_core::{
    config::{PhoneAccountProtocol, PhoneSpeechSettings},
    link::{PeerInput as _, PhoneSpeechPhase},
};
use rpt_advanced_standalone::providers::ProviderSet;
use rptadv_jitter_client::{Admission, Jitter, Kind};
use std::sync::Mutex;

/// Obtain a real native adaptation skip, not a missing packet or invented reset.
fn native_phone_cut() -> (u64, u64) {
    let providers = ProviderSet::load().unwrap();
    let descriptor = providers
        .descriptor("librptadv_jitter_adapter.so.1")
        .unwrap();
    // The retained provider set outlives this opaque client.
    let mut jitter = unsafe { Jitter::phone(descriptor.as_ptr()) }.unwrap();
    let origin = (1u64 << 32) + 1000;
    let mut packet = 0;
    let mut payload = [0; 160];
    for now_ms in (0..=1600u64).step_by(10) {
        while packet <= now_ms {
            assert_eq!(
                jitter
                    .put(origin + packet * 8, 160, packet * 1_000_000, &[1])
                    .unwrap(),
                Admission::Accepted
            );
            packet += 20;
        }
        for _ in 0..8 {
            let decision = jitter.get(now_ms * 1_000_000, 160, &mut payload).unwrap();
            if decision.kind == Kind::NotDue {
                break;
            }
            assert_eq!(decision.kind, Kind::Packet);
            let before = jitter.observe().unwrap();
            jitter.advance(now_ms * 1_000_000, 0).unwrap();
            let after = jitter.observe().unwrap();
            assert_eq!(
                (after.missing_span, after.insertion_span, after.limit_resets),
                (0, 0, 0)
            );
            assert_eq!(after.late_drops, 0);
            let skipped = after.skipped_span - before.skipped_span;
            if skipped > 0 {
                let start = decision.timestamp + u64::from(decision.span);
                eprintln!(
                    "native_cut poll_ms={now_ms} start={start} end={} skipped={skipped} delay_before={} delay_after={}",
                    start + skipped,
                    before.delay,
                    after.delay
                );
                assert!(after.delay < before.delay);
                return (start, start + skipped);
            }
        }
    }
    panic!("the existing bounded no-gap trace must exercise native delay reduction");
}

/// Independent genuine PCM offers use the real owner clock, never native replay time.
/// All original receipts and accepted coordinates are checked before moving on.
fn genuine_speech(
    session: &mut PrivatePhoneSession,
    state: &Arc<Mutex<State>>,
    input: &mut media::PhoneMediaInput,
    speech: &[f32],
    tick: &mut u64,
) -> usize {
    let mut genuine = 0;
    for block in speech.chunks(160) {
        let (epoch, born, start, credits) = {
            let mut source = state.lock().unwrap();
            let (epoch, born) = source.source_reference.unwrap();
            source.source_birth = Some(born);
            let start = session.source_end;
            let credits = source.source_credits.len();
            source.input.push_back(Input::Audio(block.to_vec()));
            (epoch, born, start, credits)
        };
        *tick += 1;
        session.step(*tick).unwrap();
        {
            let source = state.lock().unwrap();
            assert!(source.input.is_empty());
            assert_eq!(source.source_birth, Some(born));
            assert_eq!(source.source_reference.unwrap().0, epoch);
            assert!(source.source_reference.unwrap().1 >= born);
            assert_eq!(&source.source_credits[credits..], &[(start, block.len())]);
        }
        for _ in 0..block.len() * 6 {
            if let Some(sample) = input.render_timed_sample() {
                if sample.provenance.as_ref().is_some_and(|p| {
                    p.real
                        && !p.plc
                        && !p.gap
                        && !p.zero_extension
                        && !p.muted
                        && p.position.is_some_and(|p| !p.negative)
                }) {
                    let age = sample.age_bound.unwrap();
                    assert_eq!(age.host_clock_epoch, epoch);
                    assert!(age.not_after_ns <= born, "no receipt rejuvenation");
                    genuine += usize::from(sample.sample.abs() > 0.001);
                }
            }
        }
    }
    genuine
}

#[test]
fn native_adaptation_cut_requires_new_authoritative_release_for_incoming_readmission() {
    native_cut_readmission(false);
}

#[test]
fn native_adaptation_recovers_active_incoming_on_fresh_original_without_digit() {
    native_cut_readmission(true);
}

fn native_cut_readmission(causal: bool) {
    let (cut_start, cut_end) = native_phone_cut();
    assert!(cut_end > cut_start);
    let (mut calls, mut session, state, source, reply) =
        crate::phone::tests::controlled_source_session_protocol(
            8000,
            false,
            PhoneAccountProtocol::Sip,
        );
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    assert!(session.timed_loss && reply.phone_media_authorized() && !reply.phone_outgoing());
    assert!(calls.mailbox(&source, &reply).is_some());
    calls.attach(&source, &reply, "1000").unwrap();
    session.step(0).unwrap();
    // Establish a consumed control revision before the first generation, so
    // later idle reconciliation really queues a stale release after the cut.
    state.lock().unwrap().input.push_back(Input::Digit(b'5'));
    session.step(1).unwrap();
    let (event_source, event_reply, digit, received) = calls.event().unwrap();
    assert_eq!(
        (&event_source, &event_reply, received),
        (&source, &reply, 1)
    );
    calls
        .actions(&event_source, &event_reply, digit, received)
        .unwrap();
    calls.reconcile_collections(|_, _, _| Some(false));
    session.step(2).unwrap();
    assert_eq!(calls.media_refresh_nodes(), ["1000"]);
    let settings = PhoneSpeechSettings::default();
    let (pending, mut old) = calls
        .prepare_pending_media(&source, &reply, None, settings)
        .unwrap();
    calls.adopt_media(&source, &reply, pending).ok().unwrap();
    session.step(3).unwrap();
    assert!(old.active());
    let revision = session.revision.0.get().unwrap();
    assert_eq!(session.settled_revision, revision);
    // The checked-in genuine speech fixture, decimated to the negotiated 8 kHz.
    let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
        .chunks_exact(12)
        .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
        .collect();
    let mut tick = 3;
    let initial = genuine_speech(&mut session, &state, &mut old, &speech, &mut tick);
    assert!(
        initial > 0,
        "old generation actually rendered genuine speech"
    );
    assert_eq!(
        old.phone_speech_state().unwrap().phase,
        PhoneSpeechPhase::Open
    );

    // Media's actual native-skip -> hard-discontinuity/gap translation is covered
    // by adaptive_delay_reduction_preserves_exact_source_gap_credit. Here only
    // the proved positive native cut triggers the existing real owner callback.
    // The product fixture starts its own source epoch; replay clocks are NOT
    // substituted for its original receipt clock or represented as one call.
    if causal {
        state.lock().unwrap().source_adaptation = Some((cut_start, (cut_end - cut_start) as u32));
    } else {
        state.lock().unwrap().source_reset = true;
    }
    tick += 1;
    session.step(tick).unwrap();
    assert_eq!(session.revision.0.get().unwrap().get(), revision.get() + 1);
    assert_eq!(session.settled_revision, revision);
    assert!(session.pending_release.is_none());
    assert_eq!(session.source_ready.revision.load(Ordering::Acquire), 0);
    assert!(!old.active());
    assert_eq!(
        old.phone_speech_state().unwrap().phase,
        PhoneSpeechPhase::Unavailable
    );
    assert!(!reply.ended() && reply.phone_media_authorized());
    assert!(calls.mailbox(&source, &reply).is_some());
    assert_eq!(calls.attachments("1000"), [(source.clone(), reply.clone())]);
    let after_cut = session.revision.0.get();

    let fresh_without_release = genuine_speech(&mut session, &state, &mut old, &speech, &mut tick);
    assert_eq!(fresh_without_release, 0);
    assert!(
        session.source_end > 0,
        "fresh PCM was accepted after the cut"
    );
    assert_eq!(session.revision.0.get(), after_cut);
    assert!(
        calls.event().is_none(),
        "genuine speech cannot invent a control event"
    );
    if causal {
        calls.reconcile_collections(|_, seen_source, seen_reply| {
            assert_eq!((seen_source, seen_reply), (&source, &reply));
            Some(false)
        });
        tick += 1;
        session.step(tick).unwrap();
        assert_eq!(
            calls.media_refresh_nodes(),
            ["1000"],
            "proved active-origin cut plus fresh original and real idle control must rearm without a digit"
        );
        assert!(calls.event().is_none());
    } else {
        calls.collection_expired(&reply);
        calls.reconcile_collections(|_, _, _| Some(false));
        tick += 1;
        session.step(tick).unwrap();
        assert!(
            calls.media_refresh_nodes().is_empty(),
            "idle control without a consumed new revision cannot rearm"
        );
        assert!(!old.active() && !reply.ended());
        assert_eq!(
            session.settled_revision, revision,
            "stale release cannot settle the cut"
        );
        assert!(session.pending_release.is_none());

        // A real subsequent symbol is consumed by the authenticated registry and
        // ordinary grammar, then the destination collector supplies its idle fence.
        state.lock().unwrap().input.push_back(Input::Digit(b'5'));
        tick += 1;
        session.step(tick).unwrap();
        let (event_source, event_reply, digit, received) = calls.event().unwrap();
        assert_eq!(
            (&event_source, &event_reply, received),
            (&source, &reply, tick)
        );
        calls
            .actions(&event_source, &event_reply, digit, received)
            .unwrap();
        assert!(calls.media_refresh_nodes().is_empty());
        calls.reconcile_collections(|_, seen_source, seen_reply| {
            assert_eq!((seen_source, seen_reply), (&source, &reply));
            Some(false)
        });
        tick += 1;
        session.step(tick).unwrap();
        assert_eq!(session.settled_revision, session.revision.0.get().unwrap());
        assert_eq!(calls.media_refresh_nodes(), ["1000"]);
    }
    let (pending, mut replacement) = calls
        .prepare_pending_media(&source, &reply, None, settings)
        .unwrap();
    assert!(!replacement.active());
    calls.adopt_media(&source, &reply, pending).ok().unwrap();
    tick += 1;
    session.step(tick).unwrap();
    assert!(replacement.active() && !old.active());
    let (old_generation, _) = media::tests::input_snapshot(&old);
    let (new_generation, _) = media::tests::input_snapshot(&replacement);
    assert_ne!(old_generation, new_generation);
    assert!(media::tests::fresh_analysis(
        session.media.as_ref().unwrap()
    ));

    assert_eq!(replacement.source(), &source);
    assert_eq!(replacement.phone_reply(), Some(&reply));
    let readmitted = genuine_speech(&mut session, &state, &mut replacement, &speech, &mut tick);
    assert!(
        readmitted > 0,
        "fresh authorized generation renders later genuine speech"
    );
    assert_eq!(
        replacement.phone_speech_state().unwrap().phase,
        PhoneSpeechPhase::Open
    );
    if causal {
        genuine_speech(
            &mut session,
            &state,
            &mut replacement,
            &[0.0; 16000],
            &mut tick,
        );
        assert_eq!(
            replacement.phone_speech_state().unwrap().phase,
            PhoneSpeechPhase::Closed,
            "later independent genuine speech reaches its own held and drained close"
        );
    }
    assert!(!old.active());
    assert!(old.render_timed_sample().is_none());
    assert!(!reply.ended());
    assert!(state.lock().unwrap().bound_radio.is_none());
    eprintln!(
        "incoming lifecycle revision_before={} revision_after_cut={} revision_after_release={} initial_genuine={initial} fresh_without_release={fresh_without_release} readmitted_genuine={readmitted}",
        revision.get(),
        after_cut.unwrap().get(),
        session.settled_revision.get()
    );
}

type RecoveryFixture = (
    PrivatePhoneSession,
    PrivatePhoneControl,
    Arc<Mutex<State>>,
    PeerReply,
    media::PhoneMediaInput,
);

fn active_recovery_fixture() -> RecoveryFixture {
    active_recovery_fixture_protocol(PhoneAccountProtocol::Sip)
}
fn active_recovery_fixture_protocol(protocol: PhoneAccountProtocol) -> RecoveryFixture {
    let (mut session, mut control, state, reply) = source_session_with_protocol(
        crate::link::session::tests::phone_source_peer_at_rate(8000),
        true,
        protocol,
    );
    control.set_attached(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    session.step(0).unwrap();
    let input = control.prepare_media(None).unwrap();
    session.step(1).unwrap();
    assert!(input.active());
    (session, control, state, reply, input)
}

fn offer_cut(session: &mut PrivatePhoneSession, state: &Arc<Mutex<State>>, tick: u64) {
    state.lock().unwrap().source_adaptation = Some((1000, 160));
    session.step(tick).unwrap();
}
fn offer_silence(session: &mut PrivatePhoneSession, state: &Arc<Mutex<State>>, tick: u64) {
    let mut source = state.lock().unwrap();
    source.source_birth = Some(source.source_reference.unwrap().1);
    source.input.push_back(Input::Audio(vec![0.0; 160]));
    drop(source);
    session.step(tick).unwrap();
}

#[test]
fn cancel_epoch_invalidates_active_origin_before_retirement() {
    let (mut session, mut control, state, reply, mut old) = active_recovery_fixture();
    // Pause the control operation after epoch invalidation, BEFORE shared retirement.
    let epoch = session.media.as_ref().unwrap().origin_epoch();
    control.source_ready.invalidate();
    assert_ne!(control.source_ready.epoch(), Some(epoch));
    assert!(!old.active());
    assert!(old.render_timed_sample().is_none());
    offer_cut(&mut session, &state, 2);
    offer_silence(&mut session, &state, 3);
    assert!(session.recovery.is_none());
    assert!(control.recovery_request().is_none());
    control.retire_generation();
    assert!(!control.media_rearm_ready());
    assert!(reply.phone_media_authorized());
}

#[test]
fn recovery_requires_active_settled_incoming_attached_sip_origin() {
    for excluded in [
        "prepared",
        "inactive",
        "unsettled",
        "unattached",
        "iax",
        "generic",
    ] {
        let (mut session, mut control, state, _, mut old) =
            active_recovery_fixture_protocol(if excluded == "iax" {
                PhoneAccountProtocol::Iax
            } else {
                PhoneAccountProtocol::Sip
            });
        let mut prepared_input = None;
        match excluded {
            "prepared" => {
                control.retire_media();
                prepared_input = Some(control.prepare_media(None).unwrap());
                // Queued candidate is prepared and never adopted before this cut.
            }
            "inactive" => control.retire_media(),
            "unsettled" => session.pending_release = Some(NonZeroU32::MIN),
            "unattached" => control.set_attached(false),
            "iax" => assert!(!session.timed_loss),
            "generic" => state.lock().unwrap().source_reset = true,
            _ => unreachable!(),
        }
        if excluded == "generic" {
            session.step(2).unwrap();
        } else {
            offer_cut(&mut session, &state, 2);
        }
        offer_silence(&mut session, &state, 3);
        assert!(control.recovery_request().is_none(), "{excluded}");
        assert!(!old.active() && old.render_timed_sample().is_none());
        assert!(prepared_input.as_ref().is_none_or(|input| !input.active()));
    }
}

#[test]
fn recovery_rejects_nonoriginal_or_pre_cut_receipts_and_zero_credit() {
    for rejected in [
        "unknown",
        "old",
        "future",
        "cross_epoch",
        "synthetic",
        "empty",
    ] {
        let (mut session, control, state, _, _old) = active_recovery_fixture();
        offer_cut(&mut session, &state, 2);
        assert!(
            session.recovery.is_some(),
            "receipt case must start with eligible cut"
        );
        {
            let mut source = state.lock().unwrap();
            let (_, now) = source.source_reference.unwrap();
            source.source_birth = match rejected {
                "unknown" => None,
                "old" => Some(
                    session
                        .recovery
                        .as_ref()
                        .unwrap()
                        .reference
                        .now_ns
                        .saturating_sub(1),
                ),
                "future" => Some(u64::MAX),
                _ => Some(now),
            };
            source.source_birth_epoch = (rejected == "cross_epoch").then_some(u64::MAX);
            source.input.push_back(match rejected {
                "synthetic" => Input::Concealed(vec![0.0; 160]),
                "empty" => Input::Audio(Vec::new()),
                _ => Input::Audio(vec![0.0; 160]),
            });
        }
        let result = session.step(3);
        // Invalid receipt contracts may terminate the source rather than qualify it.
        assert!(
            control.recovery_request().is_none(),
            "{rejected}: {result:?}"
        );
    }
}

#[test]
fn recovery_approval_is_bounded_idempotent_and_new_source_activity_wins() {
    for cancel in ["digit", "onset", "generic", "retire", "same_node", "none"] {
        let (mut session, mut control, state, reply, old) = active_recovery_fixture();
        offer_cut(&mut session, &state, 2);
        offer_silence(&mut session, &state, 3);
        let ticket = control.recovery_request().unwrap();
        for _ in 0..16 {
            control
                .replies
                .push(PrivateCommand::CollectionEnded(NonZeroU32::MIN))
                .ok()
                .unwrap();
        }
        assert!(!control.approve_recovery(&reply, ticket));
        session.step(4).unwrap();
        assert!(control.approve_recovery(&reply, ticket));
        let slots = control.replies.slots();
        assert!(control.approve_recovery(&reply, ticket));
        assert_eq!(control.replies.slots(), slots);
        assert!(!control.media_rearm_ready(), "queued is not acknowledged");
        match cancel {
            "digit" => state.lock().unwrap().input.push_back(Input::Digit(b'5')),
            "onset" => {
                let mut source = state.lock().unwrap();
                source.source_birth = Some(source.source_reference.unwrap().1);
                source
                    .input
                    .push_back(Input::Audio(crate::link::session::tests::five_tone(640)));
            }
            "generic" => state.lock().unwrap().source_reset = true,
            "retire" => control.retire_media(),
            "same_node" => {
                control.set_attached(false);
                control.retire_media();
                control.set_attached(true);
            }
            _ => {}
        }
        session.step(5).unwrap();
        assert!(!old.active());
        if cancel == "none" {
            assert!(control.media_rearm_ready());
            let consumed = control.consumed_revision;
            control.retire_for_graph_replacement();
            let (pending, mut fresh) = control
                .prepare_pending_voiced_media(None, PhoneSpeechSettings::default())
                .unwrap();
            assert!(!fresh.active());
            control.adopt_media(pending).ok().unwrap();
            session.step(6).unwrap();
            assert!(fresh.active());
            assert!(
                control.recovery_request().is_none(),
                "adoption consumes the ticket"
            );
            assert_eq!(
                control.consumed_revision, consumed,
                "no fabricated consumed digit"
            );
            offer_silence(&mut session, &state, 7);
            for _ in 0..960 {
                let _ = fresh.render_timed_sample();
            }
            assert_eq!(
                fresh.phone_speech_state().unwrap().phase,
                PhoneSpeechPhase::Unavailable
            );
        } else {
            assert!(control.recovery_request().is_none(), "{cancel}");
            assert!(!control.media_rearm_ready(), "{cancel}");
            assert!(!control.approve_recovery(&reply, ticket));
        }
    }
}

#[test]
fn repeated_cut_replaces_one_ticket_and_candidate_cancellation_never_reuses_generation() {
    let (mut session, mut control, state, reply, old) = active_recovery_fixture();
    offer_cut(&mut session, &state, 2);
    offer_silence(&mut session, &state, 3);
    let first = control.recovery_request().unwrap();
    assert!(control.approve_recovery(&reply, first));
    offer_cut(&mut session, &state, 4);
    assert!(control.recovery_request().is_none());
    offer_silence(&mut session, &state, 5);
    let second = control.recovery_request().unwrap();
    assert_ne!(first, second);
    assert!(!control.approve_recovery(&reply, first));
    assert!(control.approve_recovery(&reply, second));
    session.step(6).unwrap();
    control.retire_for_graph_replacement();
    let (pending, discarded) = control.prepare_pending_media(None).unwrap();
    drop(pending);
    assert!(!discarded.active());
    control.retire_for_graph_replacement();
    let (pending, replacement) = control.prepare_pending_media(None).unwrap();
    control.adopt_media(pending).ok().unwrap();
    control.retire_media(); // after preparation and enqueue, before adoption
    session.step(7).unwrap();
    assert!(!old.active() && !discarded.active() && !replacement.active());
    assert!(control.recovery_request().is_none());
}

#[test]
fn recovery_epoch_exhaustion_is_permanent_and_cut_callback_has_no_heap_operations() {
    let (mut session, mut control, state, _, old) = active_recovery_fixture();
    state.lock().unwrap().source_adaptation = Some((1000, 160));
    let mut reference = SourceTimeReference::default();
    let (result, alloc, free) =
        crate::fixture::heap_operations(|| session.read_source(2, &mut reference));
    result.unwrap();
    assert_eq!((alloc, free), (0, 0));
    assert!(!old.active());
    control
        .source_ready
        .cancellation_epoch
        .store(u32::MAX, Ordering::Release);
    control.retire_media();
    assert!(control.source_ready.epoch().is_none());
    control.retire_media();
    assert!(control.source_ready.epoch().is_none());
    offer_silence(&mut session, &state, 3);
    assert!(control.recovery_request().is_none());
    assert!(matches!(
        control.prepare_pending_media(None),
        Err(Error::Admission)
    ));
    eprintln!(
        "recovery sizes session={} control={} shared={} ticket={} cut={} origin={}",
        std::mem::size_of::<PrivatePhoneSession>(),
        std::mem::size_of::<PrivatePhoneControl>(),
        std::mem::size_of::<SourceReady>(),
        std::mem::size_of::<RecoveryTicket>(),
        std::mem::size_of::<RecoveryCut>(),
        std::mem::size_of::<MediaOrigin>()
    );
}

#[test]
fn pending_cut_retry_keeps_first_active_origin_and_cut_reference() {
    let (mut session, mut control, state, reply) = source_session_with_protocol(
        crate::link::session::tests::phone_source_peer_inband(8000),
        true,
        PhoneAccountProtocol::Sip,
    );
    control.set_attached(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    session.step(0).unwrap();
    let old = control.prepare_media(None).unwrap();
    session.step(1).unwrap();
    for _ in 0..64 {
        session
            .digits
            .push(PrivateDigit {
                received_ms: 0,
                revision: NonZeroU32::MIN,
                symbol: '5',
            })
            .ok()
            .unwrap();
    }
    offer_cut(&mut session, &state, 2);
    assert!(!old.active());
    assert!(state.lock().unwrap().source_adaptation.is_some());
    let captured = session
        .recovery
        .as_ref()
        .expect("first refused cut retains eligible origin")
        .reference
        .now_ns;
    assert_eq!(session.revision.0.get(), Some(NonZeroU32::MIN));
    while control.digits.pop().is_ok() {}
    session.step(3).unwrap();
    assert!(state.lock().unwrap().source_adaptation.is_none());
    assert_eq!(
        session.recovery.as_ref().unwrap().reference.now_ns,
        captured
    );
    assert_eq!(session.revision.0.get().unwrap().get(), 2);
    offer_silence(&mut session, &state, 4);
    let ticket = control.recovery_request().unwrap();
    assert!(control.approve_recovery(&reply, ticket));
    session.step(5).unwrap();
    assert!(control.media_rearm_ready());
}

#[test]
fn pending_recovery_cannot_cross_source_finish_or_clock_replacement() {
    for finish in [true, false] {
        let (mut session, mut control, state, reply, _old) = active_recovery_fixture();
        offer_cut(&mut session, &state, 2);
        offer_silence(&mut session, &state, 3);
        let ticket = control.recovery_request().unwrap();
        assert!(control.approve_recovery(&reply, ticket));
        if finish {
            state.lock().unwrap().source_finish = true;
        } else {
            session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
        }
        let result = session.step(4);
        if finish {
            result.unwrap();
            offer_silence(&mut session, &state, 5);
        } else {
            assert!(
                matches!(result, Err(Error::Hangup)),
                "clock replacement is terminal"
            );
        }
        assert!(control.recovery_request().is_none(), "finish={finish}");
        assert!(!control.media_rearm_ready());
    }
}

#[test]
fn recovery_approval_waits_for_real_provider_drain_without_duplicate_queueing() {
    let (mut session, mut control, state, reply, _old) = active_recovery_fixture();
    offer_cut(&mut session, &state, 2);
    offer_silence(&mut session, &state, 3);
    let ticket = control.recovery_request().unwrap();
    assert!(control.approve_recovery(&reply, ticket));
    {
        let mut input = state.lock().unwrap();
        input.source_birth = Some(input.source_reference.unwrap().1);
        input.input.push_back(Input::Audio(vec![0.0; 480]));
    }
    session.step(4).unwrap();
    assert!(!state.lock().unwrap().input.is_empty());
    assert!(!control.media_rearm_ready());
    session.step(5).unwrap();
    assert!(!state.lock().unwrap().input.is_empty());
    session.step(6).unwrap();
    assert!(state.lock().unwrap().input.is_empty());
    assert!(
        control.media_rearm_ready(),
        "exact queued approval must wait for real suffix drain"
    );
}

#[test]
fn recovery_approval_and_new_silent_render_have_no_heap_operations() {
    let (mut session, mut control, state, reply, old) = active_recovery_fixture();
    offer_cut(&mut session, &state, 2);
    offer_silence(&mut session, &state, 3);
    let ticket = control.recovery_request().unwrap();
    state.lock().unwrap().deadlines.reserve(32);
    let (approved, alloc, free) =
        crate::fixture::heap_operations(|| control.approve_recovery(&reply, ticket));
    assert!(approved);
    assert_eq!((alloc, free), (0, 0));
    let (result, alloc, free) = crate::fixture::heap_operations(|| session.step(4));
    result.unwrap();
    assert_eq!((alloc, free), (0, 0));
    control.retire_for_graph_replacement();
    let (pending, mut fresh) = control
        .prepare_pending_voiced_media(None, PhoneSpeechSettings::default())
        .unwrap();
    control.adopt_media(pending).ok().unwrap();
    session.step(5).unwrap();
    offer_silence(&mut session, &state, 6);
    let (_, alloc, free) = crate::fixture::heap_operations(|| {
        for _ in 0..960 {
            let _ = fresh.render_timed_sample();
        }
    });
    assert_eq!((alloc, free), (0, 0));
    assert_eq!(
        fresh.phone_speech_state().unwrap().phase,
        PhoneSpeechPhase::Unavailable
    );
    assert!(!old.active());
}

#[test]
fn outgoing_adaptation_keeps_existing_release_without_incoming_ticket() {
    let (io, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
    let (account, call) = io.generations();
    let source = PhoneSource::new(
        io.phone_account().unwrap().clone(),
        account,
        io.identity().call_id().unwrap(),
        call,
        13,
    )
    .unwrap();
    let reply = rpt_advanced_core::link::PeerSignals::for_outgoing_phone(source).reply_target();
    let (mut session, mut control) =
        PrivatePhoneSession::prepare(io, reply.clone(), PhoneAccountProtocol::Sip).unwrap();
    control.set_attached(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    session.step(0).unwrap();
    let old = control.prepare_media(None).unwrap();
    session.step(1).unwrap();
    assert!(old.active());
    offer_cut(&mut session, &state, 2);
    assert!(!old.active());
    assert!(control.recovery_request().is_none());
    assert!(
        control.media_rearm_ready(),
        "ordinary outgoing reset release is unchanged"
    );
    assert!(!reply.ended() && reply.phone_outgoing());
}

#[test]
fn recovery_evidence_requires_positive_credit_and_actual_raw_tail_drain() {
    let (mut session, mut control, state, _) = source_session_with_protocol(
        crate::link::session::tests::phone_source_peer_inband(8000),
        true,
        PhoneAccountProtocol::Sip,
    );
    control.set_attached(true);
    session.set_source_clock(SourceTimeClock::new(Instant::now()).unwrap());
    session.step(0).unwrap();
    let old = control.prepare_media(None).unwrap();
    session.step(1).unwrap();
    offer_cut(&mut session, &state, 2);
    for _ in 0..64 {
        session
            .digits
            .push(PrivateDigit {
                received_ms: 0,
                revision: NonZeroU32::MIN,
                symbol: '5',
            })
            .ok()
            .unwrap();
    }
    let (credits, born) = {
        let mut input = state.lock().unwrap();
        let born = input.source_reference.unwrap().1;
        input.source_birth = Some(born);
        input.input.push_back(Input::Audio(vec![0.0; 480]));
        (input.source_credits.len(), born)
    };
    session.step(3).unwrap();
    assert_eq!(state.lock().unwrap().source_credits.len(), credits + 1);
    assert_eq!(state.lock().unwrap().source_credits.last().unwrap().1, 0);
    assert!(!session.recovery.as_ref().unwrap().fresh);
    assert!(control.recovery_request().is_none());
    while control.digits.pop().is_ok() {}
    session.step(4).unwrap();
    assert!(session.recovery.as_ref().unwrap().fresh);
    assert_eq!(state.lock().unwrap().source_credits.len(), credits + 2);
    assert!(state.lock().unwrap().source_credits.last().unwrap().1 > 0);
    assert!(
        control.recovery_request().is_none(),
        "partial offer is not drained"
    );
    session.step(5).unwrap();
    assert!(control.recovery_request().is_none());
    session.step(6).unwrap();
    assert!(control.recovery_request().is_some());
    assert_eq!(state.lock().unwrap().source_birth, Some(born));
    assert!(!old.active());
}
