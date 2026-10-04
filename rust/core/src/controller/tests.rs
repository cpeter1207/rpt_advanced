use super::*;

thread_local! {
    static AUDIO_ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

struct AllocationCounter;

// SAFETY: This test allocator delegates all allocation/layout contracts to System.
unsafe impl std::alloc::GlobalAlloc for AllocationCounter {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let _ = AUDIO_ALLOCATIONS.try_with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        let _ = AUDIO_ALLOCATIONS.try_with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
        unsafe { std::alloc::System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static TEST_ALLOCATOR: AllocationCounter = AllocationCounter;

fn media(value: f32, count: usize) -> PreparedMedia {
    PreparedMedia::new(Some(vec![value; count]), "E", MorseSettings::default()).unwrap()
}

struct PendingMedia;

impl crate::audio::PcmStreamReader for PendingMedia {
    fn render(&mut self, _: &mut [f32]) -> crate::audio::PcmRead {
        crate::audio::PcmRead::Pending
    }
}

#[test]
fn pending_media_does_not_key_transmitter_while_producer_starts() {
    let media = PreparedMedia::new_stream(Box::new(PendingMedia)).unwrap();
    let id = Identifier {
        media,
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: None,
    };
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![id],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    node.process_audio(false, false, &[], &mut [0.0; 64]);

    assert!(matches!(
        node.telemetry.active,
        Some(telemetry::Source::Identifier(0))
    ));
    assert!(!node.keyed);
}

#[test]
fn activity_snapshot_exposes_current_receive_state_and_bounded_status_capacity() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let activity = node.activity();
    assert!(!activity.is_active());
    assert!(control.can_queue_status());
    node.process_event(true, false);
    assert!(activity.is_active());
    node.process_event(false, false);
    assert!(!activity.is_active());
    for _ in 0..4 {
        assert!(control.queue_prepared_status(media(0.1, 8)));
    }
    assert!(!control.can_queue_status());
}

struct TestPeerInput {
    signals: crate::link::PeerSignals,
    sample: Option<f32>,
    enabled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl TestPeerInput {
    fn new(sample: Option<f32>) -> Self {
        let signals = crate::link::PeerSignals::new();
        if sample.is_some() {
            signals.publish_pcm();
        }
        Self {
            signals,
            sample,
            enabled: None,
        }
    }
}

impl crate::link::PeerInput for TestPeerInput {
    fn source_rate(&self) -> u32 {
        48000
    }

    fn signals(&self) -> &crate::link::PeerSignals {
        &self.signals
    }

    fn available(&self) -> u64 {
        self.sample.map_or(0, |_| 20_000)
    }

    fn render(&mut self, output: &mut [f32]) -> bool {
        let Some(sample) = self.sample.filter(|_| {
            self.enabled
                .as_ref()
                .is_none_or(|enabled| enabled.load(std::sync::atomic::Ordering::Acquire))
        }) else {
            return false;
        };
        output.fill(sample);
        true
    }
}

fn grouped_test_audio_peer(
    direct: &str,
    slot: std::num::NonZeroUsize,
    selection: crate::link::GroupSelection,
    input: TestPeerInput,
    maximum: usize,
) -> (
    crate::link::AudioPeer<TestPeerInput>,
    crate::audio::LinkAudioConsumer,
) {
    let (outbound, consumer) = crate::audio::LinkAudioQueue::new(maximum * 2)
        .unwrap()
        .into_endpoints();
    let peer = crate::link::AudioPeer::new(
        direct,
        crate::link::Mode::TRANSCEIVE,
        input,
        outbound,
        maximum,
        0,
    )
    .unwrap()
    .with_group(selection, slot);
    (peer, consumer)
}

#[test]
fn standby_group_audio_never_reaches_rf_or_any_peer() {
    use crate::link::LinkAudio;
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };

    let selection = crate::link::GroupSelection::new();
    selection.publish_desired(NonZeroUsize::new(1));
    let (selected, mut selected_out) = grouped_test_audio_peer(
        "200",
        NonZeroUsize::new(1).unwrap(),
        selection.clone(),
        TestPeerInput::new(None),
        1,
    );
    let standby_enabled = Arc::new(AtomicBool::new(false));
    let mut standby_input = TestPeerInput::new(Some(0.7));
    standby_input.enabled = Some(Arc::clone(&standby_enabled));
    let (standby, mut standby_out) = grouped_test_audio_peer(
        "201",
        NonZeroUsize::new(2).unwrap(),
        selection,
        standby_input,
        1,
    );
    let (mut links, mut dispatcher) = LinkAudio::new(vec![selected, standby], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    let mut rf = [0.0];
    links.process(&mut node, false, &mut rf).unwrap();
    dispatcher.dispatch(1);
    standby_enabled.store(true, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut rf).unwrap();
    dispatcher.dispatch(1);
    let mut sent = [0.0];
    assert_eq!(rf, [0.0]);
    assert_eq!(selected_out.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);
    assert_eq!(standby_out.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);
}

#[test]
fn selected_group_audio_reaches_rf_but_not_a_standby_peer() {
    use crate::link::LinkAudio;
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };

    let selection = crate::link::GroupSelection::new();
    selection.publish_desired(NonZeroUsize::new(1));
    let selected_enabled = Arc::new(AtomicBool::new(false));
    let mut selected_input = TestPeerInput::new(Some(0.25));
    selected_input.enabled = Some(Arc::clone(&selected_enabled));
    let (selected, mut selected_out) = grouped_test_audio_peer(
        "200",
        NonZeroUsize::new(1).unwrap(),
        selection.clone(),
        selected_input,
        1,
    );
    let standby_enabled = Arc::new(AtomicBool::new(false));
    let mut standby = TestPeerInput::new(Some(0.7));
    standby.enabled = Some(standby_enabled);
    let (standby, mut standby_out) =
        grouped_test_audio_peer("201", NonZeroUsize::new(2).unwrap(), selection, standby, 1);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![selected, standby], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    let mut rf = [0.0];
    links.process(&mut node, false, &mut rf).unwrap();
    dispatcher.dispatch(1);
    selected_enabled.store(true, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut rf).unwrap();
    dispatcher.dispatch(1);

    assert_eq!(rf, [0.25]);
    let mut sent = [0.0];
    assert_eq!(selected_out.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);
    assert_eq!(standby_out.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);
}

#[test]
fn parrot_capture_includes_local_and_receive_active_nonforwarded_peer_audio() {
    use crate::link::{AudioPeer, LinkAudio, Mode};
    use std::sync::{Arc, atomic::AtomicBool};

    let peer_enabled = Arc::new(AtomicBool::new(true));
    let mut input = TestPeerInput::new(Some(0.2));
    input.enabled = Some(Arc::clone(&peer_enabled));
    let (local_out, _) = crate::audio::LinkAudioQueue::new(4)
        .unwrap()
        .into_endpoints();
    let local_monitor = AudioPeer::new("100", Mode::LOCAL_MONITOR, input, local_out, 2, 0).unwrap();
    let (mut links, _) = LinkAudio::new(vec![local_monitor], 2).unwrap();
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

    links.process(&mut node, true, &mut [0.1, 0.1]).unwrap();
    assert_eq!(links.active_count(), 1);
    peer_enabled.store(false, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut [0.0, 0.0]).unwrap();
    assert_eq!(links.active_count(), 0);

    let clip = control.take_parrot_capture().unwrap();
    assert_eq!(clip.samples(), &[0.3, 0.3]);
}

struct ParrotSamples {
    offset: usize,
}

impl crate::audio::PcmStreamReader for ParrotSamples {
    fn start(&mut self) {
        self.offset = 0;
    }

    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        if self.offset == 2 {
            return crate::audio::PcmRead::Finished;
        }
        output[0] = [0.4, 0.5][self.offset];
        self.offset += 1;
        crate::audio::PcmRead::Samples(1)
    }
}

#[test]
fn parrot_bypasses_monitor_and_fallback_peer_routing_only_for_its_audio() {
    use crate::link::{AudioPeer, LinkAudio, Mode};

    let (to_transceiver, mut from_transceiver) = crate::audio::LinkAudioQueue::new(8)
        .unwrap()
        .into_endpoints();
    let (to_monitor, mut from_monitor) = crate::audio::LinkAudioQueue::new(8)
        .unwrap()
        .into_endpoints();
    let peers = vec![
        AudioPeer::new(
            "200",
            Mode::TRANSCEIVE,
            TestPeerInput::new(None),
            to_transceiver,
            4,
            0,
        )
        .unwrap(),
        AudioPeer::new(
            "201",
            Mode::LOCAL_MONITOR,
            TestPeerInput::new(None),
            to_monitor,
            4,
            0,
        )
        .unwrap(),
    ];
    let (mut links, mut dispatcher) = LinkAudio::new(peers, 4).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let media =
        PreparedMedia::new_parrot_stream(None, Box::new(ParrotSamples { offset: 0 })).unwrap();
    assert!(control.queue_prepared_parrot(media));

    let mut local = [0.0; 2];
    links.process(&mut node, false, &mut local).unwrap();
    assert_eq!(local, [0.4, 0.5]);
    assert!(node.parrot_playback_active());
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut sent = [0.0; 2];
    assert_eq!(from_transceiver.read(&mut sent), 0);
    assert_eq!(sent, [0.4, 0.5]);
    assert_eq!(from_monitor.read(&mut sent), 0);
    assert_eq!(sent, [0.4, 0.5]);
}

struct OneSampleParrot;

impl crate::audio::PcmStreamReader for OneSampleParrot {
    fn start(&mut self) {}

    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        output[0] = 0.4;
        crate::audio::PcmRead::FinalSamples(1)
    }
}

