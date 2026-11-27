use super::*;
use crate::audio::{LinkAudioConsumer, LinkAudioQueue};
use crate::controller::{ControllerSettings, CourtesySettings};

#[path = "text_audio_tests.rs"]
mod text;

#[test]
fn idle_peer_processing_follows_the_native_clock_without_consuming_media() {
    struct ClockedInput {
        signals: PeerSignals,
        available: u64,
        real: Option<usize>,
        finite: Option<bool>,
        renders: usize,
        rendered_samples: usize,
        silent_samples: usize,
    }
    impl PeerInput for ClockedInput {
        fn source_rate(&self) -> u32 {
            48_000
        }
        fn signals(&self) -> &PeerSignals {
            &self.signals
        }
        fn available(&self) -> u64 {
            self.available
        }
        fn finite_playout(&self) -> Option<bool> {
            self.finite
        }
        fn render(&mut self, output: &mut [f32]) -> Option<usize> {
            self.renders += 1;
            self.rendered_samples += output.len();
            output.fill(0.25);
            self.real
        }
        fn silence(&mut self, scratch: &mut [f32]) {
            self.silent_samples += scratch.len();
            // A stateful graph tail must never become media or activity.
            scratch.fill(9.0);
        }
    }
    fn tick(links: &mut LinkAudio<ClockedInput>, controller: &mut NodeController, count: usize) {
        links
            .process(
                controller,
                false,
                &mut [0.0; 960][..count],
                SourcePosition::default(),
            )
            .unwrap();
    }
    let mut input = ClockedInput {
        signals: PeerSignals::new(),
        available: 0,
        real: Some(0),
        finite: None,
        renders: 0,
        rendered_samples: 0,
        silent_samples: 0,
    };
    assert!(!input.source_timing_available());
    assert_eq!(input.render_timed_sample(), None);
    assert_eq!(
        input.renders, 0,
        "absent timed capability cannot consume legacy input"
    );
    let (writer, _reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, input, writer, 960, 0).unwrap();
    let (mut links, _dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, _control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for count in [3, 17] {
        tick(&mut links, &mut controller, count);
    }
    assert_eq!(links.peers[0].input.silent_samples, 20);
    assert_eq!(links.peers[0].input.renders, 0);
    assert_eq!(links.peers[0].input.available, 0);
    assert!(!links.peers[0].active);
    assert_eq!(links.peers[0].input.signals.activity_edge(), 0);
    assert!(
        links.peers[0].audio[..17]
            .iter()
            .all(|sample| *sample == 0.0)
    );

    links.peers[0].input.available = 12_480;
    links.peers[0].input.real = Some(480);
    links.peers[0].input.signals.publish_pcm();
    links.peers[0].input.signals.set_radio_keyed(true);
    tick(&mut links, &mut controller, 480);
    assert!(links.peers[0].active);
    links.peers[0].input.available = 0;
    links.peers[0].input.real = Some(0);
    links.peers[0].input.signals.set_radio_keyed(false);
    for _ in 0..3 {
        tick(&mut links, &mut controller, 960);
    }
    assert_eq!(links.peers[0].input.renders, 4);
    assert_eq!(links.peers[0].input.rendered_samples, 480 + 2_880);
    assert_eq!(links.peers[0].input.silent_samples, 20);
    assert!(!links.peers[0].active);
    let unkey_edge = links.peers[0].input.signals.activity_edge();
    tick(&mut links, &mut controller, 479);
    assert_eq!(links.peers[0].input.renders, 4);
    assert_eq!(links.peers[0].input.silent_samples, 499);
    assert_eq!(links.peers[0].input.signals.activity_edge(), unkey_edge);

    links.peers[0].input.finite = Some(true);
    tick(&mut links, &mut controller, 17);
    assert_eq!(links.peers[0].input.renders, 5);
    assert_eq!(links.peers[0].input.silent_samples, 499);
    links.peers[0].input.finite = Some(false);
    tick(&mut links, &mut controller, 1);
    links.peers[0].input.finite = Some(true);
    links.peers[0].input.signals.end();
    tick(&mut links, &mut controller, 3);
    assert_eq!(links.peers[0].input.renders, 5);
    assert_eq!(links.peers[0].input.silent_samples, 503);
    assert!(!links.peers[0].active);
}

struct FiniteInput {
    signals: PeerSignals,
    samples: std::collections::VecDeque<f32>,
}
impl PeerInput for FiniteInput {
    fn source_rate(&self) -> u32 {
        48000
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        self.samples.len() as u64
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        let count = output.len().min(self.samples.len());
        for sample in &mut output[..count] {
            *sample = self.samples.pop_front().unwrap();
        }
        output[count..].fill(0.0);
        Some(count)
    }
}

#[test]
fn retained_private_policy_holds_network_isolation_until_confirmed() {
    use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};
    let private = PhoneSource::new(
        PhoneAccountId::new("account").unwrap(),
        1,
        PhoneCallId::new([3; 16]).unwrap(),
        1,
        3,
    )
    .unwrap();
    let handset = PeerSignals::for_phone(private.clone());
    assert!(handset.authenticate_phone());
    let grant = handset.reply_target();
    let permit = Arc::new(AtomicBool::new(false));
    let queue = || LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let (source_writer, _source_out) = queue();
    let (dest_writer, mut dest_out) = queue();
    let (handset_writer, mut handset_out) = queue();
    let source = FiniteInput {
        signals: PeerSignals::new(),
        samples: vec![0.25; 48_000 * 260 / 1_000].into(),
    };
    source.signals.publish_pcm();
    source.signals.set_radio_keyed(true);
    let direct = AudioPeer::new("100", Mode::TRANSCEIVE, source, source_writer, 4, 0).unwrap();
    let dest = AudioPeer::new(
        "200",
        Mode::TRANSCEIVE,
        FiniteInput {
            signals: PeerSignals::new(),
            samples: Default::default(),
        },
        dest_writer,
        4,
        0,
    )
    .unwrap();
    let caller = AudioPeer::for_phone(
        &private,
        &grant,
        Mode::phone(false),
        FiniteInput {
            signals: PeerSignals::redirected(&grant),
            samples: Default::default(),
        },
        handset_writer,
        4,
    )
    .unwrap()
    .with_phone_policy(false, true, Arc::clone(&permit));
    let (mut graph, mut dispatch) = LinkAudio::new(vec![direct, dest, caller], 4).unwrap();
    let (mut node, _control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for confirmed in [false, true] {
        permit.store(confirmed, Ordering::Release);
        let mut rf = [0.0; 4];
        graph
            .process(&mut node, false, &mut rf, SourcePosition::default())
            .unwrap();
        dispatch.dispatch(1);
        let mut samples = [0.0; 4];
        assert_eq!(dest_out.read_span(&mut samples).is_some(), confirmed);
        assert_eq!(samples, if confirmed { [0.25; 4] } else { [0.0; 4] });
        let mut handset_pcm = [0.0; 4];
        assert_eq!(handset_out.read_span(&mut handset_pcm).is_some(), confirmed);
        assert_eq!(handset_pcm, samples);
    }
}

#[test]
fn attached_phone_destinations_receive_mix_minus_without_expanding_public_forwarding() {
    use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};

    const A: [f32; 4] = [0.125, -0.125, 0.125, -0.125];
    const B: [f32; 4] = [0.25, -0.25, -0.25, 0.25];
    const LOCAL: [f32; 4] = [0.0625, 0.0625, -0.0625, -0.0625];
    const LINK: [f32; 4] = [0.03125; 4];
    let queue = || LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
    let source = |id: u8| {
        PhoneSource::new(
            PhoneAccountId::new("account").unwrap(),
            1,
            PhoneCallId::new([id; 16]).unwrap(),
            u64::from(id),
            u64::from(id),
        )
        .unwrap()
    };
    let read = |output: &mut LinkAudioConsumer| {
        let mut samples = [0.0; 4];
        output.read_span(&mut samples).map(|span| {
            assert_eq!(span.frames, samples.len());
            samples
        })
    };
    let active_input = |signals: PeerSignals, samples: [f32; 4]| {
        signals.publish_pcm();
        signals.set_radio_keyed(true);
        FiniteInput {
            signals,
            samples: samples.repeat(3_200).into(),
        }
    };
    let unattached = source(3);
    let unverified = PeerSignals::for_phone(unattached.clone());
    let reply = unverified.reply_target();
    let (writer, _) = queue();
    assert!(
        AudioPeer::for_phone(
            &unattached,
            &reply,
            Mode::phone(false),
            active_input(unverified, [0.5; 4]),
            writer,
            4,
        )
        .is_err()
    );

    // Prove the already-supported forwarding mode before exercising local-only policy.
    for forward_public in [true, false] {
        let phone = |id, samples| {
            let source = source(id);
            let signals = PeerSignals::for_phone(source.clone());
            assert!(signals.authenticate_phone());
            let reply = signals.reply_target();
            let (writer, output) = queue();
            let peer = AudioPeer::for_phone(
                &source,
                &reply,
                Mode::phone(forward_public),
                active_input(signals, samples),
                writer,
                4,
            )
            .unwrap();
            (peer, output)
        };
        let (a, mut a_output) = phone(1, A);
        let (b, mut b_output) = phone(2, B);
        let (writer, mut linked_output) = queue();
        let link = AudioPeer::new(
            "200",
            Mode::TRANSCEIVE,
            FiniteInput {
                signals: PeerSignals::new(),
                samples: Default::default(),
            },
            writer,
            4,
            0,
        )
        .unwrap();
        let (mut graph, mut dispatcher) = LinkAudio::new(vec![a, b, link], 4).unwrap();
        let (mut node, _) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();

        graph
            .process(&mut node, false, &mut [0.0; 4], SourcePosition::default())
            .unwrap();
        dispatcher.dispatch(1);
        let phones_only = (read(&mut a_output), read(&mut b_output)) == (Some(B), Some(A));
        let both_callers = std::array::from_fn(|i| A[i] + B[i]);
        let public_phone_only = read(&mut linked_output) == forward_public.then_some(both_callers);

        let linked = &mut graph.peers[2].input;
        linked.samples = LINK.repeat(3_200).into();
        linked.signals.publish_pcm();
        linked.signals.set_radio_keyed(true);
        let mut local = LOCAL;
        graph
            .process(&mut node, true, &mut local, SourcePosition::default())
            .unwrap();
        dispatcher.dispatch(1);
        let mixed_a = std::array::from_fn(|i| LOCAL[i] + LINK[i] + B[i]);
        let mixed_b = std::array::from_fn(|i| LOCAL[i] + LINK[i] + A[i]);
        let mixed_phones =
            (read(&mut a_output), read(&mut b_output)) == (Some(mixed_a), Some(mixed_b));
        let public_expected =
            std::array::from_fn(|i| LOCAL[i] + if forward_public { both_callers[i] } else { 0.0 });
        let public_mixed = read(&mut linked_output) == Some(public_expected);

        graph.peers[2]
            .input
            .signals
            .reply_target()
            .set_rx_muted(true);
        let mut local = LOCAL;
        graph
            .process(&mut node, true, &mut local, SourcePosition::default())
            .unwrap();
        dispatcher.dispatch(1);
        let muted_a = std::array::from_fn(|i| LOCAL[i] + B[i]);
        let muted_b = std::array::from_fn(|i| LOCAL[i] + A[i]);
        let muted_link =
            (read(&mut a_output), read(&mut b_output)) == (Some(muted_a), Some(muted_b));
        let public_receive_mute = read(&mut linked_output) == Some(public_expected);
        eprintln!(
            "PHONE_MIX_MINUS forward_public={forward_public} phones_only={phones_only} mixed_phones={mixed_phones} muted_link={muted_link} public_phone_only={public_phone_only} public_mixed={public_mixed} public_receive_mute={public_receive_mute}"
        );
        assert!(
            public_phone_only && public_mixed && public_receive_mute,
            "phone-to-phone audio must preserve public forwarding policy"
        );
        assert!(
            phones_only && mixed_phones && muted_link,
            "each attached caller must receive the other caller and eligible shared sources without its own PCM"
        );

        graph.peers[1].mode = Mode::LOCAL_MONITOR;
        let mut local = LOCAL;
        graph
            .process(&mut node, true, &mut local, SourcePosition::default())
            .unwrap();
        dispatcher.dispatch(1);
        assert!(
            graph.peers[1].active,
            "the monitor source must still be active"
        );
        let caller = read(&mut a_output) == Some(LOCAL);
        let monitor_quiet = read(&mut b_output).is_none();
        let public_expected =
            std::array::from_fn(|i| LOCAL[i] + if forward_public { A[i] } else { 0.0 });
        let public = read(&mut linked_output) == Some(public_expected);
        eprintln!(
            "PHONE_LOCAL_MONITOR forward_public={forward_public} caller={caller} monitor_quiet={monitor_quiet} public={public}"
        );
        assert!(
            caller && monitor_quiet && public,
            "an RF-only monitor must retain its ordinary media routing"
        );
    }
}

