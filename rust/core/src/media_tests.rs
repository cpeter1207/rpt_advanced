use super::*;

#[test]
fn cancellation_is_shared_and_never_resets() {
    let cancellation = Cancellation::default();
    let worker = cancellation.clone();
    assert!(!worker.is_cancelled());
    cancellation.cancel();
    assert!(worker.is_cancelled());
    worker.cancel();
    assert!(cancellation.is_cancelled());
}
#[test]
fn prepared_audio_rejects_unplayable_data_and_retains_source_rate() {
    for (rate, samples) in [
        (0, vec![0.0]),
        (22050, vec![]),
        (22050, vec![f32::NAN]),
        (22050, vec![f32::INFINITY]),
    ] {
        assert_eq!(
            PreparedAudio::new(rate, samples),
            Err(MediaError::InvalidOutput)
        );
    }
    let audio = PreparedAudio::new(22050, vec![-1.0, 32767.0 / 32768.0]).unwrap();
    let retained = audio.clone();
    drop(audio);
    assert_eq!(retained.sample_rate_hz(), 22050);
    assert_eq!(retained.samples(), &[-1.0, 0.9999695]);
}
