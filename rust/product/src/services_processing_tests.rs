use super::*;
use rpt_advanced_core::link::{PeerInput as Input, PeerSignals};
use std::sync::atomic::AtomicUsize;

const CONFIGURATION: &str = "[link]\nlimiter_enabled=yes\n";

struct State {
    mode: usize,
    processed: AtomicUsize,
    processed_samples: AtomicUsize,
    nonzero_samples: AtomicUsize,
    destroyed: AtomicUsize,
    previous: std::sync::atomic::AtomicU32,
}

unsafe extern "C" fn process(context: *mut c_void, samples: *mut f32, count: usize) -> i32 {
    let state = unsafe { &*context.cast::<State>() };
    state.processed.fetch_add(1, Ordering::Relaxed);
    let samples = unsafe { std::slice::from_raw_parts_mut(samples, count) };
    state.processed_samples.fetch_add(count, Ordering::Relaxed);
    state.nonzero_samples.fetch_add(
        samples.iter().filter(|sample| **sample != 0.0).count(),
        Ordering::Relaxed,
    );
    if state.mode == 7 {
        samples.fill(9.0);
        return -1;
    }
    for sample in samples {
        if state.mode == 10 {
            *sample = f32::from_bits(state.previous.swap(sample.to_bits(), Ordering::Relaxed));
        } else {
            *sample *= 0.5;
        }
    }
    0
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    unsafe { &*context.cast::<State>() }
        .destroyed
        .fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn prepare(
    context: *mut c_void,
    configuration: *const c_char,
    length: usize,
    local: *const c_char,
    local_length: usize,
    maximum: usize,
    output: *mut abi::rptadv_pcm_processor_v1,
) -> i32 {
    assert_eq!(
        unsafe { std::slice::from_raw_parts(configuration.cast::<u8>(), length) },
        CONFIGURATION.as_bytes()
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(local.cast::<u8>(), local_length) },
        b"1000"
    );
    assert_eq!(maximum, 4);
    let state = unsafe { &*context.cast::<State>() };
    match state.mode {
        1 => return 1,
        2 => return -1,
        8 => return 2,
        _ => {}
    }
    let output = unsafe { &mut *output };
    output.context = context;
    output.process = Some(process);
    output.destroy = Some(destroy);
    match state.mode {
        3 => output.struct_size -= 1,
        4 => output.abi_version = 2,
        5 => output.process = None,
        6 => output.context = ptr::null_mut(),
        9 => output.destroy = None,
        _ => {}
    }
    0
}

fn fixture(mode: usize) -> (Box<State>, HostServices) {
    let mut state = Box::new(State {
        mode,
        processed: AtomicUsize::new(0),
        processed_samples: AtomicUsize::new(0),
        nonzero_samples: AtomicUsize::new(0),
        destroyed: AtomicUsize::new(0),
        previous: std::sync::atomic::AtomicU32::new(0),
    });
    let services = host_services(|table| {
        table.context = ptr::from_mut(state.as_mut()).cast();
        table.peer_processing_prepare = Some(prepare);
    });
    (state, services)
}

struct Source {
    signals: PeerSignals,
    real: Option<usize>,
    renders: usize,
}
impl Source {
    fn new(real: Option<usize>) -> Self {
        Self {
            signals: PeerSignals::default(),
            real,
            renders: 0,
        }
    }
}
impl Input for Source {
    fn source_rate(&self) -> u32 {
        8000
    }
    fn source_timing_available(&self) -> bool {
        true
    }
    fn render_timed_sample(&mut self) -> Option<rpt_advanced_core::link::TimedPeerSample> {
        use rpt_advanced_core::link::{SampleProvenance, SourceCoordinate, TimedPeerSample};
        self.real?;
        self.renders += 1;
        Some(TimedPeerSample {
            sample: self.renders as f32 * 0.25,
            provenance: Some(SampleProvenance {
                position: Some(SourceCoordinate {
                    generation: 7,
                    frame: self.renders as u64,
                    fraction_billionths: 0,
                    negative: false,
                }),
                real: self.real != Some(0),
                plc: self.real == Some(0),
                ..SampleProvenance::default()
            }),
        })
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        9
    }
    fn finite_playout(&self) -> Option<bool> {
        Some(true)
    }
    fn render(&mut self, samples: &mut [f32]) -> Option<usize> {
        self.renders += 1;
        samples.fill(0.5);
        self.real
    }
}