#[test]
fn authenticated_phone_hears_only_the_selected_network_group_member() {
    use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};
    use std::num::NonZeroUsize;

    let selection = GroupSelection::new();
    let first = NonZeroUsize::new(1).unwrap();
    let second = NonZeroUsize::new(2).unwrap();
    let queue = || LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
    let empty = |signals| FiniteInput {
        signals,
        samples: Default::default(),
    };
    let link = |name, slot| {
        let (writer, _) = queue();
        AudioPeer::new(
            name,
            Mode::TRANSCEIVE,
            empty(PeerSignals::new()),
            writer,
            4,
            0,
        )
        .map(|peer| peer.with_group(selection.clone(), slot))
        .unwrap()
    };
    let phone = PhoneSource::new(
        PhoneAccountId::new("account").unwrap(),
        1,
        PhoneCallId::new([5; 16]).unwrap(),
        1,
        5,
    )
    .unwrap();
    let signals = PeerSignals::for_phone(phone.clone());
    assert!(signals.authenticate_phone());
    let reply = signals.reply_target();
    let (writer, mut output) = queue();
    let caller = AudioPeer::for_phone(
        &phone,
        &reply,
        Mode::phone(false),
        empty(signals),
        writer,
        4,
    )
    .unwrap();
    let (mut graph, mut dispatcher) =
        LinkAudio::new(vec![link("200", first), link("300", second), caller], 4).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut playout = |graph: &mut LinkAudio<FiniteInput>| {
        graph
            .process(&mut node, false, &mut [0.0; 4], SourcePosition::default())
            .unwrap();
        dispatcher.dispatch(1);
        let mut pcm = [0.0; 4];
        output.read_span(&mut pcm).map(|span| {
            assert_eq!(span.frames, pcm.len());
            pcm
        })
    };

    selection.publish_desired(Some(second));
    assert_eq!(playout(&mut graph), None);
    assert_eq!(selection.active(), Some(second));
    for (peer, level) in graph.peers[..2].iter_mut().zip([0.5, 0.25]) {
        peer.input.samples = vec![level; 12_512].into();
        peer.input.signals.publish_pcm();
        peer.input.signals.set_radio_keyed(true);
    }
    assert_eq!(playout(&mut graph), Some([0.25; 4]));

    selection.publish_desired(Some(first));
    assert_eq!(playout(&mut graph), Some([0.25; 4]));
    assert_eq!(selection.active(), Some(second));
    for peer in &mut graph.peers[..2] {
        peer.input.signals.reply_target().set_rx_muted(true);
    }
    assert_eq!(playout(&mut graph), None);
    assert_eq!(selection.active(), Some(first));
    for peer in &mut graph.peers[..2] {
        peer.input.signals.reply_target().set_rx_muted(false);
    }
    assert_eq!(playout(&mut graph), Some([0.5; 4]));
}

