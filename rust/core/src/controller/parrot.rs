//! Bounded parrot-burst measurement primitives shared by control and media owners.

use rtrb::{Consumer, Producer, RingBuffer};

/// Fixed retained audio bound: thirty seconds at the native 48 kHz rate.
pub const MAX_PARROT_SAMPLES: usize = 30 * crate::NATIVE_SAMPLE_RATE_HZ as usize;

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

/// Completed native-rate burst transferred to control without copying on the callback.
pub struct CapturedParrot {
    samples: Vec<f32>,
}

impl CapturedParrot {
    /// Retained prefix, already clamped to normalized F32.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Callback-owned bounded capture with two preallocated recording slots.
pub(crate) struct ParrotCapture {
    active: bool,
    recording: bool,
    current: Option<Vec<f32>>,
    free: Vec<Vec<f32>>,
    completed: Producer<CapturedParrot>,
    recycled: Consumer<Vec<f32>>,
}

/// Control-side endpoints for completed clips and capture-buffer recycling.
pub(crate) struct ParrotCaptureControl {
    completed: Consumer<CapturedParrot>,
    recycled: Producer<Vec<f32>>,
}

impl ParrotCapture {
    /// Allocate the fixed buffers and bounded cross-owner queues before callbacks start.
    pub(crate) fn new() -> (Self, ParrotCaptureControl) {
        let free = vec![
            Vec::with_capacity(MAX_PARROT_SAMPLES),
            Vec::with_capacity(MAX_PARROT_SAMPLES),
        ];
        let (completed, take_completed) = RingBuffer::new(2);
        let (recycle, recycled) = RingBuffer::new(2);
        (
            Self {
                active: false,
                recording: false,
                current: None,
                free,
                completed,
                recycled,
            },
            ParrotCaptureControl {
                completed: take_completed,
                recycled: recycle,
            },
        )
    }

    /// Capture normalized mixed PCM only while at least one input source is active.
    pub(crate) fn observe(&mut self, active: bool, samples: &[f32]) {
        while let Ok(buffer) = self.recycled.pop() {
            self.free.push(buffer);
        }

        if active && !self.active {
            self.current = self.free.pop();
            self.recording = self.current.is_some();
        }
        if active && self.recording {
            let current = self.current.as_mut().expect("recording owns a buffer");
            let count = samples.len().min(MAX_PARROT_SAMPLES - current.len());
            current.extend(samples[..count].iter().map(|sample| {
                if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                }
            }));
        }
        if !active && self.active && self.recording {
            let mut buffer = self.current.take().expect("recording owns a buffer");
            self.recording = false;
            if buffer.is_empty() {
                buffer.clear();
                self.free.push(buffer);
            } else if let Err(rtrb::PushError::Full(clip)) =
                self.completed.push(CapturedParrot { samples: buffer })
            {
                let mut buffer = clip.samples;
                buffer.clear();
                self.free.push(buffer);
            }
        }
        self.active = active;
    }
}

impl ParrotCaptureControl {
    /// Take one completed receive burst on the serialized control owner.
    pub fn take_completed(&mut self) -> Option<CapturedParrot> {
        self.completed.pop().ok()
    }

    /// Return retained storage after the media owner has copied or discarded its samples.
    pub fn recycle(&mut self, mut clip: CapturedParrot) {
        clip.samples.clear();
        let _ = self.recycled.push(clip.samples);
    }
}

#[cfg(test)]
#[path = "parrot_tests.rs"]
mod tests;
