use super::*;
use rpt_advanced_core::link::{
    PeerInput as _, PeerReply, PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource,
    SampleProvenance, SourceCoordinate, SourceTimeBound, TimedPeerSample,
};
use std::sync::atomic::{AtomicU32, AtomicU64};

pub(crate) struct State {
    prepared: u64,
    delay: AtomicU64,
    accepted: AtomicU64,
    delivered: AtomicU64,
    flags: AtomicU32,
    fault: AtomicU32,
    previous: AtomicU32,
    calls: AtomicU64,
    queries: AtomicU64,
}

unsafe extern "C" fn process(context: *mut c_void, pcm: *mut f32, count: usize) -> i32 {
    let state = unsafe { &*context.cast::<State>() };
    state.calls.fetch_add(1, Ordering::Relaxed);
    state.accepted.fetch_add(count as u64, Ordering::Relaxed);
    state.delivered.fetch_add(count as u64, Ordering::Relaxed);
    for sample in unsafe { std::slice::from_raw_parts_mut(pcm, count) } {
        if state.delay.load(Ordering::Relaxed) == 1 {
            *sample = f32::from_bits(state.previous.swap(sample.to_bits(), Ordering::Relaxed));
        }
    }
    if state.fault.load(Ordering::Relaxed) == 7 {
        -1
    } else {
        0
    }
}
unsafe extern "C" fn query(context: *mut c_void, output: *mut abi::UrpProgrammeBuffering) -> i32 {
    let state = unsafe { &*context.cast::<State>() };
    state.queries.fetch_add(1, Ordering::Relaxed);
    let output = unsafe { &mut *output };
    output.flags = state.flags.load(Ordering::Relaxed);
    output.prepared_max_delay_frames = state.prepared;
    output.current_max_delay_frames = state.delay.load(Ordering::Relaxed);
    output.accepted_frames = state.accepted.load(Ordering::Relaxed);
    output.delivered_frames = state.delivered.load(Ordering::Relaxed);
    match state.fault.load(Ordering::Relaxed) {
        1 => return -1,
        2 => output.struct_size = 63,
        3 => output.abi_version = 2,
        4 => output.reserved = 1,
        5 => output.accepted_frames -= 1,
        6 => output.zero_prefix_frames = 1,
        8 => output.prepared_max_delay_frames += 1,
        9 => output.delivered_frames = output.accepted_frames + 1,
        10 => output.zero_suffix_frames = 1,
        11 => output.output_fifo_frames = output.current_max_delay_frames + 1,
        12 => output.current_max_delay_frames = output.prepared_max_delay_frames + 1,
        13 => output.delivered_frames -= 2,
        _ => {}
    }
    0
}
unsafe extern "C" fn destroy(_: *mut c_void) {}

struct Source {
    signals: PeerSignals,
    reply: Option<PeerReply>,
    next: u64,
    generation: u64,
    epoch: u64,
    known: bool,
    plc: bool,
    fault_at: Option<(u64, u8)>,
}
impl Source {
    fn new(phone: bool) -> Self {
        let source = PhoneSource::new(
            PhoneAccountId::new("provider").unwrap(),
            7,
            PhoneCallId::new([1; 16]).unwrap(),
            9,
            13,
        )
        .unwrap();
        let signals = PeerSignals::for_phone(source);
        let reply = phone.then(|| signals.reply_target());
        Self {
            signals,
            reply,
            next: 0,
            generation: 9,
            epoch: 7,
            known: true,
            plc: false,
            fault_at: None,
        }
    }
}
impl rpt_advanced_core::link::PeerInput for Source {
    fn phone_reply(&self) -> Option<&PeerReply> {
        self.reply.as_ref()
    }
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn source_timing_available(&self) -> bool {
        true
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        100_000
    }
    fn render(&mut self, samples: &mut [f32]) -> Option<usize> {
        self.next += samples.len() as u64;
        samples.fill(0.25);
        Some(samples.len())
    }
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        self.next += 1;
        let fault = self
            .fault_at
            .filter(|(at, _)| *at == self.next)
            .map(|(_, kind)| kind);
        Some(TimedPeerSample {
            sample: self.next as f32 * 0.25,
            age_bound: (self.known && fault != Some(0)).then_some(SourceTimeBound {
                host_clock_epoch: self.epoch + u64::from(fault == Some(1)),
                not_after_ns: self.next * 1_000_000,
            }),
            provenance: Some(SampleProvenance {
                position: Some(SourceCoordinate {
                    generation: self.generation + u64::from(fault == Some(2)),
                    frame: self.next,
                    fraction_billionths: 0,
                    negative: false,
                }),
                real: fault != Some(4),
                plc: self.plc || matches!(fault, Some(3 | 4)),
                ..SampleProvenance::default()
            }),
        })
    }
}
/// Qualified serial processor fixture shared with the actual prepared phone path.
pub(crate) fn processor_fixture(prepared: u64, delay: u64) -> (Box<State>, PcmProcessor) {
    let mut state = Box::new(State {
        prepared,
        delay: AtomicU64::new(delay),
        // SDK setup counters do not start at zero.
        accepted: AtomicU64::new(64),
        delivered: AtomicU64::new(64),
        flags: AtomicU32::new(15),
        fault: AtomicU32::new(0),
        previous: AtomicU32::new(0),
        calls: AtomicU64::new(0),
        queries: AtomicU64::new(0),
    });
    let processor = PcmProcessor {
        maximum: 960,
        api: abi::rptadv_pcm_processor_v1 {
            struct_size: size_of::<abi::rptadv_pcm_processor_v1>() as u32,
            abi_version: 1,
            context: ptr::from_mut(state.as_mut()).cast(),
            process: Some(process),
            destroy: Some(destroy),
            programme_buffering: Some(query),
        },
    };
    (state, processor)
}
fn fixture(prepared: u64, delay: u64, phone: bool) -> (Box<State>, ProcessedPeer<Source>) {
    let (state, processor) = processor_fixture(prepared, delay);
    (
        state,
        ProcessedPeer::new(Source::new(phone), Some(processor)),
    )
}