#[test]
fn parrot_final_callback_block_reaches_every_peer() {
    use crate::link::{AudioPeer, LinkAudio, Mode};

    let (outbound, mut peer_output) = crate::audio::LinkAudioQueue::new(4)
        .unwrap()
        .into_endpoints();
    let peer = AudioPeer::new(
        "200",
        Mode::TRANSCEIVE,
        TestPeerInput::new(None),
        outbound,
        1,
        0,
    )
    .unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 1).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.queue_prepared_parrot(
        PreparedMedia::new_parrot_stream(None, Box::new(OneSampleParrot)).unwrap()
    ));

    links.process(&mut node, false, &mut [0.0]).unwrap();
    assert!(!node.parrot_playback_active());
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut sent = [0.0];
    assert_eq!(peer_output.read(&mut sent), 0);
    assert_eq!(sent, [0.4]);
}

fn queued_parrot() -> PreparedMedia {
    PreparedMedia::new_parrot_stream(None, Box::new(ParrotSamples { offset: 0 })).unwrap()
}

#[test]
fn receive_activity_discards_a_queued_parrot_before_playback() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.queue_prepared_parrot(queued_parrot()));
    let mut audio = [0.0; 1];
    let mut parrot = [0.0; 1];
    let mut parrot_active = false;

    node.process_audio_with_parrot(
        false,
        false,
        true,
        &[],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut parrot_active,
        },
    );

    assert_eq!(parrot, [0.0]);
    assert!(!parrot_active);
    assert!(!node.parrot_playback_active());
    control.reclaim().for_each(drop);
}

