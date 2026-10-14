//! Bounded outbound PCM with source-time spans; never rebase dropped media.

use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

/// Error returned when a queue cannot be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQueueError {
    /// The requested capacity was zero.
    ZeroCapacity,
}

/// Immutable origin of a contiguous native-rate PCM span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioSpan {
    /// Configuration generation that admitted these samples.
    pub generation: u64,
    /// Destination's receive-qualified burst identity.
    pub burst: u64,
    /// First sample on the stream timeline, including any previously dropped samples.
    pub source_start: u64,
}

/// One read never crosses a source span, even when later audio is already queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadSpan {
    /// Origin adjusted for any prefix consumed on a previous read.
    pub span: AudioSpan,
    /// Real samples copied to the output prefix.
    pub frames: usize,
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
    spans: (Producer<ReadSpan>, Consumer<ReadSpan>),
    statistics: Arc<Statistics>,
}

/// Single producer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioProducer {
    producer: Producer<f32>,
    spans: Producer<ReadSpan>,
    statistics: Arc<Statistics>,
}

/// Single consumer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioConsumer {
    consumer: Consumer<f32>,
    spans: Consumer<ReadSpan>,
    pending: Option<ReadSpan>,
    statistics: Arc<Statistics>,
}

impl LinkAudioQueue {
    /// Allocate a fixed-capacity SPSC queue outside the real-time path.
    pub fn new(capacity: usize, span_capacity: usize) -> Result<Self, LinkQueueError> {
        if capacity == 0 || span_capacity == 0 {
            return Err(LinkQueueError::ZeroCapacity);
        }
        let (producer, consumer) = RingBuffer::new(capacity);
        Ok(Self {
            producer,
            consumer,
            spans: RingBuffer::new(span_capacity),
            statistics: Arc::new(Statistics::default()),
        })
    }

    /// Split this queue into its single producer and single consumer endpoints.
    #[must_use]
    pub fn into_endpoints(self) -> (LinkAudioProducer, LinkAudioConsumer) {
        (
            LinkAudioProducer {
                producer: self.producer,
                spans: self.spans.0,
                statistics: Arc::clone(&self.statistics),
            },
            LinkAudioConsumer {
                consumer: self.consumer,
                spans: self.spans.1,
                pending: None,
                statistics: self.statistics,
            },
        )
    }
}

impl LinkAudioProducer {
    /// Write leading samples that fit and return the rejected newest-sample count.
    pub fn write(&mut self, span: AudioSpan, audio: &[f32]) -> usize {
        // Sole producer: a free descriptor cannot be taken by another writer.
        // Publish samples first; the consumer reads only after acquiring this span.
        let written = if self.spans.slots() == 0 {
            0
        } else {
            self.producer.push_partial_slice(audio).0.len()
        };
        if written != 0 {
            self.spans
                .push(ReadSpan {
                    span,
                    frames: written,
                })
                .expect("reserved span");
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
    /// Copy only the current span, leaving the unused output untouched.
    pub fn read_span(&mut self, output: &mut [f32]) -> Option<ReadSpan> {
        if output.is_empty() {
            return None;
        }
        if self.pending.is_none() {
            self.pending = Some(self.spans.pop().ok()?);
        }
        let pending = self.pending.as_mut().expect("current span");
        let count = pending.frames.min(output.len());
        // The acquired descriptor publishes all of its PCM before it becomes visible.
        self.consumer
            .pop_entire_slice(&mut output[..count])
            .expect("published span PCM");
        let read = ReadSpan {
            span: pending.span,
            frames: count,
        };
        pending.span.source_start = pending.span.source_start.wrapping_add(count as u64);
        pending.frames -= count;
        if pending.frames == 0 {
            self.pending = None;
        }
        Some(read)
    }

    /// Unclocked PCM for synchronous hosts; asynchronous egress uses `read_span`.
    /// Fill any shortfall with silence and return its length.
    pub fn read(&mut self, output: &mut [f32]) -> usize {
        let mut read = 0;
        while read < output.len() {
            let Some(span) = self.read_span(&mut output[read..]) else {
                break;
            };
            read += span.frames;
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
