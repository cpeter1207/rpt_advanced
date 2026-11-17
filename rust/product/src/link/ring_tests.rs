use super::*;
use std::sync::OnceLock;

type PushCallback = unsafe extern "C" fn(*mut ffi::rpcr3_ring, *const f32, u64, *mut u64) -> i32;
static PUSH_CALLBACK: OnceLock<PushCallback> = OnceLock::new();
type MuteCallback =
    unsafe extern "C" fn(*mut ffi::rpcr3_ring, u64, *const MuteSpan, u32, *mut u32) -> i32;
static MUTE_CALLBACK: OnceLock<MuteCallback> = OnceLock::new();
thread_local! {
    static TIMED_RESULT_MODE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static PUSH_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PUSH_SCRIPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PUSH_CANCEL: std::cell::Cell<Option<(*const std::sync::atomic::AtomicU64, u64, usize)>> = const { std::cell::Cell::new(None) };
    static GAP_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

unsafe extern "C" fn malformed_timed_sample(
    _: *mut ffi::rpcr3_ring,
    sample: *mut f32,
    timing: *mut ffi::rpcr3_timed_sample_v1,
) -> i32 {
    // SAFETY: the consumer supplies complete writable stack records.
    let (sample, timing) = unsafe { (&mut *sample, &mut *timing) };
    *sample = 0.75;
    match TIMED_RESULT_MODE.get() {
        0 => return -3,
        1 => timing.struct_size = 8,
        2 => timing.reserved = 1,
        _ => timing.source_fraction_billionths = 1_000_000_000,
    }
    0
}

#[test]
fn timed_consumer_rejects_failed_or_malformed_provider_results() {
    for mode in 0..4 {
        TIMED_RESULT_MODE.set(mode);
        // SAFETY: the copied table and original callbacks remain live until
        // both endpoints drop at this iteration's end; no foreign state is used.
        let (mut producer, mut consumer) = unsafe {
            let mut api = *ffi::rpcr3_descriptor();
            api.ring_consumer_render_sample_timed = Some(malformed_timed_sample);
            InboundRing::from_descriptor(8000, InboundPolicy::Phone, Box::leak(Box::new(api)))
        }
        .unwrap();
        assert_eq!(producer.write(&[0.25; 160]), Ok(160));
        assert_eq!(consumer.render_timed_sample(), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, 160);
    }
}

#[test]
fn tagged_source_muting_follows_finite_resets_without_becoming_loss_or_plc() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
    for (start, interval_only, current_mute, expected_audible) in [
        (1000, false, false, false),
        (1640, true, false, false),
        (2280, false, false, true),
        (2920, false, true, false),
        (3560, false, false, true),
    ] {
        let spans = [MuteSpan {
            start,
            end: start + 640,
        }];
        let tagged = if interval_only || start >= 2280 {
            &[][..]
        } else {
            &spans[..]
        };
        assert_eq!(
            producer.write_tagged(&[0.5; 640], 7, start, tagged),
            Ok(640)
        );
        if interval_only {
            assert_eq!(producer.mute(7, &spans), Ok(1));
        }
        producer.signals().set_dtmf_muted(current_mute);
        let token = producer.finish().unwrap();
        let mut real = 0;
        let mut audible = false;
        for _ in 0..10 {
            let mut output = [0.0; 960];
            real += consumer.render(&mut output).unwrap();
            audible |= output.iter().any(|sample| sample.abs() > 0.1);
            if producer.finish_status(token) == Ok(FinishState::Drained) {
                break;
            }
        }
        assert_eq!(producer.finish_status(token), Ok(FinishState::Drained));
        assert!(real != 0);
        assert_eq!(audible, expected_audible, "source start {start}");
        assert_eq!(producer.observer().observe().unwrap().missing_samples, 0);
    }
}

