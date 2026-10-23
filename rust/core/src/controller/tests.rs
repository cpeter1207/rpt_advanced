use super::*;

#[path = "site_tests.rs"]
mod site;

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

#[test]
fn configured_message_suppresses_repeat_but_still_switches_to_morse_on_receive() {
    for (local, linked, input, remote) in [
        (true, false, 0.2, 0.0),
        (false, true, 0.0, 0.3),
        (true, true, 0.2, 0.3),
    ] {
        let (mut node, mut control) = NodeController::new(
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
        node.now = 12_000;
        assert!(
            control.queue_prepared_status(media(0.75, 48).with_playback_policy(PlaybackPolicy {
                repeat_during_playback: false,
                ..Default::default()
            }))
        );
        let mut first = [0.0];
        node.process_audio(false, false, &[], &mut first);
        assert_eq!(first, [0.75]);
        let mut output = [input; 96];
        node.process_audio(local, linked, &[remote; 96], &mut output);
        assert_eq!(output[0], 0.0, "repeat must stop at the Morse transition");
        assert!(output.iter().any(|sample| *sample != 0.0));
        assert!(output.iter().all(|sample| sample.abs() <= 0.502));
    }
}

#[test]
fn configured_message_survives_traffic_lockout_without_leaking_timed_out_receive() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            telemetry_duck_db: 0,
            transmit_timeout_ms: 1,
            timeout_lockout_ms: 100,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.now = 12_000;
    assert!(
        control
            .queue_prepared_status(media(0.75, 8).with_playback_policy(PlaybackPolicy::default()))
    );
    node.process_audio(false, false, &[], &mut [0.0]);
    let mut output = [0.25; 4800];
    assert!(node.process_audio(true, false, &[0.25; 4800], &mut output));
    assert!(
        output[96..].iter().any(|sample| *sample != 0.0),
        "Morse remains audible after traffic timeout"
    );
    assert!(
        output[96..].iter().all(|sample| sample.abs() <= 0.502),
        "timed-out receiver must not ride message PTT"
    );
}

#[test]
fn replaced_occurrence_cannot_publish_a_first_sample_when_control_wins_during_render() {
    struct CancelOnRender(Arc<MessageOccurrence>);
    impl crate::audio::PcmStreamReader for CancelOnRender {
        fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
            self.0.cancel_pending();
            output[0] = 0.75;
            crate::audio::PcmRead::FinalSamples(1)
        }
    }
    let occurrence = Arc::new(MessageOccurrence::default());
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.now = 12_000;
    assert!(
        control.queue_prepared_status(
            PreparedMedia::new_stream(Box::new(CancelOnRender(occurrence.clone())))
                .unwrap()
                .with_occurrence(Some(occurrence))
        )
    );
    let mut output = [0.0; 2];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.0; 2]);
    let retired = control.reclaim().collect::<Vec<_>>();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].0.outcome(), PlaybackOutcome::Cancelled);
}

#[test]
fn scheduled_zero_wait_allows_only_an_immediately_eligible_first_sample() {
    for (local, linked, activity, count) in [
        (false, false, false, 4),
        (true, false, true, 0),
        (false, true, true, 0),
        (false, false, true, 0),
    ] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        node.now = 12_000;
        assert!(control.queue_prepared_status(media(0.75, 4).with_delivery(
            node.now,
            PoliteDelivery {
                maximum_wait_ms: 0,
                ..Default::default()
            }
        )));
        let mut output = [0.0; 8];
        node.process_audio_outputs(local, linked, activity, &[], &mut output, None);
        assert_eq!(
            output.iter().filter(|sample| **sample == 0.75).count(),
            count,
            "source activity={local}/{linked}/{activity}"
        );
        assert_eq!(control.reclaim().count(), 1);
    }
}

#[test]
fn scheduled_zero_wait_does_not_extend_through_async_preparation() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.now = 12_000;
    let mut preparations = MessagePreparation::new(1);
    let prepared = preparations.register(0).unwrap().with_delivery(
        node.now,
        PoliteDelivery {
            maximum_wait_ms: 0,
            ..Default::default()
        },
    );
    control.set_message_preparation(preparations);
    assert!(control.queue_prepared_status(prepared));
    node.process_audio(false, false, &[], &mut [0.0]);
    let request = control
        .take_message_request()
        .expect("the immediate slot permits one preparation attempt");
    assert!(
        control
            .complete_message(request.attempt, Ok(media(0.75, 4)))
            .is_ok()
    );
    let mut output = [0.0; 4];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.0; 4], "late ready PCM cannot renew a zero wait");
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn scheduled_morse_expiry_selects_the_complete_fallback_even_after_receive_deferral() {
    let (mut node, mut control) = NodeController::new(
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
    node.now = 12_000;
    assert!(control.queue_prepared_status(media(0.75, 4).with_delivery(
        12_000,
        PoliteDelivery {
            expiry: PoliteExpiry::Morse,
            ..Default::default()
        }
    )));
    node.process_audio(true, false, &[], &mut [0.0; 48]);
    let mut output = [0.0; 96];
    node.process_audio(false, false, &[], &mut output);
    assert!(output.iter().any(|sample| *sample != 0.0));
    assert!(
        output.iter().all(|sample| sample.abs() <= 0.502),
        "expiry must never return to primary speech"
    );
}

#[test]
fn scheduled_morse_expiry_respects_skip_or_wait_behind_another_message() {
    for busy in [PoliteBusy::Skip, PoliteBusy::Wait] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        node.now = 12_000;
        assert!(control.queue_prepared_status(media(0.75, 4)));
        node.process_audio(false, false, &[], &mut [0.0]);
        assert!(control.queue_prepared_status(media(0.9, 4).with_delivery(
            node.now,
            PoliteDelivery {
                expiry: PoliteExpiry::Morse,
                busy,
                ..Default::default()
            }
        )));
        let mut output = [0.0; 100];
        node.process_audio(false, false, &[], &mut output);
        assert_eq!(
            &output[..3],
            &[0.75; 3],
            "playing media must not be interrupted by expiry"
        );
        assert!(output[3..].iter().all(|sample| sample.abs() <= 0.502));
        assert_eq!(
            output[3..].iter().any(|sample| *sample != 0.0),
            busy == PoliteBusy::Wait
        );
    }
}