#[test]
fn source_timing_survives_bypass_but_opaque_delayed_dsp_stays_unknown() {
    let mut bypass = ProcessedPeer::new(Source::new(Some(1)), None);
    assert!(bypass.source_timing_available());
    let first = bypass.render_timed_sample().unwrap();
    assert_eq!(first.sample, 0.25);
    assert_eq!(first.provenance.unwrap().position.unwrap().frame, 1);
    bypass.input.signals.set_dtmf_muted(true);
    let muted = bypass.render_timed_sample().unwrap();
    assert_eq!(muted.sample, 0.0);
    assert!(muted.provenance.unwrap().muted);

    let (state, services) = fixture(10);
    let processor = services
        .prepare_peer_processing(CONFIGURATION, "1000", 4)
        .unwrap();
    let mut delayed = ProcessedPeer::new(Source::new(Some(1)), processor);
    assert!(!delayed.source_timing_available());
    assert_eq!(delayed.render_timed_sample().unwrap().sample, 0.0);
    let second = delayed.render_timed_sample().unwrap();
    assert_eq!(
        second.sample, 0.25,
        "DSP output belongs to the previous input"
    );
    assert_eq!(
        second.provenance, None,
        "opaque DSP cannot relabel old PCM as current source"
    );
    delayed.input.signals.set_dtmf_muted(true);
    let muted = delayed.render_timed_sample().unwrap();
    assert_eq!((muted.sample, muted.provenance), (0.0, None));
    assert_eq!(state.processed.load(Ordering::Relaxed), 3);
    drop(delayed);
    assert_eq!(state.destroyed.load(Ordering::Relaxed), 1);

    let mut failed = ProcessedPeer::new(Source::new(None), None);
    assert_eq!(failed.render_timed_sample(), None);
    let (state, services) = fixture(7);
    let processor = services
        .prepare_peer_processing(CONFIGURATION, "1000", 4)
        .unwrap();
    let mut failed = ProcessedPeer::new(Source::new(Some(1)), processor);
    assert_eq!(failed.render_timed_sample(), None);
    assert_eq!(state.processed.load(Ordering::Relaxed), 1);
}

#[test]
fn idle_peer_graph_receives_only_silence_without_reading_or_keying_its_source() {
    for mode in [0, 7] {
        let (state, services) = fixture(mode);
        let processor = services
            .prepare_peer_processing(CONFIGURATION, "1000", 4)
            .unwrap();
        let mut input = ProcessedPeer::new(Source::new(Some(4)), processor);
        let edge = input.signals().activity_edge();
        let mut scratch = [7.0; 4];
        for count in [3, 1] {
            input.silence(&mut scratch[..count]);
            assert_eq!(&scratch[..count], &[0.0; 4][..count]);
        }
        assert_eq!(input.input.renders, 0);
        assert_eq!(input.input.real, Some(4));
        assert_eq!(input.available(), 9);
        assert!(!input.signals().radio_keyed());
        assert_eq!(input.signals().activity_edge(), edge);
        assert_eq!(state.processed.load(Ordering::Relaxed), 2);
        assert_eq!(state.processed_samples.load(Ordering::Relaxed), 4);
        assert_eq!(state.nonzero_samples.load(Ordering::Relaxed), 0);
        let mut oversized = [7.0; 5];
        input.silence(&mut oversized);
        assert_eq!(oversized, [0.0; 5]);
        assert_eq!(state.processed.load(Ordering::Relaxed), 2);
    }
    let mut bypass = ProcessedPeer::new(Source::new(None), None);
    let mut scratch = [7.0; 4];
    bypass.silence(&mut scratch);
    assert_eq!(scratch, [0.0; 4]);
    assert_eq!(bypass.input.renders, 0);
}

#[test]
fn host_peer_processor_bypasses_or_rejects_without_leaking_transferred_owners() {
    assert!(
        host_services(|_| {})
            .prepare_peer_processing(CONFIGURATION, "1000", 4)
            .unwrap()
            .is_none()
    );
    for mode in 1..=9 {
        if mode == 7 {
            continue;
        }
        let (state, services) = fixture(mode);
        let result = services.prepare_peer_processing(CONFIGURATION, "1000", 4);
        if mode == 1 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(matches!(result, Err(Error::Operation)));
        }
        assert_eq!(
            state.destroyed.load(Ordering::Relaxed),
            usize::from(matches!(mode, 3..=5))
        );
    }
}

