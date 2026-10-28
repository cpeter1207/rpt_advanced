//! Exercise the controller client against the dynamic source-rate detector.

use rpt_advanced_core::audio::DtmfDetector;

fn detect(rate: u32, partition: usize) -> (Vec<char>, Vec<std::ops::Range<i64>>) {
    let mut detector = DtmfDetector::at_rate(true, rate).unwrap();
    let mut pcm = (0..rate / 5)
        .map(|sample| {
            let time = sample as f32 / rate as f32;
            0.2 * ((time * 697.0 * std::f32::consts::TAU).sin()
                + (time * 1209.0 * std::f32::consts::TAU).sin())
        })
        .collect::<Vec<_>>();
    let mut digits = Vec::new();
    let mut spans = Vec::new();
    let mut offset = 0_i64;
    for chunk in pcm.chunks_mut(partition) {
        detector
            .process_spans(
                true,
                chunk,
                |digit| digits.push(digit),
                |span| spans.push(span.start + offset..span.end + offset),
            )
            .unwrap();
        offset += chunk.len() as i64;
    }
    assert!(detector.suppressing());
    detector
        .process_spans(false, &mut [], |digit| digits.push(digit), |_| {})
        .unwrap();
    assert!(!detector.suppressing());
    // Normalize partitioned intervals to their union, not their callback count.
    let mut united: Vec<std::ops::Range<i64>> = Vec::new();
    for span in spans {
        if let Some(last) = united.last_mut().filter(|last| span.start <= last.end) {
            last.end = last.end.max(span.end);
        } else {
            united.push(span);
        }
    }
    (digits, united)
}

#[test]
fn source_rate_spans_keep_qualifying_audio_across_partitions() {
    for rate in [8_000, 48_000] {
        let expected = detect(rate, rate as usize / 5);
        assert_eq!(expected.0, ['1']);
        assert_eq!(expected.1.len(), 1);
        assert_eq!(expected.1[0], 0..i64::from(rate / 5));
        assert_eq!(detect(rate, 37), expected);
        assert_eq!(detect(rate, 1), expected);
    }
    assert!(DtmfDetector::at_rate(true, 16_000).is_err());
}
