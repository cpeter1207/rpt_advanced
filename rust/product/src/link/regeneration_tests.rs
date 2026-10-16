use super::*;
use crate::link::ring::{InboundPolicy, InboundRing};
use rpt_advanced_core::link::PeerInput;

fn fixture(
    maximum: usize,
) -> (
    rtrb::Producer<char>,
    RegenerationProducer,
    super::super::ring::InboundConsumer,
) {
    let (digits, incoming) = rtrb::RingBuffer::new(127);
    let (writer, reader) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
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
    digits.push('5').unwrap();
    digits.push('5').unwrap();
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
    digits.push('6').unwrap();
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
    digits.push('1').unwrap();
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
        digits.push('D').unwrap();
        let now = Instant::now();
        producer.pump(now).unwrap();
        producer.pump(now + Duration::from_millis(500)).unwrap();
        assert!(reader.render_sample().unwrap().is_some());
    }
    let (mut digits, mut producer, _) = fixture(960);
    digits.push('x').unwrap();
    let now = Instant::now();
    producer.pump(now).unwrap();
    assert!(producer.pump(now + Duration::from_millis(500)).is_err());
    let (_, incoming) = rtrb::RingBuffer::new(127);
    let (writer, _) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
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