#[test]
fn qualified_delayed_phone_keeps_oldest_real_birth_without_exact_provenance() {
    let (_state, mut peer) = fixture(1, 1, true);
    assert!(peer.source_timing_available());
    assert_eq!(peer.phone_reply(), peer.input.phone_reply());
    let first = peer.render_timed_sample().unwrap();
    assert_eq!(first.sample, 0.0);
    assert_eq!(
        first.age_bound, None,
        "history before this prepared admission is unknown"
    );
    let second = peer.render_timed_sample().unwrap();
    assert_eq!(second.sample, 0.25);
    assert_eq!(
        second.provenance, None,
        "a bound is not an exact coordinate"
    );
    assert_eq!(
        second.age_bound,
        Some(SourceTimeBound {
            host_clock_epoch: 7,
            not_after_ns: 1_000_000
        }),
        "processing must not renew original receipt time"
    );
}

#[test]
fn invalid_live_query_never_qualifies_source_age() {
    for fault in 1..=13 {
        let (state, mut peer) = fixture(1, 1, true);
        peer.render_timed_sample();
        state.fault.store(fault, Ordering::Relaxed);
        let sample = peer.render_timed_sample();
        if fault == 7 {
            assert_eq!(sample, None);
        } else {
            assert_eq!(sample.unwrap().age_bound, None, "fault {fault}");
        }
    }
    for missing in [1, 2, 4, 8] {
        let (state, mut peer) = fixture(1, 1, true);
        peer.render_timed_sample();
        state.flags.store(15 & !missing, Ordering::Relaxed);
        assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    }
}

#[test]
fn unknown_source_or_mixed_lifecycle_history_is_never_relabelled() {
    for kind in 0..4 {
        let (_state, mut peer) = fixture(1, 1, true);
        peer.render_timed_sample();
        match kind {
            0 => peer.input.known = false,
            1 => peer.input.epoch = 8,
            2 => peer.input.generation = 10,
            _ => peer.input.plc = true,
        }
        assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    }
}

#[test]
fn public_opaque_peers_do_not_claim_phone_history() {
    let (_state, mut peer) = fixture(1, 1, false);
    assert!(!peer.source_timing_available());
    peer.render_timed_sample();
    assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
}

#[test]
fn large_prepared_plan_keeps_bounded_storage_and_checks_actual_live_history() {
    let (state, mut peer) = fixture(48_000, 24_001, true);
    assert!(peer.history.as_ref().unwrap().metadata_bytes() <= 1024);
    assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    state.delay.store(1, Ordering::Relaxed);
    assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    let bound = peer.render_timed_sample().unwrap().age_bound.unwrap();
    assert_eq!(
        bound.not_after_ns, 3_000_000,
        "fresh retained original history, not current receipt"
    );
    assert_eq!(
        state.prepared, 48_000,
        "the provider's actual maximum is never truncated"
    );
}

#[test]
fn zero_delay_uses_current_original_bound_without_a_history_array() {
    let (_state, mut peer) = fixture(0, 0, true);
    assert!(peer.history.as_ref().unwrap().metadata_bytes() < 128);
    assert_eq!(
        peer.render_timed_sample()
            .unwrap()
            .age_bound
            .unwrap()
            .not_after_ns,
        1_000_000
    );
    peer.input.epoch = 8;
    assert_eq!(
        peer.render_timed_sample()
            .unwrap()
            .age_bound
            .unwrap()
            .host_clock_epoch,
        8
    );
    peer.input.known = false;
    assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
}

