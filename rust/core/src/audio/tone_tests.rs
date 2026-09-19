use super::ToneSequence;

fn render_all(sequence: &mut ToneSequence, block: usize) -> Vec<f32> {
    let mut rendered = Vec::new();
    loop {
        let mut output = vec![99.0; block];
        let count = sequence.render(&mut output);
        rendered.extend_from_slice(&output[..count]);
        if count == 0 {
            return rendered;
        }
    }
}

#[test]
fn tone_parses_readable_compact_and_silence_at_48khz() {
    let mut sequence = ToneSequence::new(
        "1000 Hz + 1100.5hZ / 2 ms / -6 dBFS, silence / 1ms, 1100.5@-12/2",
        -20,
    )
    .unwrap();
    let output = render_all(&mut sequence, 4096);
    assert_eq!(output.len(), 240);
    assert!(output[96..144].iter().all(|sample| *sample == 0.0));
    assert!(
        ToneSequence::new("1000+1000@0/1", -20)
            .unwrap()
            .rendered_samples()
            == 48
    );
}

#[test]
fn tone_rejects_unsafe_or_malformed_values() {
    for text in [
        "",
        "1000",
        "1000+/1",
        "silence+1000/1",
        "1000@-6/1/-5",
        "1000/0",
        "1000/60001",
        "24000/1",
        "1000/1,",
        "1000e3/1",
        "1000@-6@-7/1",
        "1000+1100+1200/1",
        "1000+0/1",
        "1000+24000/1",
        "1000/1/?",
        "1000/-1",
        "1000/ms",
        "1000/99999999999999999999999999999999999999",
        "1000//-6",
        "./1",
        "1.2.3/1",
        "9999999999999999999999999999999999999999/1",
    ] {
        assert!(ToneSequence::new(text, -6).is_err(), "{text}");
    }
    assert!(ToneSequence::new(&format!("{}/1", "1".repeat(65)), -6).is_err());
}

#[test]
fn trait_rendering_obeys_exact_tone_duration() {
    let mut sequence = ToneSequence::new("1000/1", -6).unwrap();
    let mut samples = [0.0; 49];
    assert_eq!(
        crate::audio::AudioSource::render(&mut sequence, &mut samples),
        48
    );
    assert_eq!(
        crate::audio::AudioSource::render(&mut sequence, &mut samples),
        0
    );
}

#[test]
fn tone_is_partition_invariant_and_preserves_phase_across_segments() {
    let mut whole = ToneSequence::new("1100/1,1100/1", -20).unwrap();
    let expected = render_all(&mut whole, 4096);
    let mut blocks = ToneSequence::new("1100/1,1100/1", -20).unwrap();
    assert_eq!(render_all(&mut blocks, 7), expected);
    assert_ne!(expected[48], 0.0);
}

#[test]
fn tone_uses_exact_native_durations_and_splits_dual_tone_level() {
    let mut silence = ToneSequence::new("0/1,silence/2", -6).unwrap();
    assert_eq!(silence.rendered_samples(), 144);
    assert!(
        render_all(&mut silence, 37)
            .iter()
            .all(|sample| *sample == 0.0)
    );

    let mut single = ToneSequence::new("1000@-6/1", -20).unwrap();
    let mut dual = ToneSequence::new("1000+1000@-6/1", -20).unwrap();
    let single = render_all(&mut single, 48);
    let dual = render_all(&mut dual, 48);
    assert!((single[12] - dual[12]).abs() < 0.000_01);
}

#[test]
fn tone_rejects_bad_default_and_total_or_segment_limits() {
    assert!(ToneSequence::new("1000/1", -61).is_err());
    assert!(ToneSequence::new("1000/60001", -6).is_err());
    let segments = std::iter::repeat_n("0/1", 257)
        .collect::<Vec<_>>()
        .join(",");
    assert!(ToneSequence::new(&segments, -6).is_err());
    assert!(ToneSequence::new("0/60000,0/60000,0/1", -6).is_err());
}
