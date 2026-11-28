use super::{PcmRead, PcmStreamReader};

struct MinimalReader;

impl PcmStreamReader for MinimalReader {
    fn render(&mut self, _: &mut [f32]) -> PcmRead {
        PcmRead::Pending
    }
}

#[test]
fn optional_stream_controls_default_to_no_fallback_and_nonblocking_noop() {
    let mut reader = MinimalReader;
    reader.start();
    assert_eq!(
        reader.select_morse_fallback(),
        super::MorseSwitch::Unavailable
    );
    reader.cancel();
}

#[test]
fn signed_pcm_rounds_without_wrapping_at_either_extreme() {
    for (sample, expected) in [
        (1.0, i16::MAX),
        (-1.0, i16::MIN),
        (f32::MAX, i16::MAX),
        (f32::MIN, i16::MIN),
        (0.75 / 32768.0, 1),
        (-0.75 / 32768.0, -1),
        (f32::INFINITY, 0),
        (f32::NEG_INFINITY, 0),
        (f32::NAN, 0),
    ] {
        assert_eq!(super::signed_pcm16(sample), expected);
    }
}

#[test]
fn block_gain_preserves_full_scale_and_recovers_at_the_pcm_rate() {
    for rate in [8000, 16000, 48000] {
        let mut guard = super::PeakGuard::new(rate);
        let mut extreme = [f32::MAX, -f32::MAX, 0.5 * f32::MAX, f32::NAN, f32::INFINITY];
        guard.apply(&mut extreme);
        for (sample, expected) in extreme.into_iter().zip([0.9, -0.9, 0.45, 0.0, 0.0]) {
            assert!((sample - expected).abs() < 0.00001);
        }
        guard.reset();
        let mut full = [-1.0, 1.0];
        guard.apply(&mut full);
        assert_eq!(full, [-1.0, 1.0]);
        let mut hot = [1.8, 0.9, -1.2];
        guard.apply(&mut hot);
        assert_eq!(hot, [0.9, 0.45, -0.6]);
        let mut recovery = vec![0.5; (rate / 10) as usize];
        guard.apply(&mut recovery);
        assert_eq!(recovery[0], 0.25);
        assert!(recovery.windows(2).all(|s| s[0] <= s[1]));
        assert_eq!(*recovery.last().unwrap(), 0.5);
    }
}
