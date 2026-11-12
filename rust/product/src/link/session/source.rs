//! Source-rate detection and retained mute metadata; the provider owns raw retries.
use crate::{
    Error,
    link::ring::{InboundProducer, MuteSpan},
    services::SourceBirth,
};
use rpt_advanced_core::audio::DtmfDetector;

const EMPTY: MuteSpan = MuteSpan { start: 0, end: 0 };

/// Collect valid lookback in this burst's half-open window before opening mute.
fn collect_mute_start(
    first: &mut Option<u64>,
    mut onset: impl FnMut(),
    start: u64,
    end: u64,
    burst_start: u64,
    range: std::ops::Range<i64>,
) {
    let from = start.saturating_add_signed(range.start).max(burst_start);
    let to = start.saturating_add_signed(range.end).min(end);
    if from < to {
        *first = Some(first.map_or(from, |old| old.min(from)));
        onset();
    }
}

#[derive(Clone, Copy)]
struct Cached {
    start: u64,
    count: usize,
    next: usize,
    concealed: bool,
    birth: Option<SourceBirth>,
}

pub(crate) struct Decoded<'a> {
    pub start: u64,
    pub pcm: &'a [f32],
    pub concealed: bool,
    pub birth: Option<SourceBirth>,
}

/// One serial analyzer. Cache only identity/cursor and mute metadata; each retry
/// rematerializes stable provider-owned PCM into the caller's reusable scratch.
pub(crate) struct SourceIngress<const N: usize = 64> {
    detector: DtmfDetector,
    muting: bool,
    command_muted: bool,
    generation: u64,
    burst_start: Option<u64>,
    cursor: u64,
    cached: Option<Cached>,
    spans: [MuteSpan; N],
    partition: usize,
    maximum: usize,
    span_count: usize,
    pending_mute: Option<MuteSpan>,
}

// PeerIo admits at most 4096 samples, divided into 64-sample partitions.
// Each pass appends at most 64 ranges; merging can only reduce that count.
fn span<const N: usize>(spans: &mut [MuteSpan; N], count: &mut usize, start: u64, end: u64) {
    if start >= end {
        return;
    }
    if *count != 0 && start <= spans[*count - 1].end {
        let previous = &mut spans[*count - 1];
        previous.start = previous.start.min(start);
        previous.end = previous.end.max(end);
    } else {
        spans[*count] = MuteSpan { start, end };
        *count += 1;
    }
}

impl SourceIngress {
    pub(super) fn new(muting: bool) -> Result<Self, Error> {
        Self::at_rate(muting, 8000, 4096)
    }
}
impl<const N: usize> SourceIngress<N> {
    /// Prepare one released source-rate detector and an exact bounded offer profile.
    pub(crate) fn at_rate(muting: bool, rate: u32, maximum: usize) -> Result<Self, Error> {
        if !matches!(rate, 8000 | 48000) {
            return Err(Error::UnsupportedFormat);
        }
        let partition = rate as usize / 125;
        if maximum == 0 || maximum.div_ceil(partition) > N {
            return Err(Error::InvalidFrame);
        }
        Ok(Self {
            detector: DtmfDetector::at_rate(muting, rate).map_err(|_| Error::UnsupportedFormat)?,
            partition,
            maximum,
            muting,
            command_muted: false,
            generation: 1,
            burst_start: None,
            cursor: 0,
            cached: None,
            spans: [EMPTY; N],
            span_count: 0,
            pending_mute: None,
        })
    }

    /// Cancel retained PCM and start a new source range. End the one already
    /// qualified digit before clearing analysis; the caller reserves its slot.
    pub(crate) fn reset(&mut self, emit: impl FnMut(char)) -> Result<(), Error> {
        self.detector
            .process_spans(false, &mut [], emit, |_| {})
            .map_err(|_| Error::InvalidFrame)?;
        self.generation = self.generation.checked_add(1).ok_or(Error::InvalidFrame)?;
        self.command_muted = false;
        self.burst_start = None;
        self.cached = None;
        self.span_count = 0;
        self.pending_mute = None;
        Ok(())
    }
    pub(super) fn set_muting(&mut self, muting: bool) {
        self.muting = muting;
        self.detector.set_muting(muting);
    }

    fn flush_mute(&mut self, inbound: &mut InboundProducer) -> Result<bool, Error> {
        if let Some(pending) = self.pending_mute {
            if inbound
                .mute(self.generation, &[pending])
                .map_err(|_| Error::Write)?
                == 0
            {
                return Ok(false);
            }
            self.pending_mute = None;
        }
        Ok(true)
    }

