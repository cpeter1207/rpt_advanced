use super::*;

fn tone() -> [f32; BLOCK] {
    std::array::from_fn(|i| 0.1 * (std::f32::consts::TAU * (i % 48) as f32 / 48.0).sin())
}

fn feed(
    tap: &mut Tap,
    reader: &mut Reader,
    samples: &[f32],
    position: &mut u64,
    generation: u64,
    source: u64,
) -> Measurement {
    let start = *position;
    *position += samples.len() as u64;
    tap.observe(
        Span {
            graph_generation: generation,
            source_generation: source,
            first_sample: start,
            end_ms: *position / 48,
        },
        samples,
    );
    reader.latest().unwrap()
}

#[test]
fn mono_windows_have_progressive_warmup_and_recognize_ebu_reference_tone() {
    let (mut tap, mut reader) = Tap::new();
    let mut position = 0;
    let tone = tone();
    let first = feed(&mut tap, &mut reader, &tone, &mut position, 9, 14);
    assert_eq!(first.quality, Quality::Warming);
    assert_eq!(first.valid_samples, 2400);
    assert!(first.peak_dbfs.is_none());
    assert!(first.lufs.is_none());

    let second = feed(&mut tap, &mut reader, &tone, &mut position, 9, 14);
    assert_eq!(second.quality, Quality::Warming);
    assert_eq!(second.monotonic_ms, 100);
    assert!((second.peak_dbfs.unwrap() - -20.0).abs() < 0.01);
    assert!((second.rms_dbfs.unwrap() - -23.0103).abs() < 0.01);
    assert!(second.lufs.is_none());

    for _ in 0..5 {
        let next = feed(&mut tap, &mut reader, &tone, &mut position, 9, 14);
        assert_eq!(next.quality, Quality::Warming);
    }
    let valid = feed(&mut tap, &mut reader, &tone, &mut position, 9, 14);
    assert_eq!(valid.monotonic_ms, 400);
    assert_eq!(valid.source_generation, 14);
    assert_eq!(valid.end_sample, 19_200);
    assert_eq!(valid.quality, Quality::Valid);
    assert_eq!(valid.signal_state, SignalState::Present);
    assert_eq!(valid.valid_samples, 19_200);
    // TECH 3341 signal #1 at -23 dBFS in each stereo channel. A single
    // channel at -20 dBFS has the same energy within the reference tolerance.
    assert!((valid.lufs.unwrap() - -23.0).abs() < 0.1, "{valid:?}");
}

#[test]
fn silence_clipping_peak_hold_and_source_generations_are_independent() {
    let (mut tap, mut reader) = Tap::new();
    let mut position = 0;
    let mut clip = [0.0; BLOCK];
    clip[BLOCK - 1] = 1.25;
    let first = feed(&mut tap, &mut reader, &clip, &mut position, 9, 14);
    assert_eq!(first.clipped_samples, 1);
    assert!(first.peak_hold_dbfs.unwrap() > 0.0);

    let silence = [0.0; BLOCK];
    for _ in 0..39 {
        let last = feed(&mut tap, &mut reader, &silence, &mut position, 9, 14);
        assert_eq!(last.peak_hold_dbfs, first.peak_hold_dbfs);
    }
    let held_peak_expired = feed(&mut tap, &mut reader, &silence, &mut position, 9, 14);
    assert_eq!(held_peak_expired.monotonic_ms, 2050);
    assert_eq!(held_peak_expired.signal_state, SignalState::Silence);
    assert_eq!(held_peak_expired.quality, Quality::Valid);
    assert!(held_peak_expired.peak_dbfs.is_none());
    assert!(held_peak_expired.peak_hold_dbfs.is_none());
    assert!(held_peak_expired.lufs.is_none());

    let new = feed(&mut tap, &mut reader, &silence, &mut position, 10, 14);
    assert_eq!(new.graph_generation, 10);
    assert!(new.measurement_epoch > held_peak_expired.measurement_epoch);
    assert_eq!(new.clipped_samples, 0);
    assert_eq!(new.quality, Quality::Warming);

    position += 48;
    let gap = feed(&mut tap, &mut reader, &silence, &mut position, 10, 14);
    assert!(gap.measurement_epoch > new.measurement_epoch);
    let replaced = feed(&mut tap, &mut reader, &silence, &mut position, 10, 15);
    assert!(replaced.measurement_epoch > gap.measurement_epoch);

    tap.invalidate(Quality::Fault);
    assert_eq!(reader.latest().unwrap().quality, Quality::Fault);
    let reopened = feed(&mut tap, &mut reader, &silence, &mut position, 10, 15);
    assert_eq!(reopened.quality, Quality::Warming);
    assert!(reopened.measurement_epoch > replaced.measurement_epoch);
}

#[test]
fn a_partial_callback_retires_the_old_epoch_before_the_next_block() {
    let (mut tap, mut reader) = Tap::new();
    let mut position = 0;
    let filled = feed(&mut tap, &mut reader, &tone(), &mut position, 1, 1);
    assert_eq!(filled.valid_samples, BLOCK as u64);
    tap.observe(
        Span {
            graph_generation: 2,
            source_generation: 1,
            first_sample: position,
            end_ms: 55,
        },
        &[0.0; 240],
    );
    assert_eq!(reader.latest(), None);
    let mut position = position + 240;
    let next = feed(
        &mut tap,
        &mut reader,
        &[0.0; BLOCK - 240],
        &mut position,
        2,
        1,
    );
    assert_eq!(next.graph_generation, 2);
    assert!(next.measurement_epoch > filled.measurement_epoch);
    assert!(next.peak_hold_dbfs.is_none());
}

#[test]
fn negotiated_codec_rate_uses_actual_window_lengths_and_weighting() {
    let (mut tap, mut reader) = Tap::at_rate(8000).unwrap();
    let mut block = [0.0; 400];
    for (sample, value) in block.iter_mut().enumerate() {
        *value = 0.1 * (std::f32::consts::TAU * (sample % 8) as f32 / 8.0).sin();
    }
    for block_index in 0..8 {
        tap.observe(
            Span {
                graph_generation: 5,
                source_generation: 6,
                first_sample: block_index * 400,
                end_ms: 50 * (block_index + 1),
            },
            &block,
        );
        let measurement = reader.latest().unwrap();
        assert_eq!(measurement.valid_samples, (block_index + 1) * 400);
        assert_eq!(measurement.end_sample, (block_index + 1) * 400);
        if block_index < 7 {
            assert_eq!(measurement.quality, Quality::Warming);
        } else {
            assert_eq!(measurement.quality, Quality::Valid);
            assert!(
                (measurement.lufs.unwrap() + 23.0).abs() < 0.1,
                "{measurement:?}"
            );
        }
    }
    assert!(Tap::at_rate(0).is_none());
}
