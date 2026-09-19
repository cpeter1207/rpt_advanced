use super::MorseRenderer;

fn render_all(renderer: &mut MorseRenderer, block: usize) -> Vec<f32> {
    let mut rendered = Vec::new();
    loop {
        let mut output = vec![99.0; block];
        let count = renderer.render(&mut output);
        rendered.extend_from_slice(&output[..count]);
        if count == 0 {
            return rendered;
        }
    }
}

#[test]
fn morse_preserves_literal_48khz_timing_and_validation() {
    for (text, speed, samples) in [
        ("", 20, 0),
        ("E", 20, 2_880),
        ("T", 20, 8_640),
        ("ET", 20, 20_160),
        ("E T", 20, 31_680),
        ("I", 20, 8_640),
        ("EE", 17, 16_941),
    ] {
        let mut renderer = MorseRenderer::new(text, speed, 1_000.0, -6).unwrap();
        assert_eq!(render_all(&mut renderer, 4_096).len(), samples);
    }
    for (text, speed, frequency, level) in [
        ("E*", 20, 1_000.0, -6),
        ("E", 0, 1_000.0, -6),
        ("E", 101, 1_000.0, -6),
        ("E", 20, 0.0, -6),
        ("E", 20, 24_000.0, -6),
        ("E", 20, f32::NAN, -6),
        ("E", 20, f32::INFINITY, -6),
        ("E", 20, 1_000.0, -61),
    ] {
        assert!(
            MorseRenderer::new(text, speed, frequency, level).is_err(),
            "{text} {speed} {frequency} {level}"
        );
    }
}

#[test]
fn morse_is_partition_invariant_and_resets_keyed_phase() {
    let mut whole = MorseRenderer::new("E T I", 20, 1_000.0, -6).unwrap();
    let expected = render_all(&mut whole, 65_536);
    let mut blocks = MorseRenderer::new("E T I", 20, 1_000.0, -6).unwrap();
    assert_eq!(blocks.render(&mut []), 0);
    assert_eq!(render_all(&mut blocks, 37), expected);
    assert!(expected[12] > 0.5);
    assert!(expected[36] < -0.5);
    assert!(expected[2_880..23_040].iter().all(|sample| *sample == 0.0));
}

#[test]
fn morse_accepts_the_complete_supported_alphabet_and_has_no_trailing_gap() {
    let mut renderer = MorseRenderer::new(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,?-=+@()'!\":;_$&",
        100,
        1_000.0,
        -6,
    )
    .unwrap();
    assert!(!render_all(&mut renderer, 257).is_empty());
    let mut e = MorseRenderer::new("E", 20, 1_000.0, -6).unwrap();
    let samples = render_all(&mut e, 4_096);
    assert_eq!(samples.len(), 2_880);
    assert_ne!(samples[samples.len() - 1], 0.0);
}

#[test]
fn trait_rendering_preserves_the_exact_dot_duration() {
    let mut renderer = MorseRenderer::new("E", 20, 1000.0, -6).unwrap();
    assert_eq!(
        crate::audio::AudioSource::render(&mut renderer, &mut [0.0; 2881]),
        2880
    );
}
