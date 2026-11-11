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
}

unsafe extern "C" fn process(context: *mut c_void, pcm: *mut f32, count: usize) -> i32 {
    let state = unsafe { &*context.cast::<State>() };
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
        Some(TimedPeerSample {
            sample: self.next as f32 * 0.25,
            age_bound: self.known.then_some(SourceTimeBound {
                host_clock_epoch: self.epoch,
                not_after_ns: self.next * 1_000_000,
            }),
            provenance: Some(SampleProvenance {
                position: Some(SourceCoordinate {
                    generation: self.generation,
                    frame: self.next,
                    fraction_billionths: 0,
                    negative: false,
                }),
                real: true,
                plc: self.plc,
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
