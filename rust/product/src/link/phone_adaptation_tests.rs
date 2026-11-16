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
    state.lock().unwrap().source_reset = true;
    tick += 1;
    session.step(tick).unwrap();
    assert_eq!(session.revision.0.get().unwrap().get(), revision.get() + 1);
    assert_eq!(session.settled_revision, revision);
    assert!(session.pending_release.is_none());
    assert_eq!(session.source_ready.load(Ordering::Acquire), 0);
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
    let (pending, mut replacement) = calls
        .prepare_pending_media(&source, &reply, None, settings)
        .unwrap();
    assert!(!replacement.active());
    calls.adopt_media(&source, &reply, pending).ok().unwrap();
    tick += 1;
    session.step(tick).unwrap();
    assert!(replacement.active() && !old.active());
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
