//! Shared finite-media consumer: real duration/EOF and endpoint retirement stay explicit.
use super::{Context, ffi, status};
use crate::link::ring::{InboundConsumer, InboundProducer, RingError};
use rpt_advanced_core::link::PeerInput;
use rpt_advanced_core::media::MediaError;
use std::ffi::c_void;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{thread, time::Duration};

/// Finite duration excludes resampler padding; an empty ring alone is not EOF.
pub(super) struct SegmentConsumer {
    pub(super) ring: InboundConsumer,
    pub(super) progress: Arc<SegmentProgress>,
    pub(super) consumed: u64,
}
/// Producer-published real source duration and an undrainable flush indication.
#[derive(Default)]
pub(super) struct SegmentProgress {
    pub(super) budget: AtomicU64,
    pub(super) undrainable: AtomicBool,
}
impl SegmentConsumer {
    pub(super) fn finished(&self, completed: bool) -> bool {
        // Observe end before duration: final writes cannot appear after this fence.
        let ended = self.ring.signals().ended() || completed;
        ended && self.consumed >= self.progress.budget.load(Ordering::Acquire)
    }
    pub(super) fn render_sample(&mut self) -> Result<Option<f32>, RingError> {
        if self.consumed >= self.progress.budget.load(Ordering::Acquire) {
            return Ok(None);
        }
        let sample = self.ring.render_sample()?;
        self.consumed += u64::from(sample.is_some());
        if sample.is_none() && self.progress.undrainable.load(Ordering::Acquire) {
            Err(RingError)
        } else {
            Ok(sample)
        }
    }
}

/// Common finite-source accounting and filter-tail drain for every media producer.
pub(super) struct FiniteWriter {
    producer: InboundProducer,
    progress: Arc<SegmentProgress>,
    rate: u32,
    delay: u64,
    source_samples: u64,
}
impl FiniteWriter {
    pub(super) fn new(
        producer: InboundProducer,
        progress: Arc<SegmentProgress>,
        rate: u32,
    ) -> Result<Self, MediaError> {
        let delay = producer
            .output_delay_samples()
            .map_err(|_| MediaError::Io)?;
        Ok(Self {
            producer,
            progress,
            rate,
            delay,
            source_samples: 0,
        })
    }
    pub(super) fn delay(&self) -> u64 {
        self.delay
    }
    pub(super) fn write(
        &mut self,
        samples: &[f32],
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), MediaError> {
        self.push(samples, cancelled, true)
    }
    fn push(
        &mut self,
        mut samples: &[f32],
        cancelled: &impl Fn() -> bool,
        real: bool,
    ) -> Result<(), MediaError> {
        while !samples.is_empty() {
            if cancelled() {
                return Err(MediaError::Cancelled);
            }
            let accepted = self.producer.write(samples).map_err(|_| MediaError::Io)?;
            if accepted == 0 {
                thread::sleep(Duration::from_millis(1));
            }
            if real {
                self.source_samples = self.source_samples.saturating_add(accepted as u64);
                self.progress.budget.store(
                    self.delay.saturating_add(
                        self.source_samples.saturating_mul(48000) / u64::from(self.rate),
                    ),
                    Ordering::Release,
                );
            }
            samples = &samples[accepted..];
        }
        Ok(())
    }
    pub(super) fn finish(mut self, cancelled: &impl Fn() -> bool) -> Result<(), MediaError> {
        self.progress.budget.store(
            self.delay.saturating_add(
                self.source_samples
                    .saturating_mul(48000)
                    .div_ceil(u64::from(self.rate)),
            ),
            Ordering::Release,
        );
        let mut remaining = self
            .delay
            .saturating_add(1)
            .saturating_mul(u64::from(self.rate))
            .div_ceil(48000);
        let zeros = [0.0; 256];
        while remaining != 0 {
            let count = remaining.min(zeros.len() as u64) as usize;
            if let Err(error) = self.push(&zeros[..count], cancelled, false) {
                self.progress.undrainable.store(true, Ordering::Release);
                return Err(error);
            }
            remaining -= count as u64;
        }
        Ok(())
    }
}

/// Validate one complete streaming-provider result before any PCM is published.
pub(super) fn read_stream(
    context: &Context,
    handle: *mut c_void,
    output: &mut [f32],
    cancellation: &ffi::rptadv_media_cancellation,
) -> Result<usize, MediaError> {
    if output.is_empty() {
        return Err(MediaError::InvalidRequest);
    }
    let mut count = 0;
    // All storage and the exclusively owned handle outlive this synchronous call.
    status(unsafe {
        (context.read_stream)(
            handle,
            cancellation,
            output.as_mut_ptr(),
            output.len(),
            &mut count,
        )
    })?;
    if count > output.len()
        || output[..count]
            .iter()
            .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
    {
        return Err(MediaError::InvalidOutput);
    }
    Ok(count)
}

/// Shared handle and inclusive source-rate bounds for file/speech providers.
pub(super) fn valid_stream(handle: *mut c_void, rate: u32) -> bool {
    !handle.is_null() && (8000..=192000).contains(&rate)
}

#[cfg(test)]
#[path = "finite_tests.rs"]
mod tests;