#[test]
fn receive_activity_interrupts_and_discards_active_parrot_audio() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.queue_prepared_parrot(queued_parrot()));
    let mut audio = [0.0; 1];
    let mut parrot = [0.0; 1];
    let mut parrot_active = false;
    node.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut parrot_active,
        },
    );
    assert_eq!(parrot, [0.4]);
    assert!(node.parrot_playback_active());

    node.process_audio_with_parrot(
        false,
        false,
        true,
        &[],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut parrot_active,
        },
    );

    assert_eq!(parrot, [0.0]);
    assert!(!parrot_active);
    assert!(!node.parrot_playback_active());
    control.reclaim().for_each(drop);
}

#[test]
fn peer_transmit_is_subtracted_from_its_own_outbound_audio() {
    use crate::link::LinkAudio;

    let (outbound, mut peer_out) = crate::audio::LinkAudioQueue::new(4)
        .unwrap()
        .into_endpoints();
    let peer = crate::link::AudioPeer::new(
        "200",
        crate::link::Mode::TRANSCEIVE,
        TestPeerInput::new(Some(0.25)),
        outbound,
        1,
        0,
    )
    .unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 1).unwrap();
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

    links.process(&mut node, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    let mut outbound = [1.0; 1];
    assert_eq!(peer_out.read(&mut outbound), 1);
    assert_eq!(outbound, [0.0]);
    let mut rf = [0.5];
    links.process(&mut node, true, &mut rf).unwrap();
    dispatcher.dispatch(1);

    assert!((rf[0] - 0.75).abs() < 0.001);
    outbound.fill(0.0);
    assert_eq!(peer_out.read(&mut outbound), 0);
    assert_eq!(outbound, [0.5]);
}

#[test]
fn monitor_destinations_are_prepared_only_when_they_belong_to_a_group() {
    use crate::link::LinkAudio;
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };

    let selection = crate::link::GroupSelection::new();
    selection.publish_desired(NonZeroUsize::new(1));
    let grouped_enabled = Arc::new(AtomicBool::new(false));
    let mut grouped_input = TestPeerInput::new(Some(0.25));
    grouped_input.enabled = Some(Arc::clone(&grouped_enabled));
    let (grouped_outbound, _) = crate::audio::LinkAudioQueue::new(2)
        .unwrap()
        .into_endpoints();
    let grouped_monitor = crate::link::AudioPeer::new(
        "200",
        crate::link::Mode::LOCAL_MONITOR,
        grouped_input,
        grouped_outbound,
        1,
        0,
    )
    .unwrap()
    .with_group(selection.clone(), NonZeroUsize::new(1).unwrap());
    let plain_enabled = Arc::new(AtomicBool::new(false));
    let mut plain_input = TestPeerInput::new(Some(0.5));
    plain_input.enabled = Some(plain_enabled);
    let (plain_outbound, _) = crate::audio::LinkAudioQueue::new(2)
        .unwrap()
        .into_endpoints();
    let plain_monitor = crate::link::AudioPeer::new(
        "201",
        crate::link::Mode::MONITOR,
        plain_input,
        plain_outbound,
        1,
        0,
    )
    .unwrap();
    let (mut links, mut dispatcher) =
        LinkAudio::new(vec![grouped_monitor, plain_monitor], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    links.process(&mut node, false, &mut [0.0]).unwrap();
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    grouped_enabled.store(true, std::sync::atomic::Ordering::Release);
    links.process(&mut node, true, &mut [0.1]).unwrap();

    assert_eq!(links.active_count(), 1);
    assert_eq!(dispatcher.dispatch(1), 1);
}

#[test]
fn group_winner_change_waits_for_all_inputs_to_be_idle() {
    use crate::link::LinkAudio;
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };

    let selection = crate::link::GroupSelection::new();
    selection.publish_desired(NonZeroUsize::new(1));
    let selected_enabled = Arc::new(AtomicBool::new(false));
    let mut selected_input = TestPeerInput::new(Some(0.2));
    selected_input.enabled = Some(Arc::clone(&selected_enabled));
    let (selected, _) = grouped_test_audio_peer(
        "200",
        NonZeroUsize::new(1).unwrap(),
        selection.clone(),
        selected_input,
        1,
    );
    let standby_enabled = Arc::new(AtomicBool::new(false));
    let mut standby_input = TestPeerInput::new(Some(0.4));
    standby_input.enabled = Some(Arc::clone(&standby_enabled));
    let (standby, _) = grouped_test_audio_peer(
        "201",
        NonZeroUsize::new(2).unwrap(),
        selection.clone(),
        standby_input,
        1,
    );
    let (mut links, _) = LinkAudio::new(vec![selected, standby], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();

    let mut rf = [0.0];
    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    links.process(&mut node, true, &mut rf).unwrap();
    selection.publish_desired(NonZeroUsize::new(2));
    links.process(&mut node, true, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));

    selected_enabled.store(true, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    selected_enabled.store(false, std::sync::atomic::Ordering::Release);
    standby_enabled.store(true, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    standby_enabled.store(false, std::sync::atomic::Ordering::Release);
    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(2));
}

