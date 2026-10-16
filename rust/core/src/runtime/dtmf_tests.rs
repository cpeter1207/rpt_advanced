use super::*;

fn feed_symbols(commands: &mut DtmfCommands, symbols: &str) -> Vec<DigitOutcome> {
    symbols
        .chars()
        .filter_map(|digit| commands.feed(DigitEvent::Digit { digit, now_ms: 1 }))
        .collect()
}

#[test]
fn pad_capture_waits_for_real_unkey() {
    for symbols in [
        String::new(),
        "*#ABCD0123456789".into(),
        "*".repeat(127),
        "#".repeat(128),
    ] {
        let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
        assert!(feed_symbols(&mut commands, "*82").is_empty());
        assert!(feed_symbols(&mut commands, &symbols).is_empty());
        assert!(
            commands
                .feed(DigitEvent::Digit {
                    digit: '\0',
                    now_ms: 9000
                })
                .is_none()
        );
        let Some(DigitOutcome::PadTest(result)) =
            commands.feed(DigitEvent::Unkey { now_ms: 10000 })
        else {
            panic!("missing pad report")
        };
        assert_eq!(result.digits, &symbols[..symbols.len().min(127)]);
        assert_eq!(result.overflowed, symbols.len() > 127);
    }
    let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
    feed_symbols(&mut commands, "*82ABC");
    commands.feed(DigitEvent::Lost);
    assert!(commands.feed(DigitEvent::Unkey { now_ms: 1 }).is_none());
    let mut peer = commands.for_source();
    assert!(feed_symbols(&mut peer, "*82").is_empty());
    assert!(peer.feed(DigitEvent::Unkey { now_ms: 1 }).is_none());
    assert_eq!(
        feed_symbols(&mut peer, "5"),
        [DigitOutcome::Regenerate('5')]
    );
    assert!(feed_symbols(&mut commands, "5").is_empty());
    assert!(feed_symbols(&mut peer, "*35").is_empty());
}

#[test]
fn remote_selection_survives_unkey_not_hash() {
    let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
    commands.select_remote("2000");
    assert!(commands.feed(DigitEvent::Unkey { now_ms: 1 }).is_none());
    assert!(
        commands
            .feed(DigitEvent::Digit {
                digit: '\0',
                now_ms: 5000
            })
            .is_none()
    );
    let outcomes = feed_symbols(&mut commands, "5");
    let [DigitOutcome::Operation(operation)] = outcomes.as_slice() else {
        panic!("not forwarding")
    };
    assert_eq!(operation.digit, Some('5'));
    assert_eq!(operation.command.node, "2000");
    assert!(feed_symbols(&mut commands, "#5").is_empty());
}

#[test]
fn destination_limit_is_stricter_than_the_collector_and_zero_requires_a_previous_node() {
    for length in [63, 64, 127] {
        let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
        let mut result = None;
        for digit in format!("*3{}#", "1".repeat(length)).chars() {
            result = commands
                .feed(DigitEvent::Digit { digit, now_ms: 1 })
                .or(result);
        }
        assert_eq!(result.is_some(), length == 63);
        if let Some(DigitOutcome::Operation(result)) = result {
            assert_eq!(result.command.node.len(), 63);
        }
    }
    let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
    for digit in "*30#".chars() {
        assert!(
            commands
                .feed(DigitEvent::Digit { digit, now_ms: 1 })
                .is_none()
        );
    }
}

#[test]
fn real_native_audio_publishes_every_keypad_symbol_in_order() {
    let (mut worker, mut dispatcher) = DtmfWorker::new(7, false).unwrap();
    let expected = [
        '1', '2', '3', 'A', '4', '5', '6', 'B', '7', '8', '9', 'C', '*', '0', '#', 'D',
    ];
    let mut found = Vec::new();
    for row in [697.0, 770.0, 852.0, 941.0] {
        for column in [1209.0, 1336.0, 1477.0, 1633.0] {
            for block in 0..5 {
                let mut pcm = (0..612)
                    .map(|offset| {
                        if block >= 2 {
                            return 0.0;
                        }
                        let time = (block * 612 + offset) as f32 / 48000.0;
                        0.031
                            * ((std::f32::consts::TAU * row * time).sin()
                                + (std::f32::consts::TAU * column * time).sin())
                    })
                    .collect::<Vec<_>>();
                worker.process(true, &mut pcm, 10);
            }
            assert_eq!(dispatcher.queued(), 1);
            let Some(DigitEvent::Digit { digit, now_ms: 10 }) = dispatcher.next(7) else {
                panic!("completed digit")
            };
            found.push(digit);
        }
    }
    assert_eq!(found, expected);
    assert_eq!(dispatcher.next(7), None);
}