#[test]
fn outgoing_phone_claims_revoke_queued_pcm_independently() {
    let isolation = Arc::new(crate::link::MediaIsolation::default());
    let (writer, mut wire) = LinkAudioQueue::new(32, 4).unwrap().into_endpoints();
    let input = FiniteInput {
        signals: PeerSignals::new(),
        samples: Default::default(),
    };
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, input, writer, 4, 0).unwrap();
    let (audio, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
    let mut audio = audio.with_media_isolation(&mut dispatcher, Arc::clone(&isolation));
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    let mut mix = |value| {
        audio
            .process(&mut node, true, &mut [value; 4], SourcePosition::default())
            .unwrap()
    };
    mix(0.25);
    dispatcher.dispatch(1);
    mix(0.50);
    let first = isolation.hold().unwrap();
    let second = isolation.hold().unwrap();
    dispatcher.dispatch(1);
    assert_eq!(
        wire.read_at_isolation_epoch(&mut [0.0; 4], isolation.epoch()),
        4
    );
    drop(first);
    assert_eq!(isolation.epoch() & 1, 1);
    mix(0.75);
    dispatcher.dispatch(1);
    assert!(wire.peek_span().is_none());
    drop(second);
    mix(1.0);
    dispatcher.dispatch(1);
    let mut samples = [0.0; 4];
    assert_eq!(
        wire.read_at_isolation_epoch(&mut samples, isolation.epoch()),
        0
    );
    assert_eq!(samples, [1.0; 4]);
}

