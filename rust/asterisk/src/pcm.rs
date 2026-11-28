pub(crate) fn decode(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
}

pub(crate) fn peak_gain(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak > 1.0 { 0.9 / peak } else { 1.0 }
}

pub(crate) fn encode(sample: f32) -> i16 {
    if sample.is_finite() {
        // Rust's float-to-integer cast saturates both endpoints after rounding.
        (sample.clamp(-1.0, 1.0) * 32768.0).round() as i16
    } else {
        0
    }
}

#[cfg(test)]
#[path = "pcm_tests.rs"]
mod tests;
