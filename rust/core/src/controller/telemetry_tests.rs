use super::*;

#[test]
fn prepared_status_queue_rejects_the_fifth_item() {
    let settings = MorseSettings::default();
    let (_, mut control) = TelemetryPlanner::new(settings, -6);
    for _ in 0..4 {
        assert!(
            control.queue_prepared_status(
                PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap()
            )
        );
    }
    assert!(!control.can_queue_status());
    assert!(
        !control
            .queue_prepared_status(PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap())
    );
}

#[test]
fn prepared_status_queue_rejects_a_full_ring_without_incrementing_outstanding() {
    let settings = MorseSettings::default();
    let (_, mut control) = TelemetryPlanner::new(settings, -6);
    for _ in 0..4 {
        control
            .pending
            .push(PreparedMedia::new(Some(vec![0.25]), "E", settings).unwrap())
            .unwrap();
    }

    assert!(
        !control.queue_prepared_status(PreparedMedia::new(Some(vec![0.5]), "T", settings).unwrap())
    );
    assert_eq!(control.outstanding, 0);
}

#[test]
fn test_pcm_stream_reports_partial_and_finished_reads_and_restarts_fallback() {
    let mut stream = TestPcmStream {
        primary: vec![0.25, 0.5],
        fallback: vec![0.75],
        offset: 0,
        use_fallback: false,
    };
    let mut output = [0.0; 1];
    assert_eq!(stream.render(&mut output), PcmRead::Samples(1));
    assert_eq!(output[0], 0.25);
    assert!(stream.select_morse_fallback());
    assert_eq!(stream.render(&mut output), PcmRead::FinalSamples(1));
    assert_eq!(output[0], 0.75);
    assert_eq!(stream.render(&mut output), PcmRead::Finished);
    stream.start();
    assert_eq!(stream.render(&mut []), PcmRead::Finished);
}