#[test]
fn finite_echo_peer_drains_a_subtarget_burst_without_changing_ordinary_peer_policy() {
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, Mode},
    };
    let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
    assert_eq!(producer.write(&[0.25; 640]), Ok(640));
    let token = producer.finish().unwrap();
    assert_eq!(producer.finish_status(token), Ok(FinishState::Pending));
    assert_eq!(producer.write(&[0.5]), Ok(0));
    let (outbound, _) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        rpt_advanced_core::link::PeerIdentity::from_operator("200").unwrap(),
        Mode::TRANSCEIVE,
        consumer,
        outbound,
        960,
        0,
    )
    .unwrap();
    let (mut links, _) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, _control) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    // Codec PLC lookahead may produce no real PCM in a tiny callback, but the
    // retained finite tail still owns receive activity until it actually drains.
    links
        .process(
            &mut controller,
            false,
            &mut [0.0; 1],
            rpt_advanced_core::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(links.active_count(), 1);
    assert_eq!(producer.finish_status(token), Ok(FinishState::Pending));
    let mut heard = false;
    let mut output = [0.0; 960];
    for _ in 0..10 {
        output.fill(0.0);
        links
            .process(
                &mut controller,
                false,
                &mut output,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        heard |= output.iter().any(|sample| sample.abs() > 0.1);
        if producer.finish_status(token) == Ok(FinishState::Drained) {
            break;
        }
    }
    assert!(
        heard,
        "a finite Echo burst must bypass the ordinary 260ms startup gate"
    );
    assert_eq!(producer.finish_status(token), Ok(FinishState::Drained));
    output.fill(0.0);
    links
        .process(
            &mut controller,
            false,
            &mut output,
            rpt_advanced_core::link::SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(links.active_count(), 0);
    assert_eq!(producer.observer().observe().unwrap().missing_samples, 0);
}

#[test]
fn ordinary_ring_accepts_pre_finite_prefix_but_echo_rejects_it_before_creation() {
    unsafe {
        let original = *ffi::rpcr3_descriptor();
        let size = std::mem::offset_of!(ffi::rpcr3_descriptor, ring_producer_finish);
        let mut prefix = vec![0_usize; size / size_of::<usize>()];
        ptr::copy_nonoverlapping(
            (&original as *const ffi::rpcr3_descriptor).cast::<u8>(),
            prefix.as_mut_ptr().cast::<u8>(),
            size,
        );
        let pointer = prefix.as_mut_ptr().cast::<ffi::rpcr3_descriptor>();
        ptr::addr_of_mut!((*pointer).struct_size).write(size as u32);
        let (mut producer, _) =
            InboundRing::from_descriptor(8000, InboundPolicy::Peer, pointer).unwrap();
        assert_eq!(producer.finish_status(1), Err(RingError));
        assert_eq!(producer.write_tagged(&[0.25], 1, 0, &[]), Ok(1));
        assert!(InboundRing::from_descriptor(8000, InboundPolicy::Echo, pointer).is_err());
    }
}

#[test]
fn finite_prefix_remains_readable_without_the_optional_timed_tail() {
    // SAFETY: copied process-lifetime descriptor functions retain their original
    // ABI. Only the readable prefix declaration is reduced for each case.
    unsafe {
        let original = *ffi::rpcr3_descriptor();
        let timed = std::mem::offset_of!(ffi::rpcr3_descriptor, ring_consumer_render_sample_timed);
        for size in [timed, timed + size_of::<usize>() - 1] {
            let mut api = original;
            api.struct_size = size as u32;
            let pointer = Box::leak(Box::new(api));
            let checked = checked_descriptor(pointer).unwrap();
            assert!(
                checked.ring_producer_finish.is_some(),
                "complete finite prefix was discarded"
            );
            assert!(checked.ring_finish_status.is_some());
            assert!(checked.ring_consumer_render_sample_timed.is_none());
            assert!(InboundRing::from_descriptor(8000, InboundPolicy::Echo, pointer).is_ok());
        }
    }
}

#[test]
fn timed_consumer_matches_actual_pcm_counts_and_retains_long_source_coordinates() {
    for (generation, start) in [(0, 0), (7, u64::MAX - 1000)] {
        let (mut producer, mut timed) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
        let (mut reference_producer, mut reference) =
            InboundRing::open(8000, InboundPolicy::Echo).unwrap();
        assert!(timed.timing_available());
        assert_eq!(
            producer.write_tagged(&[0.25; 640], generation, start, &[]),
            Ok(640)
        );
        assert_eq!(
            reference_producer.write_tagged(&[0.25; 640], generation, start, &[]),
            Ok(640)
        );
        producer.finish().unwrap();
        reference_producer.finish().unwrap();
        let (mut positions, mut fractions, mut negative) = (0, false, false);
        for _ in 0..4608 {
            let actual = timed.render_timed_sample().unwrap();
            let mut pcm = [0.0];
            let real = reference.render(&mut pcm).unwrap();
            let provenance = actual.provenance.unwrap();
            assert_eq!(actual.sample.to_bits(), pcm[0].to_bits());
            assert_eq!(provenance.real, real != 0);
            if let Some(position) = provenance.position {
                positions += 1;
                assert_eq!(position.generation, generation);
                assert!(position.fraction_billionths < 1_000_000_000);
                fractions |= position.fraction_billionths != 0;
                negative |= position.negative;
                if start != 0 {
                    assert!(position.frame > u64::MAX - 1100);
                }
            }
        }
        assert!(positions > 3000);
        assert!(fractions);
        assert_eq!(negative, start == 0);
    }
}

#[test]
fn timed_consumer_does_not_consume_an_untimed_prefix_or_relabel_untagged_pcm() {
    // SAFETY: the complete copied finite prefix retains its original functions.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.struct_size =
            std::mem::offset_of!(ffi::rpcr3_descriptor, ring_consumer_render_sample_timed) as u32;
        let (mut producer, mut consumer) =
            InboundRing::from_descriptor(8000, InboundPolicy::Echo, Box::leak(Box::new(api)))
                .unwrap();
        assert_eq!(producer.write(&[0.25; 640]), Ok(640));
        producer.finish().unwrap();
        let before = consumer.observe().unwrap().available_samples;
        assert!(!consumer.timing_available());
        assert_eq!(consumer.render_timed_sample(), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, before);
        let mut processed = crate::services::ProcessedPeer::new(consumer, None);
        assert!(!processed.source_timing_available());
        assert_eq!(PeerInput::render_timed_sample(&mut processed), None);
        assert_eq!(processed.available(), before);
        assert!(processed.render(&mut [0.0; 960]).unwrap() > 0);
    }
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
    assert_eq!(producer.write(&[0.25; 640]), Ok(640));
    producer.finish().unwrap();
    for _ in 0..4000 {
        assert_eq!(
            consumer
                .render_timed_sample()
                .unwrap()
                .provenance
                .unwrap()
                .position,
            None
        );
    }
}

#[test]
fn phone_tagged_prefix_preserves_crossing_future_and_lookback_mutes() {
    for (start, end) in [(0, 320), (200, 320), (80, 240)] {
        let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
        let spans = [MuteSpan { start, end }];
        assert_eq!(producer.write_tagged(&[0.25; 320], 7, 0, &spans), Ok(160));
        assert_eq!((spans[0].start, spans[0].end), (start, end));
        assert_eq!(producer.write_tagged(&[0.25; 160], 7, 160, &spans), Ok(160));
        for packet in 2..8 {
            assert_eq!(
                producer.write_tagged(&[0.25; 160], 7, packet * 160, &[]),
                Ok(160)
            );
        }
        let (mut heard_muted, mut heard_clear) = (false, false);
        for _ in 0..3200 {
            let sample = consumer.render_timed_sample().unwrap();
            let timing = sample.provenance.unwrap();
            let Some(position) = timing.position.filter(|position| !position.negative) else {
                continue;
            };
            if position.frame >= start + 4 && position.frame + 4 < end {
                heard_muted = true;
                assert!(timing.muted);
                assert_eq!(sample.sample, 0.0);
            } else if position.frame > 32
                && (position.frame + 4 < start || position.frame > end + 4)
            {
                heard_clear = true;
                assert!(!timing.muted);
                assert!(sample.sample.abs() > 0.1);
            }
        }
        assert!((heard_muted, heard_clear) == (true, true));
    }
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
    for packet in 0..2 {
        assert_eq!(
            producer.write_tagged(&[0.25; 160], 7, packet * 160, &[]),
            Ok(160)
        );
    }
    let lookback = [MuteSpan { start: 0, end: 100 }];
    assert_eq!(
        producer.write_tagged(&[0.25; 320], 7, 320, &lookback),
        Ok(160)
    );
    for packet in 3..8 {
        assert_eq!(
            producer.write_tagged(&[0.25; 160], 7, packet * 160, &[]),
            Ok(160)
        );
    }
    let mut heard = false;
    for _ in 0..960 {
        let sample = consumer.render_timed_sample().unwrap();
        let timing = sample.provenance.unwrap();
        if timing
            .position
            .is_some_and(|position| !position.negative && (32..80).contains(&position.frame))
        {
            heard = true;
            assert!(timing.muted);
            assert_eq!(sample.sample, 0.0);
        }
    }
    assert!(heard);
}

#[test]
fn phone_tagged_prefix_validates_the_original_offer_before_clipping() {
    for (source_start, start, end, count) in [
        (0, 200, 190, 1),
        (0, 0, 321, 1),
        (3000, 0, 100, 1),
        (u64::MAX - 200, 0, 0, 0),
        (0, 200, 320, 65),
    ] {
        let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
        let spans = vec![MuteSpan { start, end }; count];
        assert_eq!(
            producer.write_tagged(&[0.25; 320], 7, source_start, &spans),
            Err(RingError)
        );
        assert_eq!(consumer.observe().unwrap().available_samples, 0);
    }
}

#[test]
fn phone_tagged_prefix_retries_after_zero_credit_without_advancing_source() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
    for packet in 0..15 {
        assert_eq!(
            producer.write_tagged(&[0.25; 160], 7, packet * 160, &[]),
            Ok(160)
        );
    }
    let spans = [MuteSpan {
        start: 2400,
        end: 2720,
    }];
    assert_eq!(producer.write_tagged(&[0.25; 320], 7, 2400, &spans), Ok(0));
    assert_eq!(consumer.observe().unwrap().available_samples, 2400);
    assert!(consumer.render(&mut [0.0; 4096]).unwrap() > 0);
    assert_eq!(
        producer.write_tagged(&[0.25; 320], 7, 2400, &spans),
        Ok(160)
    );
    assert_eq!(
        producer.write_tagged(&[0.25; 160], 7, 2560, &spans),
        Ok(160)
    );
    let mut heard = false;
    for _ in 0..14_000 {
        let sample = consumer.render_timed_sample().unwrap();
        let timing = sample.provenance.unwrap();
        if timing
            .position
            .is_some_and(|position| !position.negative && (2404..2716).contains(&position.frame))
        {
            heard = true;
            assert!(timing.muted);
            assert_eq!(sample.sample, 0.0);
        }
    }
    assert!(heard);
}