#[test]
fn scheduled_morse_expiry_without_a_complete_substitute_is_skipped() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.now = 12_000;
    let message = PreparedMedia::new(Some(vec![0.75; 4]), "", MorseSettings::default()).unwrap();
    assert!(control.queue_prepared_status(message.with_delivery(
        node.now,
        PoliteDelivery {
            expiry: PoliteExpiry::Morse,
            ..Default::default()
        }
    )));
    let mut output = [0.0; 96];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.0; 96]);
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn scheduled_morse_expiry_cannot_wait_through_half_duplex_or_site_inhibition() {
    for inhibit in [false, true] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: inhibit,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        node.now = 12_000;
        control.set_site_inhibit(inhibit);
        assert!(control.queue_prepared_status(media(0.75, 4).with_delivery(
            node.now,
            PoliteDelivery {
                expiry: PoliteExpiry::Morse,
                busy: PoliteBusy::Wait,
                ..Default::default()
            }
        )));
        node.process_audio(!inhibit, false, &[], &mut [0.0]);
        assert_eq!(
            control.reclaim().count(),
            1,
            "RF safety blocks expiry instead of deferring it"
        );
    }
}

#[test]
fn composition_completion_without_preparation_returns_the_callers_media() {
    let (_, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let attempt = MessageAttemptId {
        generation: 7,
        sequence: 3,
    };
    let rejected = control
        .complete_message(attempt, Ok(media(0.5, 1)))
        .expect_err("an unconfigured preparation owner cannot accept a completion");
    assert_eq!(rejected.attempt, attempt);
    let mut returned = rejected.result.unwrap();
    let mut output = [0.0];
    assert_eq!(returned.0.render(false, &mut output), 1);
    assert_eq!(output, [0.5]);
}

#[test]
fn tail_scheduling_and_identifier_preemption_do_not_allocate_on_audio() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            hang_ms: 1,
            ..Default::default()
        },
        vec![Identifier {
            media: media(0.25, 1),
            interval_ms: 2,
            priority: 1,
            first_key_only: false,
            regardless_of_activity: false,
            polite_maximum_wait_ms: Some(2),
        }],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_tails(vec![Tail {
        media: media(0.5, 480),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    let mut buffer = [0.0; 480];
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(true, false, &[], &mut buffer[..48]);
    node.process_audio(false, false, &[], &mut buffer);
    node.process_audio(true, false, &[], &mut buffer);
    node.process_audio(false, false, &[], &mut buffer);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    assert!(node.satisfied[0] != 0, "expired ID was exercised");
}

#[test]
fn composition_pre_sample_deferral_refreshes_attempt_without_audio_allocation() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut preparations = MessagePreparation::new(3);
    let peer = crate::link::PeerSignals::new();
    assert!(
        control.queue_prepared_status(
            preparations
                .register(0)
                .unwrap()
                .reply_to(Some(peer.reply_target()))
        )
    );
    control.set_message_preparation(preparations);
    node.process_audio(false, false, &[], &mut [0.0]);
    let first = control.take_message_request().unwrap();
    assert!(
        control
            .complete_message(first.attempt, Ok(media(0.5, 16)))
            .is_ok()
    );
    peer.set_radio_keyed(true);
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(false, false, &[], &mut [0.0]);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    assert!(control.take_message_request().is_none());
    peer.set_radio_keyed(false);
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(false, false, &[], &mut [0.0]);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    let second = control
        .take_message_request()
        .expect("deferred values need a fresh attempt");
    assert_ne!(first.attempt, second.attempt);
    assert!(
        control
            .complete_message(second.attempt, Err(ControllerError))
            .is_ok()
    );
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio(false, false, &[], &mut [0.0]);
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(calls, Some(0));
    assert!(!node.keyed);
    assert_eq!(control.reclaim().count(), 1);
}

#[test]
fn receive_defers_status_and_switches_identifiers_and_announcements_to_morse() {
    for source in 0..4 {
        for (local, linked, activity) in [
            (true, false, true),
            (false, true, true),
            (false, false, true),
        ] {
            for before_first_sample in [false, true] {
                let peer = crate::link::PeerSignals::new();
                let id = Identifier {
                    media: media(0.75, 100),
                    interval_ms: 100000,
                    priority: 1,
                    first_key_only: false,
                    regardless_of_activity: true,
                    polite_maximum_wait_ms: None,
                };
                let (mut node, mut control) = NodeController::new(
                    ControllerSettings {
                        full_duplex: true,
                        ..ControllerSettings::default()
                    },
                    vec![id],
                    vec![Announcement {
                        media: media(0.75, 100),
                        interval_ms: 100000,
                    }],
                    CourtesySettings::default(),
                )
                .unwrap();
                node.process_audio(false, false, &[], &mut [0.0; 12000]);
                node.telemetry.active = Some(match source {
                    0 => telemetry::Source::Identifier(0),
                    1 => telemetry::Source::Announcement(0),
                    _ => {
                        let reply = (source == 3).then(|| peer.reply_target());
                        assert!(control.queue_prepared_status(media(0.75, 100).reply_to(reply)));
                        node.telemetry.poll_status();
                        node.telemetry.start_status();
                        telemetry::Source::Status
                    }
                });
                let render = |node: &mut NodeController, receiving, linked, active| {
                    let mut radio = [0.0];
                    let mut parrot = [0.0];
                    let mut parrot_active = false;
                    let mut replies = [PeerReplySample::default()];
                    node.process_audio_with_parrot(
                        receiving,
                        linked,
                        active,
                        &[],
                        &mut radio,
                        ParrotOutput {
                            samples: &mut parrot,
                            active: &mut parrot_active,
                            replies: Some(&mut replies),
                            regeneration: None,
                        },
                    );
                    if source == 3 {
                        assert_eq!(radio, [0.0]);
                        if replies[0].sample != 0.0 {
                            assert_eq!(replies[0].target, peer.reply_target().identity());
                        }
                        replies[0].sample
                    } else {
                        radio[0]
                    }
                };
                if !before_first_sample {
                    assert_eq!(render(&mut node, false, false, false), 0.75);
                }
                assert_eq!(
                    render(&mut node, local, linked, activity),
                    0.0,
                    "source {source}, receive {local}/{linked}/{activity}, initial {before_first_sample}"
                );
                if source >= 2 && before_first_sample {
                    node.process_event(false, false);
                    node.now += 12_000;
                    assert_eq!(render(&mut node, false, false, false), 0.75);
                    assert_eq!(render(&mut node, local, linked, activity), 0.0);
                    continue;
                }
                let second = render(&mut node, false, false, false);
                assert!(
                    second > 0.0 && second < 0.5,
                    "Morse must stay latched after unkey"
                );
                assert!(
                    render(&mut node, local, linked, activity) > second,
                    "another receive edge must not restart Morse"
                );
            }
        }
    }
}

struct FailedMedia;
impl crate::audio::PcmStreamReader for FailedMedia {
    fn render(&mut self, _: &mut [f32]) -> crate::audio::PcmRead {
        crate::audio::PcmRead::Failed
    }
}

#[test]
fn playback_failure_does_not_satisfy_identifier() {
    let ids = [1, 2]
        .into_iter()
        .map(|priority| Identifier {
            media: PreparedMedia::new_stream(Box::new(FailedMedia)).unwrap(),
            interval_ms: 1,
            priority,
            first_key_only: false,
            regardless_of_activity: true,
            polite_maximum_wait_ms: None,
        })
        .collect();
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        ids,
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.process_audio(false, false, &[], &mut [0.0; 50]);
    assert_eq!(node.satisfied, [0, 0]);
    assert!(
        node.ids.iter().all(|id| id.media.0.is_finished()),
        "a failed high-priority ID must not starve another due ID"
    );
}

#[test]
fn playback_failure_retries_on_interval_without_silent_ptt() {
    struct CountedFailure(Arc<std::sync::atomic::AtomicUsize>);
    impl crate::audio::PcmStreamReader for CountedFailure {
        fn start(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn render(&mut self, _: &mut [f32]) -> crate::audio::PcmRead {
            crate::audio::PcmRead::Failed
        }
    }
    for announcement in [false, true] {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let media =
            PreparedMedia::new_stream(Box::new(CountedFailure(Arc::clone(&attempts)))).unwrap();
        let (ids, announcements) = if announcement {
            (
                vec![],
                vec![Announcement {
                    media,
                    interval_ms: 10,
                }],
            )
        } else {
            (
                vec![Identifier {
                    media,
                    interval_ms: 10,
                    priority: 1,
                    first_key_only: false,
                    regardless_of_activity: true,
                    polite_maximum_wait_ms: None,
                }],
                vec![],
            )
        };
        let (mut node, _) = NodeController::new(
            ControllerSettings::default(),
            ids,
            announcements,
            CourtesySettings::default(),
        )
        .unwrap();
        node.process_audio(false, false, &[], &mut [0.0; 481]);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        node.process_audio(false, false, &[], &mut [0.0; 479]);
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            1,
            "failed media must not retry per sample"
        );
        assert!(!node.keyed, "failed media must release demand");
        assert!(node.satisfied.iter().all(|time| *time == 0));
        assert!(node.announcements.iter().all(|a| a.satisfied == 0));
        node.process_audio(false, false, &[], &mut [0.0]);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }
}

fn regenerate(
    node: &mut NodeController,
    receiving: bool,
    samples: &mut [f32],
    tones: &[Option<f32>],
) -> bool {
    let mut parrot = vec![0.0; samples.len()];
    let mut active = false;
    node.process_audio_with_parrot(
        receiving,
        false,
        receiving,
        &[],
        samples,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: None,
            regeneration: Some(tones),
        },
    )
}