#[test]
fn peer_processor_filters_native_ring_output_including_concealment_once() {
    let (state, services) = fixture(0);
    let processor = services
        .prepare_peer_processing(CONFIGURATION, "1000", 4)
        .unwrap();
    let mut input = ProcessedPeer::new(Source::new(Some(0)), processor);
    assert_eq!(input.source_rate(), 8000);
    assert_eq!(input.available(), 9);
    assert_eq!(input.finite_playout(), Some(true));
    assert!(!input.signals().ended());
    let mut samples = [0.0; 4];
    assert_eq!(input.render(&mut samples), Some(0));
    assert_eq!(samples, [0.25; 4]);
    assert_eq!(state.processed.load(Ordering::Relaxed), 1);
    assert_eq!(input.render_sample(), None);
    input.input.real = Some(1);
    assert_eq!(input.render_sample(), Some(0.25));
    assert_eq!(state.processed.load(Ordering::Relaxed), 2);
    drop(input);
    assert_eq!(state.destroyed.load(Ordering::Relaxed), 1);
}

#[test]
fn peer_processor_failure_silences_only_its_block_and_preserves_bypass() {
    let (state, services) = fixture(7);
    let processor = services
        .prepare_peer_processing(CONFIGURATION, "1000", 4)
        .unwrap();
    let mut input = ProcessedPeer::new(Source::new(Some(4)), processor);
    let mut samples = [0.0; 4];
    assert_eq!(input.render(&mut samples), None);
    assert_eq!(samples, [0.0; 4]);
    assert_eq!(state.processed.load(Ordering::Relaxed), 1);
    drop(input);
    assert_eq!(state.destroyed.load(Ordering::Relaxed), 1);
    let mut bypass = ProcessedPeer::new(Source::new(Some(4)), None);
    assert_eq!(bypass.render(&mut samples), Some(4));
    assert_eq!(samples, [0.5; 4]);
    let mut unavailable = ProcessedPeer::new(Source::new(None), None);
    assert_eq!(unavailable.render(&mut samples), None);
    assert_eq!(samples, [0.0; 4]);
    assert_eq!(unavailable.render_sample(), None);
    let mut bypass_sample = ProcessedPeer::new(Source::new(Some(1)), None);
    assert_eq!(bypass_sample.render_sample(), Some(0.5));
}

#[test]
fn peer_processor_rejects_oversized_blocks_and_failed_single_samples() {
    for mode in [0, 7] {
        let (state, services) = fixture(mode);
        let processor = services
            .prepare_peer_processing(CONFIGURATION, "1000", 4)
            .unwrap();
        let mut input = ProcessedPeer::new(Source::new(Some(1)), processor);
        let mut oversized = [1.0; 5];
        assert_eq!(input.render(&mut oversized), None);
        assert_eq!(oversized, [0.0; 5]);
        assert_eq!(state.processed.load(Ordering::Relaxed), 0);
        if mode == 7 {
            assert_eq!(input.render_sample(), None);
            assert_eq!(state.processed.load(Ordering::Relaxed), 1);
        }
        drop(input);
        assert_eq!(state.destroyed.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn peer_processor_mutes_final_output_using_current_dtmf_state() {
    let (state, services) = fixture(0);
    let processor = services
        .prepare_peer_processing(CONFIGURATION, "1000", 4)
        .unwrap();
    let mut input = ProcessedPeer::new(Source::new(Some(1)), processor);
    for muted in [true, false] {
        // Ring output can precede a detector-state change, and stateful DSP can
        // retain tone history. Qualification must therefore gate the final PCM.
        input.signals().set_dtmf_muted(muted);
        let mut samples = [0.0; 4];
        assert_eq!(input.render(&mut samples), Some(1));
        let expected = if muted { 0.0 } else { 0.25 };
        assert_eq!(samples, [expected; 4]);
        assert_eq!(input.render_sample(), Some(expected));
    }
    assert_eq!(state.processed.load(Ordering::Relaxed), 4);
}
