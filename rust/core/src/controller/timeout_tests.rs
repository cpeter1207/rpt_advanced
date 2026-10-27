use super::*;

fn protected_node() -> (NodeController, ControllerControl) {
    let (mut node, control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            hang_ms: 1000,
            transmit_timeout_ms: 1,
            telemetry_duck_db: -20,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_timeout_media([media(0.25, 4), media(0.5, 4)]);
    (node, control)
}

#[test]
fn timeout_speaks_over_muted_traffic_then_drops_without_traffic_hang() {
    let (mut node, _) = protected_node();
    let mut audio = [0.5; 4800];
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    let keyed = node.process_audio(true, true, &[0.125; 4800], &mut audio);
    let allocations = AUDIO_ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(allocations, 0);
    assert!(
        !keyed,
        "timeout notice must not retain the one-second traffic hang"
    );
    assert_eq!(&audio[..48], &[0.625; 48]);
    assert_eq!(
        &audio[48..52],
        &[0.25; 4],
        "speech must not switch to Morse or duck"
    );
    assert!(audio[52..].iter().all(|sample| *sample == 0.0));
    let mut muted = [0.8; 960];
    assert!(!node.process_audio(true, true, &[0.1; 960], &mut muted));
    assert_eq!(muted, [0.0; 960]);
}

#[test]
fn timeout_waits_for_every_input_then_finishes_clear_speech_before_unmuting() {
    let (mut node, _) = protected_node();
    node.process_audio(true, false, &[], &mut [0.8; 4800]);
    let mut unselected_peer_active = [0.0; 960];
    assert!(!node.process_audio_outputs(
        false,
        false,
        true,
        &[],
        &mut unselected_peer_active,
        None
    ));
    assert_eq!(unselected_peer_active, [0.0; 960]);
    let mut start_clear = [0.0; 2];
    assert!(node.process_audio(false, false, &[], &mut start_clear));
    assert_eq!(start_clear, [0.5; 2]);
    let mut rekey = [0.8; 4];
    assert!(node.process_audio(true, false, &[], &mut rekey));
    assert_eq!(rekey, [0.5, 0.5, 0.8, 0.8]);
}

#[test]
fn timeout_notice_obeys_site_inhibit_without_losing_recovery() {
    let (mut node, control) = protected_node();
    node.process_audio(true, false, &[], &mut [0.8; 49]);
    control.set_site_inhibit(true);
    let mut inhibited = [0.8; 960];
    assert!(!node.process_audio(true, false, &[], &mut inhibited));
    assert_eq!(inhibited, [0.0; 960]);
    control.set_site_inhibit(false);
    assert!(node.process_event(true, false));
    let mut notice = [0.8; 3];
    assert!(node.process_audio(true, false, &[], &mut notice));
    assert_eq!(notice, [0.25; 3]);
}

#[test]
fn timeout_notices_are_local_only_even_with_regeneration_and_peer_fanout() {
    let (mut node, _) = protected_node();
    node.process_audio(true, true, &[], &mut [0.5; 48]);
    let mut audio = [0.5; 4];
    let mut parrot = [1.0; 4];
    let mut active = false;
    let mut replies = [PeerReplySample::default(); 4];
    assert!(node.process_audio_with_parrot(
        true,
        true,
        true,
        &[0.125; 4],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: Some(&mut replies),
            regeneration: Some(&[Some(0.75); 4]),
            local_media: Default::default(),
        },
    ));
    assert_eq!(audio, [0.25; 4]);
    assert_eq!(parrot, [0.0; 4]);
    assert!(!active);
    assert!(replies.iter().all(|reply| reply.target == 0
        && reply.sample == 0.0
        && reply.broadcast.is_none()
        && !reply.suppress_repeat));
}

#[test]
fn timeout_recovery_can_repeat_and_is_callback_partition_invariant() {
    let render = |partition| {
        let (mut node, _) = protected_node();
        let mut output = Vec::new();
        for _ in 0..2 {
            for (active, samples) in [(true, 4800), (false, 4800)] {
                let mut audio = vec![if active { 0.8 } else { 0.0 }; samples];
                for chunk in audio.chunks_mut(partition) {
                    node.process_audio(active, false, &[], chunk);
                }
                output.extend(audio);
            }
        }
        assert!(!node.keyed);
        assert_eq!(output.iter().filter(|sample| **sample == 0.25).count(), 8);
        assert_eq!(output.iter().filter(|sample| **sample == 0.5).count(), 8);
        output
    };
    assert_eq!(render(1), render(960));
    assert_eq!(render(137), render(480));
}

