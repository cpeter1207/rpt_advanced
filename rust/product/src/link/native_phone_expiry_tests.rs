//! Keep actual expired source receipts through the occupied RTP/ring seam and
//! verify the public fanout admission, without constructing an AudioSpan.
use super::*;
use crate::{
    link::{
        input::AudioInput,
        ring::{InboundPolicy, InboundRing, tests::TimedTapGuard},
        session::tests::peer_with_egress,
    },
    services::ProcessedPeer,
};
use rpt_advanced_core::{
    controller::{ControllerSettings, CourtesySettings, NodeController},
    link::{AudioPeer, LinkAudio, Mode, PeerIdentity, RetirementDomain, SourcePosition},
};

/// Stop the source at exactly 32/80 real ring credit, wait on the *original*
/// clock, then release just enough space for the retained 48. No sleep, native
/// decision injection, or fabricated source birth/deadline.
#[test]
fn native_retained_suffix_original_cap_survives_stall_and_gates_actual_fanout() {
    const BLOCK: usize = 96; // 2 ms / 16 source frames: select a block *inside* the 48.
    for stale in [false, true] {
        let tap = TimedTapGuard::next();
        let (mut call, mut input, now, first) = stalled_with_pcm(1, true);
        assert_eq!(
            (first.accepted, first.offered, observe().0.skipped_span),
            (32, 80, 80)
        );
        let generation = media::tests::input_snapshot(&input);
        let age_limit = first.born + 500_000_000;
        let last_refusal = if stale {
            age_limit + 100_000_000
        } else {
            now + 1_000_000
        };
        for check in [now, (now + last_refusal) / 2, last_refusal] {
            step(&mut call.s, check);
            let refused = credits();
            assert_eq!(refused.len(), 1);
            let c = refused[0];
            assert_eq!(
                (c.start, c.offered, c.accepted, c.born, c.epoch),
                (first.start + 32, 48, 0, first.born, first.epoch)
            );
            assert_eq!(input.available(), 2400);
            assert_eq!(media::tests::input_snapshot(&input), generation);
        }
        drain_to(&mut input, 2300);
        let resumed_at = last_refusal + 1_000_000;
        step(&mut call.s, resumed_at);
        let c = credits()[0];
        assert_eq!(
            (c.start, c.offered, c.accepted, c.born, c.epoch, c.synthetic),
            (first.start + 32, 48, 48, first.born, first.epoch, 0)
        );
        step(&mut call.s, resumed_at + 40_000_000); // release real lookahead already in native jitter
        assert_eq!(media::tests::input_snapshot(&input), generation);
        assert!(
            media::tests::hold_snapshot(call.s.media.as_ref().unwrap())
                .0
                .is_some()
        );

        let source = call.reply.phone_source().unwrap();
        let back = input.take_output().unwrap();
        let phone = AudioPeer::for_phone(
            source,
            &call.reply,
            Mode::TRANSCEIVE,
            AudioInput::Phone(input),
            back,
            BLOCK,
        )
        .unwrap();
        let (_writer, ordinary) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
        let (writer, mut reader) = LinkAudioQueue::new(960, 8).unwrap().into_endpoints();
        let public = AudioPeer::new(
            PeerIdentity::from_operator("2000").unwrap(),
            Mode::TRANSCEIVE,
            AudioInput::Peer(ProcessedPeer::new(ordinary, None)),
            writer,
            BLOCK,
            0,
        )
        .unwrap();
        let domain = RetirementDomain::default();
        let (mixer, mut dispatcher) =
            LinkAudio::new_in_domain(vec![phone, public], BLOCK, domain.clone()).unwrap();
        let mut mixer = mixer.with_phone_clock(first.epoch);
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        controller.bind_phone_retirement(&mut control, domain);
        let mut selected = None;
        for block in 0..250_u64 {
            let before = tap.0.snapshot();
            let capture = resumed_at + 40_000_000 + block * 2_000_000;
            let mut rf = [0.; BLOCK];
            mixer
                .process(
                    &mut controller,
                    false,
                    &mut rf,
                    SourcePosition {
                        generation: 1,
                        sample: block * BLOCK as u64,
                        stream: 1,
                        born_ns: Some(capture),
                    },
                )
                .unwrap();
            dispatcher.dispatch(2);
            let after = tap.0.snapshot();
            if after.calls == before.calls + BLOCK as u64
                && before.coordinate.is_some_and(|p| {
                    p.generation == generation.0 && !p.negative && p.frame >= c.start
                })
                && after.coordinate.is_some_and(|p| {
                    p.generation == generation.0 && !p.negative && p.frame < c.start + 48
                })
            {
                assert!(
                    rf.iter().any(|s| s.abs() > 0.001),
                    "select genuine non-silent recovered source frames"
                );
                let programme = mixer.rf_programme();
                assert!(programme.private && programme.active);
                let bound = programme.age_bound.unwrap();
                assert_eq!(bound.host_clock_epoch, c.epoch);
                assert!(bound.not_after_ns <= c.born);
                let span = reader.peek_span().expect("actual typed public fanout");
                assert_eq!(
                    (span.frames, span.span.born_ns, span.span.phone_expires_ns),
                    (BLOCK, Some(capture), bound.not_after_ns + 500_000_000)
                );
                assert_eq!(capture >= span.span.phone_expires_ns, stale);
                selected = Some(capture);
                break;
            }
            // Discard observation-only preceding blocks, never synthesize or
            // rewrite the target span. It remains in the actual destination queue.
            while reader.read_span(&mut [0.; BLOCK]).is_some() {}
        }
        let capture = selected.expect("a complete actual mixed block inside the resumed suffix");
        let (mut io, state) = peer_with_egress(48000, true, true);
        let mut outbound = Outbound::new(48000, reader).unwrap();
        let clock = call.s.source_clock.as_ref().unwrap().clone();
        clock_tests::set(1 + capture);
        outbound.bind_media_clock(clock).unwrap();
        outbound.step(&mut io, capture / 1_000_000).unwrap();
        let stats = outbound.statistics.snapshot();
        assert_eq!(
            stats.expired_native_samples,
            if stale { BLOCK as u64 } else { 0 }
        );
        assert_eq!(stats.accepted, u64::from(!stale));
        let sent = state.lock().unwrap();
        assert_eq!(sent.prepared.len(), usize::from(!stale));
        assert!(sent.audio.is_empty());
    }
}