#[test]
fn timed_phone_prefix_gap_mute_and_reset_keep_actual_source_provenance() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
    assert_eq!(producer.write_tagged(&[0.25; 320], 7, 0, &[]), Ok(160));
    assert_eq!(producer.write_tagged(&[0.25; 160], 7, 160, &[]), Ok(160));
    let mute = [MuteSpan {
        start: 400,
        end: 420,
    }];
    for start in [320, 480] {
        let spans = if start == 320 { &mute[..] } else { &[] };
        assert_eq!(
            producer.write_tagged(&[0.25; 160], 7, start, spans),
            Ok(160)
        );
    }
    assert_eq!(producer.gap(8), Ok(8));
    for packet in 0..6 {
        assert_eq!(
            producer.write_tagged(&[0.5; 160], 7, 648 + packet * 160, &[]),
            Ok(160)
        );
    }
    let (mut gap, mut plc, mut muted) = (false, false, false);
    for _ in 0..10_000 {
        let output = consumer.render_timed_sample().unwrap();
        let source = output.provenance.unwrap();
        if source.gap {
            gap = true;
            assert!(source.position.is_some());
            plc |= source.plc && !source.real;
        }
        if source.muted {
            muted = true;
            assert_eq!(output.sample, 0.0);
        }
        if gap && plc && muted {
            break;
        }
    }
    assert!((gap, plc, muted) == (true, true, true));
    producer.signals().set_dtmf_muted(true);
    let output = consumer.render_timed_sample().unwrap();
    assert_eq!(output.sample, 0.0);
    assert!(output.provenance.unwrap().muted);
    producer.signals().set_dtmf_muted(false);
    consumer.reset().unwrap();
    assert_eq!(
        consumer
            .render_timed_sample()
            .unwrap()
            .provenance
            .unwrap()
            .position,
        None
    );
    for packet in 0..8 {
        assert_eq!(
            producer.write_tagged(&[0.75; 160], 8, 1000 + packet * 160, &[]),
            Ok(160)
        );
    }
    let mut mapped = 0;
    for _ in 0..960 {
        if let Some(position) = consumer
            .render_timed_sample()
            .unwrap()
            .provenance
            .unwrap()
            .position
        {
            mapped += 1;
            assert_eq!(position.generation, 8);
        }
    }
    assert!(mapped > 0);
}

