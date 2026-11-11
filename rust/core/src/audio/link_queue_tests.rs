use super::LinkAudioQueue;

#[test]
fn egress_span_partition_and_overflow() {
    use super::AudioSpan;
    for count in [1, 240, 480, 960, 4096] {
        let (mut producer, mut consumer) =
            LinkAudioQueue::new(count + 1, 2).unwrap().into_endpoints();
        let first = AudioSpan {
            generation: 7,
            burst: 3,
            source_start: 100,
            ..AudioSpan::default()
        };
        let second = AudioSpan {
            source_start: 100 + count as u64,
            ..first
        };
        assert_eq!(producer.write(first, &vec![0.25; count]), 0);
        assert_eq!(producer.write(second, &[0.5, 0.6]), 1);
        let mut output = vec![9.0; count.max(2)];
        let mut position = 100;
        let mut remaining = count;
        while remaining != 0 {
            let wanted = remaining.min(37);
            let read = consumer.read_span(&mut output[..wanted]).unwrap();
            assert_eq!(
                read.span,
                AudioSpan {
                    source_start: position,
                    ..first
                }
            );
            assert_eq!(read.frames, wanted);
            assert_eq!(&output[..wanted], vec![0.25; wanted]);
            remaining -= wanted;
            position += wanted as u64;
        }
        let read = consumer.read_span(&mut output).unwrap();
        assert_eq!((read.span, read.frames, output[0]), (second, 1, 0.5));
        assert!(consumer.read_span(&mut output).is_none());
        // Rejected samples leave a source-time gap; later samples never slide backward.
        let third = AudioSpan {
            generation: 8,
            burst: 4,
            source_start: second.source_start + 2,
            ..AudioSpan::default()
        };
        assert_eq!(producer.write(third, &[0.7]), 0);
        assert_eq!(consumer.read_span(&mut output).unwrap().span, third);
        assert_eq!(producer.dropped_samples(), 1);
    }
    let (mut producer, mut consumer) = LinkAudioQueue::new(8, 1).unwrap().into_endpoints();
    let span = AudioSpan {
        generation: 1,
        burst: 1,
        source_start: 0,
        ..AudioSpan::default()
    };
    assert_eq!(producer.write(span, &[1.0]), 0);
    assert_eq!(
        producer.write(span, &[2.0, 3.0]),
        2,
        "descriptor exhaustion rejects whole newest span"
    );
    assert!(consumer.read_span(&mut []).is_none());
    assert_eq!(consumer.read_span(&mut [0.0; 8]).unwrap().frames, 1);
    assert!(consumer.read_span(&mut [0.0]).is_none());
    assert_eq!(producer.dropped_samples(), 2);
}

#[test]
fn queue_keeps_oldest_samples_and_silences_shortfall() {
    assert!(LinkAudioQueue::new(0, 64).is_err());
    assert!(LinkAudioQueue::new(3, 0).is_err());
    let (mut producer, mut consumer) = LinkAudioQueue::new(3, 64).unwrap().into_endpoints();
    assert_eq!(
        producer.write(super::AudioSpan::default(), &[10.0, 20.0]),
        0
    );
    let mut first = [99.0];
    assert_eq!(consumer.read(&mut first), 0);
    assert_eq!(first, [10.0]);
    assert_eq!(
        producer.write(super::AudioSpan::default(), &[30.0, 40.0, 50.0]),
        1
    );
    let mut output = [99.0; 5];
    assert_eq!(consumer.read(&mut output), 2);
    assert_eq!(output, [20.0, 30.0, 40.0, 0.0, 0.0]);
    assert_eq!(consumer.shortfall_samples(), 2);
    assert_eq!(producer.dropped_samples(), 1);
}