#[test]
fn untimed_or_idle_processing_requires_new_observation_and_fresh_history() {
    for mode in 0..3 {
        let (_state, mut peer) = fixture(1, 1, true);
        peer.render_timed_sample();
        assert!(peer.render_timed_sample().unwrap().age_bound.is_some());
        let mut scratch = [0.0; 4];
        match mode {
            0 => {
                peer.render(&mut scratch);
            }
            1 => peer.silence(&mut scratch),
            _ => {
                peer.render_sample();
            }
        }
        assert_eq!(
            peer.render_timed_sample().unwrap().age_bound,
            None,
            "establish actual counter baseline"
        );
        assert_eq!(
            peer.render_timed_sample().unwrap().age_bound,
            None,
            "unknown old graph history"
        );
        assert!(peer.render_timed_sample().unwrap().age_bound.is_some());
    }
}

#[test]
fn poisoned_bucket_recovers_only_after_qualified_history_no_longer_overlaps_it() {
    let (_state, mut peer) = fixture(1, 1, true);
    peer.input.known = false;
    peer.render_timed_sample();
    peer.input.known = true;
    for _ in 1..=960 {
        assert_eq!(peer.render_timed_sample().unwrap().age_bound, None);
    }
    let bound = peer.render_timed_sample().unwrap().age_bound.unwrap();
    assert_eq!(bound.not_after_ns, 961_000_000);
}

#[test]
fn retained_bucket_wrap_keeps_conservative_oldest_original_receipt() {
    let (_state, mut peer) = fixture(1, 1, true);
    for _ in 0..3_000 {
        let sample = peer.render_timed_sample().unwrap();
        if let Some(bound) = sample.age_bound {
            let exact_previous_birth = (peer.input.next - 1) * 1_000_000;
            assert!(bound.not_after_ns <= exact_previous_birth);
            assert!(exact_previous_birth - bound.not_after_ns < 960_000_000);
        }
    }
    assert!(peer.render_timed_sample().unwrap().age_bound.is_some());
}

#[test]
fn unqualified_prepared_counts_or_bound_never_allocate_phone_history() {
    for missing in [1, 2] {
        let (state, mut peer) = fixture(1, 1, true);
        state.flags.store(15 & !missing, Ordering::Relaxed);
        let mut other = ProcessedPeer::new(Source::new(true), peer.processor.take());
        assert!(other.history.is_none());
        assert!(!other.source_timing_available());
        assert_eq!(other.render_timed_sample().unwrap().age_bound, None);
        state.flags.store(15, Ordering::Relaxed);
        assert!(
            !other.source_timing_available(),
            "an unqualified plan cannot adopt retained output midway"
        );
        assert_eq!(other.render_timed_sample().unwrap().age_bound, None);
    }
}

#[test]
fn prepared_history_audio_paths_allocate_nothing() {
    let (allocation, measured) = crate::fixture::allocations(|| std::hint::black_box(Box::new(7)));
    assert_eq!(measured, 1, "positive control for the allocation counter");
    drop(allocation);
    let (state, mut peer) = fixture(48_000, 1, true);
    let (_, allocations) = crate::fixture::allocations(|| {
        for _ in 0..3_000 {
            peer.render_timed_sample();
        }
        peer.input.known = false;
        peer.render_timed_sample();
        peer.input.known = true;
        for _ in 0..1_000 {
            peer.render_timed_sample();
        }
        state.delay.store(24_001, Ordering::Relaxed);
        peer.render_timed_sample();
        state.delay.store(1, Ordering::Relaxed);
        peer.render_timed_sample();
        let mut scratch = [0.0; 960];
        peer.silence(&mut scratch);
        peer.render(&mut scratch);
        peer.render_sample();
    });
    assert_eq!(allocations, 0);
}

#[test]
fn timed_block_history_batches_dsp_and_keeps_original_activity_and_oldest_age() {
    for phone in [false, true] {
        for delay in [0, 1] {
            let (state, mut peer) = fixture(delay, delay, phone);
            let mut pcm = [0.0; 4];
            let first = peer.render_timed_block(&mut pcm).unwrap();
            assert_eq!(first.real, 4);
            assert_eq!(peer.input.next, 4, "one input fill, not two renders");
            assert_eq!(state.calls.load(Ordering::Relaxed), 1);
            if phone {
                assert_eq!(
                    state.queries.load(Ordering::Relaxed),
                    2,
                    "prepare plus one block query"
                );
                assert_eq!(first.age_bound.is_some(), delay == 0);
            } else {
                assert_eq!(first.age_bound, None);
                assert_eq!(
                    state.queries.load(Ordering::Relaxed),
                    0,
                    "ordinary block path stays unchanged"
                );
            }
            // A changing last-frame delay cannot erase the prepared lookback of
            // an earlier sample in the same output block.
            state.delay.store(0, Ordering::Relaxed);
            let second = peer.render_timed_block(&mut pcm).unwrap();
            assert_eq!(second.real, 4);
            assert_eq!(state.calls.load(Ordering::Relaxed), 2);
            if phone {
                let bound = second.age_bound.unwrap();
                assert_eq!(bound.host_clock_epoch, 7);
                assert!(bound.not_after_ns <= (5 - delay) * 1_000_000);
            } else {
                assert_eq!(second.age_bound, None);
            }
        }
    }
}