#[test]
fn group_winner_can_change_while_transmitter_is_keyed_without_input_activity() {
    use crate::link::LinkAudio;
    use std::num::NonZeroUsize;

    let selection = crate::link::GroupSelection::new();
    selection.publish_desired(NonZeroUsize::new(1));
    let (first, _) = grouped_test_audio_peer(
        "200",
        NonZeroUsize::new(1).unwrap(),
        selection.clone(),
        TestPeerInput::new(None),
        1,
    );
    let (second, _) = grouped_test_audio_peer(
        "201",
        NonZeroUsize::new(2).unwrap(),
        selection.clone(),
        TestPeerInput::new(None),
        1,
    );
    let (mut links, _) = LinkAudio::new(vec![first, second], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.keyed = true;
    let mut rf = [0.0];

    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    selection.publish_desired(NonZeroUsize::new(2));
    links.process(&mut node, false, &mut rf).unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(2));
}

fn test_audio_peer(
    direct: &str,
    mode: crate::link::Mode,
    sample: Option<f32>,
    maximum: usize,
) -> (
    crate::link::AudioPeer<TestPeerInput>,
    crate::audio::LinkAudioConsumer,
) {
    let (outbound, consumer) = crate::audio::LinkAudioQueue::new(maximum * 2)
        .unwrap()
        .into_endpoints();
    (
        crate::link::AudioPeer::new(
            direct,
            mode,
            TestPeerInput::new(sample),
            outbound,
            maximum,
            0,
        )
        .unwrap(),
        consumer,
    )
}

#[test]
fn courtesy_queue_bounds_identity_and_retains_other_sources() {
    let mut planner = courtesy::CourtesyPlanner::new(CourtesySettings {
        receiver: Some(media(0.1, 48)),
        link: Some(media(0.2, 48)),
        peers: vec![("selected".into(), media(0.3, 48))],
    });
    assert_eq!(planner.pop(), None);
    assert!(!planner.ready(u64::MAX));
    let invalid = "x".repeat(64);
    planner.schedule(&invalid, "selected", 0);
    assert!(!planner.pending());
    planner.schedule("", "", 2);
    planner.schedule("direct", "selected", 3);
    planner.schedule("other", "", 4);
    planner.cancel(&invalid);
    planner.cancel("direct");
    assert!(!planner.ready(1));
    assert!(planner.ready(2));
    assert_eq!(planner.pop(), Some(0));
    assert!(!planner.ready(3));
    assert!(planner.ready(4));
    assert_eq!(planner.pop(), Some(1));
    assert_eq!(planner.pop(), None);
    let maximum = "x".repeat(63);
    for _ in 0..17 {
        planner.schedule(&maximum, "selected", 5);
    }
    for _ in 0..16 {
        assert_eq!(planner.pop(), Some(2));
    }
    assert_eq!(planner.pop(), None);
    planner.schedule(&maximum, "", 6);
    planner.cancel(&maximum);
    assert!(!planner.pending());
}