#[test]
fn final_digit_precedes_unkey_terminator_with_twenty_millisecond_callbacks() {
    let (mut worker, mut dispatcher) = DtmfWorker::new(7, true).unwrap();
    let mut elapsed_samples = 0;
    for (receiving, frequencies) in [
        (true, Some((941.0_f32, 1209.0_f32))), // '*'
        (true, None),
        (true, Some((697.0, 1336.0))), // '2', held until unkey
        (false, None),
    ] {
        for _ in 0..2 {
            let mut audio = std::array::from_fn::<_, 960, _>(|offset| {
                frequencies.map_or(0.0, |(row, column)| {
                    let phase =
                        std::f32::consts::TAU * (elapsed_samples + offset) as f32 / 48_000.0;
                    0.031 * ((phase * row).sin() + (phase * column).sin())
                })
            });
            elapsed_samples += audio.len();
            worker.process(receiving, &mut audio, elapsed_samples as u64 / 48);
        }
    }
    let mut digits = Vec::new();
    while let Some(event) = dispatcher.next(7) {
        digits.push(event);
    }
    assert!(matches!(
        digits.as_slice(),
        [
            DigitEvent::Digit { digit: '*', .. },
            DigitEvent::Digit { digit: '2', .. },
            DigitEvent::Unkey { .. }
        ]
    ));
}

#[test]
fn worker_reports_qualified_dtmf_muting_until_the_tone_leaves() {
    let (mut worker, _) = DtmfWorker::new(7, true).unwrap();
    assert!(!worker.suppressing());
    let mut tone = (0..1224)
        .map(|offset| {
            let time = offset as f32 / 48_000.0;
            0.031
                * ((std::f32::consts::TAU * 697.0 * time).sin()
                    + (std::f32::consts::TAU * 1209.0 * time).sin())
        })
        .collect::<Vec<_>>();
    worker.process(true, &mut tone, 1);
    assert!(worker.suppressing());
    let mut silence = vec![0.0; 612];
    worker.process(true, &mut silence, 2);
    assert!(!worker.suppressing());
}

#[test]
fn suppressed_control_discards_queued_prefix_but_accepts_later_digits() {
    let (mut worker, mut dispatcher) = DtmfPublisher::new(7);
    worker.decoded('*', 1);
    worker.decoded('3', 1);
    dispatcher.discard();
    assert_eq!(dispatcher.next(7), None);
    worker.decoded('6', 2);
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '6',
            now_ms: 2
        })
    );
}
#[test]
fn old_generation_overflow_cannot_invalidate_a_new_command() {
    let (mut worker, mut dispatcher) = DtmfPublisher::new(7);
    for _ in 0..257 {
        worker.decoded('*', 1);
    }
    assert_eq!(dispatcher.next(8), None);
    assert_eq!(dispatcher.queued(), 0);
    assert_eq!(dispatcher.dropped(), 1);
}
#[test]
fn timeout_unkey_and_overflow_match_worker_contract() {
    let (mut worker, mut dispatcher) = DtmfPublisher::new(7);
    worker.decoded('5', 5000);
    worker.finish_frame(true, 5000, true);
    worker.finish_frame(true, 5100, false);
    worker.finish_frame(true, 8000, false);
    worker.finish_frame(true, 8000, false);
    worker.finish_frame(false, 8001, false);
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '5',
            now_ms: 5000
        })
    );
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '\0',
            now_ms: 8000
        })
    );
    assert_eq!(dispatcher.next(7), Some(DigitEvent::Unkey { now_ms: 8001 }));
    assert_eq!(dispatcher.next(7), None);
    worker.decoded('5', 8100);
    worker.finish_frame(true, 8100, true);
    worker.finish_frame(false, 8200, false);
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '5',
            now_ms: 8100
        })
    );
    assert_eq!(dispatcher.next(7), Some(DigitEvent::Unkey { now_ms: 8200 }));
    for _ in 0..258 {
        worker.decoded('5', 9000);
    }
    assert_eq!(dispatcher.next(7), Some(DigitEvent::Lost));
    assert_eq!(dispatcher.next(7), None);
    worker.decoded('6', 10000);
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '6',
            now_ms: 10000
        })
    );
    worker.decoded('*', 11000);
    assert_eq!(dispatcher.next(8), None);
    assert_eq!(dispatcher.dropped(), 2);
}

