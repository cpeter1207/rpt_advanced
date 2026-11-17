//! Bounded outbound PCM with source-time spans; never rebase dropped media.

use crate::link::RetirementDomain;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

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
    /// Physical stream identity; changes on reopen, not ordinary configuration reload.
    pub stream: u64,
    /// Immutable monotonic source time; absent until the native clock is observed.
    pub born_ns: Option<u64>,
}

/// One read never crosses a source span, even when later audio is already queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadSpan {
    /// Origin adjusted for any prefix consumed on a previous read.
    pub span: AudioSpan,
    /// Real samples copied to the output prefix.
    pub frames: usize,
}

// A revision can never reach this value: domain saturation is unavailable.
// The sentinel therefore leaves ordinary-only queued media explicitly unmarked.
const UNGUARDED: u64 = u64::MAX;

#[derive(Clone, Copy, Debug)]
struct QueuedSpan {
    read: ReadSpan,
    revision: u64,
}
struct Statistics {
    dropped: AtomicU64,
    shortfall: AtomicU64,
    retirement: OnceLock<RetirementDomain>,
    // Sole consumer writes/reads this scalar. Keeping it in the existing shared
    // allocation preserves the consumer's size without another allocation or
    // unsafe interior mutability. It is ignored whenever pending is None.
    pending_revision: AtomicU64,
}
impl Default for Statistics {
    fn default() -> Self {
        Self {
            dropped: AtomicU64::new(0),
            shortfall: AtomicU64::new(0),
            retirement: OnceLock::new(),
            pending_revision: AtomicU64::new(UNGUARDED),
        }
    }
}

/// A plain outbound PCM queue before its producer and consumer are assigned.
pub struct LinkAudioQueue {
    producer: Producer<f32>,
    consumer: Consumer<f32>,
    spans: (Producer<QueuedSpan>, Consumer<QueuedSpan>),
    statistics: Arc<Statistics>,
}

/// Single producer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioProducer {
    producer: Producer<f32>,
    spans: Producer<QueuedSpan>,
    statistics: Arc<Statistics>,
}

/// Single consumer endpoint for a [`LinkAudioQueue`].
pub struct LinkAudioConsumer {
    consumer: Consumer<f32>,
    spans: Consumer<QueuedSpan>,
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
    /// Bind the existing shared queue allocation once, on control before publication.
    /// No callback registration, allocation or per-write reference-count work follows.
    pub fn bind_retirement_domain(&mut self, domain: &RetirementDomain) -> bool {
        self.statistics.retirement.set(domain.clone()).is_ok()
    }

    /// Write ordinary-only leading samples and return the rejected newest count.
    pub fn write(&mut self, span: AudioSpan, audio: &[f32]) -> usize {
        self.publish(span, audio, UNGUARDED, true)
    }

    /// Store a private-tainted span with its original prepared node revision.
    /// Acceptance is queue storage, never irrevocable presentation. Even a late
    /// publication is checked against this revision when the consumer acquires it.
    pub fn write_revocable(
        &mut self,
        span: AudioSpan,
        audio: &[f32],
        revision: Option<u64>,
    ) -> usize {
        let valid = self.statistics.retirement.get().is_some()
            && revision.is_some_and(|revision| revision != UNGUARDED);
        self.publish(span, audio, revision.unwrap_or(UNGUARDED), valid)
    }

