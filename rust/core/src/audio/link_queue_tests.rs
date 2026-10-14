use super::LinkAudioQueue;

#[test]
fn egress_span_partition_and_overflow() {
    use super::AudioSpan;
    for count in [1, 240, 480, 960, 4096] {
        let (mut producer, mut consumer) =
            LinkAudioQueue::new(count + 1, 2).unwrap().into_endpoints();
        let first = AudioSpan {
            generation: 7,
            burst: 3,
            source_start: 100,
            ..AudioSpan::default()
        };
        let second = AudioSpan {
            source_start: 100 + count as u64,
            ..first
        };
        assert_eq!(producer.write(first, &vec![0.25; count]), 0);
        assert_eq!(producer.write(second, &[0.5, 0.6]), 1);
        let mut output = vec![9.0; count.max(2)];
        let mut position = 100;
        let mut remaining = count;
        while remaining != 0 {
            let wanted = remaining.min(37);
            let read = consumer.read_span(&mut output[..wanted]).unwrap();
            assert_eq!(
                read.span,
                AudioSpan {
                    source_start: position,
                    ..first
                }
            );
            assert_eq!(read.frames, wanted);
            assert_eq!(&output[..wanted], vec![0.25; wanted]);
            remaining -= wanted;
            position += wanted as u64;
        }
        let read = consumer.read_span(&mut output).unwrap();
        assert_eq!((read.span, read.frames, output[0]), (second, 1, 0.5));
        assert!(consumer.read_span(&mut output).is_none());
        // Rejected samples leave a source-time gap; later samples never slide backward.
        let third = AudioSpan {
            generation: 8,
            burst: 4,
            source_start: second.source_start + 2,
            ..AudioSpan::default()
        };
        assert_eq!(producer.write(third, &[0.7]), 0);
        assert_eq!(consumer.read_span(&mut output).unwrap().span, third);
        assert_eq!(producer.dropped_samples(), 1);
    }
    let (mut producer, mut consumer) = LinkAudioQueue::new(8, 1).unwrap().into_endpoints();
    let span = AudioSpan {
        generation: 1,
        burst: 1,
        source_start: 0,
        ..AudioSpan::default()
    };
    assert_eq!(producer.write(span, &[1.0]), 0);
    assert_eq!(
        producer.write(span, &[2.0, 3.0]),
        2,
        "descriptor exhaustion rejects whole newest span"
    );
    assert!(consumer.read_span(&mut []).is_none());
    assert_eq!(consumer.read_span(&mut [0.0; 8]).unwrap().frames, 1);
    assert!(consumer.read_span(&mut [0.0]).is_none());
    assert_eq!(producer.dropped_samples(), 2);
}

#[test]
fn queue_keeps_oldest_samples_and_silences_shortfall() {
    assert!(LinkAudioQueue::new(0, 64).is_err());
    let (mut producer, mut consumer) = LinkAudioQueue::new(3, 64).unwrap().into_endpoints();
    assert_eq!(
        producer.write(super::AudioSpan::default(), &[10.0, 20.0]),
        0
    );
    let mut first = [99.0];
    assert_eq!(consumer.read(&mut first), 0);
    assert_eq!(first, [10.0]);
    assert_eq!(
        producer.write(super::AudioSpan::default(), &[30.0, 40.0, 50.0]),
        1
    );
    let mut output = [99.0; 5];
    assert_eq!(consumer.read(&mut output), 2);
    assert_eq!(output, [20.0, 30.0, 40.0, 0.0, 0.0]);
    assert_eq!(consumer.shortfall_samples(), 2);
    assert_eq!(producer.dropped_samples(), 1);
}

#[test]
fn egress_span_peek_and_fractional_birth_survive_partitions() {
    use super::AudioSpan;
    let (mut producer, mut consumer) = LinkAudioQueue::new(1000, 2).unwrap().into_endpoints();
    let span = AudioSpan {
        source_start: 17,
        born_ns: Some(1_000_000_000),
        ..AudioSpan::default()
    };
    producer.write(span, &[0.5; 997]);
    let mut total = 0;
    while let Some(next) = consumer.peek_span() {
        assert_eq!(consumer.peek_span(), Some(next));
        let read = consumer.read_span(&mut [0.0; 37]).unwrap();
        assert_eq!(read.span.source_start, 17 + total);
        assert_eq!(
            read.span.born_ns,
            Some(
                1_000_000_000 + ((17 + total) * 1_000_000_000 / 48_000)
                    - 17 * 1_000_000_000 / 48_000
            )
        );
        total += read.frames as u64;
    }
    assert_eq!(total, 997);
}

#[test]
fn queue_endpoints_transfer_samples_between_threads() {
    let (producer, consumer) = LinkAudioQueue::new(16, 64).unwrap().into_endpoints();
    let writer = std::thread::spawn(move || {
        let mut producer = producer;
        assert_eq!(
            producer.write(super::AudioSpan::default(), &[1.0, 2.0, 3.0, 4.0]),
            0
        );
    });
    let reader = std::thread::spawn(move || {
        let mut consumer = consumer;
        let mut output = [0.0; 4];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut received = 0;
        // Per-sample publication permits valid partial reads; retain each prefix.
        while received < output.len() {
            assert!(std::time::Instant::now() < deadline, "PCM handoff stalled");
            let remaining = &mut output[received..];
            received += remaining.len() - consumer.read(remaining);
            std::thread::yield_now();
        }
        output
    });
    writer.join().unwrap();
    assert_eq!(reader.join().unwrap(), [1.0, 2.0, 3.0, 4.0]);
}