#[test]
fn phone_ring_prepares_one_preroll_owner_and_bounded_source_prefixes() {
    for rate in [0, 7999, 48001] {
        assert!(InboundRing::open(rate, InboundPolicy::Phone).is_err());
    }
    for rate in [8000, 16000, 48000] {
        let (mut producer, consumer) = InboundRing::open(rate, InboundPolicy::Phone).unwrap();
        let observation = consumer.observe().unwrap();
        assert_eq!(observation.reserve_samples, u64::from(rate) * 160 / 1000);
        assert_eq!(observation.target_samples, observation.reserve_samples);
        assert_eq!(observation.capacity_samples, u64::from(rate) * 300 / 1000);
        let packet = rate as usize / 50;
        let input = vec![0.25; packet + 1];
        assert_eq!(producer.write(&input), Ok(packet));
        assert_eq!(consumer.observe().unwrap().available_samples, packet as u64);
        assert_eq!(producer.write(&input[packet..]), Ok(1));
        assert_eq!(producer.gap(packet as u64 + 1), Ok(packet as u64));
        assert_eq!(
            consumer.observe().unwrap().available_samples,
            (packet * 2 + 1) as u64
        );
    }
}

#[test]
fn phone_policy_survives_processing_into_the_common_link_audio_gate() {
    use crate::services::ProcessedPeer;
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, Mode, SourcePosition},
    };
    let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Phone).unwrap();
    for _ in 0..8 {
        assert_eq!(producer.write(&[0.25; 160]), Ok(160));
    }
    let (outbound, _) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        rpt_advanced_core::link::PeerIdentity::from_operator("200").unwrap(),
        Mode::TRANSCEIVE,
        ProcessedPeer::new(consumer, None),
        outbound,
        960,
        0,
    )
    .unwrap();
    let (mut links, _) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut output = [0.0; 960];
    links
        .process(
            &mut controller,
            false,
            &mut output,
            SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(
        links.active_count(),
        1,
        "phone reserve must not wait for ordinary 260ms priming"
    );
    assert!(output.iter().any(|sample| sample.abs() > 0.1));
}

#[test]
fn peer_preparation_reserves_one_maximum_callback_and_producer_write() {
    for (rate, reserve, target, capacity) in [(8000, 685, 2080, 6176), (48000, 4102, 12480, 16576)]
    {
        let (mut producer, mut consumer) = InboundRing::open(rate, InboundPolicy::Peer).unwrap();
        let observation = consumer.observe().unwrap();
        assert_eq!(observation.reserve_samples, reserve);
        assert_eq!(observation.target_samples, target);
        assert_eq!(observation.capacity_samples, capacity);
        assert_eq!(producer.write(&[0.25; 4096]), Ok(4096));
        assert_eq!(producer.write(&[0.5; 4097]), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, 4096);
        let mut output = [0.5; 4097];
        assert_eq!(consumer.render(&mut output), Err(RingError));
        assert_eq!(consumer.observe().unwrap().available_samples, 4096);
        assert_eq!(output, [0.0; 4097]);
    }
}

#[test]
fn peer_gap_accepts_only_the_available_source_prefix() {
    let (mut producer, mut consumer) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    for _ in 0..4 {
        assert_eq!(producer.write(&[0.25; 4096]), Ok(4096));
    }
    assert_eq!(producer.gap(4096), Ok(192));
    assert_eq!(consumer.observe().unwrap().available_samples, 16576);
    assert_eq!(producer.gap(1), Ok(0));
    assert_eq!(producer.gap(4097), Err(RingError));
    let mut output = [0.0; 4096];
    assert_eq!(consumer.render(&mut output), Ok(3916));
    assert_eq!(producer.gap(3904), Ok(3904));
    assert_eq!(producer.write(&[0.5; 64]), Ok(64));
}

#[test]
fn local_shortfall_is_silence_without_concealment_or_plc_delay() {
    let (mut producer, mut consumer) = InboundRing::open(
        48000,
        InboundPolicy::Local {
            squelch_delay_ms: 0,
        },
    )
    .unwrap();
    assert_eq!(producer.write(&[0.25; 4096]), Ok(4096));
    let mut output = [0.0; 4096];
    assert_eq!(consumer.render(&mut output), Ok(4096));
    // Intrinsic converter history starts at zero, independently of PLC.
    assert!(output[0].abs() < 0.001);
    assert!(
        output[3584..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.001)
    );
    assert_eq!(consumer.render(&mut output), Ok(0));
    assert_eq!(output, [0.0; 4096]);
}