    fn publish(&mut self, span: AudioSpan, audio: &[f32], revision: u64, valid: bool) -> usize {
        // Sole producer: a free descriptor cannot be taken by another writer.
        // Publish samples first; the consumer reads only after acquiring this span.
        let written = if !valid || self.spans.slots() == 0 {
            0
        } else {
            self.producer.push_partial_slice(audio).0.len()
        };
        if written != 0 {
            self.spans
                .push(QueuedSpan {
                    read: ReadSpan {
                        span,
                        frames: written,
                    },
                    revision,
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
    /// Newest native samples rejected by the single producer at queue capacity.
    pub fn dropped_samples(&self) -> u64 {
        self.statistics.dropped.load(Ordering::Relaxed)
    }
    fn current(&self, revision: u64) -> bool {
        revision == UNGUARDED
            || self
                .statistics
                .retirement
                .get()
                .expect("revocable publication has a prepared domain")
                .revision()
                == Some(revision)
    }
    fn discard_pending(&mut self) {
        let pending = self.pending.take().expect("discarded current span");
        self.statistics
            .pending_revision
            .store(UNGUARDED, Ordering::Relaxed);
        // Acquiring its descriptor also acquired every PCM slot. Dropping stale
        // media advances only the existing consumer; no scratch copy is needed.
        self.consumer
            .read_chunk(pending.frames)
            .expect("published span PCM")
            .commit_all();
    }

    /// Inspect a live source span. Stale marked spans are discarded in FIFO order;
    /// work is bounded by the entry snapshot even if the producer keeps refilling.
    pub fn peek_span(&mut self) -> Option<ReadSpan> {
        self.peek_current().map(|queued| queued.read)
    }
    fn peek_current(&mut self) -> Option<QueuedSpan> {
        let maximum = self.spans.slots().saturating_add(1);
        for _ in 0..maximum {
            if self.pending.is_none() {
                let queued = self.spans.pop().ok()?;
                self.statistics
                    .pending_revision
                    .store(queued.revision, Ordering::Relaxed);
                self.pending = Some(queued.read);
            }
            let pending = QueuedSpan {
                read: self.pending.expect("acquired current span"),
                revision: self.statistics.pending_revision.load(Ordering::Relaxed),
            };
            if self.current(pending.revision) {
                return Some(pending);
            }
            self.discard_pending();
        }
        None
    }
    /// Copy only a currently valid span. Retirement can cancel a preceding peek
    /// or this copy; a cancelled copy zeros its touched prefix and returns None.
    /// An existing peeked or partly read span is never replaced by its successor
    /// in the same call. On cancellation, callers must reevaluate the next span's
    /// admission conditions (such as its due time) before another read.
    /// PCM returned successfully has crossed the consumer-admission boundary;
    /// retained conversion/provider history requires its own retirement fence.
    pub fn read_span(&mut self, output: &mut [f32]) -> Option<ReadSpan> {
        self.read_current(output).map(|queued| queued.read)
    }

    fn read_current(&mut self, output: &mut [f32]) -> Option<QueuedSpan> {
        if output.is_empty() {
            return None;
        }
        let queued = if let Some(read) = self.pending {
            let queued = QueuedSpan {
                read,
                revision: self.statistics.pending_revision.load(Ordering::Relaxed),
            };
            if !self.current(queued.revision) {
                self.discard_pending();
                return None;
            }
            queued
        } else {
            self.peek_current()?
        };
        let pending = self.pending.as_mut().expect("current span");
        let count = pending.frames.min(output.len());
        self.consumer
            .pop_entire_slice(&mut output[..count])
            .expect("published span PCM");
        let read = QueuedSpan {
            read: ReadSpan {
                span: pending.span,
                frames: count,
            },
            revision: queued.revision,
        };
        let next = pending.span.source_start.wrapping_add(count as u64);
        let elapsed = (u128::from(next) * 1_000_000_000 / 48_000)
            .saturating_sub(u128::from(pending.span.source_start) * 1_000_000_000 / 48_000);
        pending.span.born_ns = pending
            .span
            .born_ns
            .and_then(|time| time.checked_add(elapsed as u64));
        pending.span.source_start = next;
        pending.frames -= count;
        if pending.frames == 0 {
            self.pending = None;
            self.statistics
                .pending_revision
                .store(UNGUARDED, Ordering::Relaxed);
        }
        #[cfg(test)]
        tests::after_copy();
        if !self.current(read.revision) {
            output[..count].fill(0.0);
            return None;
        }
        Some(read)
    }

    /// Unclocked PCM for synchronous hosts; asynchronous egress uses read_span.
    /// A read spanning a retired private contribution is conservatively silenced
    /// as a whole. Ordinary-only reads are independent of node retirement.
    pub fn read(&mut self, output: &mut [f32]) -> usize {
        let mut read = 0;
        let mut revision = None;
        let mut consistent = true;
        while read < output.len() {
            let Some(queued) = self.read_current(&mut output[read..]) else {
                break;
            };
            if queued.revision != UNGUARDED {
                consistent &= revision.is_none_or(|previous| previous == queued.revision);
                revision = Some(queued.revision);
            }
            read += queued.read.frames;
        }
        if !consistent || revision.is_some_and(|revision| !self.current(revision)) {
            read = 0;
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
