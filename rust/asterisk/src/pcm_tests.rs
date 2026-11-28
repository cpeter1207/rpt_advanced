use super::*;

#[test]
fn signed_linear_boundary_rounds_and_saturates_without_wrapping() {
    for sample in i16::MIN..=i16::MAX {
        assert_eq!(encode(decode(sample)), sample);
    }
    assert_eq!(decode(i16::MIN), -1.0);
    assert_eq!(decode(16384), 0.5);
    for (input, output) in [
        (-2.0, -32768),
        (2.0, 32767),
        (0.0, 0),
        (0.5 / 32768.0, 1),
        (-0.5 / 32768.0, -1),
        (f32::NAN, 0),
        (f32::INFINITY, 0),
        (f32::NEG_INFINITY, 0),
    ] {
        assert_eq!(encode(input), output);
    }
}

#[test]
fn signed_pcm_conversion_scales_the_whole_peak_and_never_wraps() {
    let signal = [0.5, 1.5, -1.25, -0.25, f32::INFINITY, f32::NAN];
    let gain = peak_gain(&signal);
    let output = signal.map(|sample| encode(sample * gain));
    assert_eq!(output, [9830, 29491, -24576, -4915, 0, 0]);
    let normalized = [-1.0, 0.5, 32767.0 / 32768.0];
    assert_eq!(peak_gain(&normalized), 1.0);
    assert_eq!(normalized.map(encode), [i16::MIN, 16384, i16::MAX]);
}
