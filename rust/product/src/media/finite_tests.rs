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

#[test]
fn finite_writer_publishes_only_real_duration_and_drains_source_rate_padding() {
    let (producer, ring) = InboundRing::open(22050, InboundPolicy::Media).unwrap();
    let progress = Arc::new(SegmentProgress::default());
    let mut writer = FiniteWriter::new(producer, Arc::clone(&progress), 22050).unwrap();
    let delay = writer.delay();
    writer.write(&[0.25; 441], &|| false).unwrap();
    writer.finish(&|| false).unwrap();
    let mut consumer = SegmentConsumer {
        ring,
        progress,
        consumed: 0,
    };
    let mut count = 0;
    while !consumer.finished(false) {
        assert!(consumer.render_sample().unwrap().is_some());
        count += 1;
        assert!(count < 2000);
    }
    assert_eq!(count, delay + 960);
}