#[test]
fn empty_audio_calls_process_edges_without_advancing_time() {
    let (mut node, _control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(node.process_audio(true, false, &[], &mut []));
    assert_eq!(node.now, 0);
    assert!(node.process_audio(false, true, &[], &mut []));
    assert_eq!(node.now, 0);
}

#[test]
fn status_rejection_retains_empty_nonfinite_and_overrange_pcm_without_consuming_capacity() {
    let (_, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for pcm in [
        vec![],
        vec![f32::NAN],
        vec![f32::INFINITY],
        vec![1.01],
        vec![-1.01],
    ] {
        let original = pcm.as_ptr();
        let rejected = control.queue_status("E", Some(pcm)).unwrap_err();
        let returned = rejected.audio.unwrap();
        assert_eq!(returned.as_ptr(), original);
    }
    for _ in 0..4 {
        control.queue_status("E", Some(vec![1.0, -1.0])).unwrap();
    }
    assert!(control.queue_status("E", None).is_err());
}

#[test]
fn courtesy_hang_and_status_are_local_only_but_receive_reaches_peers() {
    use crate::link::LinkAudio;

    const MAXIMUM: usize = 12_010;
    let (peer, mut outbound) = test_audio_peer("200", crate::link::Mode::TRANSCEIVE, None, MAXIMUM);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], MAXIMUM).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            hang_ms: 100,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings {
            receiver: Some(media(0.4, 1)),
            ..CourtesySettings::default()
        },
    )
    .unwrap();

    links.process(&mut node, true, &mut [0.25]).unwrap();
    dispatcher.dispatch(1);
    let mut sent = [0.0];
    assert_eq!(outbound.read(&mut sent), 0);
    assert_eq!(sent, [0.25]);

    links.process(&mut node, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    assert_eq!(outbound.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);

    links.process(&mut node, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    assert_eq!(outbound.read(&mut sent), 1);

    control.queue_status("E", Some(vec![0.6])).unwrap();
    let mut elapsed = [0.0; MAXIMUM];
    links.process(&mut node, false, &mut elapsed).unwrap();
    dispatcher.dispatch(1);
    let mut status = [0.0; MAXIMUM];
    assert_eq!(outbound.read(&mut status), MAXIMUM);
    assert!(status.iter().all(|sample| *sample == 0.0));
}

#[test]
fn mix_minus_queues_other_forwarding_peers_but_not_the_destination_itself() {
    use crate::link::LinkAudio;

    let (peer, mut outbound) = test_audio_peer("200", crate::link::Mode::TRANSCEIVE, Some(0.4), 1);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    links.process(&mut node, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    let mut sent = [0.0];
    assert_eq!(outbound.read(&mut sent), 1);

    let (destination, mut outbound) =
        test_audio_peer("200", crate::link::Mode::TRANSCEIVE, None, 1);
    let (source, _unused) = test_audio_peer("201", crate::link::Mode::MONITOR, Some(0.7), 1);
    let (mut links, mut dispatcher) = LinkAudio::new(vec![destination, source], 1).unwrap();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    links.process(&mut node, false, &mut [0.0]).unwrap();
    dispatcher.dispatch(1);
    assert_eq!(outbound.read(&mut sent), 0);
    assert_eq!(sent, [0.7]);
}

#[test]
fn loopback_pool_never_allocates_or_drops_owned_buffers_on_audio_full() {
    use crate::{
        audio::LinkAudioQueue,
        link::{AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals},
    };
    struct Input(PeerSignals);
    impl PeerInput for Input {
        fn source_rate(&self) -> u32 {
            48000
        }
        fn signals(&self) -> &PeerSignals {
            &self.0
        }
        fn available(&self) -> u64 {
            0
        }
        fn render(&mut self, _: &mut [f32]) -> bool {
            false
        }
    }
    let (outbound, mut consumer) = LinkAudioQueue::new(1920).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        "200",
        Mode::TRANSCEIVE,
        Input(PeerSignals::new()),
        outbound,
        960,
        0,
    )
    .unwrap();
    let (mut audio, mut dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut node, _control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let status = audio.status();
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    for _ in 0..3 {
        audio.process(&mut node, true, &mut [0.25; 960]).unwrap();
    }
    let dropped = status.dropped_blocks();
    let dispatched = dispatcher.dispatch(2);
    let mut output = [0.0; 1920];
    let missing = consumer.read(&mut output);
    audio.process(&mut node, true, &mut [0.5; 960]).unwrap();
    dispatcher.dispatch(2);
    let allocations = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(allocations, Some(0));
    assert_eq!(dropped, 1);
    assert_eq!(dispatched, 2);
    assert_eq!(missing, 0);
    assert!(output.iter().all(|sample| *sample == 0.25));
}

#[test]
fn duplex_hang_and_partition_invariance() {
    for full_duplex in [false, true] {
        let config = ControllerSettings {
            full_duplex,
            hang_ms: 2,
            ..ControllerSettings::default()
        };
        let (mut controller, _control) =
            NodeController::new(config, vec![], vec![], CourtesySettings::default()).unwrap();
        let mut input = [0.25; 48];
        assert_eq!(
            controller.process_audio(true, false, &[], &mut input),
            full_duplex
        );
        assert_eq!(input[0], if full_duplex { 0.25 } else { 0.0 });
        assert_eq!(controller.activity().last_sample(), Some(47));
        controller.process_audio(false, false, &[], &mut [0.0; 96]);
        assert!(!controller.process_event(false, false));
    }
}

#[test]
fn status_delays_and_returns_ownership_on_full_queue() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for _ in 0..4 {
        control.queue_status("E", Some(vec![0.5])).unwrap();
    }
    assert!(control.queue_status("E", Some(vec![0.25])).is_err());
    let mut out = vec![0.0; 12_000];
    node.process_audio(false, false, &[], &mut out);
    assert!(out.iter().all(|sample| *sample == 0.0));
    node.process_audio(false, false, &[], &mut [0.0; 4]);
    assert_eq!(control.reclaim().count(), 4);
}

#[test]
fn identifiers_precede_periodic_announcements() {
    let identifier = Identifier {
        media: media(0.1, 1),
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: None,
    };
    let announcements = vec![
        Announcement {
            media: media(0.2, 1),
            interval_ms: 1,
        },
        Announcement {
            media: media(0.3, 1),
            interval_ms: 1,
        },
    ];
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![identifier],
        announcements,
        CourtesySettings::default(),
    )
    .unwrap();
    let mut output = [0.0; 51];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(&output[48..], &[0.1, 0.2, 0.3]);
}