    // Loss is not contiguous fresh tone. Complete only an already qualified
    // real digit; never let analysis windows straddle unknown or codec-PLC PCM.
    pub(crate) fn discontinuity(&mut self, mut emit: impl FnMut(char)) -> Result<(), Error> {
        if self.cached.is_some() {
            return Err(Error::InvalidFrame);
        }
        let command_muted = &mut self.command_muted;
        let muting = self.muting;
        self.detector
            .process_spans(
                false,
                &mut [],
                |digit| {
                    *command_muted = muting && digit != '#';
                    emit(digit);
                },
                std::mem::drop,
            )
            .map_err(|_| Error::InvalidFrame)
    }

    /// Bound a new offer or the still-unaccepted suffix; never redetect a retry.
    pub(crate) fn limit(&self, count: usize) -> usize {
        count.min(
            self.cached
                .map_or(self.maximum, |cached| cached.count - cached.next),
        )
    }
    /// Conservative upper bound on completed digits from this bounded analysis.
    pub(crate) fn digit_capacity(&self, count: usize) -> usize {
        if self.cached.is_some() {
            0
        } else {
            count.div_ceil(self.partition)
        }
    }
    pub(crate) fn muted(&self) -> bool {
        self.command_muted || self.detector.suppressing()
    }
    /// A native authoritative command terminator releases command-wide suppression;
    /// an actually present tone remains suppressed until the detector releases it.
    pub(crate) fn end_command(&mut self) {
        self.command_muted = false;
    }
    /// Analyze an offer once, then reconstruct each retry from its borrowed raw PCM.
    /// Callbacks are synchronous and must remain nonblocking.
    pub(crate) fn prepare(
        &mut self,
        scratch: &mut [f32],
        block: Decoded<'_>,
        mut emit: impl FnMut(char),
        mut onset: impl FnMut(),
    ) -> Result<(), Error> {
        if block.pcm.is_empty()
            || block.pcm.len() > scratch.len()
            || block.pcm.len() > self.limit(block.pcm.len())
            || block.start.checked_add(block.pcm.len() as u64).is_none()
        {
            return Err(Error::InvalidFrame);
        }
        if let Some(cached) = self.cached {
            if block.start != cached.start + cached.next as u64
                || block.concealed != cached.concealed
                || block.birth != cached.birth
            {
                return Err(Error::InvalidFrame);
            }
        }
        scratch[..block.pcm.len()].copy_from_slice(block.pcm);
        if self.cached.is_none() {
            let burst_start = *self.burst_start.get_or_insert(block.start);
            let concealed_muted = block.concealed && self.muted();
            if block.concealed {
                self.discontinuity(&mut emit)?;
            }
            self.span_count = 0;
            // Fixed eight-ms partitions preserve the existing completion/mute granularity.
            for (part, pcm) in scratch[..block.pcm.len()]
                .chunks_mut(self.partition)
                .enumerate()
            {
                let start = block.start + (part * self.partition) as u64;
                let end = start + pcm.len() as u64;
                let was_muted = self.command_muted;
                let mut digit = None;
                let mut first = None;
                if !block.concealed {
                    self.detector
                        .process_spans(
                            true,
                            pcm,
                            |decoded| {
                                digit = Some(decoded);
                                emit(decoded);
                            },
                            |range| {
                                collect_mute_start(
                                    &mut first,
                                    &mut onset,
                                    start,
                                    end,
                                    burst_start,
                                    range,
                                )
                            },
                        )
                        .map_err(|_| Error::InvalidFrame)?;
                }
                let muted = self.muting
                    && (was_muted || first.is_some() || digit.is_some() || concealed_muted);
                if muted {
                    span(
                        &mut self.spans,
                        &mut self.span_count,
                        first.unwrap_or(start),
                        end,
                    );
                }
                if !block.concealed {
                    self.command_muted = muted && digit != Some('#');
                }
            }
            self.cached = Some(Cached {
                start: block.start,
                count: block.pcm.len(),
                next: 0,
                concealed: block.concealed,
                birth: block.birth,
            });
        }
        // Never rely on detector-mutated scratch surviving an intervening prompt.
        // Reapply the cached union, including command-wide suppression, to this prefix.
        let end = block.start + block.pcm.len() as u64;
        for interval in &self.spans[..self.span_count] {
            let first = interval.start.max(block.start);
            let last = interval.end.min(end);
            if first < last {
                scratch[(first - block.start) as usize..(last - block.start) as usize].fill(0.0);
            }
        }
        Ok(())
    }
    /// Advance only actual accepted source samples after publication succeeds.
    pub(crate) fn accepted(&mut self, count: usize) -> Result<(), Error> {
        let cached = self.cached.as_mut().ok_or(Error::InvalidFrame)?;
        if count > cached.count - cached.next {
            return Err(Error::InvalidFrame);
        }
        cached.next += count;
        self.cursor = cached.start + cached.next as u64;
        if cached.next == cached.count {
            self.cached = None;
        }
        Ok(())
    }
    pub(super) fn audio(
        &mut self,
        inbound: &mut InboundProducer,
        scratch: &mut [f32],
        block: Decoded<'_>,
        emit: impl FnMut(char),
    ) -> Result<usize, Error> {
        if !self.flush_mute(inbound)? || block.pcm.is_empty() {
            return Ok(0);
        }
        let start = block.start;
        let count = block.pcm.len();
        self.prepare(scratch, block, emit, || {
            inbound.signals().set_dtmf_muted(true)
        })?;
        let end = start + count as u64;
        let mut offered = [EMPTY; N];
        let mut intervals = 0;
        for interval in &self.spans[..self.span_count] {
            span(
                &mut offered,
                &mut intervals,
                interval.start,
                interval.end.min(end),
            );
        }
        let accepted = inbound
            .write_tagged(
                &scratch[..count],
                self.generation,
                start,
                &offered[..intervals],
            )
            .map_err(|_| Error::Write)?;
        self.accepted(accepted)?;
        if accepted != 0 {
            inbound.signals().set_dtmf_muted(self.muted());
        }
        Ok(accepted)
    }

