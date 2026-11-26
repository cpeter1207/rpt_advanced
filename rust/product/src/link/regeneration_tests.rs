use super::*;
use crate::link::ring::InboundRing;
use rpt_advanced_core::link::PeerInput;

fn fixture(
    maximum: usize,
) -> (
    rtrb::Producer<RegenerationDigit>,
    RegenerationProducer,
    super::super::ring::InboundConsumer,
) {
    let (digits, incoming) = rtrb::RingBuffer::new(127);
    let (writer, reader) = InboundRing::regeneration().unwrap();
    let producer = RegenerationProducer::new(
        ResolvedDtmfRegenerationSettings::default(),
        incoming,
        writer,
        maximum,
    )
    .unwrap();
    (digits, producer, reader)
}

#[test]
fn regeneration_waits_500ms_then_streams_without_unkey() {
    let (mut digits, mut producer, mut reader) = fixture(960);
    let delay = producer.writer.output_delay_samples().unwrap() as usize;
    let now = Instant::now();
    digits.push('5'.into()).unwrap();
    digits.push('5'.into()).unwrap();
    producer.pump(now).unwrap();
    producer.pump(now + Duration::from_millis(499)).unwrap();
    assert_eq!(reader.available(), 0);
    let mut actual = Vec::new();
    for _ in 0..30 {
        producer.pump(now + Duration::from_millis(500)).unwrap();
        while let Some(sample) = reader.render_sample().unwrap() {
            actual.push(sample);
        }
    }
    let mut generator = DtmfGenerator::new('5', 200, 50, -12).unwrap();
    let mut expected = vec![0.0; 12000];
    assert_eq!(generator.render(&mut expected).unwrap(), 12000);
    let expected: Vec<_> = std::iter::repeat_n(0.0, delay)
        .chain(expected.repeat(2))
        .collect();
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual.iter().zip(&expected).all(|(a, b)| a == b),
        "PCM changed beyond the ring's declared delay"
    );
    digits.push('6'.into()).unwrap();
    producer.pump(now + Duration::from_secs(1)).unwrap();
    assert_eq!(reader.available(), 0);
    producer.pump(now + Duration::from_millis(1500)).unwrap();
    assert!(reader.available() > 0);
}

#[test]
fn regeneration_preserves_partial_writes_and_bounded_work() {
    let (mut digits, mut producer, mut reader) = fixture(960);
    let delay = producer.writer.output_delay_samples().unwrap() as usize;
    let now = Instant::now();
    digits.push('1'.into()).unwrap();
    producer.pump(now).unwrap();
    for _ in 0..30 {
        producer.pump(now + Duration::from_millis(500)).unwrap();
    }
    assert_eq!(reader.available(), 8192);
    let mut actual = Vec::new();
    for _ in 0..30 {
        for _ in 0..137 {
            if let Some(sample) = reader.render_sample().unwrap() {
                actual.push(sample);
            }
        }
        producer.pump(now + Duration::from_millis(500)).unwrap();
    }
    while let Some(sample) = reader.render_sample().unwrap() {
        actual.push(sample);
    }
    let mut expected = vec![0.0; 12000];
    DtmfGenerator::new('1', 200, 50, -12)
        .unwrap()
        .render(&mut expected)
        .unwrap();
    let expected: Vec<_> = std::iter::repeat_n(0.0, delay).chain(expected).collect();
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual.iter().zip(&expected).all(|(a, b)| a == b),
        "partial writes changed PCM"
    );
}

#[test]
fn regeneration_validates_bounds_and_handles_bad_queued_digit() {
    for (duration_ms, gap_ms, level_dbfs) in [(1, 1, -60), (1000, 1000, 0)] {
        let (mut digits, mut producer, mut reader) = fixture(960);
        producer.settings = ResolvedDtmfRegenerationSettings {
            enabled: true,
            duration_ms,
            gap_ms,
            level_dbfs,
        };
        digits.push('D'.into()).unwrap();
        let now = Instant::now();
        producer.pump(now).unwrap();
        producer.pump(now + Duration::from_millis(500)).unwrap();
        assert!(reader.render_sample().unwrap().is_some());
    }
    let (mut digits, mut producer, _) = fixture(960);
    digits.push('x'.into()).unwrap();
    let now = Instant::now();
    producer.pump(now).unwrap();
    assert!(producer.pump(now + Duration::from_millis(500)).is_err());
    let (_, incoming) = rtrb::RingBuffer::new(127);
    let (writer, _) = InboundRing::regeneration().unwrap();
    assert!(
        RegenerationProducer::new(
            ResolvedDtmfRegenerationSettings::default(),
            incoming,
            writer,
            0
        )
        .is_err()
    );
}

#[test]
fn queued_received_digit_cannot_rekey_after_its_source_is_muted_then_unmuted() {
    crate::host::tests::with_ingress_mute(|signals, host, port, clock, received| {
        let (mut digits, mut producer, mut reader) = fixture(960);
        let now = Instant::now();
        let [first, second] = received.map(|out| {
            let rpt_advanced_core::runtime::dtmf::DigitOutcome::ReceivedDigit(symbol, origin) = out
            else {
                panic!("eligible received symbol")
            };
            RegenerationDigit {
                symbol,
                origin: Some(origin),
            }
        });
        let original = first.origin.clone().unwrap();
        digits.push(first).unwrap();
        digits.push(second).unwrap();
        producer.pump(now).unwrap();
        let due = now + Duration::from_millis(500);
        for _ in 0..4 {
            producer.pump(due).unwrap();
        }
        assert!(reader.available() > 2000);
        let mute = rpt_advanced_core::command_language::prepare_runtime_action(
            &serde_json::json!({"action":"rx_mute_toggle","target":"peer","parameter":"2000"}),
        )
        .unwrap();
        let mut toggle = || {
            host.runtime
                .management_action(port, "operator", &mute, clock)
                .unwrap();
        };
        toggle();
        assert!(signals.rx_muted());
        toggle();
        assert!(!signals.rx_muted() && !original.current());
        for _ in 0..5000 {
            assert!(
                reader.render_sample().unwrap().is_none(),
                "revoked PCM or key demand"
            );
        }
        // The registered command collector silently discards a noncommand.
        for symbol in ['6', '#'] {
            assert!(
                host.runtime
                    .node(&port.to_string())
                    .unwrap()
                    .links()
                    .peer_digit("2000", symbol, clock.now_ms, signals.reply_target())
                    .is_none()
            );
        }
        // Independent intentional signaling still plays after revoked input drains.
        digits.push('6'.into()).unwrap();
        assert!(
            signals
                .reply_target()
                .regeneration_origin()
                .unwrap()
                .current()
        );
        let mut audible = false;
        for _ in 0..45 {
            producer.pump(due + Duration::from_secs(1)).unwrap();
            for _ in 0..960 {
                audible |= reader.render_sample().unwrap().is_some_and(|s| s != 0.0);
            }
        }
        assert!(audible, "legitimate generated signaling must play");
    });
}
