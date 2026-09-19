use super::{DtmfDetector, DtmfDigit};

const RATE: f32 = 48_000.0;
const FRAME: usize = 612;

#[test]
fn sustained_digit_restarts_release_qualification_after_one_short_gap() {
    let mut detector = DtmfDetector::new(false);
    let mut digits = Vec::new();
    for block in 0..4 {
        detector.process(true, &mut tone(DtmfDigit::One, block * FRAME), |digit| {
            digits.push(digit)
        });
    }
    detector.process(true, &mut [0.0; FRAME], |digit| digits.push(digit));
    detector.process(true, &mut tone(DtmfDigit::One, 5 * FRAME), |digit| {
        digits.push(digit)
    });
    assert!(digits.is_empty());
    for _ in 0..3 {
        detector.process(true, &mut [0.0; FRAME], |digit| digits.push(digit));
    }
    assert_eq!(digits, [DtmfDigit::One]);
}

#[test]
fn detector_rejects_single_frequencies_excessive_twist_and_competing_rows_or_columns() {
    for components in [
        vec![(697.0, 0.02)],
        vec![(697.0, 0.25)],
        vec![(1209.0, 0.25)],
        vec![(697.0, 0.05), (1209.0, 0.8)],
        vec![(697.0, 0.8), (1209.0, 0.05)],
        vec![(697.0, 0.25), (770.0, 0.25), (1209.0, 0.25)],
        vec![(697.0, 0.25), (1209.0, 0.25), (1336.0, 0.25)],
        vec![(697.0, 0.25), (770.0, 0.11), (1209.0, 0.25)],
        vec![(697.0, 0.25), (1209.0, 0.25), (1336.0, 0.11)],
        vec![(697.0, 0.05), (1209.0, 0.05), (2100.0, 0.8)],
    ] {
        let mut detector = DtmfDetector::new(false);
        let mut digits = Vec::new();
        for block in 0..5 {
            let mut audio = (0..FRAME)
                .map(|offset| {
                    let phase = std::f32::consts::TAU * (block * FRAME + offset) as f32 / RATE;
                    components
                        .iter()
                        .map(|(frequency, amplitude)| amplitude * (phase * frequency).sin())
                        .sum()
                })
                .collect::<Vec<f32>>();
            detector.process(true, &mut audio, |digit| digits.push(digit));
        }
        detector.process(false, &mut [], |digit| digits.push(digit));
        assert!(digits.is_empty(), "{components:?}: {digits:?}");
    }
}

#[test]
fn keypad_frequency_table_and_runtime_muting_switch_match_all_symbols() {
    let digits = [
        DtmfDigit::One,
        DtmfDigit::Two,
        DtmfDigit::Three,
        DtmfDigit::A,
        DtmfDigit::Four,
        DtmfDigit::Five,
        DtmfDigit::Six,
        DtmfDigit::B,
        DtmfDigit::Seven,
        DtmfDigit::Eight,
        DtmfDigit::Nine,
        DtmfDigit::C,
        DtmfDigit::Star,
        DtmfDigit::Zero,
        DtmfDigit::Hash,
        DtmfDigit::D,
    ];
    for (index, digit) in digits.into_iter().enumerate() {
        assert_eq!(
            digit.frequencies(),
            (
                [697.0, 770.0, 852.0, 941.0][index / 4],
                [1209.0, 1336.0, 1477.0, 1633.0][index % 4]
            )
        );
    }
    for muted in [false, true] {
        let mut detector = DtmfDetector::new(!muted);
        detector.set_muting(muted);
        for block in 0..2 {
            detector.process(true, &mut tone(DtmfDigit::One, block * FRAME), |_| {
                panic!("early completion")
            });
        }
        let mut qualified = tone(DtmfDigit::One, 2 * FRAME);
        detector.process(true, &mut qualified, |_| panic!("early completion"));
        assert_eq!(detector.suppressing(), muted);
        assert_eq!(qualified.iter().all(|sample| *sample == 0.0), muted);
        let mut release = voice(3 * FRAME);
        detector.process(true, &mut release, |_| panic!("early completion"));
        assert_eq!(release.iter().all(|sample| *sample == 0.0), muted);
        let mut middle = voice(4 * FRAME);
        detector.process(true, &mut middle, |_| panic!("early completion"));
        assert!(middle.iter().any(|sample| *sample != 0.0));
        let mut output = voice(5 * FRAME);
        let mut result = None;
        detector.process(true, &mut output, |digit| result = Some(digit));
        assert_eq!(result, Some(DtmfDigit::One));
        assert!(output.iter().any(|sample| *sample != 0.0));
    }
}