#[test]
fn timed_block_history_keeps_unknown_middle_input_and_epoch_mismatch_sticky() {
    for fault in 0..=4 {
        let (_state, mut peer) = fixture(0, 0, true);
        peer.input.fault_at = Some((2, fault));
        let block = peer.render_timed_block(&mut [0.0; 4]).unwrap();
        assert_eq!(
            block.real,
            if fault == 4 { 3 } else { 4 },
            "preserve original real/PLC count"
        );
        assert_eq!(
            block.age_bound, None,
            "fault {fault} cannot be hidden by a valid last sample"
        );
    }
}

#[test]
fn timed_block_history_rejects_failed_reset_and_no_output_observations() {
    for fault in 1..=13 {
        let (state, mut peer) = fixture(1, 1, true);
        peer.render_timed_block(&mut [0.0; 4]).unwrap();
        state.fault.store(fault, Ordering::Relaxed);
        let mut pcm = [9.0; 4];
        let block = peer.render_timed_block(&mut pcm);
        if fault == 7 {
            assert!(block.is_none());
            assert_eq!(pcm, [0.0; 4]);
        } else {
            assert_eq!(block.unwrap().age_bound, None, "fault {fault}");
        }
    }
    for missing in [1, 2, 4, 8] {
        let (state, mut peer) = fixture(1, 1, true);
        peer.render_timed_block(&mut [0.0; 4]).unwrap();
        state.flags.store(15 & !missing, Ordering::Relaxed);
        assert_eq!(
            peer.render_timed_block(&mut [0.0; 4]).unwrap().age_bound,
            None
        );
    }
    let (state, mut peer) = fixture(1, 1, true);
    peer.render_timed_block(&mut [0.0; 4]).unwrap();
    state.accepted.store(0, Ordering::Relaxed);
    state.delivered.store(0, Ordering::Relaxed);
    assert_eq!(
        peer.render_timed_block(&mut [0.0; 4]).unwrap().age_bound,
        None
    );
}

#[test]
fn timed_block_history_horizon_and_empty_blocks_do_not_invent_age_or_work() {
    let (state, mut peer) = fixture(48_000, 0, true);
    let bytes = peer.history.as_ref().unwrap().metadata_bytes();
    assert!(bytes <= 1024);
    let queries = state.queries.load(Ordering::Relaxed);
    let empty = peer.render_timed_block(&mut []).unwrap();
    assert_eq!(empty.real, 0);
    assert_eq!(empty.age_bound, None);
    assert_eq!(state.calls.load(Ordering::Relaxed), 0);
    assert_eq!(state.queries.load(Ordering::Relaxed), queries);
    let (_, allocations) = crate::fixture::allocations(|| {
        for _ in 0..60 {
            let block = peer.render_timed_block(&mut [0.0; 480]).unwrap();
            assert_eq!(block.real, 480);
            assert_eq!(
                block.age_bound, None,
                "last-output delay0 cannot qualify a larger whole-block bound"
            );
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(peer.history.as_ref().unwrap().metadata_bytes(), bytes);
}

#[test]
fn timed_block_history_rejects_oversize_before_source_or_processor_work() {
    let (state, mut peer) = fixture(1, 1, true);
    let mut pcm = [9.0; 961];
    assert_eq!(peer.render_timed_block(&mut pcm), None);
    assert_eq!(pcm, [0.0; 961]);
    assert_eq!(peer.input.next, 0);
    assert_eq!(state.calls.load(Ordering::Relaxed), 0);
    assert_eq!(state.queries.load(Ordering::Relaxed), 1);
}

#[test]
fn timed_block_history_wrap_keeps_earlier_bound_before_bucket_reuse() {
    for prepared in [1, 960, 961, 24_000] {
        let (_state, mut peer) = fixture(prepared, 0, true);
        let mut qualified = 0;
        for _ in 0..80 {
            let first = peer.input.next;
            let block = peer.render_timed_block(&mut [0.0; 960]).unwrap();
            if let Some(bound) = block.age_bound {
                qualified += 1;
                assert_eq!(bound.host_clock_epoch, 7);
                let earliest = (first + 1 - prepared) * 1_000_000;
                assert!(bound.not_after_ns <= earliest);
                assert!(earliest - bound.not_after_ns < 960_000_000);
            }
        }
        assert!(qualified > 0);
    }
}
