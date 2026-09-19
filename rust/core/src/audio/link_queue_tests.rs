use super::LinkAudioQueue;

#[test]
fn queue_keeps_oldest_samples_and_silences_shortfall() {
    assert!(LinkAudioQueue::new(0).is_err());
    let (mut producer, mut consumer) = LinkAudioQueue::new(3).unwrap().into_endpoints();
    assert_eq!(producer.write(&[10.0, 20.0]), 0);
    let mut first = [99.0];
    assert_eq!(consumer.read(&mut first), 0);
    assert_eq!(first, [10.0]);
    assert_eq!(producer.write(&[30.0, 40.0, 50.0]), 1);
    let mut output = [99.0; 5];
    assert_eq!(consumer.read(&mut output), 2);
    assert_eq!(output, [20.0, 30.0, 40.0, 0.0, 0.0]);
    assert_eq!(consumer.shortfall_samples(), 2);
    assert_eq!(producer.dropped_samples(), 1);
}

#[test]
fn queue_endpoints_transfer_samples_between_threads() {
    let (producer, consumer) = LinkAudioQueue::new(16).unwrap().into_endpoints();
    let writer = std::thread::spawn(move || {
        let mut producer = producer;
        assert_eq!(producer.write(&[1.0, 2.0, 3.0, 4.0]), 0);
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