#[test]
fn regeneration_gap_is_not_missing_pcm_and_media_keeps_advancing() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.process_audio(false, false, &[], &mut [0.0; 12000]);
    assert!(control.queue_prepared_status(media(0.4, 3)));
    let mut audio = [0.0; 4];
    assert!(!regenerate(
        &mut node,
        false,
        &mut audio,
        &[Some(0.2), Some(0.0), None, None]
    ));
    assert_eq!(audio, [0.2, 0.0, 0.4, 0.0]);
    assert_eq!(control.reclaim().count(), 1);
    assert!(regenerate(&mut node, false, &mut [0.0], &[Some(0.0)]));
    assert!(!regenerate(&mut node, false, &mut [0.0], &[None]));
}

#[test]
fn regeneration_obeys_transmit_inhibits_and_has_no_ctcss_pseudo_activity() {
    for full_duplex in [false, true] {
        let (mut node, _) = NodeController::new(
            ControllerSettings {
                full_duplex,
                hang_ms: 0,
                ctcss_encode_on_input: true,
                transmit_timeout_ms: 1,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        let mut audio = [0.5; 64];
        assert!(!regenerate(&mut node, true, &mut audio, &[Some(0.2); 64]));
        if full_duplex {
            assert_eq!(audio[0], 0.2);
            assert_eq!(audio[63], 0.0);
        } else {
            assert_eq!(audio, [0.0; 64]);
        }
    }
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            ctcss_encode_on_input: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(regenerate(&mut node, false, &mut [0.0], &[Some(0.2)]));
    assert!(!node.ctcss_enabled());
}

#[test]
fn regeneration_is_partition_invariant() {
    let render = |partition: usize| {
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
        let tones: Vec<_> = (0..1920)
            .map(|n| {
                if n < 960 {
                    Some(if n < 480 { 0.25 } else { 0.0 })
                } else {
                    None
                }
            })
            .collect();
        let mut audio = vec![0.5; 1920];
        for (pcm, tone) in audio.chunks_mut(partition).zip(tones.chunks(partition)) {
            regenerate(&mut node, true, pcm, tone);
        }
        audio
    };
    assert_eq!(render(1), render(137));
    assert_eq!(render(960), render(137));
}

#[test]
fn regeneration_consumption_does_not_allocate_or_free_on_audio() {
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut audio = [0.0; 3];
    let mut parrot = [0.0; 3];
    let mut active = false;
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: None,
            regeneration: Some(&[Some(0.2), Some(0.0), None]),
        },
    );
    let allocations = AUDIO_ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(allocations, 0);
    assert_eq!(audio, [0.2, 0.0, 0.0]);
}

