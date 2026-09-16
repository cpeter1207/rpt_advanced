pub(crate) fn decode(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
}

pub(crate) fn encode(sample: f32) -> i16 {
    (sample * 32768.0).round() as i16
}

#[cfg(test)]
#[path = "pcm_tests.rs"]
mod tests;