#[test]
fn lost_work_invalidates_prefix_and_remote_hash_ends_forwarding() {
    let mut commands = DtmfCommands::new(crate::command::DtmfCommandMap::standard());
    for digit in "*3524".chars() {
        assert!(
            commands
                .feed(DigitEvent::Digit { digit, now_ms: 1 })
                .is_none()
        );
    }
    commands.feed(DigitEvent::Lost);
    for digit in "950#".chars() {
        assert!(
            commands
                .feed(DigitEvent::Digit { digit, now_ms: 2 })
                .is_none()
        );
    }
    let mut result = None;
    for digit in "*3524950#".chars() {
        result = commands
            .feed(DigitEvent::Digit { digit, now_ms: 3 })
            .or(result);
    }
    let Some(DigitOutcome::Operation(operation)) = result.take() else {
        panic!("operation")
    };
    assert_eq!(operation.command.node, "524950");
    commands.select_remote("2000");
    let DigitOutcome::Operation(forwarded) = commands
        .feed(DigitEvent::Digit {
            digit: '*',
            now_ms: 4,
        })
        .unwrap()
    else {
        panic!("forwarded operation")
    };
    assert_eq!(
        (forwarded.command.node.as_str(), forwarded.digit),
        ("2000", Some('*'))
    );
    assert!(
        commands
            .feed(DigitEvent::Digit {
                digit: '#',
                now_ms: 5
            })
            .is_none()
    );
    assert!(
        commands
            .feed(DigitEvent::Digit {
                digit: '1',
                now_ms: 6
            })
            .is_none()
    );
    for digit in "*30#".chars() {
        result = commands.feed(DigitEvent::Digit { digit, now_ms: 7 });
    }
    let Some(DigitOutcome::Operation(operation)) = result else {
        panic!("operation")
    };
    assert_eq!(operation.command.node, "524950");
}

#[test]
fn parrot_dtmf_requires_both_argon2id_codes_and_expires_after_idle_timeout() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let hash = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let unlock_hash = hash(b"123456", "c29tZXNhbHQ");
    let lock_hash = hash(b"654321", "bG9ja3NhbHQ");
    let mut commands = DtmfCommands::new(DtmfCommandMap::standard());
    commands.configure_admin(&unlock_hash, "", 1000);
    assert!(enter(&mut commands, "*800123456#", 1).is_none());
    assert!(enter(&mut commands, "*804", 2).is_none());

    commands.configure_admin(&unlock_hash, &lock_hash, 1000);
    assert!(enter(&mut commands, "*800000000#", 3).is_none());
    assert!(enter(&mut commands, "*804", 4).is_none());
    assert!(enter(&mut commands, "*800123456#", 5).is_none());
    let enabled = enter(&mut commands, "*804", 6).unwrap();
    assert_eq!(enabled.command.action, LinkAction::ParrotEnable);
    assert!(commands.consume_parrot_authorization(enabled.admin_authorized, 6));
    assert!(!commands.consume_parrot_authorization(false, 6));
    assert!(!commands.consume_parrot_authorization(false, 2006));
    assert!(enter(&mut commands, "*805", 1005).is_none());
    assert!(enter(&mut commands, "*800123456#", 1006).is_none());
    assert!(enter(&mut commands, "*801000000#", 1007).is_none());
    assert!(enter(&mut commands, "*805", 1008).is_some());
    assert!(enter(&mut commands, "*801654321#", 1009).is_none());
    assert!(enter(&mut commands, "*804", 1010).is_none());
}

#[test]
fn parrot_authorization_handles_disabled_timeout_and_deadline_boundaries() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let unlock_hash = digest(b"1234", "c29tZXNhbHQ");
    let lock_hash = digest(b"5678", "bG9ja3NhbHQ");
    let mut commands = DtmfCommands::new(DtmfCommandMap::standard());

    commands.configure_admin(&unlock_hash, &lock_hash, 0);
    assert!(enter(&mut commands, "*8001234#", 10).is_none());
    assert!(enter(&mut commands, "*804", 11).is_none());
    assert!(!commands.consume_parrot_authorization(true, 11));
    commands.confirm_parrot_action(11);
    assert!(enter(&mut commands, "*8015678#", 12).is_none());

    commands.configure_admin(&unlock_hash, &lock_hash, 100);
    assert!(enter(&mut commands, "*8001234#", 20).is_none());
    assert!(commands.consume_parrot_authorization(true, 119));
    assert!(!commands.consume_parrot_authorization(true, 120));
    assert_eq!(commands.admin_until_ms, 0);

    commands.configure_admin(&unlock_hash, &lock_hash, 100);
    assert!(enter(&mut commands, "*8001234#", 30).is_none());
    commands.confirm_parrot_action(40);
    assert_eq!(commands.admin_until_ms, 140);
    commands.confirm_parrot_action(140);
    assert_eq!(commands.admin_until_ms, 140);
}

fn enter(commands: &mut DtmfCommands, input: &str, now_ms: u64) -> Option<DigitOperation> {
    let mut result = None;
    for digit in input.chars() {
        result = commands
            .feed(DigitEvent::Digit { digit, now_ms })
            .or(result);
    }
    result.map(|outcome| match outcome {
        DigitOutcome::Operation(operation) => operation,
        _ => panic!("operation"),
    })
}
