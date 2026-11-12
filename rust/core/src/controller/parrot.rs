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

/// Original private contribution retained with a clip, independent of its amplitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParrotPhoneSource {
    /// Oldest original receipt; unknown remains unknown across capture and transfer.
    pub age_bound: Option<crate::link::SourceTimeBound>,
    /// Node revision captured before the source render, never renewed during replay.
    pub retirement_revision: Option<u64>,
}
impl ParrotPhoneSource {
    pub(crate) fn current(&self, domain: Option<&crate::link::RetirementDomain>) -> bool {
        self.retirement_revision.is_some()
            && domain.and_then(crate::link::RetirementDomain::revision) == self.retirement_revision
    }
    pub(crate) fn include(&mut self, other: Self) {
        if self.retirement_revision != other.retirement_revision {
            self.retirement_revision = None;
        }
        self.age_bound = self
            .age_bound
            .zip(other.age_bound)
            .and_then(|(first, next)| {
                (first.host_clock_epoch != 0 && first.host_clock_epoch == next.host_clock_epoch)
                    .then_some(crate::link::SourceTimeBound {
                        host_clock_epoch: first.host_clock_epoch,
                        not_after_ns: first.not_after_ns.min(next.not_after_ns),
                    })
            });
    }
}

/// Completed native-rate burst transferred to control without copying on the callback.
pub struct CapturedParrot {
    samples: Vec<f32>,
    phone: Option<ParrotPhoneSource>,
}

impl CapturedParrot {
    /// Private taint is retained even when age is unknown or all PCM is zero.
    pub fn phone_source(&self) -> Option<ParrotPhoneSource> {
        self.phone
    }
    /// Retained prefix, already clamped to normalized F32.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Callback-owned bounded capture with two preallocated recording slots.
pub(crate) struct ParrotCapture {
    active: bool,
    recording: bool,
    phone: Option<ParrotPhoneSource>,
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

/// Lock-free live buffer lifecycle requests, produced by control and consumed by audio.
pub(crate) enum ParrotRequest {
    /// Transfer newly allocated bounded recording buffers to the callback owner.
    Enable(ParrotCapture),
    /// Return callback-owned recording buffers for control-side reclamation.
    Disable,
}

/// Callback acknowledgement; returned buffers are dropped only on the control owner.
pub(crate) enum ParrotAck {
    /// The callback has installed the capture owner.
    Enabled,
    /// The callback has stopped capture and returned its allocated buffers.
    Disabled(ParrotCapture),
}

/// Callback-side half of the live lifecycle queues.
pub(crate) struct ParrotLifecycle {
    pub requests: Consumer<ParrotRequest>,
    pub acknowledgements: Producer<ParrotAck>,
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
                phone: None,
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

    /// Create the bounded lock-free transfer queues for live enable/disable operations.
    pub(crate) fn lifecycle() -> (
        Producer<ParrotRequest>,
        Consumer<ParrotAck>,
        ParrotLifecycle,
    ) {
        let (requests, take_requests) = RingBuffer::new(1);
        let (acknowledgements, take_acknowledgements) = RingBuffer::new(1);
        (
            requests,
            take_acknowledgements,
            ParrotLifecycle {
                requests: take_requests,
                acknowledgements,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn observe(&mut self, active: bool, samples: &[f32]) {
        self.observe_with_phone(active, samples, None, None);
    }

    fn cancel_burst(&mut self) {
        if let Some(mut buffer) = self.current.take() {
            buffer.clear();
            self.free.push(buffer);
        }
        self.recording = false;
        self.phone = None;
    }

    /// Capture into existing storage; retired inseparable private clips are discarded whole.
    pub(crate) fn observe_with_phone(
        &mut self,
        active: bool,
        samples: &[f32],
        phone: Option<ParrotPhoneSource>,
        domain: Option<&crate::link::RetirementDomain>,
    ) {
        while let Ok(buffer) = self.recycled.pop() {
            self.free.push(buffer);
        }
        if active && !self.active {
            self.current = self.free.pop();
            self.recording = self.current.is_some();
            self.phone = None;
        }
        if self.phone.is_some_and(|source| !source.current(domain)) {
            self.cancel_burst();
        }
        if active && self.recording {
            let count = samples.len().min(
                MAX_PARROT_SAMPLES
                    - self
                        .current
                        .as_ref()
                        .expect("recording owns a buffer")
                        .len(),
            );
            if count != 0 {
                if let Some(mut phone) = phone {
                    phone.age_bound = phone.age_bound.filter(|bound| bound.host_clock_epoch != 0);
                    if let Some(retained) = self.phone.as_mut() {
                        retained.include(phone);
                    } else {
                        self.phone = Some(phone);
                    }
                }
                if self.phone.is_some_and(|source| !source.current(domain)) {
                    self.cancel_burst();
                } else {
                    self.current
                        .as_mut()
                        .expect("recording owns a buffer")
                        .extend(samples[..count].iter().map(|sample| {
                            if sample.is_finite() {
                                sample.clamp(-1.0, 1.0)
                            } else {
                                0.0
                            }
                        }));
                    if self.phone.is_some_and(|source| !source.current(domain)) {
                        self.cancel_burst();
                    }
                }
            }
        }
        if !active && self.active && self.recording {
            let mut buffer = self.current.take().expect("recording owns a buffer");
            self.recording = false;
            let phone = self.phone.take();
            if buffer.is_empty() {
                buffer.clear();
                self.free.push(buffer);
            } else if let Err(rtrb::PushError::Full(clip)) = self.completed.push(CapturedParrot {
                samples: buffer,
                phone,
            }) {
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