#[test]
fn half_duplex_link_timeout_still_speaks_when_local_receive_becomes_active() {
    let (mut node, _) = protected_node();
    node.settings.full_duplex = false;
    node.process_audio(false, true, &[0.5; 49], &mut [0.0; 49]);
    let mut audio = [0.8; 3];
    assert!(node.process_audio(true, true, &[0.5; 3], &mut audio));
    assert_eq!(audio, [0.25; 3]);
}

#[test]
fn pending_or_failed_timeout_producer_cannot_leak_traffic_or_latch_ptt() {
    use crate::audio::{PcmRead, PcmStreamReader};
    use std::sync::atomic::{AtomicU8, AtomicUsize};
    struct Stream {
        state: Arc<AtomicU8>,
        starts: Arc<AtomicUsize>,
        remaining: usize,
    }
    impl PcmStreamReader for Stream {
        fn start(&mut self) {
            self.starts.fetch_add(1, Ordering::Relaxed);
            self.remaining = 4;
        }
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            match self.state.load(Ordering::Acquire) {
                0 => PcmRead::Pending,
                1 => {
                    output[0] = 0.25;
                    self.remaining -= 1;
                    if self.remaining == 0 {
                        PcmRead::FinalSamples(1)
                    } else {
                        PcmRead::Samples(1)
                    }
                }
                _ => PcmRead::Failed,
            }
        }
    }
    let state = Arc::new(AtomicU8::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let (mut node, _) = protected_node();
    node.set_timeout_media(std::array::from_fn(|_| {
        PreparedMedia::new_stream(Box::new(Stream {
            state: state.clone(),
            starts: starts.clone(),
            remaining: 0,
        }))
        .unwrap()
    }));
    let mut pending = [0.8; 4800];
    assert!(!node.process_audio(true, false, &[], &mut pending));
    assert!(pending[48..].iter().all(|sample| *sample == 0.0));
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    state.store(1, Ordering::Release);
    let mut notice = [0.8; 4800];
    assert!(!node.process_audio(true, false, &[], &mut notice));
    assert_eq!(&notice[..4], &[0.25; 4]);
    assert!(notice[4..].iter().all(|sample| *sample == 0.0));
    state.store(2, Ordering::Release);
    assert!(!node.process_audio(false, false, &[], &mut [0.0; 4800]));
    assert_eq!(starts.load(Ordering::Relaxed), 2);
    let mut restored = [0.8; 4];
    assert!(node.process_audio(true, false, &[], &mut restored));
    assert_eq!(restored, [0.8; 4]);
}

