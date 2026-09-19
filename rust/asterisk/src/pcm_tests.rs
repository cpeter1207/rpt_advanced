use super::*;

#[test]
fn signed_linear_boundary_preserves_every_word_and_clips_only_at_egress() {
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
        (f32::INFINITY, 32767),
        (f32::NEG_INFINITY, -32768),
    ] {
        assert_eq!(encode(input), output);
    }
}
