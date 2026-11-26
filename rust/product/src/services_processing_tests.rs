use super::*;
use crate::configuration_radio::{PreparationError, PreparedPeer, fixture_native_peer};
use rpt_advanced_core::link::{PeerInput as Input, PeerSignals};
use std::sync::{
    Arc,
    atomic::{AtomicI16, AtomicUsize},
};

struct State {
    mode: usize,
    processed: AtomicUsize,
    processed_samples: AtomicUsize,
    nonzero_samples: AtomicUsize,
    destroyed: AtomicUsize,
    previous: AtomicI16,
}

unsafe extern "C" fn process(
    context: *mut c_void,
    direction: u32,
    rate: u32,
    samples: *mut i16,
    count: u32,
) -> i32 {
    assert_eq!(direction, abi::URP_AST_LINK_DIRECTION_READ);
    assert_eq!(rate, 8000);
    // SAFETY: the typed graph keeps the transferred Arc and PCM block live.
    let state = unsafe { &*context.cast::<State>() };
    state.processed.fetch_add(1, Ordering::Relaxed);
    let samples = unsafe { std::slice::from_raw_parts_mut(samples, count as usize) };
    state
        .processed_samples
        .fetch_add(samples.len(), Ordering::Relaxed);
    state.nonzero_samples.fetch_add(
        samples.iter().filter(|sample| **sample != 0).count(),
        Ordering::Relaxed,
    );
    if state.mode == 7 {
        samples.fill(i16::MAX);
        return -1;
    }
    for sample in samples {
        if state.mode == 10 {
            *sample = state.previous.swap(*sample, Ordering::Relaxed);
        } else {
            *sample /= 2;
        }
    }
    0
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    // SAFETY: the factory transferred precisely this Arc into the typed graph.
    let state = unsafe { Arc::from_raw(context.cast::<State>()) };
    state.destroyed.fetch_add(1, Ordering::Relaxed);
}

fn fixture(mode: usize) -> (Arc<State>, Result<PreparedPeer, PreparationError>) {
    let state = Arc::new(State {
        mode,
        processed: AtomicUsize::new(0),
        processed_samples: AtomicUsize::new(0),
        nonzero_samples: AtomicUsize::new(0),
        destroyed: AtomicUsize::new(0),
        previous: AtomicI16::new(0),
    });
    // SAFETY: the factory transfers an Arc only on success, and the corresponding
    // typed callbacks process the same context and release it once on destroy.
    let graph = unsafe {
        fixture_native_peer(
            8000,
            |table| {
                table.link_process = Some(process);
                table.link_destroy = Some(destroy);
            },
            |ffmpeg, output| {
                assert!(!ffmpeg.is_null());
                match mode {
                    1 => 1,
                    2 => -1,
                    8 => 2,
                    9 => 0, // Success without a transferred handle is incompatible.
                    _ => {
                        *output = Arc::into_raw(state.clone()).cast_mut().cast();
                        0
                    }
                }
            },
        )
    };
    (state, graph)
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
    let mut bypass = ProcessedPeer::new(Source::new(Some(1)));
    assert!(bypass.source_timing_available());
    let first = bypass.render_timed_sample().unwrap();
    assert_eq!(first.sample, 0.25);
    assert_eq!(first.provenance.unwrap().position.unwrap().frame, 1);
    bypass.input.signals.set_dtmf_muted(true);
    let muted = bypass.render_timed_sample().unwrap();
    assert_eq!(muted.sample, 0.0);
    assert!(muted.provenance.unwrap().muted);

    let (state, processor) = fixture(10);
    let mut delayed = ProcessedPeer::modular(Source::new(Some(1)), processor.unwrap(), 4);
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

    let mut failed = ProcessedPeer::new(Source::new(None));
    assert_eq!(failed.render_timed_sample(), None);
    let (state, processor) = fixture(7);
    let mut failed = ProcessedPeer::modular(Source::new(Some(1)), processor.unwrap(), 4);
    assert_eq!(failed.render_timed_sample(), None);
    assert_eq!(state.processed.load(Ordering::Relaxed), 1);
}

