//! Block peak protection at full-scale output boundaries.

/// One mono stream's gain, with immediate attack and a 100 ms release to unity.
pub struct PeakGuard {
    gain: f32,
    release_step: f32,
}

impl PeakGuard {
    /// Start at unity gain for a stream at a nonzero PCM sample rate.
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0);
        Self {
            gain: 1.0,
            release_step: 10.0 / rate as f32,
        }
    }

    /// Discard attenuation when the owner begins an unrelated stream.
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Protect a fresh block in place and silence nonfinite samples.
    pub fn apply(&mut self, pcm: &mut [f32]) {
        let mut peak = 0.0_f32;
        for sample in pcm.iter_mut() {
            if sample.is_finite() {
                peak = peak.max(sample.abs());
            } else {
                *sample = 0.0;
            }
        }
        let ceiling = if peak > 1.0 { 0.9 / peak } else { 1.0 };
        self.gain = self.gain.min(ceiling);
        if self.gain < 1.0 {
            for sample in pcm {
                *sample *= self.gain;
                self.gain = (self.gain + self.release_step).min(ceiling);
            }
        }
    }
}

/// Quantize one normalized sample at a signed-16 boundary, with finite saturation.
pub fn signed_pcm16(sample: f32) -> i16 {
    if sample.is_finite() {
        (sample.clamp(-1.0, 1.0) * 32768.0).round() as i16
    } else {
        0
    }
}