    pub(super) fn gap(
        &mut self,
        inbound: &mut InboundProducer,
        start: u64,
        count: usize,
        emit: impl FnMut(char),
    ) -> Result<usize, Error> {
        if self.cached.is_some() {
            return Err(Error::InvalidFrame);
        }
        if !self.flush_mute(inbound)? {
            return Ok(0);
        }
        self.discontinuity(emit)?;
        inbound.signals().set_dtmf_muted(self.command_muted);
        let accepted = inbound
            .gap(count.min(4096) as u64)
            .map_err(|_| Error::Write)?;
        self.cursor = start + accepted;
        if self.command_muted && accepted != 0 {
            self.pending_mute = Some(MuteSpan {
                start,
                end: self.cursor,
            });
            self.flush_mute(inbound)?;
        }
        Ok(accepted as usize)
    }

    pub(super) fn finish(
        &mut self,
        inbound: &mut InboundProducer,
        mut emit: impl FnMut(char),
    ) -> Result<bool, Error> {
        if self.cached.is_some() {
            return Err(Error::InvalidFrame);
        }
        if !self.flush_mute(inbound)? {
            return Ok(false);
        }
        // A successful finish immediately installs PeerSession's finite token;
        // further polls use that token rather than finishing the source again.
        self.discontinuity(&mut emit)?;
        self.command_muted = false;
        inbound.signals().set_dtmf_muted(false);
        Ok(true)
    }

