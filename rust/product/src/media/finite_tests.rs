//! Finite media EOF comes from the producer and its real budget, not temporary emptiness.
use super::*;
use crate::link::ring::{InboundPolicy, InboundRing};

#[test]
fn finite_stream_budget_and_stall() {
    let (producer, ring) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
    let progress = Arc::new(SegmentProgress::default());
    let mut consumer = SegmentConsumer {
        ring,
        progress: Arc::clone(&progress),
        consumed: 0,
    };
    assert!(!consumer.finished(false));
    assert_eq!(consumer.render_sample().unwrap(), None);
    progress.budget.store(1, Ordering::Release);
    progress.undrainable.store(true, Ordering::Release);
    assert!(consumer.render_sample().is_err());
    assert!(!consumer.finished(true));
    consumer.consumed = 1;
    assert!(consumer.finished(true));
    drop(producer);
    assert!(consumer.finished(false));
}