#[test]
fn finite_media_ring_accepts_source_rates_above_native_without_latency_target_or_plc() {
    let (mut producer, mut consumer) = InboundRing::open(96000, InboundPolicy::Media).unwrap();
    let observation = consumer.observe().unwrap();
    assert_eq!(observation.reserve_samples, 0);
    assert_eq!(observation.target_samples, 0);
    assert_eq!(observation.capacity_samples, 8192);
    assert_eq!(producer.write(&[0.25; 2048]), Ok(2048));

    let mut rendered = Vec::new();
    for _ in 0..2048 {
        if let Some(sample) = consumer.render_sample().unwrap() {
            rendered.push(sample);
        }
    }
    assert!(rendered.len() >= 900);
    assert!(
        rendered[256..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.01)
    );
}

#[test]
fn peer_playout_has_plc_lookahead_without_counting_it_as_loss() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    assert_eq!(producer.write(&[0.25; 2048]), Ok(2048));
    let mut output = [0.0; 960];
    assert_eq!(consumer.render(&mut output), Ok(780));
    assert_eq!(output[..180], [0.0; 180]);
    // Converter FIR startup is additional to the 180-sample PLC lookahead.
    // Continue real input long enough to observe settled audio.
    assert_eq!(consumer.render(&mut output), Ok(960));
    assert_eq!(consumer.render(&mut output), Ok(960));
    assert!(
        output[832..]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.001)
    );
    assert_eq!(consumer.observe().unwrap().missing_samples, 0);
}

#[test]
fn peer_tail_drains_before_courtesy_instead_of_replaying_on_next_burst() {
    use rpt_advanced_core::{
        audio::{LinkAudioQueue, PcmRead, PcmStreamReader},
        controller::{ControllerSettings, CourtesySettings, NodeController, PreparedMedia},
        link::{AudioPeer, LinkAudio, Mode},
    };

    struct Courtesy;
    impl PcmStreamReader for Courtesy {
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            output.fill(0.75);
            PcmRead::FinalSamples(output.len())
        }
    }
    let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    let (outbound, _) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        rpt_advanced_core::link::PeerIdentity::from_operator("200").unwrap(),
        Mode::TRANSCEIVE,
        consumer,
        outbound,
        960,
        0,
    )
    .unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let courtesy = CourtesySettings {
        link: Some(PreparedMedia::new_stream(Box::new(Courtesy)).unwrap()),
        ..CourtesySettings::default()
    };
    let (mut controller, _control) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            courtesy_delay_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        courtesy,
    )
    .unwrap();
    let mut output = [0.0; 960];
    for _ in 0..100 {
        assert_eq!(producer.write(&[0.25; 160]), Ok(160));
        output.fill(0.0);
        links
            .process(
                &mut controller,
                false,
                &mut output,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        dispatcher.dispatch(1);
    }
    let mut tail_samples = 0;
    let mut courtesy_started = false;
    for _ in 0..25 {
        output.fill(0.0);
        links
            .process(
                &mut controller,
                false,
                &mut output,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        dispatcher.dispatch(1);
        for &sample in &output {
            if sample > 0.6 {
                assert!(
                    tail_samples > 8_000,
                    "courtesy cut off buffered speech: {tail_samples} samples"
                );
                courtesy_started = true;
            } else if sample > 0.2 {
                assert!(!courtesy_started, "old speech followed the courtesy tone");
                tail_samples += 1;
            }
        }
    }
    assert!(
        courtesy_started,
        "concealment must not hold receive forever"
    );
    assert_eq!(links.active_count(), 0);
    assert_eq!(producer.write(&[-0.25; 160]), Ok(160));
    output.fill(0.0);
    links
        .process(
            &mut controller,
            false,
            &mut output,
            rpt_advanced_core::link::SourcePosition::default(),
        )
        .unwrap();
    // Allow the bounded FIR/PLC recovery transition, not a saved speech tail.
    assert!(output.iter().filter(|&&sample| sample > 0.2).count() < 480);
    dispatcher.dispatch(1);
    for _ in 0..4 {
        assert_eq!(producer.write(&[-0.25; 160]), Ok(160));
        output.fill(0.0);
        links
            .process(
                &mut controller,
                false,
                &mut output,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        dispatcher.dispatch(1);
        assert!(output.iter().all(|&sample| sample < 0.2));
    }
    assert!(output.iter().sum::<f32>() / 960.0 < -0.2);
    assert_eq!(producer.observer().observe().unwrap().discarded_samples, 0);
}

unsafe extern "C" fn failed_create(
    _: *const ffi::rpcr3_config,
    _: *mut *mut ffi::rpcr3_ring,
) -> i32 {
    -1
}
unsafe extern "C" fn empty_create(
    _: *const ffi::rpcr3_config,
    _: *mut *mut ffi::rpcr3_ring,
) -> i32 {
    0
}
unsafe extern "C" fn failed_push(
    _: *mut ffi::rpcr3_ring,
    _: *const f32,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_gap(_: *mut ffi::rpcr3_ring, _: u64, _: *mut u64) -> i32 {
    -1
}
unsafe extern "C" fn rejected_tagged(
    _: *mut ffi::rpcr3_ring,
    _: *const f32,
    _: u64,
    _: u64,
    _: u64,
    _: *const ffi::rpcr3_mute_span,
    _: u32,
    accepted: *mut u64,
) -> i32 {
    unsafe {
        *accepted = 0;
    }
    if PUSH_SCRIPT.with(|mode| mode.get()) == 0 {
        -1
    } else {
        0
    }
}
unsafe extern "C" fn rejected_finish(_: *mut ffi::rpcr3_ring, token: *mut u64) -> i32 {
    unsafe {
        *token = 0;
    }
    if PUSH_SCRIPT.with(|mode| mode.get()) == 0 {
        -1
    } else {
        0
    }
}
unsafe extern "C" fn scripted_finish_status(
    _: *const ffi::rpcr3_ring,
    _: u64,
    status: *mut u32,
) -> i32 {
    let mode = PUSH_SCRIPT.with(|mode| mode.get()) as u32;
    unsafe {
        *status = mode;
    }
    if mode == 4 { -1 } else { 0 }
}
unsafe extern "C" fn overreported_push(
    _: *mut ffi::rpcr3_ring,
    _: *const f32,
    count: u64,
    accepted: *mut u64,
) -> i32 {
    unsafe { *accepted = count + 1 };
    0
}
unsafe extern "C" fn overreported_gap(
    _: *mut ffi::rpcr3_ring,
    count: u64,
    accepted: *mut u64,
) -> i32 {
    unsafe { *accepted = count + 1 };
    0
}
unsafe extern "C" fn pending_overreported_gap(
    _: *mut ffi::rpcr3_ring,
    count: u64,
    accepted: *mut u64,
) -> i32 {
    let call = GAP_CALLS.with(|calls| calls.replace(calls.get() + 1));
    unsafe { *accepted = if call == 0 { 0 } else { count + 1 } };
    0
}
unsafe extern "C" fn failed_render(
    _: *mut ffi::rpcr3_ring,
    _: *mut f32,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_render_sample(
    _: *mut ffi::rpcr3_ring,
    _: *mut f32,
    _: *mut bool,
) -> i32 {
    -1
}

static SAMPLE_THEN_FAIL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe extern "C" fn sample_then_fail_render(
    _: *mut ffi::rpcr3_ring,
    sample: *mut f32,
    real: *mut bool,
) -> i32 {
    if SAMPLE_THEN_FAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
        unsafe {
            *sample = 0.25;
            *real = true;
        }
        0
    } else {
        -1
    }
}
unsafe extern "C" fn failed_observe(
    _: *const ffi::rpcr3_ring,
    _: *mut ffi::rpcr3_observation,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_output_delay(_: *const ffi::rpcr3_ring, _: *mut u64) -> i32 {
    -1
}
unsafe extern "C" fn failed_reset(_: *mut ffi::rpcr3_ring) -> i32 {
    -1
}
unsafe extern "C" fn scripted_push(
    ring: *mut ffi::rpcr3_ring,
    samples: *const f32,
    count: u64,
    accepted: *mut u64,
) -> i32 {
    let call = PUSH_CALLS.with(|calls| {
        let call = calls.get();
        calls.set(call + 1);
        call
    });
    let mode = PUSH_SCRIPT.with(std::cell::Cell::get);
    let code = match (mode, call) {
        (1, 0) => {
            unsafe { *accepted = 0 };
            0
        }
        (2, 0) => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count / 2, accepted) },
        (3, 1) => -1,
        (5, 1) => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count / 2, accepted) },
        _ => unsafe { PUSH_CALLBACK.get().unwrap()(ring, samples, count, accepted) },
    };
    PUSH_CANCEL.with(|cancel| {
        if let Some((target, generation, target_call)) = cancel.get() {
            if target_call == call {
                // SAFETY: the test installs this pointer for a synchronous run_job call and clears it here.
                unsafe { (*target).store(generation, std::sync::atomic::Ordering::Release) };
                cancel.set(None);
            }
        }
    });
    code
}