#[test]
fn egress_span_peek_and_fractional_birth_survive_partitions() {
    use super::AudioSpan;
    let (mut producer, mut consumer) = LinkAudioQueue::new(1000, 2).unwrap().into_endpoints();
    let span = AudioSpan {
        source_start: 17,
        born_ns: Some(1_000_000_000),
        ..AudioSpan::default()
    };
    producer.write(span, &[0.5; 997]);
    let mut total = 0;
    while let Some(next) = consumer.peek_span() {
        assert_eq!(consumer.peek_span(), Some(next));
        let read = consumer.read_span(&mut [0.0; 37]).unwrap();
        assert_eq!(read.span.source_start, 17 + total);
        assert_eq!(
            read.span.born_ns,
            Some(
                1_000_000_000 + ((17 + total) * 1_000_000_000 / 48_000)
                    - 17 * 1_000_000_000 / 48_000
            )
        );
        total += read.frames as u64;
    }
    assert_eq!(total, 997);
}

#[test]
fn queue_endpoints_transfer_samples_between_threads() {
    let (producer, consumer) = LinkAudioQueue::new(16, 64).unwrap().into_endpoints();
    let writer = std::thread::spawn(move || {
        let mut producer = producer;
        assert_eq!(
            producer.write(super::AudioSpan::default(), &[1.0, 2.0, 3.0, 4.0]),
            0
        );
    });
    let reader = std::thread::spawn(move || {
        let mut consumer = consumer;
        let mut output = [0.0; 4];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut received = 0;
        // Per-sample publication permits valid partial reads; retain each prefix.
        while received < output.len() {
            assert!(std::time::Instant::now() < deadline, "PCM handoff stalled");
            let remaining = &mut output[received..];
            received += remaining.len() - consumer.read(remaining);
            std::thread::yield_now();
        }
        output
    });
    writer.join().unwrap();
    assert_eq!(reader.join().unwrap(), [1.0, 2.0, 3.0, 4.0]);
}

fn revocable_queue(
    capacity: usize,
    spans: usize,
) -> (
    super::LinkAudioProducer,
    super::LinkAudioConsumer,
    crate::link::RetirementDomain,
    crate::link::PeerReply,
) {
    let domain = crate::link::RetirementDomain::default();
    let source = crate::link::PeerSignals::new().reply_target();
    source.bind_retirement_domain(&domain);
    let (mut writer, reader) = LinkAudioQueue::new(capacity, spans)
        .unwrap()
        .into_endpoints();
    assert!(writer.bind_retirement_domain(&domain));
    assert!(!writer.bind_retirement_domain(&domain));
    (writer, reader, domain, source)
}

#[test]
fn revocable_queue_keeps_unmarked_fifo_and_discards_late_marked_publication() {
    use super::AudioSpan;
    let (mut writer, mut reader, domain, source) = revocable_queue(16, 8);
    let span = |source_start| AudioSpan {
        source_start,
        ..AudioSpan::default()
    };
    assert_eq!(writer.write(span(0), &[1.0, 1.0]), 0);
    let revision = domain.revision();
    source.retire();
    // The descriptor is published after retirement has completed. Its old
    // revision remains revocable when the actual consumer acquires it.
    assert_eq!(writer.write_revocable(span(2), &[2.0, 2.0], revision), 0);
    assert_eq!(writer.write(span(4), &[3.0, 3.0]), 0);
    let mut pcm = [0.0; 2];
    assert_eq!(reader.read_span(&mut pcm).unwrap().span.source_start, 0);
    assert_eq!(pcm, [1.0; 2]);
    assert_eq!(reader.peek_span().unwrap().span.source_start, 4);
    assert_eq!(reader.read_span(&mut pcm).unwrap().span.source_start, 4);
    assert_eq!(pcm, [3.0; 2]);
    assert!(reader.peek_span().is_none());
    assert_eq!(
        writer.write_revocable(span(6), &[4.0], domain.revision()),
        0
    );
    assert_eq!(reader.read_span(&mut pcm).unwrap().span.source_start, 6);
    assert_eq!(pcm[0], 4.0);
    assert_eq!(
        std::mem::size_of::<super::QueuedSpan>(),
        std::mem::size_of::<super::ReadSpan>() + std::mem::size_of::<u64>()
    );
    // Pending revision uses the existing shared allocation, preserving every
    // ordinary consumer/control redirect without boxing or unsafe aliasing.
    let original_consumer_fields = std::mem::size_of::<rtrb::Consumer<f32>>()
        + std::mem::size_of::<rtrb::Consumer<super::ReadSpan>>()
        + std::mem::size_of::<Option<super::ReadSpan>>()
        + std::mem::size_of::<std::sync::Arc<super::Statistics>>();
    assert_eq!(
        std::mem::size_of::<super::LinkAudioConsumer>(),
        original_consumer_fields
    );
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(std::mem::size_of::<super::QueuedSpan>(), 64);
        assert_eq!(std::mem::size_of::<super::LinkAudioConsumer>(), 112);
    }
    eprintln!(
        "queue_owner_bytes descriptor={} old_descriptor={} consumer={}",
        std::mem::size_of::<super::QueuedSpan>(),
        std::mem::size_of::<super::ReadSpan>(),
        std::mem::size_of::<super::LinkAudioConsumer>()
    );
}

