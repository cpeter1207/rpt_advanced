//! Bounded parrot-burst measurement primitives shared by control and media owners.

/// Peak and RMS levels rounded to integer dBFS for the spoken parrot report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParrotLevels {
    /// Maximum absolute sample level in dBFS.
    pub peak_dbfs: i32,
    /// Root-mean-square level in dBFS.
    pub rms_dbfs: i32,
}

impl ParrotLevels {
    /// Measure normalized PCM; empty input has no measurement and silence floors at -120 dBFS.
    pub fn measure(samples: &[f32]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }

        let (peak, sum_squares) = samples.iter().fold((0.0_f64, 0.0_f64), |acc, sample| {
            let value = f64::from(*sample).clamp(-1.0, 1.0);
            (acc.0.max(value.abs()), acc.1 + value * value)
        });
        let rms = (sum_squares / samples.len() as f64).sqrt();
        Some(Self {
            peak_dbfs: dbfs(peak),
            rms_dbfs: dbfs(rms),
        })
    }
}

fn dbfs(level: f64) -> i32 {
    if level <= 0.0 {
        -120
    } else {
        (20.0 * level.log10()).round().clamp(-120.0, 0.0) as i32
    }
}

#[cfg(test)]
#[path = "parrot_tests.rs"]
mod tests;