pub(crate) fn cancel_on_push_open(
    _: u32,
    _: InboundPolicy,
) -> Result<(InboundProducer, InboundConsumer), RingError> {
    scripted_cancel_on_push_endpoints()
}

pub(crate) fn set_cancel_on_push(
    cancelled: &std::sync::atomic::AtomicU64,
    generation: u64,
    call: usize,
    partial: bool,
) {
    let mode = if partial { 5 } else { 4 };
    PUSH_CALLS.with(|calls| calls.set(0));
    PUSH_SCRIPT.with(|script| script.set(mode));
    PUSH_CANCEL.with(|cancel| cancel.set(Some((cancelled, generation, call))));
}

pub(crate) fn set_cancel_on_failed_push(
    cancelled: &std::sync::atomic::AtomicU64,
    generation: u64,
    call: usize,
) {
    PUSH_CALLS.with(|calls| calls.set(0));
    PUSH_SCRIPT.with(|script| script.set(3));
    PUSH_CANCEL.with(|cancel| cancel.set(Some((cancelled, generation, call))));
}

fn scripted_cancel_on_push_endpoints() -> Result<(InboundProducer, InboundConsumer), RingError> {
    // SAFETY: install a copied immutable descriptor; the thread-local cancellation target remains
    // valid for the synchronous producer callback in the owning worker test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        let _ = PUSH_CALLBACK.set(api.ring_producer_push.unwrap());
        api.ring_producer_push = Some(scripted_push);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
    }
}