#[test]
fn peer_reply_completion_and_cancellation_never_allocate_or_free_on_audio() {
    let signal = crate::link::PeerSignals::new();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            ctcss_encode_on_input: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for ended in [false, true] {
        assert!(control.queue_prepared_status(media(0.4, 1).reply_to(Some(signal.reply_target()))));
        if ended {
            signal.end();
        }
        let mut radio = [0.0];
        let mut parrot = [0.0];
        let mut active = false;
        let mut replies = [PeerReplySample::default()];
        AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
        let keyed = node.process_audio_with_parrot(
            false,
            false,
            false,
            &[],
            &mut radio,
            ParrotOutput {
                samples: &mut parrot,
                active: &mut active,
                replies: Some(&mut replies),
                regeneration: None,
            },
        );
        let allocations = AUDIO_ALLOCATIONS.with(|count| count.replace(None).unwrap());
        assert_eq!(allocations, 0);
        assert!(!keyed);
        assert!(!node.ctcss_enabled());
        assert_eq!(radio, [0.0]);
        assert_eq!(replies[0].sample, if ended { 0.0 } else { 0.4 });
        assert_eq!(control.reclaim().count(), 1);
    }
}

struct PendingMedia;

impl crate::audio::PcmStreamReader for PendingMedia {
    fn render(&mut self, _: &mut [f32]) -> crate::audio::PcmRead {
        crate::audio::PcmRead::Pending
    }
    fn select_morse_fallback(&mut self) -> crate::audio::MorseSwitch {
        crate::audio::MorseSwitch::Pending
    }
}

#[test]
fn pending_peer_reply_stays_silent_and_ended_reply_is_reclaimed() {
    let signal = crate::link::PeerSignals::new();
    let courtesy = CourtesySettings {
        link: Some(media(0.2, 1)),
        ..CourtesySettings::default()
    };
    let (mut node, mut control) =
        NodeController::new(ControllerSettings::default(), vec![], vec![], courtesy).unwrap();
    node.link_unkeyed("peer", "", false);
    assert!(node.courtesy.ready(node.now));
    let pending = PreparedMedia::new_stream(Box::new(PendingMedia))
        .unwrap()
        .reply_to(Some(signal.reply_target()));
    assert!(control.queue_prepared_status(pending));
    let mut radio = [0.0];
    let mut parrot = [0.0];
    let mut active = false;
    let mut replies = [PeerReplySample::default()];
    let mut process = |node: &mut NodeController, receiving, replies: &mut [PeerReplySample; 1]| {
        node.process_audio_with_parrot(
            receiving,
            false,
            false,
            &[],
            &mut radio,
            ParrotOutput {
                samples: &mut parrot,
                active: &mut active,
                replies: Some(replies),
                regeneration: None,
            },
        )
    };
    assert!(!process(&mut node, true, &mut replies));
    assert_eq!(replies[0].sample, 0.0);
    assert!(node.telemetry.active.is_none());
    // An idle source admits its reply, but pending PCM must not address silence.
    replies[0].target = signal.reply_identity();
    assert!(!process(&mut node, false, &mut replies));
    assert_eq!(replies[0].target, 0);
    assert_eq!(replies[0].sample, 0.0);
    assert!(node.telemetry.active == Some(telemetry::Source::Status));
    signal.end();
    assert!(!process(&mut node, false, &mut replies));
    assert!(node.telemetry.active.is_none());
    assert_eq!(control.reclaim().count(), 1);
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
fn composition_pending_between_segments_retains_ptt_only_after_audio_starts() {
    struct Gapped {
        phase: Arc<std::sync::atomic::AtomicUsize>,
        started: bool,
    }
    impl crate::audio::PcmStreamReader for Gapped {
        fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
            match self.phase.load(Ordering::Acquire) {
                1 if !self.started => {
                    self.started = true;
                    output[0] = 0.25;
                    crate::audio::PcmRead::Samples(1)
                }
                2 => {
                    output[0] = 0.5;
                    crate::audio::PcmRead::FinalSamples(1)
                }
                _ => crate::audio::PcmRead::Pending,
            }
        }
    }
    let phase = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let id = Identifier {
        media: PreparedMedia::new_stream(Box::new(Gapped {
            phase: Arc::clone(&phase),
            started: false,
        }))
        .unwrap(),
        interval_ms: 100_000,
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
    node.now = 4_800_000;
    node.process_audio(false, false, &[], &mut [0.0; 64]);
    assert!(!node.keyed, "preparation alone must not key");
    phase.store(1, Ordering::Release);
    node.process_audio(false, false, &[], &mut [0.0; 1]);
    assert!(node.keyed);
    node.process_audio(false, false, &[], &mut [0.0; 3_000]);
    assert!(
        node.keyed,
        "one admitted composition must not unkey between segments"
    );
    phase.store(2, Ordering::Release);
    node.process_audio(false, false, &[], &mut [0.0; 3_000]);
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

#[test]
fn parrot_buffers_are_allocated_and_freed_on_control_around_lock_free_handoff() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    assert!(control.set_parrot_enabled(true));
    let allocations = AUDIO_ALLOCATIONS.with(|count| count.get().unwrap());
    assert!(
        allocations >= 4,
        "capture buffers and queues allocate on control"
    );

    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_event(false, false);
    assert_eq!(AUDIO_ALLOCATIONS.with(|count| count.get().unwrap()), 0);
    control.reclaim().for_each(drop);
    assert!(control.parrot_enabled());
    assert!(control.parrot.is_some());
    node.observe_parrot(true, &[0.25]);

    assert!(control.set_parrot_enabled(false));
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    node.process_event(false, false);
    assert_eq!(AUDIO_ALLOCATIONS.with(|count| count.get().unwrap()), 0);
    control.reclaim().for_each(drop);
    assert!(!control.parrot_enabled());
    assert!(control.parrot.is_none());
    assert!(control.take_parrot_capture().is_none());
    assert!(AUDIO_ALLOCATIONS.with(|count| count.get().unwrap()) >= 4);
    AUDIO_ALLOCATIONS.with(|count| count.set(None));
}

#[test]
fn parrot_output_size_mismatch_skips_sample_processing() {
    let (mut node, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut audio = [0.25];
    let mut parrot = [];
    let mut active = true;

    node.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut audio,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: None,
            regeneration: None,
        },
    );

    assert_eq!(audio, [0.25]);
    assert!(!active);
    assert_eq!(node.now, 0);
}