#[test]
fn timeout_requires_deadline_and_unkey() {
    let config = ControllerSettings {
        full_duplex: true,
        transmit_timeout_ms: 1,
        timeout_lockout_ms: 2,
        ..ControllerSettings::default()
    };
    let (mut node, _) =
        NodeController::new(config, vec![], vec![], CourtesySettings::default()).unwrap();
    assert!(!node.process_audio(true, false, &[], &mut [0.0; 49]));
    assert!(!node.process_audio(true, false, &[], &mut [0.0; 144]));
    node.process_event(false, false);
    assert!(node.process_event(true, false));
}

#[test]
fn courtesy_selection_cancellation_fifo_and_kerchunk() {
    let courtesy = CourtesySettings {
        receiver: Some(media(0.1, 1)),
        link: Some(media(0.2, 1)),
        peers: vec![
            ("north".into(), media(0.3, 1)),
            ("downstream".into(), media(0.4, 1)),
        ],
    };
    let (mut node, _) =
        NodeController::new(ControllerSettings::default(), vec![], vec![], courtesy).unwrap();
    node.link_unkeyed("north", "downstream", false);
    node.link_unkeyed("south", "unknown", false);
    node.link_keyed("north");
    let mut output = [0.0];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.2]);
    node.link_unkeyed("north", "downstream", false);
    node.link_unkeyed("north", "unknown", false);
    node.link_unkeyed("south", "", true);
    let mut output = [0.0; 3];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.4, 0.3, 0.0]);
    for _ in 0..17 {
        node.link_unkeyed("south", "", false);
    }
    let mut output = [0.0; 17];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output.iter().filter(|v| **v == 0.2).count(), 16);
}

#[test]
fn local_courtesy_delay_cancellation_and_short_receive_suppression() {
    let settings = ControllerSettings {
        full_duplex: true,
        courtesy_delay_ms: 1,
        kerchunk_max_ms: 1,
        ..ControllerSettings::default()
    };
    let courtesy = CourtesySettings {
        receiver: Some(media(0.4, 1)),
        ..CourtesySettings::default()
    };
    let (mut node, _) = NodeController::new(settings, vec![], vec![], courtesy).unwrap();
    node.process_audio(true, false, &[], &mut [0.0; 48]);
    node.process_audio(false, false, &[], &mut [0.0; 96]);
    assert!(!node.courtesy.pending());
    node.process_audio(true, false, &[], &mut [0.0; 49]);
    node.process_event(false, false);
    assert!(node.courtesy.pending());
    node.process_event(true, false);
    assert!(!node.courtesy.pending());
    node.process_audio(true, false, &[], &mut [0.0; 49]);
    let mut output = [0.0; 49];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output[47], 0.0);
    assert_eq!(output[48], 0.4);
}

#[test]
fn polite_deadline_and_sample_free_interruption_select_morse() {
    let id = Identifier {
        media: media(0.5, 100),
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: Some(1),
    };
    let settings = ControllerSettings {
        full_duplex: true,
        ..ControllerSettings::default()
    };
    let (mut node, _) =
        NodeController::new(settings, vec![id], vec![], CourtesySettings::default()).unwrap();
    node.process_audio(true, false, &[], &mut [0.0; 96]);
    assert!(node.telemetry.active.is_none());
    let mut output = [0.0];
    node.process_audio(true, false, &[], &mut output);
    assert_eq!(output[0], 0.0); // first Morse sample, not prepared 0.5
    assert!(matches!(
        node.telemetry.active,
        Some(telemetry::Source::Identifier(0))
    ));
    let mut subsequent = [0.0];
    node.process_audio(false, false, &[], &mut subsequent);
    assert!(subsequent[0] > 0.0 && subsequent[0] < 0.5);
}

#[test]
fn receive_event_interrupts_prepared_identifier_without_consuming_morse() {
    let id = Identifier {
        media: media(0.5, 100),
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: None,
    };
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![id],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.process_audio(false, false, &[], &mut [0.0; 49]);
    for _ in 0..3 {
        node.process_event(true, false);
    }
    let mut output = [1.0; 2];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output[0], 0.0);
    assert!(output[1] > 0.0 && output[1] < 0.5);
}

#[test]
fn courtesy_ducks_without_switching_media_and_partitions_match() {
    fn run(partition: usize) -> Vec<f32> {
        let config = ControllerSettings {
            full_duplex: true,
            telemetry_duck_db: -6,
            ..ControllerSettings::default()
        };
        let courtesy = CourtesySettings {
            link: Some(media(0.5, 1000)),
            ..CourtesySettings::default()
        };
        let (mut node, _) = NodeController::new(config, vec![], vec![], courtesy).unwrap();
        node.link_unkeyed("peer", "", false);
        node.process_event(false, false);
        let mut output = vec![0.0; 960];
        for chunk in output.chunks_mut(partition) {
            node.process_audio(true, false, &[], chunk);
        }
        assert!(output[0] < 0.5 && output[0] > 0.49);
        assert!((output[959] - 0.5 * 10_f32.powf(-0.3)).abs() < 0.0001);
        output
    }
    assert_eq!(run(960), run(13));
}