#[test]
fn remote_base_link_activity_remains_visible_before_transmit_muting() {
    let input = FiniteInput {
        signals: PeerSignals::new(),
        samples: [0.25; 4].into_iter().collect(),
    };
    input.signals.set_radio_keyed(true);
    input.signals.publish_pcm();
    let (writer, _reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, input, writer, 4, 0).unwrap();
    let (mut audio, _dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
    let (mut node, _control) = NodeController::new(
        ControllerSettings {
            full_duplex: false,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(!audio.peer_source_active());
    assert!(
        !audio
            .process(&mut node, true, &mut [0.0; 4], SourcePosition::default())
            .unwrap()
    );
    assert!(audio.peer_source_active());
    audio.peers[0].input.signals.end();
    audio
        .process(&mut node, false, &mut [0.0; 4], SourcePosition::default())
        .unwrap();
    assert!(!audio.peer_source_active());
}

#[test]
fn regeneration_replaces_rf_not_peer_program_or_parrot_capture() {
    let input = |samples: &[f32]| FiniteInput {
        signals: PeerSignals::new(),
        samples: samples.iter().copied().collect(),
    };
    let (writer, mut reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, input(&[]), writer, 4, 0).unwrap();
    let (audio, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
    let mut audio = audio.with_regeneration(input(&[0.25, 0.0]));
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut rf = [0.5; 4];
    audio
        .process(&mut node, true, &mut rf, SourcePosition::default())
        .unwrap();
    assert_eq!(rf, [0.25, 0.0, 0.5, 0.5]);
    dispatcher.dispatch(1);
    let mut remote = [0.0; 4];
    assert_eq!(reader.read(&mut remote), 0);
    assert_eq!(remote, [0.5; 4]);
    audio
        .process(&mut node, false, &mut [0.0], SourcePosition::default())
        .unwrap();
    assert_eq!(control.take_parrot_capture().unwrap().samples(), &[0.5; 4]);
    assert!(
        audio
            .process(&mut node, false, &mut [0.0; 5], SourcePosition::default())
            .is_err()
    );
}

#[test]
fn isolated_phone_programme_cannot_replay_after_its_claim_ends() {
    use crate::{
        audio::{PcmRead, PcmStreamReader},
        controller::{ControllerControl, PreparedMedia},
        link::MediaIsolation,
        media::PreparedAudio,
    };

    struct Recording {
        audio: PreparedAudio,
        cursor: usize,
    }
    impl PcmStreamReader for Recording {
        fn start(&mut self) {
            self.cursor = 0;
        }

        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            let remaining = &self.audio.samples()[self.cursor..];
            let count = output.len().min(remaining.len());
            output[..count].copy_from_slice(&remaining[..count]);
            self.cursor += count;
            if count == remaining.len() {
                PcmRead::FinalSamples(count)
            } else {
                PcmRead::Samples(count)
            }
        }
    }

    fn prepare_capture(control: &mut ControllerControl) -> bool {
        let Some(clip) = control.take_parrot_capture() else {
            return false;
        };
        let audio = PreparedAudio::new(48_000, clip.samples().to_vec()).unwrap();
        control.recycle_parrot_capture(clip);
        assert!(
            control.queue_prepared_parrot(PreparedMedia::new_parrot_stream(
                None,
                Box::new(Recording { audio, cursor: 0 }),
            ))
        );
        true
    }

    let isolation = Arc::new(MediaIsolation::default());
    let (output, mut remote) = LinkAudioQueue::new(32, 4).unwrap().into_endpoints();
    let input = FiniteInput {
        signals: PeerSignals::new(),
        samples: Default::default(),
    };
    let direct = AudioPeer::new("200", Mode::TRANSCEIVE, input, output, 4, 0).unwrap();
    let (graph, mut dispatch) = LinkAudio::new(vec![direct], 4).unwrap();
    let mut graph = graph.with_media_isolation(&mut dispatch, Arc::clone(&isolation));
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut tick = |receiving, level| {
        let mut station = [level; 4];
        graph
            .process(
                &mut node,
                receiving,
                &mut station,
                SourcePosition::default(),
            )
            .unwrap();
        dispatch.dispatch(1);
        let mut peer = [0.0; 4];
        remote.read_at_isolation_epoch(&mut peer, isolation.epoch());
        (station, peer)
    };

    tick(true, 0.25);
    tick(false, 0.0);
    assert!(
        prepare_capture(&mut control),
        "ordinary parrot must capture"
    );
    assert_eq!(
        tick(false, 0.0),
        ([0.25; 4], [0.25; 4]),
        "ordinary parrot must replay"
    );
    control.reclaim().for_each(drop);

    // Start a burst in the normal epoch and put unique RF programme on the isolated call.
    tick(true, 0.125);
    let claim = isolation.hold().unwrap();
    let (_, peer) = tick(true, 0.875);
    assert_eq!(peer, [0.0; 4], "the live isolation gate must work");
    tick(false, 0.0);
    // Exercise the off-audio copy and queue before the call drops its claim.
    let captured = prepare_capture(&mut control);
    drop(claim);
    let leaked = (0..3).any(|_| {
        let (station, peer) = tick(false, 0.0);
        station
            .into_iter()
            .chain(peer)
            .any(|sample| sample == 0.875)
    });
    eprintln!(
        "A5_PARROT_ISOLATION ordinary_replay=true live_isolation=true captured={captured} private_replay={leaked}"
    );
    assert!(
        !leaked,
        "station parrot replay exposed isolated telephone-call programme"
    );
}

#[test]
fn configured_message_broadcast_obeys_destination_and_peer_mode() {
    use crate::controller::{MorseSettings, PlaybackDestination, PlaybackPolicy, PreparedMedia};
    for (destination, mode, expected) in [
        (PlaybackDestination::Local, Mode::TRANSCEIVE, false),
        (PlaybackDestination::LocalAndLinks, Mode::TRANSCEIVE, true),
        (PlaybackDestination::LocalAndLinks, Mode::MONITOR, false),
        (PlaybackDestination::Peers, Mode::TRANSCEIVE, true),
        (PlaybackDestination::Peers, Mode::MONITOR, false),
    ] {
        let (mut peer, _, mut output) = peer_with_kerchunk("200", 4, 0);
        peer.mode = mode;
        let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
        assert!(
            control.queue_prepared_status(
                PreparedMedia::new(Some(vec![0.25; 4]), "E", MorseSettings::default())
                    .unwrap()
                    .with_playback_policy(PlaybackPolicy {
                        destination,
                        ..Default::default()
                    })
            )
        );
        let mut rf = [0.0; 4];
        let keyed = links
            .process(&mut controller, false, &mut rf, SourcePosition::default())
            .unwrap();
        if destination == PlaybackDestination::Peers {
            assert!(!keyed);
            assert_eq!(rf, [0.0; 4]);
        } else {
            assert_eq!(rf, [0.25; 4]);
        }
        dispatcher.dispatch(1);
        let mut remote = [0.0; 4];
        assert_eq!(output.read_span(&mut remote).is_some(), expected);
        assert_eq!(remote, if expected { [0.25; 4] } else { [0.0; 4] });
    }
}

#[test]
fn configured_message_suppression_drains_peers_without_negative_mix_minus() {
    use crate::controller::{MorseSettings, PlaybackPolicy, PreparedMedia};
    let mut peers = Vec::new();
    let mut readers = Vec::new();
    for (name, sample) in [("200", 0.1), ("300", 0.2)] {
        let signals = PeerSignals::new();
        signals.publish_pcm();
        signals.set_radio_keyed(true);
        let (writer, reader) = LinkAudioQueue::new(4, 64).unwrap().into_endpoints();
        peers.push(
            AudioPeer::new(
                name,
                Mode::TRANSCEIVE,
                FiniteInput {
                    signals,
                    samples: std::iter::repeat_n(sample, 12_480).collect(),
                },
                writer,
                4,
                0,
            )
            .unwrap(),
        );
        readers.push(reader);
    }
    let (mut links, mut dispatcher) = LinkAudio::new(peers, 4).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            telemetry_duck_db: 0,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
    assert!(
        control.queue_prepared_status(
            PreparedMedia::new(Some(vec![0.75; 48]), "E", MorseSettings::default())
                .unwrap()
                .with_playback_policy(PlaybackPolicy {
                    repeat_during_playback: false,
                    ..Default::default()
                })
        )
    );
    controller.process_audio(false, false, &[], &mut [0.0]);
    links
        .process(
            &mut controller,
            true,
            &mut [0.3; 4],
            SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    for reader in &mut readers {
        let mut remote = [0.0; 4];
        assert!(reader.read_span(&mut remote).is_some());
        assert_eq!(
            remote, [0.0; 4],
            "suppressed own contribution must not be subtracted"
        );
    }
    assert!(
        links
            .peers
            .iter()
            .all(|peer| peer.input.samples.len() == 12_476 && peer.active)
    );
}

#[test]
fn egress_dispatch_notifies_receive_only_activity_without_echoing_pcm() {
    let (mut peer, signals, mut output) = peer_with_kerchunk("200", 960, 0);
    peer.mode = Mode::MONITOR;
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    signals.set_radio_keyed(true);
    let mut notified = Vec::new();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 960],
            SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        notified,
        [0],
        "inbound activity must wake its idle serialized owner"
    );
    assert!(
        output.read_span(&mut [0.0; 960]).is_none(),
        "an activity wake must not echo input"
    );
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 960],
            SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        notified,
        [0],
        "unchanged activity must not poll an idle owner"
    );
}

