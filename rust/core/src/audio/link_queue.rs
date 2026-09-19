//! Plain bounded outbound peer PCM handoff.

use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

/// Error returned when a queue cannot be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQueueError {
    /// The requested capacity was zero.
    ZeroCapacity,
}

#[derive(Default)]
struct Statistics {
    dropped: AtomicU64,
    shortfall: AtomicU64,
}

/// A plain outbound PCM queue before its producer and consumer are assigned.
pub struct LinkAudioQueue {
    producer: Producer<f32>,
    consumer: Consumer<f32>,
    statistics: Arc<Statistics>,
}

/// Single producer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioProducer {
    producer: Producer<f32>,
    statistics: Arc<Statistics>,
}

/// Single consumer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioConsumer {
    consumer: Consumer<f32>,
    statistics: Arc<Statistics>,
}

impl LinkAudioQueue {
    /// Allocate a fixed-capacity SPSC queue outside the real-time path.
    pub fn new(capacity: usize) -> Result<Self, LinkQueueError> {
        if capacity == 0 {
            return Err(LinkQueueError::ZeroCapacity);
        }
        let (producer, consumer) = RingBuffer::new(capacity);
        Ok(Self {
            producer,
            consumer,
            statistics: Arc::new(Statistics::default()),
        })
    }

    /// Split this queue into its single producer and single consumer endpoints.
    #[must_use]
    pub fn into_endpoints(self) -> (LinkAudioProducer, LinkAudioConsumer) {
        (
            LinkAudioProducer {
                producer: self.producer,
                statistics: Arc::clone(&self.statistics),
            },
            LinkAudioConsumer {
                consumer: self.consumer,
                statistics: self.statistics,
            },
        )
    }
}

impl LinkAudioProducer {
    /// Write leading samples that fit and return the rejected newest-sample count.
    pub fn write(&mut self, audio: &[f32]) -> usize {
        let mut written = 0;
        for sample in audio {
            if self.producer.push(*sample).is_err() {
                break;
            }
            written += 1;
        }
        let dropped = audio.len() - written;
        self.statistics.dropped.fetch_add(
            u64::try_from(dropped).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        dropped
    }

    /// Return the cumulative number of newest samples rejected because the queue was full.
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.statistics.dropped.load(Ordering::Relaxed)
    }
}

impl LinkAudioConsumer {
    /// Read queued samples in order, fill any shortfall with silence, and return the shortfall.
    pub fn read(&mut self, output: &mut [f32]) -> usize {
        let mut read = 0;
        while read < output.len() {
            let Ok(sample) = self.consumer.pop() else {
                break;
            };
            output[read] = sample;
            read += 1;
        }
        output[read..].fill(0.0);
        let shortfall = output.len() - read;
        self.statistics.shortfall.fetch_add(
            u64::try_from(shortfall).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        shortfall
    }

    /// Return the cumulative output samples supplied as silence because the queue was empty.
    #[must_use]
    pub fn shortfall_samples(&self) -> u64 {
        self.statistics.shortfall.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
#[path = "link_queue_tests.rs"]
mod tests;