#[test]
fn timeout_holds_a_one_shot_producer_without_retiring_its_recipe() {
    use crate::audio::{PcmRead, PcmStreamReader};
    struct OneShot {
        retired: Arc<AtomicBool>,
        held: Arc<AtomicBool>,
    }
    impl PcmStreamReader for OneShot {
        fn start(&mut self) {
            self.held.store(false, Ordering::Release);
        }
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            if self.retired.load(Ordering::Acquire) || self.held.load(Ordering::Acquire) {
                return PcmRead::Pending;
            }
            output.fill(0.75);
            PcmRead::Samples(output.len())
        }
        fn defer_before_output(&mut self) -> bool {
            self.held.store(true, Ordering::Release);
            true
        }
        fn cancel(&mut self) {
            self.retired.store(true, Ordering::Release);
        }
    }
    let retired = Arc::new(AtomicBool::new(false));
    let held = Arc::new(AtomicBool::new(false));
    let (mut node, control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            transmit_timeout_ms: 1,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings {
            receiver: Some(
                PreparedMedia::new_stream(Box::new(OneShot {
                    retired: retired.clone(),
                    held: held.clone(),
                }))
                .unwrap(),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    node.set_timeout_media([media(0.25, 4), media(0.5, 4)]);
    node.process_audio(true, false, &[], &mut [0.8]);
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(node.telemetry.active == Some(Source::Courtesy(0)));
    node.process_audio(true, false, &[], &mut [0.8; 4800]);
    assert!(
        !retired.load(Ordering::Acquire),
        "a timeout is not terminal message cancellation"
    );
    assert!(
        held.load(Ordering::Acquire),
        "held media must release the shared producer"
    );
    let mut resumed = [0.0; 5];
    node.process_audio(false, false, &[], &mut resumed);
    assert_eq!(resumed, [0.5, 0.5, 0.5, 0.5, 0.75]);
    assert!(!held.load(Ordering::Acquire));
    drop(control);
}

/// Model the serialized station producer: held ordinary media must release it for notices.
struct SharedProducerReader {
    ordinary_active: Arc<AtomicBool>,
    ordinary: bool,
    remaining: usize,
}

impl crate::audio::PcmStreamReader for SharedProducerReader {
    fn start(&mut self) {
        if self.ordinary {
            self.ordinary_active.store(true, Ordering::Release);
        }
        self.remaining = 4;
    }

    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        use crate::audio::PcmRead;
        if !self.ordinary && self.ordinary_active.load(Ordering::Acquire) {
            return PcmRead::Pending;
        }
        let made = if self.ordinary {
            output.len()
        } else {
            output.len().min(self.remaining)
        };
        output[..made].fill(if self.ordinary { 0.75 } else { 0.25 });
        if self.ordinary {
            PcmRead::Samples(made)
        } else {
            self.remaining -= made;
            if self.remaining == 0 {
                PcmRead::FinalSamples(made)
            } else {
                PcmRead::Samples(made)
            }
        }
    }

    fn defer_before_output(&mut self) -> bool {
        self.cancel();
        true
    }

    fn select_morse_fallback(&mut self) -> crate::audio::MorseSwitch {
        if self.ordinary {
            self.ordinary_active.store(true, Ordering::Release);
        }
        crate::audio::MorseSwitch::Selected
    }

    fn cancel(&mut self) {
        if self.ordinary {
            self.ordinary_active.store(false, Ordering::Release);
        }
    }
}

fn shared_producer_reader(
    active: &Arc<AtomicBool>,
    ordinary: bool,
) -> Box<dyn crate::audio::PcmStreamReader> {
    Box::new(SharedProducerReader {
        ordinary_active: active.clone(),
        ordinary,
        remaining: 0,
    })
}

#[test]
fn timeout_notices_release_a_tail_suspended_by_an_identifier() {
    let active = Arc::new(AtomicBool::new(false));
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            transmit_timeout_ms: 1,
            ..Default::default()
        },
        vec![Identifier {
            media: media(0.125, 1000),
            interval_ms: 1,
            priority: 1,
            first_key_only: false,
            regardless_of_activity: true,
            polite_maximum_wait_ms: None,
        }],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_tails(vec![Tail {
        media: PreparedMedia::new_stream(shared_producer_reader(&active, true)).unwrap(),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    node.set_timeout_media(std::array::from_fn(|_| {
        PreparedMedia::new_stream(shared_producer_reader(&active, false)).unwrap()
    }));
    node.process_audio(true, false, &[], &mut [0.8]);
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(node.tails.items[0].started);
    node.now = 48;
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(node.tails.items[0].suspended);
    assert!(node.telemetry.active == Some(Source::Identifier(0)));

    let mut output = [0.8; 96];
    node.process_audio(true, false, &[], &mut output);
    assert_eq!(
        &output[47..51],
        &[0.25; 4],
        "a suspended tail must release the producer throughout the priority notice"
    );
    assert!(output[51..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn timeout_notices_release_an_idle_parrot_recording_producer() {
    for report in [false, true] {
        let active = Arc::new(AtomicBool::new(false));
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                parrot_enabled: true,
                full_duplex: true,
                transmit_timeout_ms: 1,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        node.set_timeout_media(std::array::from_fn(|_| {
            PreparedMedia::new_stream(shared_producer_reader(&active, false)).unwrap()
        }));
        assert!(
            control.queue_prepared_parrot(PreparedMedia::new_parrot_stream(
                report.then(|| shared_producer_reader(&active, true)),
                shared_producer_reader(&active, true),
            ))
        );
        let mut output = [0.0; 52];
        node.process_audio(false, false, &[], &mut output);
        assert_eq!(&output[..48], &[0.75; 48]);
        assert_eq!(
            &output[48..],
            &[0.25; 4],
            "a parrot replay can exceed a short watchdog while every receive input stays idle"
        );
    }
}
