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
