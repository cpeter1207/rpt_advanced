//! Prove the scalar pending-tone inference against the released detector itself.
use rpt_advanced_core::audio::DtmfDetector;

fn verify_pending_transitions(rate: u32) {
    let part = rate as usize / 125;
    let tone = |milliseconds: usize, column: f32| -> Vec<f32> {
        (0..rate as usize * milliseconds / 1000)
            .map(|n| {
                let phase = n as f32 * std::f32::consts::TAU / rate as f32;
                0.25 * ((phase * 770.0).sin() + (phase * column).sin())
            })
            .collect()
    };
    let quiet = || vec![0.0; rate as usize * 80 / 1000];
    let scenarios = [
        [tone(80, 1336.0), quiet()].concat(),
        [tone(80, 1336.0), tone(80, 1477.0), quiet()].concat(),
        [tone(8, 1336.0), quiet()].concat(),
        [
            quiet(),
            tone(80, 1336.0),
            quiet(),
            tone(80, 1336.0),
            quiet(),
        ]
        .concat(),
    ];
    let mut saw_pending_quiet_tail = false;
    let mut saw_replacement = false;
    for input in scenarios {
        let mut detector = DtmfDetector::at_rate(true, rate).unwrap();
        let mut pending = false;
        let mut previous_active = None;
        for (index, raw) in input.chunks(part).enumerate() {
            let mut pcm = raw.to_vec();
            let mut completed = None;
            let mut reported_mute = false;
            detector
                .process_spans(
                    true,
                    &mut pcm,
                    |digit| {
                        assert!(
                            completed.replace(digit).is_none(),
                            "one transition per 8ms partition"
                        );
                    },
                    |range| reported_mute |= range.start < range.end,
                )
                .unwrap();
            let next_pending = if completed.is_some() {
                detector.suppressing()
            } else {
                pending || reported_mute
            };
            let new_onset = next_pending && (!pending || completed.is_some());

            // Independent destructive oracle: replay only real input, then ask the
            // actual released detector to emit its qualified active digit on loss.
            let mut oracle = DtmfDetector::at_rate(true, rate).unwrap();
            let end = ((index + 1) * part).min(input.len());
            let mut prefix = input[..end].to_vec();
            oracle
                .process_spans(true, &mut prefix, |_| {}, |_| {})
                .unwrap();
            let mut active = None;
            oracle
                .process_spans(
                    false,
                    &mut [],
                    |digit| {
                        assert!(active.replace(digit).is_none());
                    },
                    |_| {},
                )
                .unwrap();
            assert_eq!(next_pending, active.is_some(), "rate={rate}, end={end}");
            assert_eq!(
                new_onset,
                active.is_some() && active != previous_active,
                "onset must not repeat on each reported mute interval"
            );
            saw_pending_quiet_tail |= active.is_some() && !detector.suppressing();
            saw_replacement |= completed.is_some() && next_pending;
            pending = next_pending;
            previous_active = active;
        }
    }
    assert!(
        saw_pending_quiet_tail,
        "prove suppressing alone misses a qualified tail"
    );
    assert!(
        saw_replacement,
        "exercise completed old tone plus newly qualified tone"
    );
}

#[test]
fn released_pending_tone_inference_8000() {
    verify_pending_transitions(8000);
}

#[test]
fn released_pending_tone_inference_48000() {
    verify_pending_transitions(48000);
}
