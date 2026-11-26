//! Shared finite-media consumer: real duration/EOF and endpoint retirement stay explicit.
use crate::link::ring::{InboundConsumer, InboundProducer, RingError};
use rpt_advanced_core::link::PeerInput;
use rpt_advanced_core::media::MediaError;
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
    /// Offer source PCM once and publish only the duration actually accepted.
    pub(super) fn offer(&mut self, samples: &[f32]) -> Result<usize, MediaError> {
        let accepted = self.producer.write(samples).map_err(|_| MediaError::Io)?;
        self.source_samples = self.source_samples.saturating_add(accepted as u64);
        self.progress.budget.store(
            self.delay
                .saturating_add(self.source_samples.saturating_mul(48000) / u64::from(self.rate)),
            Ordering::Release,
        );
        Ok(accepted)
    }

    /// Fix the final duration once. Padding supplies filter context, never programme frames.
    pub(super) fn start_tail(&self) -> u64 {
        self.progress.budget.store(
            self.delay.saturating_add(
                self.source_samples
                    .saturating_mul(48000)
                    .div_ceil(u64::from(self.rate)),
            ),
            Ordering::Release,
        );
        self.delay
            .saturating_add(1)
            .saturating_mul(u64::from(self.rate))
            .div_ceil(48000)
    }

    pub(super) fn offer_tail(&mut self, remaining: &mut u64) -> Result<(), MediaError> {
        let zeros = [0.0; 256];
        let count = (*remaining).min(zeros.len() as u64) as usize;
        if count != 0 {
            let accepted = self.producer.write(&zeros[..count]).map_err(|_| {
                self.progress.undrainable.store(true, Ordering::Release);
                MediaError::Io
            })?;
            *remaining -= accepted as u64;
        }
        Ok(())
    }

    /// The separate text preparation executor may wait for the reader to free capacity.
    pub(super) fn write(
        &mut self,
        mut samples: &[f32],
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), MediaError> {
        while !samples.is_empty() {
            if cancelled() {
                return Err(MediaError::Cancelled);
            }
            let n = self.offer(samples)?;
            if n == 0 {
                thread::sleep(Duration::from_millis(1));
            }
            samples = &samples[n..];
        }
        Ok(())
    }

    pub(super) fn finish(mut self, cancelled: &impl Fn() -> bool) -> Result<(), MediaError> {
        let mut remaining = self.start_tail();
        while remaining != 0 {
            if cancelled() {
                self.progress.undrainable.store(true, Ordering::Release);
                return Err(MediaError::Cancelled);
            }
            let before = remaining;
            self.offer_tail(&mut remaining)?;
            if remaining == before {
                thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "finite_tests.rs"]
mod tests;