#[test]
fn revocable_queue_cancels_partial_and_peeked_spans_without_rebasing_following_audio() {
    let (mut writer, mut reader, domain, source) = revocable_queue(16, 8);
    let first = super::AudioSpan {
        source_start: 100,
        ..super::AudioSpan::default()
    };
    assert_eq!(
        writer.write_revocable(first, &[0.25; 8], domain.revision()),
        0
    );
    assert_eq!(reader.read_span(&mut [0.0; 3]).unwrap().frames, 3);
    assert_eq!(
        reader
            .statistics
            .pending_revision
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(reader.peek_span().unwrap().span.source_start, 103);
    source.retire();
    assert!(reader.read_span(&mut [0.0; 3]).is_none());
    assert_eq!(
        reader
            .statistics
            .pending_revision
            .load(std::sync::atomic::Ordering::Relaxed),
        super::UNGUARDED
    );
    let next = super::AudioSpan {
        source_start: 108,
        ..first
    };
    assert_eq!(writer.write(next, &[0.5; 2]), 0);
    let mut pcm = [0.0; 2];
    assert_eq!(reader.read_span(&mut pcm).unwrap().span.source_start, 108);
    assert_eq!(pcm, [0.5; 2]);
}

#[test]
fn revocable_queue_requires_prepared_domain_and_qualified_revision() {
    let (mut writer, _) = LinkAudioQueue::new(16, 8).unwrap().into_endpoints();
    let span = super::AudioSpan::default();
    assert_eq!(writer.write_revocable(span, &[0.5], Some(0)), 1);
    let domain = crate::link::RetirementDomain::default();
    assert!(writer.bind_retirement_domain(&domain));
    for revision in [None, Some(u64::MAX)] {
        assert_eq!(writer.write_revocable(span, &[0.5], revision), 1);
    }
    assert_eq!(writer.dropped_samples(), 3);
}

thread_local! {
    static AFTER_COPY: std::cell::RefCell<Option<(usize, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}
pub(super) fn after_copy() {
    let hook = AFTER_COPY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let (remaining, _) = slot.as_mut()?;
        if *remaining != 0 {
            *remaining -= 1;
            return None;
        }
        slot.take().map(|(_, hook)| hook)
    });
    if let Some(hook) = hook {
        hook();
    }
}