#[test]
fn parrot_lifecycle_retries_a_pending_ack_after_the_ack_queue_drains() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let (capture, _) = parrot::ParrotCapture::new();
    node.parrot_lifecycle
        .acknowledgements
        .push(parrot::ParrotAck::Disabled(capture))
        .unwrap();
    node.parrot_pending_ack = Some(parrot::ParrotAck::Enabled);

    node.process_event(false, false);
    assert!(matches!(
        node.parrot_pending_ack,
        Some(parrot::ParrotAck::Enabled)
    ));

    control.reclaim().for_each(drop);
    node.process_event(false, false);
    assert!(node.parrot_pending_ack.is_none());
    control.reclaim().for_each(drop);
}

#[test]
fn parrot_enable_ack_waits_when_the_callback_ack_queue_is_full() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(control.set_parrot_enabled(true));
    let (capture, _) = parrot::ParrotCapture::new();
    node.parrot_lifecycle
        .acknowledgements
        .push(parrot::ParrotAck::Disabled(capture))
        .unwrap();

    node.process_event(false, false);
    assert!(node.parrot_capture.is_some());
    assert!(matches!(
        node.parrot_pending_ack,
        Some(parrot::ParrotAck::Enabled)
    ));

    control.reclaim().for_each(drop);
    node.process_event(false, false);
    assert!(node.parrot_pending_ack.is_none());
    control.reclaim().for_each(drop);
    assert!(control.parrot.is_some());
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

    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        let sample = self.sample.filter(|_| {
            self.enabled
                .as_ref()
                .is_none_or(|enabled| enabled.load(std::sync::atomic::Ordering::Acquire))
        })?;
        output.fill(sample);
        Some(output.len())
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
    let (outbound, consumer) = crate::audio::LinkAudioQueue::new(maximum * 2, 64)
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
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    standby_enabled.store(true, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    selected_enabled.store(true, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    let (local_out, _) = crate::audio::LinkAudioQueue::new(4, 64)
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

    links
        .process(
            &mut node,
            true,
            &mut [0.1, 0.1],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(links.active_count(), 1);
    peer_enabled.store(false, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut [0.0, 0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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

    let (to_transceiver, mut from_transceiver) = crate::audio::LinkAudioQueue::new(8, 64)
        .unwrap()
        .into_endpoints();
    let (to_monitor, mut from_monitor) = crate::audio::LinkAudioQueue::new(8, 64)
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
    let media = PreparedMedia::new_parrot_stream(None, Box::new(ParrotSamples { offset: 0 }));
    assert!(control.queue_prepared_parrot(media));

    let mut local = [0.0; 2];
    links
        .process(
            &mut node,
            false,
            &mut local,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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

    let (outbound, mut peer_output) = crate::audio::LinkAudioQueue::new(4, 64)
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
    assert!(
        control.queue_prepared_parrot(PreparedMedia::new_parrot_stream(
            None,
            Box::new(OneSampleParrot)
        ))
    );

    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert!(!node.parrot_playback_active());
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut sent = [0.0];
    assert_eq!(peer_output.read(&mut sent), 0);
    assert_eq!(sent, [0.4]);
}

fn queued_parrot() -> PreparedMedia {
    PreparedMedia::new_parrot_stream(None, Box::new(ParrotSamples { offset: 0 }))
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
            replies: None,
            regeneration: None,
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
            replies: None,
            regeneration: None,
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
            replies: None,
            regeneration: None,
        },
    );

    assert_eq!(parrot, [0.0]);
    assert!(!parrot_active);
    assert!(!node.parrot_playback_active());
    control.reclaim().for_each(drop);
}

#[test]
fn disabling_parrot_interrupts_playback_at_the_next_audio_boundary() {
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
    let mut rf = [0.0];
    let mut parrot = [0.0];
    let mut active = false;
    node.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut rf,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: None,
            regeneration: None,
        },
    );
    assert_eq!(parrot, [0.4]);
    assert!(active);
    assert!(control.set_parrot_enabled(false));
    node.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut rf,
        ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: None,
            regeneration: None,
        },
    );
    assert_eq!(parrot, [0.0]);
    assert!(!active);
    control.reclaim().for_each(drop);
    assert!(!control.parrot_enabled());
    assert!(control.parrot.is_none());
}

