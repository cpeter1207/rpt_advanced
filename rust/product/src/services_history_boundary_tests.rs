use super::*;
use rpt_advanced_core::link::{SampleProvenance, SourceCoordinate};

fn sample() -> TimedPeerSample {
    TimedPeerSample {
        sample: 0.5,
        age_bound: Some(SourceTimeBound {
            host_clock_epoch: 7,
            not_after_ns: 30,
        }),
        provenance: Some(SampleProvenance {
            position: Some(SourceCoordinate {
                generation: 9,
                frame: 1,
                fraction_billionths: 0,
                negative: false,
            }),
            real: true,
            ..SampleProvenance::default()
        }),
    }
}
fn history(prepared: u64) -> History {
    let (_state, mut processor) =
        crate::services::history_tests::processor_fixture(prepared, prepared);
    *History::prepare(&mut processor).unwrap()
}
fn state(accepted: u64, prepared: u64) -> abi::UrpProgrammeBuffering {
    // SAFETY: the ABI record contains only integer scalar fields.
    let mut state: abi::UrpProgrammeBuffering = unsafe { std::mem::zeroed() };
    state.struct_size = std::mem::size_of_val(&state) as u32;
    state.abi_version = 1;
    state.flags = 15;
    state.accepted_frames = accepted;
    state.delivered_frames = accepted;
    state.prepared_max_delay_frames = prepared;
    state.current_max_delay_frames = prepared;
    state
}

#[test]
fn invalid_source_coordinates_and_gaps_never_qualify_original_age() {
    for defect in 0..4 {
        let mut input = sample();
        let provenance = input.provenance.as_mut().unwrap();
        match defect {
            0 => provenance.position.as_mut().unwrap().negative = true,
            1 => provenance.position.as_mut().unwrap().fraction_billionths = 1_000_000_000,
            2 => provenance.gap = true,
            _ => provenance.position.as_mut().unwrap().fraction_billionths = 999_999_999,
        }
        assert_eq!(
            history(0).output(input, Some(state(65, 0))),
            (defect == 3).then_some(SourceTimeBound {
                host_clock_epoch: 7,
                not_after_ns: 30,
            })
        );
    }
}

#[test]
fn source_frame_overflow_invalidates_the_block_and_requires_fresh_history() {
    let mut history = history(0);
    history.frames = u64::MAX;
    let mut block = history.begin_block();
    history.append_block(&mut block, sample());
    assert_eq!(history.frames, 0);
    assert_eq!(history.counters, None);
    assert!(block.unknown);
    assert_eq!(history.finish_block(block, Some(state(65, 0))), None);
    let mut fresh = history.begin_block();
    history.append_block(&mut fresh, sample());
    assert_eq!(
        history.finish_block(fresh, Some(state(66, 0))),
        Some(SourceTimeBound {
            host_clock_epoch: 7,
            not_after_ns: 30
        })
    );
}

#[test]
fn overwritten_or_mixed_lifecycle_buckets_cannot_qualify_delayed_output() {
    for defect in 0..4 {
        let mut history = history(WIDTH);
        history.frames = WIDTH;
        history.buckets[0] = Bucket {
            index: if defect == 0 { 2 } else { 0 },
            generation: if defect == 1 { 10 } else { 9 },
            bound: SourceTimeBound {
                host_clock_epoch: if defect == 2 { 8 } else { 7 },
                not_after_ns: 10,
            },
        };
        assert_eq!(
            history.output(sample(), Some(state(65, WIDTH))),
            (defect == 3).then_some(SourceTimeBound {
                host_clock_epoch: 7,
                not_after_ns: 10,
            })
        );
    }
}

#[test]
fn explicitly_mapped_synthetic_continuity_keeps_original_age() {
    let mut history = history(0);
    let mut input = sample();
    let provenance = input.provenance.as_mut().unwrap();
    provenance.real = false;
    provenance.plc = true;
    assert_eq!(history.output(input, Some(state(65, 0))), input.age_bound);
    assert!(history.output_qualified());
}

#[test]
fn genuine_delivery_requires_full_dependency_interval_not_possible_tail() {
    for block_path in [false, true] {
        for delay in [0, 1, 8] {
            let mut history = history(delay);
            let mut accepted = 64;
            let mut render = |history: &mut History, input: TimedPeerSample| {
                accepted += 1;
                if block_path {
                    let mut block = history.begin_block();
                    history.append_block(&mut block, input);
                    history.finish_block(block, Some(state(accepted, delay)))
                } else {
                    history.output(input, Some(state(accepted, delay)))
                }
            };
            let mut muted = sample();
            muted.provenance.as_mut().unwrap().muted = true;
            for _ in 0..=delay {
                render(&mut history, muted);
            }
            for _ in 0..delay {
                assert!(render(&mut history, sample()).is_some());
                assert!(
                    history.output_qualified(),
                    "possible tail remains available"
                );
                assert!(
                    !history.output_genuine(),
                    "new input alone cannot prove onset"
                );
            }
            assert!(render(&mut history, sample()).is_some());
            assert!(history.output_genuine());
            let mut synthetic = sample();
            synthetic.provenance.as_mut().unwrap().real = false;
            synthetic.provenance.as_mut().unwrap().plc = true;
            assert!(render(&mut history, synthetic).is_some());
            assert!(
                history.output_qualified(),
                "existing bounded tail is preserved"
            );
            assert!(
                !history.output_genuine(),
                "synthetic cannot mint genuine delivery"
            );
            history.invalidate();
            assert!(!history.output_genuine());
        }
    }
}

#[test]
fn accepted_without_delivery_cannot_prove_genuine_onset() {
    let mut history = history(0);
    let mut buffered = state(65, 0);
    buffered.delivered_frames = 64;
    assert!(history.output(sample(), Some(buffered)).is_some());
    assert!(
        history.output_qualified(),
        "possible tail is still conservative"
    );
    assert!(
        !history.output_genuine(),
        "accepted counter alone is not delivery"
    );
}