#[test]
fn revocable_queue_rechecks_after_copy_and_cancels_a_mixed_unclocked_read() {
    for second_span in [false, true] {
        let (mut writer, mut reader, domain, source) = revocable_queue(16, 8);
        let span = super::AudioSpan::default();
        assert_eq!(
            writer.write_revocable(span, &[0.25; 2], domain.revision()),
            0
        );
        assert_eq!(
            writer.write_revocable(span, &[0.5; 2], domain.revision()),
            0
        );
        AFTER_COPY.with(|slot| {
            *slot.borrow_mut() = Some((usize::from(second_span), Box::new(move || source.retire())))
        });
        let mut pcm = [9.0; 4];
        if second_span {
            assert_eq!(reader.read(&mut pcm), 4);
            assert_eq!(pcm, [0.0; 4]);
        } else {
            assert!(reader.read_span(&mut pcm[..2]).is_none());
            assert_eq!(&pcm[..2], &[0.0; 2]);
            assert!(reader.peek_span().is_none());
        }
    }
}

#[test]
fn revocable_queue_bounds_stale_discard_to_the_fixed_span_snapshot() {
    let (mut writer, mut reader, domain, source) = revocable_queue(64, 8);
    for _ in 0..8 {
        assert_eq!(
            writer.write_revocable(super::AudioSpan::default(), &[0.5; 8], domain.revision()),
            0
        );
    }
    source.retire();
    assert!(reader.peek_span().is_none());
    assert_eq!(reader.consumer.slots(), 0);
    assert_eq!(writer.write(super::AudioSpan::default(), &[0.75; 64]), 0);
    assert_eq!(reader.read(&mut [0.0; 64]), 0);
}

#[test]
fn revocable_queue_zero_frames_are_never_published_or_returned_as_events() {
    for marked in [false, true] {
        let (mut writer, mut reader, domain, _) = revocable_queue(8, 2);
        let span = super::AudioSpan::default();
        let write = |writer: &mut super::LinkAudioProducer, pcm: &[f32]| {
            if marked {
                writer.write_revocable(span, pcm, domain.revision())
            } else {
                writer.write(span, pcm)
            }
        };
        assert_eq!(write(&mut writer, &[]), 0);
        assert_eq!(reader.spans.slots(), 0);
        assert_eq!(reader.consumer.slots(), 0);
        assert!(reader.peek_span().is_none());
        assert_eq!(write(&mut writer, &[0.25]), 0);
        assert!(reader.read_span(&mut []).is_none());
        assert_eq!(reader.peek_span().unwrap().frames, 1);
        assert_eq!(reader.read_span(&mut [0.0]).unwrap().frames, 1);
        assert!(reader.pending.is_none());
        assert_eq!(
            reader
                .statistics
                .pending_revision
                .load(std::sync::atomic::Ordering::Relaxed),
            super::UNGUARDED
        );
        assert!(reader.peek_span().is_none());
    }
}

#[test]
fn revocable_queue_cancelled_peek_never_substitutes_the_following_span() {
    for prefix in [0, 1] {
        let (mut writer, mut reader, domain, source) = revocable_queue(16, 4);
        let first = super::AudioSpan {
            born_ns: Some(0),
            ..super::AudioSpan::default()
        };
        let future = super::AudioSpan {
            source_start: 2,
            born_ns: Some(20_000_000),
            ..first
        };
        assert_eq!(
            writer.write_revocable(first, &[0.25; 2], domain.revision()),
            0
        );
        assert_eq!(writer.write(future, &[0.5; 2]), 0);
        if prefix != 0 {
            assert_eq!(reader.read_span(&mut [0.0]).unwrap().frames, prefix);
        }
        let pending = reader.peek_span().unwrap();
        assert_eq!(pending.span.source_start, prefix as u64);
        assert_eq!(pending.frames, 2 - prefix);
        source.retire();
        let mut output = [9.0; 2];
        assert!(
            reader.read_span(&mut output).is_none(),
            "a peek is not permission to consume a different source span"
        );
        assert_eq!(reader.peek_span().unwrap().span, future);
        assert_eq!(reader.read_span(&mut output).unwrap().span, future);
        assert_eq!(output, [0.5; 2]);
    }
}
