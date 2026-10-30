//! Source-rate detection and retained mute metadata; the provider owns raw retries.
use crate::{
    Error,
    link::ring::{InboundProducer, MuteSpan},
};
use rpt_advanced_core::{audio::DtmfDetector, link::PeerSignals};

const EMPTY: MuteSpan = MuteSpan { start: 0, end: 0 };

/// Collect valid lookback in this burst's half-open window before opening mute.
fn collect_mute_start(
    first: &mut Option<u64>,
    signals: &PeerSignals,
    start: u64,
    end: u64,
    burst_start: u64,
    range: std::ops::Range<i64>,
) {
    let from = start.saturating_add_signed(range.start).max(burst_start);
    let to = start.saturating_add_signed(range.end).min(end);
    if from < to {
        *first = Some(first.map_or(from, |old| old.min(from)));
        signals.set_dtmf_muted(true);
    }
}

#[derive(Clone, Copy)]
struct Cached {
    start: u64,
    count: usize,
    next: usize,
    concealed: bool,
}

pub(super) struct Decoded<'a> {
    pub start: u64,
    pub pcm: &'a [f32],
    pub concealed: bool,
}

/// One serial receive owner. Cached metadata describes transformed samples in
/// PeerSession's existing scratch block; raw rejected PCM remains provider-owned.
pub(super) struct SourceIngress {
    detector: DtmfDetector,
    muting: bool,
    command_muted: bool,
    generation: u64,
    burst_start: Option<u64>,
    cursor: u64,
    cached: Option<Cached>,
    spans: [MuteSpan; 64],
    span_count: usize,
    pending_mute: Option<MuteSpan>,
}

fn span(spans: &mut [MuteSpan; 64], count: &mut usize, start: u64, end: u64) -> Result<(), Error> {
    if start >= end {
        return Ok(());
    }
    if *count != 0 && start <= spans[*count - 1].end {
        let previous = &mut spans[*count - 1];
        previous.start = previous.start.min(start);
        previous.end = previous.end.max(end);
    } else {
        let output = spans.get_mut(*count).ok_or(Error::InvalidFrame)?;
        *output = MuteSpan { start, end };
        *count += 1;
    }
    Ok(())
}

impl SourceIngress {
    pub(super) fn new(muting: bool) -> Result<Self, Error> {
        Ok(Self {
            detector: DtmfDetector::at_rate(muting, 8000).map_err(|_| Error::UnsupportedFormat)?,
            muting,
            command_muted: false,
            generation: 1,
            burst_start: None,
            cursor: 0,
            cached: None,
            spans: [EMPTY; 64],
            span_count: 0,
            pending_mute: None,
        })
    }

    pub(super) fn reset(&mut self) -> Result<(), Error> {
        self.detector
            .process_spans(false, &mut [], |_| {}, |_| {})
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
    fn discontinuity(&mut self, mut emit: impl FnMut(char)) -> Result<(), Error> {
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
                |_| {},
            )
            .map_err(|_| Error::InvalidFrame)
    }

    pub(super) fn audio(
        &mut self,
        inbound: &mut InboundProducer,
        scratch: &mut [f32],
        block: Decoded<'_>,
        mut emit: impl FnMut(char),
    ) -> Result<usize, Error> {
        if !self.flush_mute(inbound)? || block.pcm.is_empty() {
            return Ok(0);
        }
        if self.cached.is_none() {
            // PeerIo bounds input to 4096 samples before entering this method;
            // the sole caller supplies its equally sized, preallocated scratch.
            let burst_start = *self.burst_start.get_or_insert(block.start);
            let concealed_muted =
                block.concealed && (self.command_muted || self.detector.suppressing());
            if block.concealed {
                self.discontinuity(&mut emit)?;
            }
            scratch[..block.pcm.len()].copy_from_slice(block.pcm);
            self.span_count = 0;
            // Eight-ms partitions bound the end-of-command mute granularity.
            // Each offered source block is detected once, never again on retry.
            for (part, pcm) in scratch[..block.pcm.len()].chunks_mut(64).enumerate() {
                let start = block.start + (part * 64) as u64;
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
                                    inbound.signals(),
                                    start,
                                    end,
                                    burst_start,
                                    range,
                                );
                            },
                        )
                        .map_err(|_| Error::InvalidFrame)?;
                }
                let muted = self.muting
                    && (was_muted || first.is_some() || digit.is_some() || concealed_muted);
                if muted {
                    // The complete muted suffix already covers every detector
                    // interval in this partition; publish their union once.
                    span(
                        &mut self.spans,
                        &mut self.span_count,
                        first.unwrap_or(start),
                        end,
                    )?;
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
            });
        }
        let cached = self.cached.as_mut().ok_or(Error::InvalidFrame)?;
        if block.start != cached.start + cached.next as u64
            || block.pcm.len() > cached.count - cached.next
            || block.concealed != cached.concealed
        {
            return Err(Error::InvalidFrame);
        }
        let end = block.start + block.pcm.len() as u64;
        let mut offered = [EMPTY; 64];
        let mut count = 0;
        for interval in &self.spans[..self.span_count] {
            span(
                &mut offered,
                &mut count,
                interval.start,
                interval.end.min(end),
            )?;
        }
        let accepted = inbound
            .write_tagged(
                &scratch[cached.next..cached.next + block.pcm.len()],
                self.generation,
                block.start,
                &offered[..count],
            )
            .map_err(|_| Error::Write)?;
        cached.next += accepted;
        self.cursor = block.start + accepted as u64;
        if cached.next == cached.count {
            self.cached = None;
        }
        if accepted != 0 {
            inbound
                .signals()
                .set_dtmf_muted(self.command_muted || self.detector.suppressing());
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

    #[test]
    fn clipped_empty_spans_are_ignored_and_disjoint_metadata_is_bounded() {
        let signals = PeerSignals::new();
        let mut first = None;
        collect_mute_start(&mut first, &signals, 100, 164, 100, -64..0);
        assert_eq!(first, None);
        assert!(!signals.dtmf_muted());
        collect_mute_start(&mut first, &signals, 100, 164, 100, 32..100);
        assert_eq!(first, Some(132));
        assert!(signals.dtmf_muted());
        collect_mute_start(&mut first, &signals, 100, 164, 100, -32..16);
        assert_eq!(first, Some(100));
        let mut spans = [EMPTY; 64];
        let mut count = 0;
        assert_eq!(span(&mut spans, &mut count, 10, 10), Ok(()));
        assert_eq!(span(&mut spans, &mut count, 10, 9), Ok(()));
        assert_eq!(count, 0);
        for index in 0..64 {
            assert_eq!(
                span(&mut spans, &mut count, index * 2, index * 2 + 1),
                Ok(())
            );
        }
        assert_eq!(count, 64);
        assert_eq!(
            span(&mut spans, &mut count, 128, 129),
            Err(Error::InvalidFrame)
        );
        assert_eq!(count, 64);
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
}