#[test]
fn malformed_ring_tables_and_failed_operations_never_publish_or_retain_invalid_audio() {
    // SAFETY: real immutable released descriptor; modified copies are retained for process lifetime.
    unsafe {
        assert!(InboundRing::from_descriptor(8000, InboundPolicy::Peer, ptr::null()).is_err());
        for rate in [0, 48001] {
            assert!(InboundRing::open(rate, InboundPolicy::Peer).is_err());
        }
        for rate in [0, 7999, 192001] {
            assert!(InboundRing::open(rate, InboundPolicy::Media).is_err());
        }
        for squelch_delay_ms in [301, u64::MAX] {
            assert!(InboundRing::open(48000, InboundPolicy::Local { squelch_delay_ms }).is_err());
        }
        let original = *ffi::rpcr3_descriptor();
        for case in 0..20 {
            let mut api = original;
            match case {
                0 => api.struct_size = 8,
                1 => api.abi_version = 2,
                2 => api.capability_name = ptr::null(),
                3 => api.capability_name = c"other".as_ptr(),
                4 => api.ring_create = None,
                5 => api.ring_destroy = None,
                6 => api.ring_producer_push = None,
                7 => api.ring_consumer_render = None,
                8 => api.ring_observe = None,
                9 => api.ring_create = Some(failed_create),
                10 => api.ring_create = Some(empty_create),
                11 => api.ring_consumer_reset = None,
                12 => api.ring_consumer_render_sample = None,
                13 => api.ring_output_delay = None,
                14 => api.ring_producer_gap = None,
                15 => api.ring_finish_status = None,
                16 => api.ring_producer_push_tagged = None,
                17 => api.ring_producer_mute = None,
                18 => api.ring_producer_push_tagged = None,
                _ => api.ring_producer_mute = None,
            }
            let policy = if case < 15 {
                InboundPolicy::Peer
            } else if case < 18 {
                InboundPolicy::Echo
            } else {
                InboundPolicy::Local {
                    squelch_delay_ms: 160,
                }
            };
            assert!(InboundRing::from_descriptor(8000, policy, Box::leak(Box::new(api))).is_err());
        }
        let mut api = original;
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        api.ring_output_delay = Some(failed_output_delay);
        api.ring_consumer_reset = Some(failed_reset);
        let (mut producer, mut consumer) =
            InboundRing::from_descriptor(8000, InboundPolicy::Peer, Box::leak(Box::new(api)))
                .unwrap();
        assert!(producer.write(&[0.5]).is_err());
        let mut samples = [0.5; 8];
        assert!(consumer.render(&mut samples).is_err());
        assert_eq!(samples, [0.0; 8]);
        assert!(consumer.observe().is_err());
        assert_eq!(PeerInput::available(&consumer), 0);
        assert!(PeerInput::render(&mut consumer, &mut samples).is_none());
        assert_eq!(PeerInput::source_rate(&consumer), 8000);
        assert!(std::ptr::eq(
            PeerInput::signals(&consumer),
            producer.signals()
        ));
        assert!(producer.observer().observe().is_err());
        assert_eq!(producer.output_delay_samples(), Err(RingError));
        assert_eq!(consumer.reset(), Err(RingError));
        let (mut producer, consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
        assert_eq!(producer.write(&[]), Ok(0));
        assert_eq!(producer.finish(), Err(RingError));
        assert_eq!(PeerInput::available(&consumer), 0);
    }
}

#[test]
fn finite_and_tagged_ring_failures_do_not_publish_success_or_played_audio() {
    // SAFETY: copied descriptors retain the real provider allocation lifecycle;
    // only the returned operation status/metadata is controlled in this test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_producer_push_tagged = Some(rejected_tagged);
        api.ring_producer_finish = Some(rejected_finish);
        api.ring_finish_status = Some(scripted_finish_status);
        let (mut producer, _) =
            InboundRing::from_descriptor(8000, InboundPolicy::Echo, Box::leak(Box::new(api)))
                .unwrap();
        for (mode, expected) in [(0, Err(RingError)), (1, Ok(0))] {
            PUSH_SCRIPT.with(|script| script.set(mode));
            assert_eq!(producer.write_tagged(&[0.25], 1, 0, &[]), expected);
            assert_eq!(producer.finish(), Err(RingError));
        }
        for (mode, expected) in [
            (0, Ok(FinishState::Pending)),
            (1, Ok(FinishState::Drained)),
            (2, Ok(FinishState::Cancelled)),
            (3, Ok(FinishState::Failed)),
            (4, Err(RingError)),
            (5, Err(RingError)),
        ] {
            PUSH_SCRIPT.with(|script| script.set(mode));
            assert_eq!(producer.finish_status(1), expected);
        }
    }
}

pub(crate) fn failed_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: retain a copied released descriptor and its real create/destroy pair.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn failed_finish_status_endpoints() -> (InboundProducer, InboundConsumer) {
    // Retain real allocation, publication and finite tokens; fail only status.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        PUSH_SCRIPT.with(|mode| mode.set(3));
        api.ring_finish_status = Some(scripted_finish_status);
        InboundRing::from_descriptor(8000, InboundPolicy::Echo, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn invalid_ingress_endpoints(
    gap: bool,
    overreported: bool,
) -> (InboundProducer, InboundConsumer) {
    // SAFETY: the copied table retains its real create/destroy pair and lives for the test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        if gap {
            api.ring_producer_gap = Some(if overreported {
                overreported_gap
            } else {
                failed_gap
            });
        } else {
            api.ring_producer_push = Some(if overreported {
                overreported_push
            } else {
                failed_push
            });
        }
        InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn pending_overreported_gap_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: the copied table retains its real allocation lifecycle for this test.
    unsafe {
        GAP_CALLS.with(|calls| calls.set(0));
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_producer_gap = Some(pending_overreported_gap);
        InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn delayed_mute_endpoints(fail: bool) -> (InboundProducer, InboundConsumer) {
    unsafe extern "C" fn mute(
        ring: *mut ffi::rpcr3_ring,
        generation: u64,
        spans: *const MuteSpan,
        count: u32,
        accepted: *mut u32,
    ) -> i32 {
        unsafe { *accepted = 0 };
        let call = PUSH_CALLS.with(|calls| calls.replace(calls.get() + 1));
        if PUSH_SCRIPT.with(|mode| mode.get()) == 7 {
            -1
        } else if call < 2 {
            0
        } else {
            unsafe { MUTE_CALLBACK.get().unwrap()(ring, generation, spans, count, accepted) }
        }
    }
    // SAFETY: retain real ring ownership; only metadata credit is delayed.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        let _ = MUTE_CALLBACK.set(api.ring_producer_mute.unwrap());
        PUSH_CALLS.with(|calls| calls.set(0));
        PUSH_SCRIPT.with(|mode| mode.set(if fail { 7 } else { 6 }));
        api.ring_producer_mute = Some(mute);
        InboundRing::from_descriptor(8000, InboundPolicy::Echo, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn failed_output_delay_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: retain a copied descriptor and its real allocation lifecycle for this test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_output_delay = Some(failed_output_delay);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn failed_media_push_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: the copied descriptor retains its released create/destroy pair for this test.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_producer_push = Some(failed_push);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api))).unwrap()
    }
}

