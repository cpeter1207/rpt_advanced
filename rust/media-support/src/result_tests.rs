use super::*;

#[test]
fn rejects_each_invalid_audio_property() {
    for (rate, samples) in [
        (0, vec![0.0]),
        (48_000, vec![]),
        (48_000, vec![f32::NAN]),
        (48_000, vec![f32::INFINITY]),
    ] {
        assert_eq!(
            PreparedAudio::new(rate, samples),
            Err(MediaError::InvalidOutput)
        );
    }
    assert!(PreparedAudio::new(48_000, vec![0.0]).is_ok());
}