#[test]
fn transmit_worker_renders_tone_courtesy_directly() {
    let sequence = "1000/2,1200+1300@-12/2";
    let mut expected_source = crate::audio::ToneSequence::new(sequence, -6).unwrap();
    let mut expected = [0.0; 192];
    assert_eq!(expected_source.render(&mut expected), expected.len());
    let courtesy = CourtesySettings {
        link: Some(
            PreparedMedia::new_with_tone(
                None,
                Some(crate::audio::ToneSequence::new(sequence, -6).unwrap()),
                "",
                MorseSettings::default(),
            )
            .unwrap(),
        ),
        ..CourtesySettings::default()
    };
    let (mut node, _) =
        NodeController::new(ControllerSettings::default(), vec![], vec![], courtesy).unwrap();
    node.link_unkeyed("peer", "", false);
    node.process_event(false, false);
    let mut output = [0.0; 192];
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(false, false, &[], &mut output);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    assert_eq!(output, expected);
}

#[test]
fn every_release_announcements_follow_hang_once_and_ids_satisfy_lower_priority() {
    let ids = vec![
        Identifier {
            media: media(0.1, 1),
            interval_ms: 1,
            priority: 2,
            first_key_only: false,
            regardless_of_activity: false,
            polite_maximum_wait_ms: Some(10),
        },
        Identifier {
            media: media(0.15, 1),
            interval_ms: 1,
            priority: 1,
            first_key_only: false,
            regardless_of_activity: false,
            polite_maximum_wait_ms: None,
        },
        Identifier {
            media: media(0.3, 1),
            interval_ms: 100000,
            priority: 3,
            first_key_only: false,
            regardless_of_activity: false,
            polite_maximum_wait_ms: None,
        },
    ];
    let announcements = vec![Announcement {
        media: media(0.2, 1),
        interval_ms: 0,
    }];
    let settings = ControllerSettings {
        full_duplex: true,
        hang_ms: 1,
        ..ControllerSettings::default()
    };
    let (mut node, _) =
        NodeController::new(settings, ids, announcements, CourtesySettings::default()).unwrap();
    node.process_audio(true, false, &[], &mut [0.0; 49]);
    let mut output = [0.0; 99];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output.iter().filter(|v| **v == 0.1).count(), 1);
    assert_eq!(output.iter().filter(|v| **v == 0.15).count(), 0);
    assert_eq!(output.iter().filter(|v| **v == 0.2).count(), 1);
    assert_eq!(&output[47..49], &[0.1, 0.2]);
}

#[test]
fn validation_retains_status_pcm_and_rejects_bad_media_and_settings() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for text in ["", "E*", &"E".repeat(128)] {
        let error = control.queue_status(text, Some(vec![0.5])).unwrap_err();
        assert_eq!(error.audio, Some(vec![0.5]));
    }
    for audio in [vec![], vec![f32::NAN], vec![1.1]] {
        assert!(PreparedMedia::new(Some(audio), "E", MorseSettings::default()).is_err());
    }
    let bad = ControllerSettings {
        telemetry_duck_db: -61,
        ..ControllerSettings::default()
    };
    assert!(NodeController::new(bad, vec![], vec![], CourtesySettings::default()).is_err());
    control.queue_status("E", None).unwrap();
    node.process_audio(false, false, &[], &mut [0.0; 12000]);
    let mut output = [0.0; 3];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output[0], 0.0);
    assert!(output[1] > 0.0);
    assert_eq!(node.activity().last_sample(), None);
}

#[test]
fn controller_preparation_rejects_unrepresentable_durations_and_invalid_peer_names() {
    for field in ["interval", "polite", "announcement"] {
        let mut ids = vec![Identifier {
            media: media(0.1, 1),
            interval_ms: 1,
            priority: 1,
            first_key_only: false,
            regardless_of_activity: true,
            polite_maximum_wait_ms: Some(1),
        }];
        let mut announcements = vec![Announcement {
            media: media(0.1, 1),
            interval_ms: 1,
        }];
        match field {
            "interval" => ids[0].interval_ms = u64::MAX,
            "polite" => ids[0].polite_maximum_wait_ms = Some(u64::MAX),
            _ => announcements[0].interval_ms = u64::MAX,
        }
        assert!(
            NodeController::new(
                ControllerSettings::default(),
                ids,
                announcements,
                CourtesySettings::default()
            )
            .is_err()
        );
    }
    for name in [String::new(), "x".repeat(64), "bad\0name".into()] {
        assert!(
            NodeController::new(
                ControllerSettings::default(),
                vec![],
                vec![],
                CourtesySettings {
                    peers: vec![(name, media(0.1, 1))],
                    ..CourtesySettings::default()
                }
            )
            .is_err()
        );
    }
    let settings = ControllerSettings {
        status_morse: MorseSettings {
            speed_wpm: 0,
            frequency_hz: 0.0,
            level_db: -6,
        },
        ..ControllerSettings::default()
    };
    assert!(NodeController::new(settings, vec![], vec![], CourtesySettings::default()).is_err());
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.link_keyed("");
    node.link_unkeyed("", "", false);
    assert!(!node.process_event(false, false));
}

