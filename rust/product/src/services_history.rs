//! Original receipt bounds retained alongside the sole serial peer graph.
//! These metadata buckets contain no PCM and never infer time from frame counters.
use super::{PcmProcessor, abi};
use rpt_advanced_core::link::{SourceTimeBound, TimedPeerSample};

// Twenty milliseconds at the processor's fixed native 48 kHz rate. The 500 ms limit
// bounds metadata storage only, not end-to-end latency or the provider's maximum.
const WIDTH: u64 = 960;
const HORIZON: u64 = 24_000;

#[derive(Clone, Copy)]
struct Bucket {
    index: u64,
    generation: u64,
    bound: SourceTimeBound,
}
impl Default for Bucket {
    fn default() -> Self {
        Self {
            index: u64::MAX,
            generation: 0,
            bound: SourceTimeBound {
                host_clock_epoch: 0,
                not_after_ns: 0,
            },
        }
    }
}
impl Bucket {
    fn source(sample: TimedPeerSample) -> Option<Self> {
        let bound = sample
            .age_bound
            .filter(|bound| bound.host_clock_epoch != 0)?;
        let provenance = sample.provenance?;
        let position = provenance.position?;
        (!position.negative
            && position.fraction_billionths < 1_000_000_000
            && provenance.real
            && !provenance.plc
            && !provenance.gap
            && !provenance.zero_extension)
            .then_some(Self {
                index: 0,
                generation: position.generation,
                bound,
            })
    }
    fn include(&mut self, other: Self) {
        if self.generation != other.generation
            || self.bound.host_clock_epoch != other.bound.host_clock_epoch
        {
            self.bound.host_clock_epoch = 0;
        }
        self.bound.not_after_ns = self.bound.not_after_ns.min(other.bound.not_after_ns);
    }
}

/// Prepared off audio only for a private timed source with a qualified SDK plan.
/// At most 26 compact buckets; public peers keep only an empty optional pointer.
pub(super) struct History {
    prepared: u64,
    horizon: u64,
    counters: Option<(u64, u64)>,
    frames: u64,
    buckets: Box<[Bucket]>,
}
impl History {
    pub(super) fn prepare(processor: &mut PcmProcessor) -> Option<Box<Self>> {
        let state = processor.buffering()?;
        let required = abi::URP_PROGRAMME_COUNTS_VALID | abi::URP_PROGRAMME_PREPARED_BOUND_VALID;
        if state.flags & required != required || state.delivered_frames > state.accepted_frames {
            return None;
        }
        let horizon = state.prepared_max_delay_frames.min(HORIZON);
        let count = if horizon == 0 {
            0
        } else {
            (horizon.div_ceil(WIDTH) + 1) as usize
        };
        Some(Box::new(Self {
            prepared: state.prepared_max_delay_frames,
            horizon,
            counters: Some((state.accepted_frames, state.delivered_frames)),
            frames: 0,
            buckets: vec![Bucket::default(); count].into_boxed_slice(),
        }))
    }

    fn clear_samples(&mut self) {
        self.frames = 0;
        self.buckets.fill(Bucket::default());
    }

    /// Untimed blocks, idle processing and failures cannot retain tagged history.
    pub(super) fn invalidate(&mut self) {
        self.clear_samples();
        self.counters = None;
    }

    /// Qualify only a completed single native sample. A block bound describes
    /// its last frame, so block rendering must not reuse this single-frame map.
    pub(super) fn output(
        &mut self,
        input: TimedPeerSample,
        state: Option<abi::UrpProgrammeBuffering>,
    ) -> Option<SourceTimeBound> {
        let Some(state) = state else {
            self.invalidate();
            return None;
        };
        let counters = (state.flags & abi::URP_PROGRAMME_COUNTS_VALID != 0
            && state.delivered_frames <= state.accepted_frames)
            .then_some((state.accepted_frames, state.delivered_frames));
        let fresh = self.counters.is_some_and(|(accepted, delivered)| {
            accepted.checked_add(1) == Some(state.accepted_frames)
                && state.delivered_frames >= delivered
        });
        // Actual counters are continuity evidence only. After any lost observation
        // a new baseline and fresh source history are required before qualification.
        self.counters = counters;
        let required = abi::URP_PROGRAMME_COUNTS_VALID
            | abi::URP_PROGRAMME_PREPARED_BOUND_VALID
            | abi::URP_PROGRAMME_CURRENT_BOUND_VALID
            | abi::URP_PROGRAMME_LAST_BLOCK_VALID;
        if !fresh
            || counters.is_none()
            || state.flags & required != required
            || state.prepared_max_delay_frames != self.prepared
            || state.current_max_delay_frames > self.prepared
            || state.current_max_delay_frames > self.horizon
            || state.output_fifo_frames > state.current_max_delay_frames
            || state.zero_prefix_frames != 0
            || state.zero_suffix_frames != 0
        {
            self.clear_samples();
            return None;
        }
        let frame = self.frames;
        let Some(next) = frame.checked_add(1) else {
            self.invalidate();
            return None;
        };
        self.frames = next;
        let source = Bucket::source(input);
        if !self.buckets.is_empty() {
            let index = frame / WIDTH;
            let count = self.buckets.len() as u64;
            let bucket = &mut self.buckets[(index % count) as usize];
            let current = Bucket {
                index,
                ..source.unwrap_or_default()
            };
            if bucket.index == index {
                bucket.include(current);
            } else {
                *bucket = current;
            }
        }
        let delay = state.current_max_delay_frames;
        if delay == 0 {
            return source.map(|source| source.bound);
        }
        // Never map pre-admission or overwritten history. Whole buckets retain
        // an older conservative bound and may deliberately withhold qualification.
        let first = frame.checked_sub(delay)? / WIDTH;
        let last = frame / WIDTH;
        let count = self.buckets.len() as u64;
        let mut oldest = source?;
        for index in first..=last {
            let bucket = self.buckets[(index % count) as usize];
            if bucket.index != index || bucket.bound.host_clock_epoch == 0 {
                return None;
            }
            oldest.include(bucket);
            if oldest.bound.host_clock_epoch == 0 {
                return None;
            }
        }
        Some(oldest.bound)
    }

    #[cfg(test)]
    pub(super) fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(&*self.buckets)
    }
}