    pub(super) fn timeout(&mut self, inbound: &InboundProducer) {
        self.command_muted = false;
        if self.cached.is_none() && self.pending_mute.is_none() {
            inbound
                .signals()
                .set_dtmf_muted(self.detector.suppressing());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::five_tone;
    use super::*;
    use crate::link::ring::{InboundPolicy, InboundRing, tests::delayed_mute_endpoints};
    use rpt_advanced_core::link::PeerSignals;

    #[test]
    fn clipped_empty_spans_are_ignored_and_disjoint_metadata_is_bounded() {
        let signals = PeerSignals::new();
        let mut first = None;
        collect_mute_start(
            &mut first,
            || signals.set_dtmf_muted(true),
            100,
            164,
            100,
            -64..0,
        );
        assert_eq!(first, None);
        assert!(!signals.dtmf_muted());
        collect_mute_start(
            &mut first,
            || signals.set_dtmf_muted(true),
            100,
            164,
            100,
            32..100,
        );
        assert_eq!(first, Some(132));
        assert!(signals.dtmf_muted());
        collect_mute_start(
            &mut first,
            || signals.set_dtmf_muted(true),
            100,
            164,
            100,
            -32..16,
        );
        assert_eq!(first, Some(100));
        let mut spans = [EMPTY; 64];
        let mut count = 0;
        span(&mut spans, &mut count, 10, 10);
        span(&mut spans, &mut count, 10, 9);
        assert_eq!(count, 0);
        for index in 0..64 {
            span(&mut spans, &mut count, index * 2, index * 2 + 1);
        }
        assert_eq!(count, 64);
        assert_eq!((spans[63].start, spans[63].end), (126, 127));
    }

    #[test]
    fn qualified_digits_end_at_loss_and_concealment_remains_muted_without_redecoding() {
        for (concealed, muting, digit) in [
            (false, true, '5'),
            (true, true, '5'),
            (false, false, '5'),
            (true, false, '5'),
            (false, true, '#'),
            (true, true, '#'),
        ] {
            let (mut inbound, _) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
            let mut source = SourceIngress::new(muting).unwrap();
            let mut scratch = [0.0; 4096];
            let mut digits = Vec::new();
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start: 0,
                        pcm: &[],
                        concealed: false
                    },
                    |_| {}
                ),
                Ok(0)
            );
            let tone = if digit == '#' {
                (0..256)
                    .map(|n| {
                        let phase = n as f32 * std::f32::consts::TAU / 8000.0;
                        0.25 * ((phase * 941.0).sin() + (phase * 1477.0).sin())
                    })
                    .collect()
            } else {
                five_tone(256)
            };
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start: 0,
                        pcm: &tone,
                        concealed: false
                    },
                    |digit| digits.push(digit)
                ),
                Ok(256)
            );
            if concealed {
                assert_eq!(
                    source.audio(
                        &mut inbound,
                        &mut scratch,
                        Decoded {
                            birth: None,
                            start: 256,
                            pcm: &tone[..160],
                            concealed: true
                        },
                        |digit| digits.push(digit)
                    ),
                    Ok(160)
                );
            } else {
                assert_eq!(
                    source.gap(&mut inbound, 256, 160, |digit| digits.push(digit)),
                    Ok(160)
                );
            }
            assert_eq!(digits, [digit]);
            assert_eq!(inbound.signals().dtmf_muted(), muting && digit != '#');
            source.timeout(&inbound);
            assert!(!inbound.signals().dtmf_muted());
            assert_eq!(
                source.finish(&mut inbound, |digit| digits.push(digit)),
                Ok(true)
            );
            assert_eq!(digits, [digit]);
        }
    }

    #[test]
    fn enabling_muting_during_digit_completion_preserves_the_completed_command() {
        let (mut inbound, _) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
        let mut source = SourceIngress::new(false).unwrap();
        let mut scratch = [0.0; 4096];
        let mut digits = Vec::new();
        // Two 12.75-ms analysis windows qualify the tone; two quiet windows
        // leave its completion pending until the third quiet window below.
        for (start, pcm) in [(0, five_tone(204)), (204, vec![0.0; 204])] {
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start,
                        pcm: &pcm,
                        concealed: false
                    },
                    |digit| digits.push(digit)
                ),
                Ok(pcm.len())
            );
        }
        assert!(digits.is_empty());
        source.set_muting(true);
        assert_eq!(
            source.audio(
                &mut inbound,
                &mut scratch,
                Decoded {
                    birth: None,
                    start: 408,
                    pcm: &[0.0; 102],
                    concealed: false
                },
                |digit| digits.push(digit)
            ),
            Ok(102)
        );
        assert_eq!(digits, ['5']);
        assert!(inbound.signals().dtmf_muted());
    }

    #[test]
    fn full_ring_gap_keeps_command_muting_without_fabricating_sample_credit() {
        let (mut inbound, _) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
        let mut source = SourceIngress::new(true).unwrap();
        let mut scratch = [0.0; 4096];
        let mut digits = Vec::new();
        for (start, pcm) in [
            (0, five_tone(256)),
            (256, vec![0.0; 4096]),
            (4352, vec![0.0; 1824]),
        ] {
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start,
                        pcm: &pcm,
                        concealed: false
                    },
                    |digit| digits.push(digit)
                ),
                Ok(pcm.len())
            );
        }
        assert_eq!(digits, ['5']);
        assert_eq!(source.gap(&mut inbound, 6176, 1, |_| {}), Ok(0));
        assert!(inbound.signals().dtmf_muted());
        assert_eq!(
            inbound.observer().observe().unwrap().available_samples,
            6176
        );
    }

    #[test]
    fn rejected_pcm_suffix_must_keep_position_length_and_concealment_identity() {
        let (mut inbound, _) = InboundRing::open(8000, InboundPolicy::Echo).unwrap();
        assert_eq!(inbound.write(&[0.1; 4096]), Ok(4096));
        let mut source = SourceIngress::new(true).unwrap();
        let mut scratch = [0.0; 4096];
        let pcm = five_tone(4096);
        assert_eq!(
            source.audio(
                &mut inbound,
                &mut scratch,
                Decoded {
                    birth: None,
                    start: 4096,
                    pcm: &pcm,
                    concealed: false
                },
                |_| {}
            ),
            Ok(2080)
        );
        let remaining = &pcm[2080..];
        for (start, samples, concealed) in [
            (6177, remaining, false),
            (6176, &pcm[..2017], false),
            (6176, remaining, true),
        ] {
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start,
                        pcm: samples,
                        concealed
                    },
                    |_| {}
                ),
                Err(Error::InvalidFrame)
            );
        }
        assert_eq!(
            source.gap(&mut inbound, 6176, 1, |_| {}),
            Err(Error::InvalidFrame)
        );
        assert_eq!(
            source.finish(&mut inbound, |_| {}),
            Err(Error::InvalidFrame)
        );
        assert_eq!(
            source.audio(
                &mut inbound,
                &mut scratch,
                Decoded {
                    birth: None,
                    start: 6176,
                    pcm: remaining,
                    concealed: false
                },
                |_| {}
            ),
            Ok(0)
        );
        assert_eq!(
            inbound.observer().observe().unwrap().available_samples,
            6176
        );
        source.timeout(&inbound);
        assert!(inbound.signals().dtmf_muted());
    }

    #[test]
    fn pending_mute_metadata_stalls_audio_gap_and_finish_without_replaying_digits() {
        for operation in 0..3 {
            let (mut inbound, _) = delayed_mute_endpoints(false);
            let mut source = SourceIngress::new(true).unwrap();
            let mut scratch = [0.0; 4096];
            let mut digits = Vec::new();
            assert_eq!(
                source.audio(
                    &mut inbound,
                    &mut scratch,
                    Decoded {
                        birth: None,
                        start: 0,
                        pcm: &five_tone(256),
                        concealed: false
                    },
                    |digit| digits.push(digit)
                ),
                Ok(256)
            );
            assert_eq!(
                source.gap(&mut inbound, 256, 160, |digit| digits.push(digit)),
                Ok(160)
            );
            source.timeout(&inbound);
            assert!(inbound.signals().dtmf_muted());
            match operation {
                0 => assert_eq!(
                    source.audio(
                        &mut inbound,
                        &mut scratch,
                        Decoded {
                            birth: None,
                            start: 416,
                            pcm: &[],
                            concealed: false
                        },
                        |digit| digits.push(digit)
                    ),
                    Ok(0)
                ),
                1 => assert_eq!(
                    source.gap(&mut inbound, 416, 1, |digit| digits.push(digit)),
                    Ok(0)
                ),
                _ => assert_eq!(
                    source.finish(&mut inbound, |digit| digits.push(digit)),
                    Ok(false)
                ),
            }
            assert_eq!(inbound.observer().observe().unwrap().available_samples, 416);
            assert_eq!(
                source.finish(&mut inbound, |digit| digits.push(digit)),
                Ok(true)
            );
            assert_eq!(digits, ['5']);
            assert!(!inbound.signals().dtmf_muted());
        }
        let (mut inbound, _) = delayed_mute_endpoints(true);
        let mut source = SourceIngress::new(true).unwrap();
        let mut scratch = [0.0; 4096];
        assert_eq!(
            source.audio(
                &mut inbound,
                &mut scratch,
                Decoded {
                    birth: None,
                    start: 0,
                    pcm: &five_tone(256),
                    concealed: false
                },
                |_| {}
            ),
            Ok(256)
        );
        assert_eq!(
            source.gap(&mut inbound, 256, 160, |_| {}),
            Err(Error::Write)
        );
        assert_eq!(source.finish(&mut inbound, |_| {}), Err(Error::Write));
    }
    #[test]
    fn source_reset_completes_once_while_cancelling_partial_retry_without_allocation() {
        for rate in [8000, 48000] {
            for accepted in [0, 1] {
                let part = rate as usize / 50;
                let mut source = SourceIngress::<4>::at_rate(true, rate, part).unwrap();
                let tone: Vec<_> = (0..part * 4)
                    .map(|n| {
                        let phase = n as f32 * std::f32::consts::TAU / rate as f32;
                        0.25 * ((phase * 770.0).sin() + (phase * 1336.0).sin())
                    })
                    .collect();
                let mut scratch = [0.0; 960];
                for (index, pcm) in tone.chunks(part).enumerate() {
                    source
                        .prepare(
                            &mut scratch,
                            Decoded {
                                start: (index * part) as u64,
                                pcm,
                                concealed: false,
                                birth: None,
                            },
                            |_| panic!("the active tone has not ended"),
                            || {},
                        )
                        .unwrap();
                    source
                        .accepted(if index == 3 { accepted } else { part })
                        .unwrap();
                }
                let mut digit = None;
                let (_, allocations) = crate::fixture::allocations(|| {
                    source
                        .reset(|symbol| {
                            assert_eq!(digit.replace(symbol), None);
                        })
                        .unwrap();
                    source
                        .reset(|_| panic!("reset cannot repeat completion"))
                        .unwrap();
                });
                assert_eq!(allocations, 0);
                assert_eq!(digit, Some('5'));
                assert!(!source.muted());
                assert_eq!(source.limit(part), part);
                source
                    .prepare(
                        &mut scratch,
                        Decoded {
                            start: 0,
                            pcm: &tone[..part],
                            concealed: false,
                            birth: None,
                        },
                        |_| panic!("the new range must not inherit qualification"),
                        || {},
                    )
                    .unwrap();
                source.accepted(part).unwrap();
            }
        }
    }

    #[test]
    fn private_source_analysis_cached_retry_keeps_birth_and_does_not_decode_twice() {
        let mut source = SourceIngress::<4>::at_rate(true, 8000, 160).unwrap();
        let mut scratch = [0.0; 960];
        let birth = Some(SourceBirth {
            host_clock_epoch: 7,
            born_ns: 9,
        });
        let mut digits = ['?'; 4];
        let mut emitted = 0;
        let mut start = 0;
        let tone = five_tone(640);
        let quiet = [0.0; 160];
        for pcm in tone.chunks(160).chain(std::iter::once(quiet.as_slice())) {
            source
                .prepare(
                    &mut scratch,
                    Decoded {
                        start,
                        pcm,
                        concealed: false,
                        birth,
                    },
                    |digit| {
                        digits[emitted] = digit;
                        emitted += 1;
                    },
                    || {},
                )
                .unwrap();
            source.accepted(160).unwrap();
            start += 160;
        }
        let raw = [0.0; 160];
        source
            .prepare(
                &mut scratch,
                Decoded {
                    start,
                    pcm: &raw,
                    concealed: false,
                    birth,
                },
                |digit| {
                    digits[emitted] = digit;
                    emitted += 1;
                },
                || {},
            )
            .unwrap();
        assert_eq!((&digits[..emitted], emitted), (&['5'][..], 1));
        source.accepted(1).unwrap();
        scratch.fill(0.25);
        let changed = Some(SourceBirth {
            host_clock_epoch: 7,
            born_ns: 10,
        });
        assert_eq!(
            source.prepare(
                &mut scratch,
                Decoded {
                    start: start + 1,
                    pcm: &raw[1..],
                    concealed: false,
                    birth: changed
                },
                |_| panic!("retry cannot decode"),
                || {}
            ),
            Err(Error::InvalidFrame)
        );
        let (_, allocations) = crate::fixture::allocations(|| {
            source
                .prepare(
                    &mut scratch,
                    Decoded {
                        start: start + 1,
                        pcm: &raw[1..],
                        concealed: false,
                        birth,
                    },
                    |_| panic!("retry cannot decode"),
                    || {},
                )
                .unwrap();
            source.accepted(0).unwrap();
            scratch.fill(0.5);
            source
                .prepare(
                    &mut scratch,
                    Decoded {
                        start: start + 1,
                        pcm: &raw[1..],
                        concealed: false,
                        birth,
                    },
                    |_| panic!("retry cannot decode"),
                    || {},
                )
                .unwrap();
            source.accepted(159).unwrap();
        });
        assert_eq!(allocations, 0);
        assert_eq!(&scratch[..159], &[0.0; 159]);
        eprintln!(
            "source_analysis_bytes ordinary={} private={}",
            std::mem::size_of::<SourceIngress>(),
            std::mem::size_of::<SourceIngress<4>>()
        );
    }
}