#[test]
fn dtmf_muting_is_independent_of_audio_partition_size() {
    let mut input = Vec::new();
    for _ in 0..4 {
        input.extend(tone(DtmfDigit::Five, input.len()));
    }
    for _ in 0..3 {
        input.extend(voice(input.len()));
    }
    let render = |partition| {
        let mut detector = DtmfDetector::new(true);
        let mut output = input.clone();
        let mut digits = Vec::new();
        for chunk in output.chunks_mut(partition) {
            detector.process(true, chunk, |digit| digits.push(digit));
        }
        (output, digits)
    };
    let expected = render(1);
    for partition in [37, FRAME, 960, input.len()] {
        assert_eq!(render(partition), expected, "partition {partition}");
    }
}

#[test]
fn every_completed_digit_survives_large_and_small_partitions() {
    let mut input = Vec::new();
    for digit in [DtmfDigit::Five, DtmfDigit::Two] {
        for _ in 0..2 {
            input.extend(tone(digit, input.len()));
        }
    }
    input.resize(input.len() + 3 * FRAME, 0.0);
    for partition in [37, FRAME, input.len()] {
        let mut detector = DtmfDetector::new(false);
        let mut digits = Vec::new();
        for chunk in input.clone().chunks_mut(partition) {
            detector.process(true, chunk, |digit| digits.push(digit));
        }
        assert_eq!(
            digits,
            [DtmfDigit::Five, DtmfDigit::Two],
            "partition {partition}"
        );
    }
}

fn tone(digit: DtmfDigit, first: usize) -> Vec<f32> {
    let (row, column) = digit.frequencies();
    (0..FRAME)
        .map(|offset| {
            let time = (first + offset) as f32 / RATE;
            0.031
                * ((core::f32::consts::TAU * row * time).sin()
                    + (core::f32::consts::TAU * column * time).sin())
        })
        .collect()
}

fn voice(first: usize) -> Vec<f32> {
    (0..FRAME)
        .map(|offset| {
            (core::f32::consts::TAU * 440.0 * (first + offset) as f32 / RATE).sin() * 0.031
        })
        .collect()
}

#[test]
fn dtmf_mutes_a_qualified_tone_without_muting_its_completion_voice() {
    let mut detector = DtmfDetector::new(true);
    let mut first = 0;
    for _ in 0..2 {
        let mut audio = tone(DtmfDigit::Five, first);
        first += FRAME;
        detector.process(true, &mut audio, |_| {
            panic!("digit completed before release")
        });
        assert!(audio.iter().any(|sample| *sample != 0.0));
    }
    let mut audio = tone(DtmfDigit::Five, first);
    first += FRAME;
    detector.process(true, &mut audio, |_| {
        panic!("digit completed before release")
    });
    assert!(audio.iter().all(|sample| *sample == 0.0));

    for interval in 0..3 {
        let mut audio = voice(first);
        first += FRAME;
        let mut digits = Vec::new();
        detector.process(true, &mut audio, |digit| digits.push(digit));
        assert_eq!(
            digits,
            if interval == 2 {
                vec![DtmfDigit::Five]
            } else {
                vec![]
            }
        );
        if interval == 0 {
            assert!(audio.iter().all(|sample| *sample == 0.0));
        } else {
            assert!(audio.iter().any(|sample| *sample != 0.0));
        }
    }
}

#[test]
fn dtmf_partitioning_preserves_detection_and_carrier_loss_finishes_digit() {
    let mut detector = DtmfDetector::new(false);
    let mut first = 0;
    for _ in 0..2 {
        let mut audio = tone(DtmfDigit::D, first);
        first += FRAME;
        detector.process(true, &mut audio, |_| {
            panic!("digit completed before release")
        });
    }
    let mut silence = vec![0.25; 3 * FRAME];
    let mut digits = Vec::new();
    for chunk in silence.chunks_mut(37) {
        detector.process(false, chunk, |digit| digits.push(digit));
    }
    assert_eq!(digits, [DtmfDigit::D]);
}