#[test]
fn peer_transmit_is_subtracted_from_its_own_outbound_audio() {
    use crate::link::LinkAudio;

    let (outbound, mut peer_out) = crate::audio::LinkAudioQueue::new(4, 64)
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

    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    let mut outbound = [1.0; 1];
    assert_eq!(peer_out.read(&mut outbound), 1);
    assert_eq!(outbound, [0.0]);
    let mut rf = [0.5];
    links
        .process(
            &mut node,
            true,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    let (grouped_outbound, _) = crate::audio::LinkAudioQueue::new(2, 64)
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
    let (plain_outbound, _) = crate::audio::LinkAudioQueue::new(2, 64)
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

    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    grouped_enabled.store(true, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            true,
            &mut [0.1],
            crate::link::SourcePosition::default(),
        )
        .unwrap();

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
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    links
        .process(
            &mut node,
            true,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    selection.publish_desired(NonZeroUsize::new(2));
    links
        .process(
            &mut node,
            true,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));

    selected_enabled.store(true, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    selected_enabled.store(false, std::sync::atomic::Ordering::Release);
    standby_enabled.store(true, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    standby_enabled.store(false, std::sync::atomic::Ordering::Release);
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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

    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    selection.publish_desired(NonZeroUsize::new(2));
    links
        .process(
            &mut node,
            false,
            &mut rf,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    let (outbound, consumer) = crate::audio::LinkAudioQueue::new(maximum * 2, 64)
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
fn courtesy_latest_unkey_validates_identity_and_cancels_only_its_source() {
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
    assert!(!planner.ready(2));
    assert!(!planner.ready(3));
    assert!(planner.ready(4));
    assert_eq!(planner.pop(), Some(1));
    assert_eq!(planner.pop(), None);
    let maximum = "x".repeat(63);
    planner.schedule(&maximum, "selected", 5);
    assert_eq!(planner.pop(), Some(2));
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

    links
        .process(
            &mut node,
            true,
            &mut [0.25],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    let mut sent = [0.0];
    assert_eq!(outbound.read(&mut sent), 0);
    assert_eq!(sent, [0.25]);

    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    assert_eq!(outbound.read(&mut sent), 1);
    assert_eq!(sent, [0.0]);

    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
    dispatcher.dispatch(1);
    assert_eq!(outbound.read(&mut sent), 1);

    control.queue_status("E", Some(vec![0.6])).unwrap();
    let mut elapsed = [0.0; MAXIMUM];
    links
        .process(
            &mut node,
            false,
            &mut elapsed,
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
    links
        .process(
            &mut node,
            false,
            &mut [0.0],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
        fn render(&mut self, _: &mut [f32]) -> Option<usize> {
            None
        }
    }
    let (outbound, mut consumer) = LinkAudioQueue::new(1920, 64).unwrap().into_endpoints();
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
        audio
            .process(
                &mut node,
                true,
                &mut [0.25; 960],
                crate::link::SourcePosition::default(),
            )
            .unwrap();
    }
    let dropped = status.dropped_blocks();
    let dispatched = dispatcher.dispatch(2);
    let mut output = [0.0; 1920];
    let missing = consumer.read(&mut output);
    audio
        .process(
            &mut node,
            true,
            &mut [0.5; 960],
            crate::link::SourcePosition::default(),
        )
        .unwrap();
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
fn status_waits_for_every_receive_source_without_a_deadline() {
    for (local, linked, activity) in [
        (true, false, true),
        (false, true, true),
        (false, false, true),
    ] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                telemetry_duck_db: 0,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        assert!(control.queue_prepared_status(media(0.75, 4)));
        for _ in 0..2 {
            node.now += samples(3_600_000);
            let mut output = [0.0; 4];
            node.process_audio_outputs(local, linked, activity, &[], &mut output, None);
            assert_eq!(output, [0.0; 4], "receive={local}/{linked}/{activity}");
            assert_eq!(control.reclaim().count(), 0);
        }
        node.process_event(false, false);
        node.now += 12_000;
        let mut output = [0.0; 4];
        node.process_audio(false, false, &[], &mut output);
        assert_eq!(output, [0.75; 4], "deferred status must retain its speech");
        assert_eq!(control.reclaim().count(), 1);
    }
}

#[test]
fn started_status_switches_to_morse_when_receive_begins() {
    for (local, linked, activity) in [
        (true, false, true),
        (false, true, true),
        (false, false, true),
    ] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                hang_ms: 0,
                telemetry_duck_db: 0,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        node.now = 12_000;
        assert!(control.queue_prepared_status(media(0.75, 4)));
        let mut first = [0.0];
        node.process_audio(false, false, &[], &mut first);
        assert_eq!(first, [0.75]);
        let mut busy = [0.0; 96];
        let keyed = node.process_audio_outputs(local, linked, activity, &[], &mut busy, None);
        assert!(keyed);
        assert!(busy.iter().any(|sample| *sample != 0.0));
        assert!(busy.iter().all(|sample| sample.abs() <= 0.502));
        assert_eq!(control.reclaim().count(), 0);
        node.process_event(false, false);
        let mut continued = [0.0; 4];
        node.process_audio(false, false, &[], &mut continued);
        assert!(continued.iter().all(|sample| sample.abs() < 0.75));
        assert_eq!(control.reclaim().count(), 0, "Morse continues after unkey");
    }
}

#[test]
fn held_status_does_not_block_an_expired_identifier() {
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            telemetry_duck_db: 0,
            ..ControllerSettings::default()
        },
        vec![Identifier {
            media: media(0.25, 100),
            interval_ms: 1000,
            priority: 1,
            first_key_only: false,
            regardless_of_activity: true,
            polite_maximum_wait_ms: Some(1),
        }],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.now = 12_000;
    assert!(
        control.queue_prepared_status(PreparedMedia::new_stream(Box::new(PendingMedia)).unwrap())
    );
    node.process_audio(false, false, &[], &mut [0.0]);
    node.now = samples(1002);
    let mut output = [0.0; 96];
    node.process_audio(false, true, &[], &mut output);
    assert!(node.telemetry.active == Some(telemetry::Source::Identifier(0)));
    assert!(
        output.iter().any(|sample| *sample != 0.0),
        "the due ID must play"
    );
    assert_eq!(
        control.reclaim().count(),
        0,
        "the held status is still pending"
    );
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
fn overlapping_sources_play_only_the_last_unkey_courtesy() {
    for (local_last, last_has_tone) in [(true, true), (false, true), (true, false), (false, false)]
    {
        let courtesy = CourtesySettings {
            receiver: (!local_last || last_has_tone).then(|| media(0.1, 4)),
            link: None,
            peers: if local_last || last_has_tone {
                vec![("north".into(), media(0.3, 4))]
            } else {
                vec![]
            },
        };
        let (mut node, _) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                courtesy_delay_ms: 1,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            courtesy,
        )
        .unwrap();
        node.link_keyed("north");
        node.process_audio(true, true, &[], &mut [0.0; 96]);
        if local_last {
            node.link_unkeyed("north", "", false);
        }
        let mut still_receiving = [0.0; 96];
        node.process_audio(local_last, !local_last, &[], &mut still_receiving);
        assert_eq!(still_receiving, [0.0; 96]);
        if !local_last {
            node.link_unkeyed("north", "", false);
        }
        let mut output = [0.0; 96];
        node.process_audio(false, false, &[], &mut output);
        let mut expected = [0.0; 96];
        if last_has_tone {
            expected[48..52].fill(if local_last { 0.1 } else { 0.3 });
        }
        assert_eq!(
            output, expected,
            "local_last={local_last}, tone={last_has_tone}"
        );
    }
}

#[test]
fn courtesy_selection_cancellation_latest_unkey_and_kerchunk() {
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
    assert_eq!(output, [0.3, 0.0, 0.0]);
    for _ in 0..17 {
        node.link_unkeyed("south", "", false);
    }
    let mut output = [0.0; 17];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output.iter().filter(|v| **v == 0.2).count(), 1);
}

#[test]
fn kerchunk_unkeys_immediately_without_hang_or_release_messages() {
    for (limit, frames, expect_hang) in [(1, 1, false), (1, 48, false), (1, 49, true), (0, 1, true)]
    {
        for release_message in [false, true] {
            let (mut node, _) = NodeController::new(
                ControllerSettings {
                    full_duplex: true,
                    hang_ms: 10,
                    kerchunk_max_ms: limit,
                    ..ControllerSettings::default()
                },
                vec![],
                if release_message {
                    vec![Announcement {
                        media: media(0.5, 1),
                        interval_ms: 0,
                    }]
                } else {
                    vec![]
                },
                CourtesySettings::default(),
            )
            .unwrap();
            assert!(node.process_audio(true, false, &[], &mut vec![0.0; frames]));
            assert_eq!(
                node.process_event(false, false),
                expect_hang,
                "limit={limit} frames={frames} message={release_message}"
            );
            if !expect_hang {
                let mut output = [0.0; 1000];
                assert!(!node.process_audio(false, false, &[], &mut output));
                assert_eq!(output, [0.0; 1000]);
            }
        }
    }
}

#[test]
fn final_kerchunk_discards_an_overlapping_sources_waiting_courtesy() {
    for local_last in [false, true] {
        let (mut node, _) = NodeController::new(
            ControllerSettings {
                full_duplex: true,
                hang_ms: 10,
                kerchunk_max_ms: 1,
                courtesy_delay_ms: 1,
                ..ControllerSettings::default()
            },
            vec![],
            vec![],
            CourtesySettings {
                receiver: Some(media(0.1, 4)),
                link: Some(media(0.2, 4)),
                ..CourtesySettings::default()
            },
        )
        .unwrap();
        if local_last {
            node.link_keyed("peer");
            node.process_audio(false, true, &[], &mut [0.0; 96]);
            node.process_audio(true, true, &[], &mut [0.0]);
            node.link_unkeyed("peer", "", false);
            node.process_audio(true, false, &[], &mut [0.0]);
        } else {
            node.process_audio(true, false, &[], &mut [0.0; 96]);
            node.link_keyed("peer");
            node.process_audio(true, true, &[], &mut [0.0]);
            node.process_audio(false, true, &[], &mut [0.0]);
            node.link_unkeyed("peer", "", true);
        }
        assert!(
            node.courtesy.pending(),
            "earlier release has a waiting tone"
        );
        assert!(
            !node.process_event(false, false),
            "final kerchunk must release PTT: local_last={local_last}"
        );
        let mut output = [0.0; 960];
        assert!(!node.process_audio(false, false, &[], &mut output));
        assert_eq!(output, [0.0; 960]);
    }
}

#[test]
fn kerchunk_does_not_interrupt_other_traffic_or_its_normal_release() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            hang_ms: 10,
            kerchunk_max_ms: 1,
            ..ControllerSettings::default()
        },
        vec![],
        vec![Announcement {
            media: media(0.25, 1),
            interval_ms: 0,
        }],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_tails(vec![Tail {
        media: media(0.5, 1),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    node.process_audio(true, false, &[], &mut [0.0; 96]);
    node.link_keyed("peer");
    node.process_audio(true, true, &[], &mut [0.0]);
    node.link_unkeyed("peer", "", true);
    assert!(
        node.process_event(true, false),
        "local receiver still needs PTT"
    );
    assert!(
        node.process_event(false, false),
        "normal local release retains hang"
    );
    assert!(
        node.tails.pending(),
        "a prior peer kerchunk cannot suppress a normal local tail"
    );
    let mut output = [0.0; 1024];
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(
        output.iter().filter(|sample| **sample == 0.25).count(),
        1,
        "the normal local release must retain its announcement"
    );
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
    // The polite ID now waits through the traffic-release hang deadline; the
    // existing announcement remains due from the last transmitted sample.
    assert_eq!(&output[47..49], &[0.2, 0.1]);
}

#[test]
fn release_announcement_does_not_key_from_idle_after_site_inhibit() {
    let (mut node, control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![Announcement {
            media: media(0.2, 1),
            interval_ms: 0,
        }],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(node.process_audio(true, false, &[], &mut [0.0]));
    control.set_site_inhibit(true);
    assert!(!node.process_audio(true, false, &[], &mut [0.0]));
    let mut output = [0.0; 2];
    assert!(!node.process_audio(false, false, &[], &mut output));
    assert_eq!(output, [0.0; 2]);
    assert!(node.announcements[0].release_pending);
    assert!(
        !node.release_pending,
        "an inhibited release cannot key from idle"
    );
    control.set_site_inhibit(false);
    let mut output = [0.0; 4];
    assert!(!node.process_audio(false, false, &[], &mut output));
    assert_eq!(output, [0.0; 4]);
    assert!(node.process_audio(true, false, &[], &mut [0.0]));
    node.process_audio(false, false, &[], &mut output);
    assert_eq!(output, [0.2, 0.0, 0.0, 0.0]);
    assert!(!node.announcements[0].release_pending);
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
    let bad = ControllerSettings {
        ctcss_hang_ms: 1,
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
    let mut output = [0.0; 48];
    node.process_audio(true, false, &[], &mut output);
    assert!(
        output[10..].iter().any(|sample| *sample != 0.0),
        "the deadline must select Morse, not the ten-sample voice ID"
    );
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
fn first_key_controller_plays_one_initial_id_and_rotates_on_later_keyups() {
    let ids = [0.25, 0.5]
        .into_iter()
        .map(|value| Identifier {
            media: media(value, 10),
            interval_ms: 1,
            priority: 1,
            first_key_only: true,
            regardless_of_activity: false,
            polite_maximum_wait_ms: Some(60_000),
        })
        .collect();
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..Default::default()
        },
        ids,
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for expected in [0.25, 0.5, 0.25] {
        node.process_audio(false, false, &[], &mut [0.0; 3000]);
        node.process_audio(true, false, &[], &mut [0.0; 48]);
        let mut output = [0.0; 9600];
        assert!(!node.process_audio(false, false, &[], &mut output));
        let audible: Vec<_> = output.into_iter().filter(|sample| *sample != 0.0).collect();
        assert_eq!(audible, vec![expected; 10]);
    }
}

#[test]
fn first_key_does_not_play_an_expired_periodic_id_before_its_initial_id() {
    let ids = [(0.25, 10, true), (0.5, 10, true), (0.75, 0, false)]
        .into_iter()
        .map(|(value, priority, first_key_only)| Identifier {
            media: media(value, 10),
            interval_ms: 940_000,
            priority,
            first_key_only,
            regardless_of_activity: false,
            polite_maximum_wait_ms: Some(60_000),
        })
        .collect();
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..Default::default()
        },
        ids,
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    // The station has been idle past both the periodic interval and polite wait.
    node.now = samples(1_000_001);
    node.process_event(true, false);
    for (polite, expected) in [(None, Some(2)), (Some(60_000), None)] {
        node.ids[0].polite_maximum_wait_ms = polite;
        assert_eq!(node.select_identifier(true, true, false), expected);
    }
    let mut played = Vec::new();
    let mut previous = None;
    for (receiving, count) in [(true, 4_800), (false, 9_600)] {
        for _ in 0..count {
            node.process_audio(receiving, false, &[], &mut [0.0]);
            let current = match node.telemetry.active {
                Some(Source::Identifier(index)) => Some(index),
                _ => None,
            };
            if current != previous {
                if let Some(index) = current {
                    played.push(index);
                }
            }
            previous = current;
        }
        if receiving {
            assert!(
                played.is_empty(),
                "the initial ID must retain its polite wait"
            );
        }
    }
    assert_eq!(played, vec![0], "one initial ID must cover this key-up");
    assert_ne!(node.satisfied[0], 0);
    assert_eq!(node.satisfied[0], node.satisfied[2]);
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
fn cancelled_status_is_reclaimed_without_allocating_or_freeing_on_audio() {
    for started in [false, true] {
        let (mut node, mut control) = NodeController::new(
            ControllerSettings::default(),
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(control.queue_prepared_status(media(0.5, 4).cancel_when(Arc::clone(&cancelled))));
        if started {
            node.now = 12_000;
            let mut first = [0.0];
            node.process_audio(false, false, &[], &mut first);
            assert_eq!(first, [0.5]);
        }
        cancelled.store(true, Ordering::Release);
        let mut output = [0.0; 8];
        AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
        node.process_audio(false, false, &[], &mut output);
        let allocations = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
        assert_eq!(allocations, Some(0));
        assert_eq!(output, [0.0; 8]);
        assert_eq!(control.reclaim().count(), 1);
    }
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
    let mut tone = crate::audio::ToneSequence::new("1000/60000", -6).unwrap();
    let calls = AUDIO_ALLOCATIONS.with(|count| count.replace(None));
    assert_eq!(tone.rendered_samples(), 2_880_000);
    // The released DSO owns setup allocations; this Rust allocator sees none.
    assert!(calls.unwrap() <= 3);
    let mut output = [0.0; 48];
    AUDIO_ALLOCATIONS.with(|count| count.set(Some(0)));
    assert_eq!(tone.render(&mut output), 48);
    assert_eq!(AUDIO_ALLOCATIONS.with(|count| count.replace(None)), Some(0));
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
fn activity_scoped_ctcss_tracks_receive_and_its_own_hangtime() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            ctcss_encode_on_input: true,
            ctcss_hang_ms: 1,
            hang_ms: 10,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    assert!(!node.ctcss_enabled());
    node.process_audio(true, false, &[], &mut [0.0]);
    assert!(node.ctcss_enabled());
    node.process_audio(false, false, &[], &mut [0.0; 48]);
    assert!(node.ctcss_enabled());
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(!node.ctcss_enabled());
}

#[test]
fn activity_scoped_ctcss_includes_pending_command_response_but_not_ids_or_courtesies() {
    let (mut response, mut control) = NodeController::new(
        ControllerSettings {
            ctcss_encode_on_input: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    control.queue_status("E", Some(vec![0.5; 2])).unwrap();
    response.process_audio(false, false, &[], &mut [0.0]);
    assert!(response.ctcss_enabled());
    response.process_audio(false, false, &[], &mut [0.0; 12_001]);
    assert!(response.ctcss_enabled());
    response.process_audio(false, false, &[], &mut [0.0]);
    assert!(!response.ctcss_enabled());

    let id = Identifier {
        media: media(0.2, 2),
        interval_ms: 1,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: true,
        polite_maximum_wait_ms: None,
    };
    let courtesy = CourtesySettings {
        link: Some(media(0.3, 2)),
        ..CourtesySettings::default()
    };
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            ctcss_encode_on_input: true,
            ..ControllerSettings::default()
        },
        vec![id],
        vec![],
        courtesy,
    )
    .unwrap();
    node.telemetry.active = Some(telemetry::Source::Identifier(0));
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(!node.ctcss_enabled());
    node.telemetry.active = Some(telemetry::Source::Courtesy(0));
    node.process_audio(false, false, &[], &mut [0.0]);
    assert!(!node.ctcss_enabled());
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