#[test]
fn egress_batched_mix_minus_preserves_routes() {
    use crate::audio::AudioSpan;
    let (mut full, _, _full_output) = peer_with_kerchunk("200", 4, 0);
    let (healthy, _, mut output) = peer_with_kerchunk("300", 4, 0);
    full.outbound
        .as_mut()
        .unwrap()
        .write(AudioSpan::default(), &[0.9; 4]);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![full, healthy], 4).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let source = SourcePosition {
        generation: 8,
        sample: 500,
        ..SourcePosition::default()
    };
    links
        .process(&mut controller, true, &mut [0.25; 2], source)
        .unwrap();
    assert_eq!(dispatcher.dispatch_batch(0, |_| {}).destinations, 0);
    let mut notified = Vec::new();
    let first = dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!(
        (first.destinations, first.completed_blocks, first.pending),
        (1, 0, true)
    );
    assert!(
        notified.is_empty(),
        "a full peer does not claim accepted audio"
    );
    links
        .process(
            &mut controller,
            true,
            &mut [0.5; 2],
            SourcePosition {
                sample: 502,
                ..source
            },
        )
        .unwrap();
    links
        .process(
            &mut controller,
            true,
            &mut [0.75; 2],
            SourcePosition {
                sample: 504,
                ..source
            },
        )
        .unwrap();
    assert_eq!(
        links.status().dropped_blocks(),
        1,
        "partial dispatch cannot recycle its block early"
    );
    let second = dispatcher.dispatch_batch(1, |index| notified.push(index));
    assert_eq!((second.destinations, second.completed_blocks), (1, 1));
    assert_eq!(notified, [1]);
    let mut received = [0.0; 4];
    let span = output.read_span(&mut received).unwrap();
    assert_eq!(
        (span.frames, span.span.generation, span.span.source_start),
        (2, 8, 500)
    );
    assert_eq!(&received[..2], &[0.25; 2]);
    assert_eq!(dispatcher.dispatch_batch(2, |_| {}).completed_blocks, 1);
    assert_eq!(
        output.read_span(&mut received).unwrap().span.source_start,
        502
    );
    assert_eq!(&received[..2], &[0.5; 2]);
    assert!(!dispatcher.dispatch_batch(1, |_| {}).pending);
    links
        .process(
            &mut controller,
            true,
            &mut [0.6; 2],
            SourcePosition {
                sample: 506,
                ..source
            },
        )
        .unwrap();
    dispatcher.dispatch_batch(2, |_| {});
    let after_drop = output.read_span(&mut received).unwrap();
    assert_eq!(
        (after_drop.span.source_start, after_drop.span.burst),
        (506, 1)
    );
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            SourcePosition {
                sample: 508,
                ..source
            },
        )
        .unwrap();
    assert_eq!(
        dispatcher.dispatch_batch(1, |_| {}).destinations,
        1,
        "disabled peers consume the batch budget too"
    );
    dispatcher.dispatch_batch(1, |_| {});
    assert!(output.read_span(&mut received).is_none());
    links
        .process(
            &mut controller,
            true,
            &mut [0.7],
            SourcePosition {
                generation: 9,
                sample: 509,
                ..source
            },
        )
        .unwrap();
    dispatcher.dispatch_batch(2, |_| {});
    let next_burst = output.read_span(&mut received).unwrap();
    assert_eq!(
        (
            next_burst.span.generation,
            next_burst.span.source_start,
            next_burst.span.burst
        ),
        (9, 509, 2)
    );
}