#[test]
fn watchdog_restarts_on_local_and_individual_link_unkey() {
    let settings = ControllerSettings {
        full_duplex: true,
        hang_ms: 2,
        transmit_timeout_ms: 2,
        ..ControllerSettings::default()
    };
    let (mut node, _) =
        NodeController::new(settings, vec![], vec![], CourtesySettings::default()).unwrap();
    assert!(node.process_audio(true, true, &[], &mut [0.0; 80]));
    node.process_event(false, true);
    assert!(node.process_audio(false, true, &[], &mut [0.0; 80]));
    node.link_unkeyed("peer", "", false);
    assert!(node.process_audio(false, true, &[], &mut [0.0; 80]));
    assert!(!node.process_audio(false, true, &[], &mut [0.0; 17]));
}

#[test]
fn all_zero_status_settings_resolve_defaults_but_partial_settings_fail() {
    for (speed, frequency, valid) in [(0, 0.0, true), (20, 0.0, false), (0, 800.0, false)] {
        let settings = ControllerSettings {
            status_morse: MorseSettings {
                speed_wpm: speed,
                frequency_hz: frequency,
                level_db: 0,
            },
            ..ControllerSettings::default()
        };
        assert_eq!(
            NodeController::new(settings, vec![], vec![], CourtesySettings::default()).is_ok(),
            valid
        );
    }
}

#[test]
fn first_key_polite_wait_starts_at_key_and_zero_samples_do_not_play() {
    let id = Identifier {
        media: media(0.5, 10),
        interval_ms: 1,
        priority: 1,
        first_key_only: true,
        regardless_of_activity: false,
        polite_maximum_wait_ms: Some(1),
    };
    let config = ControllerSettings {
        full_duplex: true,
        ..ControllerSettings::default()
    };
    let (mut node, _) =
        NodeController::new(config, vec![id], vec![], CourtesySettings::default()).unwrap();
    node.process_audio(false, false, &[], &mut [0.0; 48]);
    assert!(node.process_event(true, false));
    assert!(node.telemetry.active.is_none());
    node.process_audio(true, false, &[], &mut [0.0; 48]);
    assert!(node.telemetry.active.is_none());
    node.process_audio(true, false, &[], &mut [0.0; 1]);
    assert!(matches!(
        node.telemetry.active,
        Some(telemetry::Source::Identifier(0))
    ));
}

#[test]
fn first_key_identifier_waits_for_its_idle_interval() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![Identifier {
            media: media(0.5, 10),
            interval_ms: 1000,
            priority: 1,
            first_key_only: true,
            regardless_of_activity: false,
            polite_maximum_wait_ms: None,
        }],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(node.process_event(true, false));
    node.process_audio(true, false, &[], &mut [0.0; 960]);
    assert!(node.telemetry.active.is_none());
}

#[test]
fn half_duplex_periodic_announcement_waits_for_receiver_and_delayed_courtesy() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: false,
            courtesy_delay_ms: 100,
            ..ControllerSettings::default()
        },
        vec![],
        vec![Announcement {
            media: media(0.2, 1),
            interval_ms: 1,
        }],
        CourtesySettings {
            receiver: Some(media(0.3, 1)),
            ..CourtesySettings::default()
        },
    )
    .unwrap();
    node.process_audio(true, false, &[], &mut [0.0; 960]);
    assert!(node.telemetry.active.is_none());
    let mut output = [0.0; 1];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.0]);
    assert!(node.courtesy.pending());
}

#[test]
fn audio_path_never_allocates_or_reclaims_prepared_media() {
    let id = Identifier {
        media: media(0.5, 100),
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: None,
    };
    let courtesy = CourtesySettings {
        link: Some(media(0.4, 10)),
        ..CourtesySettings::default()
    };
    let announcements = vec![Announcement {
        media: media(0.3, 10),
        interval_ms: 0,
    }];
    let config = ControllerSettings {
        full_duplex: true,
        ..ControllerSettings::default()
    };
    let (mut node, mut control) =
        NodeController::new(config, vec![id], announcements, courtesy).unwrap();
    control.queue_status("E", Some(vec![0.6; 10])).unwrap();
    let mut output = [0.0; 16000];
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(false, false, &[], &mut output[..49]);
    node.process_event(true, true);
    node.process_audio(true, true, &[], &mut output[..48]);
    node.link_unkeyed("peer", "", false);
    node.process_audio(false, false, &[], &mut output);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn tone_configuration_does_not_allocate_duration_sized_pcm() {
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    let tone = crate::audio::ToneSequence::new("1000/60000", -6).unwrap();
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(tone.rendered_samples(), 2_880_000);
    assert_eq!(calls, Some(3));
}

#[test]
fn half_duplex_link_audio_during_local_receive_does_not_advance_transmit_hang() {
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.process_audio(false, false, &[], &mut [0.0]);
    node.process_audio(false, true, &[], &mut [0.0]);
    assert_eq!(node.last_audio, 1);

    node.process_audio(true, true, &[], &mut [0.0]);

    assert_eq!(node.last_audio, 1);
}

#[test]
fn empty_courtesy_override_falls_back_to_generic_media() {
    let empty = PreparedMedia::new(None, "", MorseSettings::default()).unwrap();
    let courtesy = CourtesySettings {
        link: Some(media(0.4, 1)),
        peers: vec![("peer".into(), empty)],
        ..CourtesySettings::default()
    };
    let (mut node, _) =
        NodeController::new(ControllerSettings::default(), vec![], vec![], courtesy).unwrap();
    node.link_unkeyed("peer", "peer", false);
    let mut output = [0.0];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.4]);
}