#[test]
fn idle_peer_graph_receives_only_silence_without_reading_or_keying_its_source() {
    for mode in [0, 7] {
        let (state, processor) = fixture(mode);
        let mut input = ProcessedPeer::modular(Source::new(Some(4)), processor.unwrap(), 4);
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
    let mut bypass = ProcessedPeer::new(Source::new(None));
    let mut scratch = [7.0; 4];
    bypass.silence(&mut scratch);
    assert_eq!(scratch, [0.0; 4]);
    assert_eq!(bypass.input.renders, 0);
}

#[test]
fn typed_peer_factory_rejects_errors_and_missing_owned_result() {
    for (mode, expected) in [
        (
            1,
            PreparationError::Rejected {
                status: 1,
                field: None,
            },
        ),
        (
            2,
            PreparationError::Rejected {
                status: -1,
                field: None,
            },
        ),
        (
            8,
            PreparationError::Rejected {
                status: 2,
                field: None,
            },
        ),
        (9, PreparationError::Incompatible),
    ] {
        let (state, result) = fixture(mode);
        assert_eq!(result.err(), Some(expected));
        assert_eq!(Arc::strong_count(&state), 1);
        assert_eq!(state.destroyed.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn peer_processor_filters_native_ring_output_including_concealment_once() {
    let (state, processor) = fixture(0);
    let mut input = ProcessedPeer::modular(Source::new(Some(0)), processor.unwrap(), 4);
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
    let (state, processor) = fixture(7);
    let mut input = ProcessedPeer::modular(Source::new(Some(4)), processor.unwrap(), 4);
    let mut samples = [0.0; 4];
    assert_eq!(input.render(&mut samples), None);
    assert_eq!(samples, [0.0; 4]);
    assert_eq!(state.processed.load(Ordering::Relaxed), 1);
    drop(input);
    assert_eq!(state.destroyed.load(Ordering::Relaxed), 1);
    let mut bypass = ProcessedPeer::new(Source::new(Some(4)));
    assert_eq!(bypass.render(&mut samples), Some(4));
    assert_eq!(samples, [0.5; 4]);
    let mut unavailable = ProcessedPeer::new(Source::new(None));
    assert_eq!(unavailable.render(&mut samples), None);
    assert_eq!(samples, [0.0; 4]);
    assert_eq!(unavailable.render_sample(), None);
    let mut bypass_sample = ProcessedPeer::new(Source::new(Some(1)));
    assert_eq!(bypass_sample.render_sample(), Some(0.5));
}

#[test]
fn peer_processor_rejects_oversized_blocks_and_failed_single_samples() {
    for mode in [0, 7] {
        let (state, processor) = fixture(mode);
        let mut input = ProcessedPeer::modular(Source::new(Some(1)), processor.unwrap(), 4);
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
    let (state, processor) = fixture(0);
    let mut input = ProcessedPeer::modular(Source::new(Some(1)), processor.unwrap(), 4);
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

#[test]
fn typed_station_preparation_selects_the_peer_graph_for_the_actual_port() {
    use crate::{configuration_radio::RadioPreparationApi, fixture::station};
    use rpt_advanced_core::config::resources::candidate::ResourceKind;
    use serde_json::json;

    let prepared = station::prepare(
        &[
            (station::LOCAL, "selected", json!({})),
            (station::SECOND_LOCAL, "other", json!({})),
        ],
        &[(
            station::LOCAL,
            ResourceKind::PeerAudio,
            "",
            json!({"input_gain_db":-6}),
        )],
    );
    let api = RadioPreparationApi::open().unwrap();
    let ports = &prepared.runtime().radio;
    let selected = api
        .prepare_link(
            ports
                .port(station::LOCAL.parse().unwrap())
                .unwrap()
                .processing
                .link
                .as_ref(),
        )
        .unwrap();
    let neutral = api
        .prepare_link(
            ports
                .port(station::SECOND_LOCAL.parse().unwrap())
                .unwrap()
                .processing
                .link
                .as_ref(),
        )
        .unwrap();
    assert!(!selected.equivalent(&neutral).unwrap());
    let graph = selected.prepare_peer(8000, 160).unwrap();
    let bypass = neutral.prepare_peer(8000, 160).unwrap();
    drop((api, prepared, selected, neutral));

    let mut selected = ProcessedPeer::modular(Source::new(Some(160)), graph, 160);
    let mut neutral = ProcessedPeer::modular(Source::new(Some(160)), bypass, 160);
    let mut wet = [0.0; 160];
    let mut dry = [0.0; 160];
    assert_eq!(selected.render(&mut wet), Some(160));
    assert_eq!(neutral.render(&mut dry), Some(160));
    assert_eq!(dry, [0.5; 160]);
    assert!(wet[120] > 0.0 && wet[120] < dry[120]);
}