struct Input {
    signals: Arc<PeerSignals>,
}

impl PeerInput for Input {
    fn source_rate(&self) -> u32 {
        48_000
    }

    fn signals(&self) -> &PeerSignals {
        &self.signals
    }

    fn available(&self) -> u64 {
        0
    }

    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        output.fill(0.0);
        None
    }
}

fn peer(name: &str, maximum: usize) -> (AudioPeer<Input>, Arc<PeerSignals>) {
    let (peer, signals, _) = peer_with_kerchunk(name, maximum, 0);
    (peer, signals)
}

fn peer_with_kerchunk(
    name: &str,
    maximum: usize,
    kerchunk_ms: u32,
) -> (AudioPeer<Input>, Arc<PeerSignals>, LinkAudioConsumer) {
    let (outbound, inbound) = LinkAudioQueue::new(maximum.max(1), 64)
        .unwrap()
        .into_endpoints();
    let signals = Arc::new(PeerSignals::new());
    (
        AudioPeer::new(
            name,
            Mode::TRANSCEIVE,
            Input {
                signals: Arc::clone(&signals),
            },
            outbound,
            maximum,
            kerchunk_ms,
        )
        .unwrap(),
        signals,
        inbound,
    )
}

#[test]
fn peer_replies_wait_for_origin_and_route_each_sample_without_local_ptt() {
    use crate::controller::{MorseSettings, PreparedMedia};
    let (mut first, first_signal, mut first_output) = peer_with_kerchunk("200", 8, 0);
    first.mode = Mode::MONITOR;
    let (second, second_signal, mut second_output) = peer_with_kerchunk("300", 8, 0);
    let selection = GroupSelection::new();
    let second = second.with_group(selection, std::num::NonZeroUsize::new(1).unwrap());
    let (mut links, mut dispatcher) = LinkAudio::new(vec![first, second], 8).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for (signal, value) in [(&first_signal, 0.2), (&second_signal, 0.4)] {
        let media = PreparedMedia::new(Some(vec![value; 2]), "", MorseSettings::default())
            .unwrap()
            .reply_to(Some(signal.reply_target()));
        assert!(control.queue_prepared_status(media));
    }
    first_signal.set_radio_keyed(true);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    assert_eq!(first_output.read(&mut [0.0]), 1);
    assert_eq!(second_output.read(&mut [0.0]), 1);
    first_signal.set_radio_keyed(false);
    let mut radio = [0.0; 8];
    assert!(
        !links
            .process(
                &mut controller,
                false,
                &mut radio,
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    dispatcher.dispatch(1);
    assert_eq!(radio, [0.0; 8]);
    let mut first_pcm = [0.0; 8];
    let mut second_pcm = [0.0; 8];
    assert_eq!(first_output.read(&mut first_pcm), 0);
    assert_eq!(second_output.read(&mut second_pcm), 0);
    assert_eq!(first_pcm, [0.2, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(second_pcm, [0.0, 0.0, 0.4, 0.4, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(control.reclaim().count(), 2);
}

#[test]
fn ended_peer_reply_never_reaches_same_number_replacement() {
    use crate::controller::{MorseSettings, PreparedMedia};
    let old = PeerSignals::new();
    let media = PreparedMedia::new(Some(vec![0.4]), "", MorseSettings::default())
        .unwrap()
        .reply_to(Some(old.reply_target()));
    old.end();
    let (replacement, _, mut output) = peer_with_kerchunk("200", 1, 0);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![replacement], 1).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.queue_prepared_status(media));
    assert!(
        !links
            .process(
                &mut controller,
                false,
                &mut [0.0],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    dispatcher.dispatch(1);
    assert_eq!(output.read(&mut [0.0]), 1);
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn addressed_reply_waits_for_local_receive_and_keeps_monitor_routes_scoped() {
    use crate::controller::{MorseSettings, PreparedMedia};
    for fallback in [false, true] {
        let (mut peer, signal, mut output) = peer_with_kerchunk("200", 4, 0);
        if fallback {
            peer = peer.with_group(
                GroupSelection::new(),
                std::num::NonZeroUsize::new(1).unwrap(),
            );
        } else {
            peer.mode = Mode::MONITOR;
        }
        let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 4).unwrap();
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: false,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        let media = PreparedMedia::new(Some(vec![0.4; 2]), "E", MorseSettings::default())
            .unwrap()
            .reply_to(Some(signal.reply_target()));
        assert!(control.queue_prepared_status(media));
        let mut radio = [0.7; 4];
        assert!(
            !links
                .process(
                    &mut controller,
                    true,
                    &mut radio,
                    crate::link::SourcePosition::default()
                )
                .unwrap()
        );
        dispatcher.dispatch(1);
        let mut sent = [0.0; 4];
        assert_eq!(output.read(&mut sent), 4);
        assert_eq!(sent, [0.0; 4]);
        assert_eq!(radio, [0.0; 4]);
        assert_eq!(control.reclaim().count(), 0, "the reply waits for unkey");
        assert!(
            !links
                .process(
                    &mut controller,
                    false,
                    &mut radio,
                    crate::link::SourcePosition::default()
                )
                .unwrap()
        );
        dispatcher.dispatch(1);
        assert_eq!(output.read(&mut sent), 0);
        assert_eq!(sent, [0.4, 0.4, 0.0, 0.0]);
        assert_eq!(radio, [0.0; 4]);
        assert_eq!(control.reclaim().count(), 1);
    }
}

#[test]
fn preparation_and_empty_graph_reject_invalid_shapes_without_dispatch() {
    for (name, maximum) in [("bad", 1), ("200", 0)] {
        let (outbound, _) = LinkAudioQueue::new(1, 64).unwrap().into_endpoints();
        assert!(
            AudioPeer::new(
                name,
                Mode::TRANSCEIVE,
                Input {
                    signals: Arc::new(PeerSignals::new()),
                },
                outbound,
                maximum,
                0,
            )
            .is_err()
        );
    }

    assert!(LinkAudio::<Input>::new(vec![], 0).is_err());
    let (undersized, _) = peer("200", 1);
    assert!(LinkAudio::new(vec![undersized], 2).is_err());

    let (mut empty, mut dispatcher) = LinkAudio::<Input>::new(vec![], 1).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let status = empty.status();
    assert_eq!(status.last_keyed(), None);
    assert!(
        !empty
            .process(
                &mut controller,
                false,
                &mut [],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    assert!(
        !empty
            .process(
                &mut controller,
                false,
                &mut [0.0],
                crate::link::SourcePosition::default()
            )
            .unwrap()
    );
    assert_eq!(dispatcher.dispatch(1), 0);

    let (prepared, _) = peer("200", 1);
    let (mut links, _) = LinkAudio::new(vec![prepared], 1).unwrap();
    assert!(
        links
            .process(
                &mut controller,
                false,
                &mut [0.0, 0.0],
                crate::link::SourcePosition::default()
            )
            .is_err()
    );
}

#[test]
fn grouped_member_selection_and_last_keyed_status_follow_callback_edges() {
    let selection = GroupSelection::new();
    let slot = std::num::NonZeroUsize::new(1).unwrap();
    selection.publish_desired(Some(slot));
    let (prepared, signals) = peer("200", 1);
    let member = GroupMemberSelection::new("primary", slot, selection);
    assert_eq!(member.label(), "primary");
    assert_eq!(member.slot(), slot);
    let prepared = prepared.with_group_member(member);
    let (mut links, _) = LinkAudio::new(vec![prepared], 1).unwrap();
    let status = links.status();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert_eq!(status.last_keyed(), None);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    signals.set_radio_keyed(true);
    links
        .process(
            &mut controller,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(status.last_keyed(), Some("200"));
}

#[test]
fn short_and_long_link_bursts_apply_kerchunk_limit() {
    for (frames, expect_courtesy) in [(1, false), (4_801, true)] {
        let (prepared, signals, mut outbound) = peer_with_kerchunk("200", 4_802, 100);
        let (mut links, _) = LinkAudio::new(vec![prepared], 4_802).unwrap();
        let courtesy = CourtesySettings {
            link: Some(
                crate::controller::PreparedMedia::new(
                    Some(vec![0.5]),
                    "",
                    crate::controller::MorseSettings::default(),
                )
                .unwrap(),
            ),
            ..CourtesySettings::default()
        };
        let (mut controller, _) = NodeController::new(
            ControllerSettings {
                courtesy_delay_ms: 0,
                hang_ms: 100,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            courtesy,
        )
        .unwrap();

        signals.set_radio_keyed(true);
        links
            .process(
                &mut controller,
                false,
                &mut vec![0.0; frames],
                crate::link::SourcePosition::default(),
            )
            .unwrap();
        signals.set_radio_keyed(false);
        let mut output = [0.0];
        let keyed = links
            .process(
                &mut controller,
                false,
                &mut output,
                crate::link::SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(keyed, expect_courtesy, "a link kerchunk has no hang");
        assert_eq!(outbound.read(&mut [0.0]), 1);
        assert_eq!(output[0] == 0.5, expect_courtesy);
    }
}

#[test]
fn ended_peer_cannot_hold_ptt_or_remain_a_dispatch_destination() {
    for was_active in [false, true] {
        let (prepared, signals, mut outbound) = peer_with_kerchunk("201", 1, 0);
        let (mut links, mut dispatcher) = LinkAudio::new(vec![prepared], 1).unwrap();
        let (mut controller, _) = NodeController::new(
            ControllerSettings {
                hang_ms: 0,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        signals.set_radio_keyed(true);
        if was_active {
            assert!(
                links
                    .process(
                        &mut controller,
                        false,
                        &mut [0.0],
                        SourcePosition::default()
                    )
                    .unwrap()
            );
            assert_eq!(links.active_count(), 1);
        }
        signals.end();
        // An in-flight source read may publish its old key after control ends it.
        signals.set_radio_keyed(true);
        let keyed = links
            .process(
                &mut controller,
                false,
                &mut [0.0],
                SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(links.active_count(), 0);
        assert!(!keyed, "an ended peer must not hold a silent carrier");
        assert_eq!(dispatcher.dispatch(1), 1);
        let mut output = [1.0];
        assert_eq!(outbound.read(&mut output), 1);
        assert_eq!(output, [0.0]);
    }
}

#[test]
fn operator_mute_drains_each_input_and_gates_its_pcm_and_key_only() {
    struct Wave {
        signals: PeerSignals,
        level: f32,
        samples: usize,
    }
    impl PeerInput for Wave {
        fn signals(&self) -> &PeerSignals {
            &self.signals
        }
        fn source_rate(&self) -> u32 {
            48000
        }
        fn available(&self) -> u64 {
            12480
        }
        fn render(&mut self, output: &mut [f32]) -> Option<usize> {
            self.samples += output.len();
            output.fill(self.level);
            Some(output.len())
        }
    }
    let mut peers = vec![];
    let mut tokens = vec![];
    for (name, level) in [("2000", 0.25), ("3000", 0.5)] {
        let sig = PeerSignals::new();
        sig.publish_pcm();
        sig.set_radio_keyed(true);
        tokens.push(sig.reply_target());
        let (w, _r) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
        peers.push(
            AudioPeer::new(
                name,
                Mode::TRANSCEIVE,
                Wave {
                    signals: sig,
                    level,
                    samples: 0,
                },
                w,
                960,
                0,
            )
            .unwrap(),
        );
    }
    let (mut mixer, _) = LinkAudio::new(peers, 960).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for (a, b, level) in [(true, true, 0.0), (true, false, 0.5), (false, true, 0.25)] {
        tokens[0].set_rx_muted(a);
        tokens[1].set_rx_muted(b);
        let mut audio = [0.0; 96];
        let ptt = mixer
            .process(
                &mut controller,
                false,
                &mut audio,
                SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(ptt, !(a && b));
        assert!(audio.iter().all(|&x| x == level));
        assert!(tokens.iter().all(|t| t.active()), "raw qualifier survives");
    }
    assert!(mixer.peers.iter().all(|p| p.input.samples == 288));
}

#[test]
fn application_audio_requires_qualified_key_and_preserves_listen_only_mix_minus() {
    let application = |session, transceive, sample, keyed| {
        let signals = PeerSignals::for_webrtc(uuid::Uuid::from_u128(session));
        signals.publish_pcm();
        signals.set_radio_keyed(keyed);
        signals.set_webrtc_deadline(1_000_000_000);
        let reply = signals.reply_target();
        let (writer, output) = LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
        let peer = AudioPeer::for_application(
            &reply,
            transceive,
            Arc::new(AtomicBool::new(true)),
            FiniteInput {
                signals,
                samples: vec![sample; 12_800].into(),
            },
            writer,
            4,
        )
        .unwrap();
        (peer, reply, output)
    };
    let (talker, talker_reply, mut self_audio) = application(1, true, 0.125, false);
    let (listener, _, mut station_audio) = application(2, false, 0.5, true);
    let (mut graph, mut dispatcher) = LinkAudio::new(vec![talker, listener], 4).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let block = |graph: &mut LinkAudio<FiniteInput>, node: &mut NodeController, born_ns| {
        graph
            .process(
                node,
                false,
                &mut [0.0; 4],
                SourcePosition {
                    born_ns,
                    ..SourcePosition::default()
                },
            )
            .unwrap();
        graph.peer_source_active()
    };

    // Receive-only input and unqualified transport PCM cannot open the station.
    assert!(!block(&mut graph, &mut node, Some(0)));
    assert!(!talker_reply.active());
    dispatcher.dispatch(1);
    let mut samples = [0.0; 4];
    assert!(station_audio.read_span(&mut samples).is_none());

    graph.peers[0].input.signals.set_radio_keyed(true);
    assert!(!block(&mut graph, &mut node, None));
    dispatcher.dispatch(1);
    assert!(block(&mut graph, &mut node, Some(0)));
    assert!(talker_reply.active());
    dispatcher.dispatch(1);
    assert!(self_audio.read_span(&mut samples).is_none());
    assert_eq!(station_audio.read_span(&mut samples).unwrap().frames, 4);
    assert_eq!(samples, [0.125; 4]);

    assert!(!block(&mut graph, &mut node, Some(999_999_000)));
    assert!(!talker_reply.active());
    talker_reply.set_rx_muted(true);
    assert!(!block(&mut graph, &mut node, Some(0)));
    assert!(!talker_reply.active());
}
