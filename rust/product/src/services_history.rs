//! Original and explicitly inherited receipt bounds beside the sole serial peer graph.
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
            // Phone TimedInput supplies a bound for explicitly mapped retained
            // synthetic continuity only. Unmapped ring shortfall has no bound.
            && ((provenance.real && !provenance.plc && !provenance.gap)
                || (!provenance.real && (provenance.plc || provenance.gap)))
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

/// Scalar accumulator survives metadata bucket reuse while filling one native block.
pub(super) struct Block {
    oldest: Option<Bucket>,
    unknown: bool,
    count: u64,
    qualified: bool,
    genuine: bool,
}
impl Block {
    fn include(&mut self, source: Option<Bucket>) {
        let Some(source) = source.filter(|source| source.bound.host_clock_epoch != 0) else {
            self.unknown = true;
            return;
        };
        if let Some(oldest) = self.oldest.as_mut() {
            oldest.include(source);
            self.unknown |= oldest.bound.host_clock_epoch == 0;
        } else {
            self.oldest = Some(source);
        }
    }
}

/// Prepared off audio only for a private timed source with a qualified SDK plan.
/// At most 26 compact buckets; public peers keep only an empty optional pointer.
pub(super) struct History {
    prepared: u64,
    horizon: u64,
    counters: Option<(u64, u64)>,
    frames: u64,
    last_qualified: Option<u64>,
    genuine_run: u64,
    output_genuine: bool,
    output_qualified: bool,
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
            last_qualified: None,
            genuine_run: 0,
            output_genuine: false,
            output_qualified: false,
            buckets: vec![Bucket::default(); count].into_boxed_slice(),
        }))
    }

    fn clear_samples(&mut self) {
        self.frames = 0;
        self.last_qualified = None;
        self.genuine_run = 0;
        self.output_genuine = false;
        self.output_qualified = false;
        self.buckets.fill(Bucket::default());
    }

    /// Untimed blocks, idle processing and failures cannot retain tagged history.
    pub(super) fn invalidate(&mut self) {
        self.clear_samples();
        self.counters = None;
    }

    /// Fold the prepared maximum lookback before incoming samples can reuse buckets.
    /// That prepared bound applies to every output, unlike the endpoint's current bound.
    pub(super) fn begin_block(&self) -> Block {
        let mut block = Block {
            oldest: None,
            unknown: self.prepared > self.horizon,
            count: 0,
            qualified: false,
            genuine: false,
        };
        if self.prepared == 0 {
            return block;
        }
        let Some(first) = self.frames.checked_sub(self.prepared) else {
            block.unknown = true;
            return block;
        };
        if block.unknown {
            return block;
        }
        block.qualified = self.last_qualified.is_some_and(|frame| frame >= first);
        let count = self.buckets.len() as u64;
        for index in first / WIDTH..=(self.frames - 1) / WIDTH {
            let bucket = self.buckets[(index % count) as usize];
            block.include((bucket.index == index).then_some(bucket));
        }
        block
    }

    /// Record original or explicitly inherited metadata during the sole PCM fill pass.
    pub(super) fn append_block(&mut self, block: &mut Block, input: TimedPeerSample) {
        let source = Bucket::source(input);
        let qualified = source.is_some() && input.provenance.is_some_and(|p| !p.muted);
        block.qualified |= qualified;
        block.include(source);
        block.count = block.count.saturating_add(1);
        if self
            .record(source, qualified, Self::genuine(input, qualified))
            .is_none()
        {
            block.unknown = true;
        }
        // The prepared upper bound applies to every output in a block. Only a
        // wholly genuine possible dependency interval proves delivered onset.
        block.genuine |= self.genuine_run > self.prepared;
    }

    /// Validate one completed native process call and its actual counter observation.
    pub(super) fn finish_block(
        &mut self,
        block: Block,
        state: Option<abi::UrpProgrammeBuffering>,
    ) -> Option<SourceTimeBound> {
        self.output_qualified = false;
        self.output_genuine = false;
        let Some(state) = state else {
            self.invalidate();
            return None;
        };
        let counters = (state.flags & abi::URP_PROGRAMME_COUNTS_VALID != 0
            && state.delivered_frames <= state.accepted_frames)
            .then_some((state.accepted_frames, state.delivered_frames));
        let fresh = self.counters.is_some_and(|(accepted, delivered)| {
            accepted.checked_add(block.count) == Some(state.accepted_frames)
                && delivered.checked_add(block.count) == Some(state.delivered_frames)
        });
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
            || state.output_fifo_frames > state.current_max_delay_frames
            || state.zero_prefix_frames != 0
            || state.zero_suffix_frames != 0
        {
            self.clear_samples();
            return None;
        }
        let bound = (!block.unknown)
            .then_some(block.oldest)
            .flatten()
            .map(|oldest| oldest.bound);
        self.output_qualified = bound.is_some() && block.qualified;
        self.output_genuine = bound.is_some() && block.genuine;
        bound
    }

    /// Voice may remain inside the qualified graph after its source gate closes.
    /// Reuse the proven DSP lookback; this scalar retains no PCM or extra delay.
    pub(super) fn output_qualified(&self) -> bool {
        self.output_qualified
    }

    /// Conservative delivery proof, distinct from possible qualified tail.
    pub(super) fn output_genuine(&self) -> bool {
        self.output_genuine
    }

    fn genuine(input: TimedPeerSample, qualified: bool) -> bool {
        qualified && input.provenance.is_some_and(|p| p.real && !p.plc && !p.gap)
    }

    fn record(&mut self, source: Option<Bucket>, qualified: bool, genuine: bool) -> Option<u64> {
        let frame = self.frames;
        let Some(next) = frame.checked_add(1) else {
            self.invalidate();
            return None;
        };
        self.frames = next;
        self.genuine_run = if genuine {
            self.genuine_run.saturating_add(1)
        } else {
            0
        };
        if qualified {
            self.last_qualified = Some(frame);
        }
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
        Some(frame)
    }

    /// Qualify only a completed single native sample. A block bound describes
    /// its last frame, so block rendering must not reuse this single-frame map.
    pub(super) fn output(
        &mut self,
        input: TimedPeerSample,
        state: Option<abi::UrpProgrammeBuffering>,
    ) -> Option<SourceTimeBound> {
        self.output_qualified = false;
        self.output_genuine = false;
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
        let delivered = self
            .counters
            .is_some_and(|(_, delivered)| delivered.checked_add(1) == Some(state.delivered_frames));
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
        let source = Bucket::source(input);
        let qualified = source.is_some() && input.provenance.is_some_and(|p| !p.muted);
        let frame = self.record(source, qualified, Self::genuine(input, qualified))?;
        let delay = state.current_max_delay_frames;
        if delay == 0 {
            self.output_qualified = qualified;
            self.output_genuine = delivered && self.genuine_run != 0;
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
        self.output_qualified = self
            .last_qualified
            .is_some_and(|last| last >= frame - delay);
        self.output_genuine = delivered && self.genuine_run > delay;
        Some(oldest.bound)
    }

    #[cfg(test)]
    pub(super) fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(&*self.buckets)
    }
}

#[cfg(test)]
#[path = "services_history_boundary_tests.rs"]
mod boundary_tests;