fn scripted_push_endpoints(mode: usize) -> (InboundProducer, InboundConsumer) {
    // SAFETY: copy the process-lifetime provider table and delegate successful writes to it.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        let _ = PUSH_CALLBACK.set(api.ring_producer_push.unwrap());
        PUSH_CALLS.with(|calls| calls.set(0));
        PUSH_SCRIPT.with(|script| script.set(mode));
        api.ring_producer_push = Some(scripted_push);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api))).unwrap()
    }
}

pub(crate) fn zero_then_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(1)
}

pub(crate) fn partial_then_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(2)
}

pub(crate) fn fail_after_initial_push_endpoints() -> (InboundProducer, InboundConsumer) {
    scripted_push_endpoints(3)
}

pub(crate) fn sample_then_fail_consumer() -> InboundConsumer {
    // SAFETY: retain the descriptor and use its real allocation lifecycle around the test callback.
    unsafe {
        SAMPLE_THEN_FAIL.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(sample_then_fail_render);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
            .unwrap()
            .1
    }
}

pub(crate) fn failed_sample_consumer() -> InboundConsumer {
    // SAFETY: retain a copied released descriptor and its real create/destroy pair.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(failed_render_sample);
        InboundRing::from_descriptor(48000, InboundPolicy::Media, Box::leak(Box::new(api)))
            .unwrap()
            .1
    }
}

#[test]
fn released_ring_endpoints_publish_render_and_report_their_generation() {
    let (mut producer, mut consumer) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    let observer = producer.observer();
    let same = producer.observer();
    let (other, _) = InboundRing::open(8000, InboundPolicy::Peer).unwrap();
    assert!(observer.same_generation(&same));
    assert!(!observer.same_generation(&other.observer()));
    assert_eq!(observer.rate(), 8000);
    assert!(matches!(observer.policy(), InboundPolicy::Peer));
    assert_eq!(producer.rate(), 8000);
    assert!(std::ptr::eq(observer.signals(), producer.signals()));

    assert_eq!(producer.write(&[0.25; 160]), Ok(160));
    assert!(producer.output_delay_samples().is_ok());
    let mut output = [0.0; 960];
    assert!(consumer.render(&mut output).is_ok());
    assert_eq!(consumer.reset(), Ok(()));
    assert_eq!(observer.observe().unwrap().available_samples, 0);
}

#[test]
fn sample_render_reports_shared_ring_failure() {
    // SAFETY: the copied descriptor and its callback remain alive through the endpoint drop.
    unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(failed_render_sample);
        let (_, mut consumer) =
            InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api)))
                .unwrap();
        assert!(consumer.render_sample().is_err());
    }
}

#[test]
fn peer_input_single_sample_maps_real_pcm_and_provider_error() {
    let mut consumer = sample_then_fail_consumer();
    assert_eq!(PeerInput::render_sample(&mut consumer), Some(0.25));
    assert_eq!(PeerInput::render_sample(&mut consumer), None);
    unsafe extern "C" fn rendered(
        _: *mut ffi::rpcr3_ring,
        sample: *mut f32,
        real: *mut bool,
    ) -> i32 {
        unsafe {
            *sample = 0.25;
            *real = true;
        }
        0
    }
    // SAFETY: retain the released create/destroy pair around one successful render.
    let (producer, mut consumer) = unsafe {
        let mut api = *ffi::rpcr3_descriptor();
        api.ring_consumer_render_sample = Some(rendered);
        InboundRing::from_descriptor(48000, InboundPolicy::Peer, Box::leak(Box::new(api))).unwrap()
    };
    producer.signals().set_dtmf_muted(true);
    assert_eq!(PeerInput::render_sample(&mut consumer), Some(0.0));
}

/// Borrow a retained test descriptor around the real local ring lifecycle.
///
/// # Safety
/// The descriptor and callbacks remain valid until both returned endpoints drop.
pub(crate) unsafe fn local_with_descriptor(
    api: *const ffi::rpcr3_descriptor,
    delay: u64,
) -> (InboundProducer, InboundConsumer) {
    // SAFETY: the test caller supplies the retained descriptor promised above.
    unsafe {
        InboundRing::from_descriptor(
            48000,
            InboundPolicy::Local {
                squelch_delay_ms: delay,
            },
            api,
        )
    }
    .unwrap()
}
