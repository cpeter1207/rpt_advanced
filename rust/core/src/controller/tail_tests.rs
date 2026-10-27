use super::*;

fn pcm(value: f32, count: usize) -> PreparedMedia {
    PreparedMedia::new(Some(vec![value; count]), "E", MorseSettings::default()).unwrap()
}

fn fixture(delay_ms: u64, interval_ms: u64, ids: Vec<Identifier>) -> NodeController {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            hang_ms: 1000,
            ..Default::default()
        },
        ids,
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_tails(vec![Tail {
        media: pcm(0.5, 4800),
        interval_ms,
        delay_ms,
    }])
    .unwrap();
    node
}

fn at(node: &mut NodeController, ms: u64, receiving: bool) -> (bool, f32) {
    node.now = samples(ms);
    let mut out = [0.0];
    let keyed = node.process_audio(receiving, false, &[0.0], &mut out);
    (keyed, out[0])
}

#[test]
fn tail_playback_outlasts_traffic_timeout_without_missing_pcm() {
    let (mut node, _) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            transmit_timeout_ms: 1,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.set_tails(vec![Tail {
        media: pcm(0.5, 4800),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    node.process_audio(true, false, &[], &mut [0.25]);
    let mut output = [0.0; 4800];
    assert!(node.process_audio(false, false, &[], &mut output));
    assert_eq!(
        output.iter().filter(|sample| **sample == 0.5).count(),
        4800,
        "traffic timeout must not interrupt a tail's PCM or PTT"
    );
}

#[test]
fn tail_zero_and_positive_delay() {
    for delay in [0, 250] {
        let mut node = fixture(delay, 0, vec![]);
        assert!(at(&mut node, 0, true).0);
        assert!(at(&mut node, 1000, false).0);
        assert_eq!(at(&mut node, 1999, false), (true, 0.0));
        if delay == 0 {
            assert_eq!(at(&mut node, 2000, false), (true, 0.5));
        } else {
            assert_eq!(at(&mut node, 2000, false), (false, 0.0));
            assert_eq!(at(&mut node, 2249, false), (false, 0.0));
            assert_eq!(at(&mut node, 2250, false), (true, 0.5));
        }
        assert!(
            !node.tails.waiting_for_unkey(node.now),
            "started playback cannot begin another post-unkey delay"
        );
    }
}

fn identifier(interval_ms: u64, wait_ms: u64) -> Identifier {
    Identifier {
        media: pcm(0.25, 480),
        interval_ms,
        priority: 1,
        first_key_only: false,
        regardless_of_activity: false,
        polite_maximum_wait_ms: Some(wait_ms),
    }
}

#[test]
fn tail_id_deadline_precedence() {
    for deadline in [1500, 2000, 2100, 2300] {
        let mut node = fixture(250, 0, vec![identifier(1000, deadline - 1000)]);
        at(&mut node, 0, true);
        at(&mut node, 1000, false);
        if deadline > 2000 {
            assert!(!at(&mut node, 2000, false).0);
        }
        if deadline > 2250 {
            assert_eq!(at(&mut node, 2250, false), (true, 0.5));
        }
        assert_eq!(
            at(&mut node, deadline, false),
            (true, 0.25),
            "deadline {deadline}"
        );
        let mut rest = [0.0; 480];
        node.process_audio(false, false, &[], &mut rest);
        if deadline <= 2000 {
            let unkey = (deadline + 60).max(2000);
            assert!(!at(&mut node, unkey, false).0);
            // Positive delay begins at the actual unkey, not nominal hang expiry.
            assert_eq!(at(&mut node, unkey + 250, false), (true, 0.5));
        } else {
            assert_eq!(at(&mut node, deadline.max(2250) + 10, false), (true, 0.5));
        }
    }
}

#[test]
fn tail_receive_pending_vs_playing() {
    let mut node = fixture(250, 0, vec![]);
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    at(&mut node, 2000, false);
    at(&mut node, 2200, true);
    assert_eq!(at(&mut node, 2250, false), (true, 0.0));
    assert!(!at(&mut node, 3250, false).0);
    assert_eq!(at(&mut node, 3500, false), (true, 0.5));
    let (_, interrupted) = at(&mut node, 3501, true);
    assert_ne!(interrupted, 0.5, "primary must stop at receive boundary");
    assert!(node.telemetry.active == Some(Source::Tail(0)));
    assert!(
        node.tails.ready(node.now).is_none(),
        "ongoing receive cannot admit the playing tail as a second attempt"
    );
    at(&mut node, 3502, false);
    assert!(node.telemetry.active == Some(Source::Tail(0)));
}

#[test]
fn tail_nonrecursive_cooldown() {
    let mut node = fixture(0, 1_800_000, vec![]);
    assert!(!at(&mut node, 2_000_000, false).0, "idle cannot arm a tail");
    at(&mut node, 2_000_001, true);
    at(&mut node, 2_001_000, false);
    assert_eq!(at(&mut node, 2_002_000, false), (true, 0.5));
    let mut rest = [0.0; 4800];
    node.process_audio(false, false, &[], &mut rest);
    at(&mut node, 2_003_000, true);
    at(&mut node, 2_004_000, false);
    assert_eq!(at(&mut node, 2_005_000, false).1, 0.0);
    assert!(!node.tails.pending(), "cooldown excludes this release");
    assert!(
        !at(&mut node, 4_000_000, false).0,
        "generated unkey is not a release"
    );
    at(&mut node, 4_000_001, true);
    at(&mut node, 4_001_000, false);
    assert_eq!(at(&mut node, 4_002_000, false), (true, 0.5));
}

#[test]
fn tail_delay_preserves_identifier_release_safeguard() {
    let mut node = fixture(250, 0, vec![identifier(1000, 1000)]);
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    assert_eq!(at(&mut node, 2000, false).1, 0.25);
    node.process_audio(false, false, &[], &mut [0.0; 479]);
    assert!(
        at(&mut node, 2010, false).0,
        "do not clip the ID's release margin"
    );
    assert!(!at(&mut node, 2060, false).0);
    assert_eq!(at(&mut node, 2309, false), (false, 0.0));
    assert_eq!(at(&mut node, 2310, false), (true, 0.5));
}

#[test]
fn tail_timeout_does_not_hold_a_completed_message() {
    let mut node = fixture(0, 10000, vec![]);
    node.settings.transmit_timeout_ms = 10;
    at(&mut node, 0, true);
    at(&mut node, 1, false);
    assert_eq!(at(&mut node, 1001, false).1, 0.5);
    let mut out = [0.0; 6000];
    assert!(node.process_audio(false, false, &[], &mut out));
    assert!(
        node.telemetry.active.is_none(),
        "completed message releases its playback slot even across a traffic timeout"
    );
    assert_eq!(
        1 + out.iter().filter(|sample| **sample == 0.5).count(),
        4800
    );
}

#[test]
fn tail_configuration_order_and_callback_partitioning() {
    let run = |partition: usize| {
        let mut node = fixture(0, 0, vec![]);
        node.set_tails(vec![
            Tail {
                media: pcm(0.5, 48),
                interval_ms: 0,
                delay_ms: 0,
            },
            Tail {
                media: pcm(0.25, 48),
                interval_ms: 0,
                delay_ms: 0,
            },
        ])
        .unwrap();
        at(&mut node, 0, true);
        at(&mut node, 1000, false);
        node.now = samples(1999);
        let mut output = vec![0.0; 256];
        for chunk in output.chunks_mut(partition) {
            node.process_audio(false, false, &[], chunk);
        }
        output
    };
    let expected = run(1);
    assert_eq!(&expected[48..96], &[0.5; 48]);
    assert_eq!(&expected[96..144], &[0.25; 48]);
    for partition in [7, 48, 128, 256] {
        assert_eq!(run(partition), expected);
    }
}

#[test]
fn tail_deferred_high_priority_id_does_not_hide_expired_id() {
    let mut high = identifier(1000, 10000);
    high.priority = 2;
    let mut node = fixture(0, 0, vec![high, identifier(1000, 1000)]);
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    at(&mut node, 2000, false);
    assert!(node.telemetry.active == Some(Source::Identifier(1)));
}

#[test]
fn tail_multiple_positive_delays_begin_at_each_actual_unkey() {
    let mut node = fixture(0, 0, vec![]);
    node.set_tails(vec![
        Tail {
            media: pcm(0.5, 1),
            interval_ms: 0,
            delay_ms: 100,
        },
        Tail {
            media: pcm(0.25, 1),
            interval_ms: 0,
            delay_ms: 200,
        },
    ])
    .unwrap();
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    assert_eq!(at(&mut node, 2000, false), (false, 0.0));
    assert_eq!(at(&mut node, 2100, false), (true, 0.5));
    assert!(at(&mut node, 2149, false).0);
    assert_eq!(at(&mut node, 2150, false), (false, 0.0));
    assert_eq!(at(&mut node, 2349, false), (false, 0.0));
    assert_eq!(at(&mut node, 2350, false), (true, 0.25));
}

#[test]
fn tail_all_sources_must_release_and_half_duplex_defers_started_morse() {
    let mut node = fixture(0, 0, vec![]);
    node.settings.full_duplex = false;
    at(&mut node, 0, true);
    node.now = samples(1000);
    node.process_audio_outputs(false, false, true, &[], &mut [0.0], None);
    // A nonforwarding peer is still receiving, even without local RF or link mix.
    node.now = samples(3000);
    let mut out = [0.0];
    node.process_audio_outputs(false, false, true, &[], &mut out, None);
    assert_eq!(out, [0.0]);
    at(&mut node, 3001, false);
    assert_eq!(at(&mut node, 4001, false), (true, 0.5));
    assert_eq!(at(&mut node, 4002, true), (false, 0.0));
    let (_, sample) = at(&mut node, 4003, false);
    assert_ne!(
        sample, 0.5,
        "release must not restore the interrupted primary"
    );
}

#[test]
fn tail_failed_preparation_does_not_start_cooldown() {
    let mut node = fixture(0, 10000, vec![]);
    let mut preparation = MessagePreparation::new(1);
    node.set_tails(vec![Tail {
        media: preparation.register(0).unwrap(),
        interval_ms: 10000,
        delay_ms: 0,
    }])
    .unwrap();
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    at(&mut node, 2000, false);
    let first = preparation.take_request().unwrap();
    assert!(
        preparation
            .complete(first.attempt, Err(ControllerError))
            .is_ok()
    );
    at(&mut node, 2001, false);
    at(&mut node, 2002, true);
    at(&mut node, 2003, false);
    at(&mut node, 3003, false);
    assert!(
        preparation.take_request().is_some(),
        "failure is eligible at next release"
    );
}

#[test]
fn tail_preempted_primary_restarts_at_its_beginning() {
    let mut node = fixture(0, 0, vec![identifier(1000, 1001)]);
    node.set_tails(vec![Tail {
        media: PreparedMedia::new(Some(vec![0.1, 0.2, 0.3]), "E", MorseSettings::default())
            .unwrap(),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    assert_eq!(at(&mut node, 2000, false).1, 0.1);
    let mut next = [0.0];
    node.process_audio(false, false, &[], &mut next);
    assert_eq!(next, [0.2]);
    assert_eq!(at(&mut node, 2001, false).1, 0.25);
    node.process_audio(false, false, &[], &mut [0.0; 479]);
    assert_eq!(at(&mut node, 2011, false).1, 0.1);
}

#[test]
fn tail_invalid_durations_preserve_the_previously_installed_policy() {
    let overflow = u64::MAX / 48 + 1;
    for (interval_ms, delay_ms) in [(overflow, 0), (0, overflow)] {
        let mut node = fixture(0, 0, vec![]);
        assert!(
            node.set_tails(vec![Tail {
                media: pcm(0.25, 1),
                interval_ms,
                delay_ms,
            }])
            .is_err()
        );
        at(&mut node, 0, true);
        at(&mut node, 1000, false);
        assert_eq!(at(&mut node, 2000, false), (true, 0.5));
    }
}

#[test]
fn tail_kerchunk_suppression_does_not_arm_a_later_idle_playback() {
    let mut node = fixture(0, 0, vec![]);
    node.settings.kerchunk_max_ms = 100;
    at(&mut node, 0, true);
    assert_eq!(at(&mut node, 50, false), (false, 0.0));
    assert_eq!(at(&mut node, 2000, false), (false, 0.0));
    assert!(!node.tails.pending());
}

#[test]
fn tail_receive_cancels_an_attempt_before_any_prepared_pcm() {
    let mut node = fixture(0, 0, vec![]);
    let mut preparation = MessagePreparation::new(1);
    node.set_tails(vec![Tail {
        media: preparation.register(0).unwrap(),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    at(&mut node, 2000, false);
    let first = preparation.take_request().unwrap();
    at(&mut node, 2001, true);
    assert!(node.telemetry.active.is_none());
    assert!(!node.tails.pending());
    assert!(
        preparation
            .complete(first.attempt, Ok(pcm(0.75, 1)))
            .is_ok()
    );
    at(&mut node, 2002, false);
    assert_eq!(at(&mut node, 3002, false).1, 0.0);
    let second = preparation.take_request().unwrap();
    assert_ne!(first.attempt, second.attempt);
    assert!(
        preparation
            .complete(second.attempt, Ok(pcm(0.25, 1)))
            .is_ok()
    );
    assert_eq!(at(&mut node, 3003, false).1, 0.25);
}

#[test]
fn tail_identifier_preempts_pending_values_only_on_an_audio_sample() {
    let mut node = fixture(0, 0, vec![identifier(1000, 1001)]);
    let mut preparation = MessagePreparation::new(1);
    node.set_tails(vec![Tail {
        media: preparation.register(0).unwrap(),
        interval_ms: 0,
        delay_ms: 0,
    }])
    .unwrap();
    at(&mut node, 0, true);
    at(&mut node, 1000, false);
    at(&mut node, 2000, false);
    let first = preparation.take_request().unwrap();
    node.now = samples(2001);
    node.process_event(false, false);
    assert!(node.telemetry.active == Some(Source::Tail(0)));
    assert_eq!(at(&mut node, 2001, false).1, 0.25);
    assert!(node.telemetry.active == Some(Source::Identifier(0)));
    assert!(
        preparation
            .complete(first.attempt, Ok(pcm(0.75, 1)))
            .is_ok()
    );
    node.process_audio(false, false, &[], &mut [0.0; 479]);
    assert_eq!(at(&mut node, 2011, false).1, 0.0);
    let second = preparation.take_request().unwrap();
    assert_ne!(first.attempt, second.attempt);
    assert!(
        preparation
            .complete(second.attempt, Ok(pcm(0.5, 1)))
            .is_ok()
    );
    assert_eq!(at(&mut node, 2012, false).1, 0.5);
}
