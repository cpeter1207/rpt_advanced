use super::*;

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
        if let Some(result) = result {
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
    let (mut worker, mut dispatcher) = DtmfWorker::new(7, false);
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
    assert_eq!(
        dispatcher.next(7),
        Some(DigitEvent::Digit {
            digit: '#',
            now_ms: 8200
        })
    );
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
    assert_eq!(result.take().unwrap().command.node, "524950");
    commands.select_remote("2000");
    let forwarded = commands
        .feed(DigitEvent::Digit {
            digit: '*',
            now_ms: 4,
        })
        .unwrap();
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
    assert_eq!(result.unwrap().command.node, "524950");
}
