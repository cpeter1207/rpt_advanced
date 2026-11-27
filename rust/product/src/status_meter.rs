//! Bounded, allocation-free observations of the actual 48 kHz mono callback signals.
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(crate) const NATIVE_RATE: u32 = 48_000;
#[cfg(test)]
const BLOCK: usize = (NATIVE_RATE / 20) as usize;
const RMS_BLOCKS: usize = 2;
const LUFS_BLOCKS: usize = 8;
const HOLD_BLOCKS: usize = 40;
const QUEUE: usize = 4;
pub(crate) const LOCAL_TAP_COUNT: usize = 10;

/// The original mono span and its owner identifiers, copied by the audio callback.
#[derive(Clone, Copy)]
pub(crate) struct Span {
    pub(crate) graph_generation: u64,
    pub(crate) source_generation: u64,
    pub(crate) first_sample: u64,
    pub(crate) end_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Quality {
    #[default]
    Warming,
    Valid,
    Fault,
    Unavailable,
}

impl Quality {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Warming => "warming",
            Self::Valid => "valid",
            Self::Fault => "fault",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SignalState {
    #[default]
    Unknown,
    Present,
    Silence,
}

impl SignalState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Present => "present",
            Self::Silence => "silence",
        }
    }
}

/// One published window. Linear audio is never retained or exposed by the observer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Measurement {
    pub(crate) graph_generation: u64,
    pub(crate) measurement_epoch: u64,
    /// Identity of this original PCM timeline, qualified by the graph and epoch.
    pub(crate) source_generation: u64,
    /// Exclusive sample position of the window endpoint on that PCM timeline.
    pub(crate) end_sample: u64,
    pub(crate) monotonic_ms: u64,
    pub(crate) valid_samples: u64,
    pub(crate) clipped_samples: u64,
    pub(crate) dropped_snapshots: u64,
    pub(crate) peak_dbfs: Option<f64>,
    pub(crate) rms_dbfs: Option<f64>,
    pub(crate) lufs: Option<f64>,
    pub(crate) peak_hold_dbfs: Option<f64>,
    pub(crate) quality: Quality,
    pub(crate) signal_state: SignalState,
}

/// Known boundaries, including expected provider points without public PCM.
pub(crate) struct Stage {
    pub(crate) id: &'static str,
    pub(crate) purpose: &'static str,
    pub(crate) inputs: &'static [&'static str],
    pub(crate) tap: Option<usize>,
}

pub(crate) struct Chain {
    pub(crate) id: &'static str,
    pub(crate) direction: &'static str,
    pub(crate) stages: &'static [Stage],
}

const fn stage(
    id: &'static str,
    purpose: &'static str,
    inputs: &'static [&'static str],
    tap: Option<usize>,
) -> Stage {
    Stage {
        id,
        purpose,
        inputs,
        tap,
    }
}

/// Ordered local PCM boundaries. Provider internals have no numerical measurements.
pub(crate) const LOCAL_CHAINS: &[Chain] = &[
    Chain {
        id: "local_rx",
        direction: "receive",
        stages: &[
            stage("device_input", "capture", &[], None),
            stage(
                "radio_receive_dsp",
                "channel_conversion_and_receive_processing",
                &["device_input"],
                None,
            ),
            stage(
                "detector_input",
                "speech_and_signaling_detector_input",
                &["radio_receive_dsp"],
                None,
            ),
            stage(
                "provider_output",
                "native_mono_receive_output",
                &["radio_receive_dsp"],
                Some(0),
            ),
            stage(
                "local_gate",
                "dtmf_and_carrier_gate_output",
                &["provider_output"],
                Some(1),
            ),
            stage(
                "local_ring_output",
                "local_delay_and_handoff_output",
                &["local_gate"],
                Some(2),
            ),
            stage(
                "station_input",
                "qualified_receive_mix_input",
                &["local_ring_output"],
                Some(6),
            ),
        ],
    },
    Chain {
        id: "local_tx",
        direction: "transmit",
        stages: &[
            stage("voice", "voice_branch", &[], Some(4)),
            stage(
                "telemetry_input",
                "public_telemetry_before_ducking",
                &[],
                Some(7),
            ),
            stage(
                "telemetry",
                "telemetry_branch",
                &["telemetry_input"],
                Some(5),
            ),
            stage("incoming_text", "independent_local_text", &[], Some(8)),
            stage("regeneration", "finite_signaling_override", &[], Some(9)),
            stage(
                "programme",
                "combined_controller_programme",
                &["voice", "telemetry", "incoming_text", "regeneration"],
                Some(3),
            ),
            stage(
                "radio_transmit_dsp",
                "transmit_processing",
                &["programme"],
                None,
            ),
            stage(
                "signaling",
                "signaling_injection",
                &["radio_transmit_dsp"],
                None,
            ),
            stage("device_output", "digital_output", &["signaling"], None),
        ],
    },
];

#[derive(Clone, Copy, Default)]
struct Bucket {
    samples: usize,
    peak: f64,
    square_sum: f64,
    k_square_sum: f64,
}

struct KWeight {
    shelf_state: [f64; 2],
    high_pass_state: [f64; 2],
    shelf_feed: [f64; 3],
    shelf_feedback: [f64; 2],
    high_pass_feedback: [f64; 2],
}

fn biquad(input: f64, state: &mut [f64; 2], b: [f64; 3], a: [f64; 2]) -> f64 {
    let output = b[0] * input + state[0];
    state[0] = b[1] * input - a[0] * output + state[1];
    state[1] = b[2] * input - a[1] * output;
    output
}

impl KWeight {
    fn new(rate: u32) -> Self {
        // Annex 1 of ITU-R BS.1770-5 gives the two 48 kHz target filters.
        // Recompute those analog prototypes at the actual negotiated rate,
        // as in FFmpeg's ebur128_init_filter.
        let pole = |frequency: f64, q: f64| {
            let scaled = (std::f64::consts::PI * frequency / f64::from(rate)).tan();
            let square = scaled * scaled;
            let damping = scaled / q;
            let normalizer = 1.0 + damping + square;
            let feedback = [
                2.0 * (square - 1.0) / normalizer,
                (1.0 - damping + square) / normalizer,
            ];
            (square, damping, normalizer, feedback)
        };
        let (square, damping, denominator, shelf_feedback) =
            pole(1_681.974_450_955_533, 0.707_175_236_955_419_6);
        let high_gain = 10.0_f64.powf(3.999_843_853_973_347 / 20.0);
        let transition = damping * high_gain.powf(0.499_666_774_154_541_6);
        let shelf_feed = [
            (high_gain + transition + square) / denominator,
            2.0 * (square - high_gain) / denominator,
            (high_gain - transition + square) / denominator,
        ];
        let (_, _, _, high_pass_feedback) = pole(38.135_470_876_024_44, 0.500_327_037_323_877_3);
        Self {
            shelf_state: [0.0; 2],
            high_pass_state: [0.0; 2],
            shelf_feed,
            shelf_feedback,
            high_pass_feedback,
        }
    }

    fn reset(&mut self) {
        self.shelf_state = [0.0; 2];
        self.high_pass_state = [0.0; 2];
    }

    fn apply(&mut self, input: f64) -> f64 {
        let shelf = biquad(
            input,
            &mut self.shelf_state,
            self.shelf_feed,
            self.shelf_feedback,
        );
        biquad(
            shelf,
            &mut self.high_pass_state,
            [1.0, -2.0, 1.0],
            self.high_pass_feedback,
        )
    }
}

struct Windows {
    rate: u32,
    block: usize,
    epoch: u64,
    position: Option<(u64, u64, u64, u64)>,
    weight: KWeight,
    accumulating: Bucket,
    history: [Bucket; HOLD_BLOCKS],
    next_bucket: usize,
    history_len: usize,
    clipped_samples: u64,
}

impl Windows {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            block: (rate / 20) as usize,
            epoch: 0,
            position: None,
            weight: KWeight::new(rate),
            accumulating: Bucket::default(),
            history: [Bucket::default(); HOLD_BLOCKS],
            next_bucket: 0,
            history_len: 0,
            clipped_samples: 0,
        }
    }
}

fn logarithmic(value: f64, factor: f64, offset: f64) -> Option<f64> {
    (value > 0.0).then(|| factor * value.log10() + offset)
}

impl Windows {
    fn reset(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.position = None;
        self.weight.reset();
        self.accumulating = Bucket::default();
        self.next_bucket = 0;
        self.history_len = 0;
        self.clipped_samples = 0;
    }

    fn push(&mut self, sample: f64) {
        let magnitude = sample.abs();
        let weighted = self.weight.apply(sample);
        self.accumulating.samples += 1;
        self.accumulating.peak = self.accumulating.peak.max(magnitude);
        self.accumulating.square_sum += sample * sample;
        self.accumulating.k_square_sum += weighted * weighted;
        self.clipped_samples = self
            .clipped_samples
            .saturating_add(u64::from(magnitude >= 1.0));
    }

    fn complete(&mut self, span: Span, end_sample: u64, monotonic_ms: u64) -> Measurement {
        let bucket = std::mem::take(&mut self.accumulating);
        self.history[self.next_bucket] = bucket;
        self.next_bucket = (self.next_bucket + 1) % HOLD_BLOCKS;
        self.history_len = (self.history_len + 1).min(HOLD_BLOCKS);
        let recent = |count| {
            (0..self.history_len.min(count)).map(|offset| {
                self.history[(self.next_bucket + HOLD_BLOCKS - 1 - offset) % HOLD_BLOCKS]
            })
        };
        let max_peak = |count| recent(count).map(|item| item.peak).fold(0.0, f64::max);
        let sample_ready = self.history_len >= RMS_BLOCKS;
        let loudness_ready = self.history_len >= LUFS_BLOCKS;
        let loudness_samples = (LUFS_BLOCKS * self.block) as f64;
        let lufs = if loudness_ready && recent(LUFS_BLOCKS).any(|item| item.peak > 0.0) {
            let weighted: f64 = recent(LUFS_BLOCKS).map(|item| item.k_square_sum).sum();
            logarithmic(weighted / loudness_samples, 10.0, -0.691)
        } else {
            None
        };
        Measurement {
            graph_generation: span.graph_generation,
            measurement_epoch: self.epoch,
            source_generation: span.source_generation,
            end_sample,
            monotonic_ms,
            valid_samples: (self.history_len.min(LUFS_BLOCKS) * self.block) as u64,
            clipped_samples: self.clipped_samples,
            peak_dbfs: sample_ready
                .then(|| logarithmic(max_peak(RMS_BLOCKS), 20.0, 0.0))
                .flatten(),
            rms_dbfs: sample_ready
                .then(|| {
                    let squares: f64 = recent(RMS_BLOCKS).map(|item| item.square_sum).sum();
                    logarithmic(squares / (RMS_BLOCKS * self.block) as f64, 10.0, 0.0)
                })
                .flatten(),
            lufs,
            peak_hold_dbfs: logarithmic(max_peak(HOLD_BLOCKS), 20.0, 0.0),
            quality: if loudness_ready {
                Quality::Valid
            } else {
                Quality::Warming
            },
            signal_state: if bucket.peak > 0.0 {
                SignalState::Present
            } else if loudness_ready {
                SignalState::Silence
            } else {
                SignalState::Unknown
            },
            ..Measurement::default()
        }
    }
}

/// One preallocated writer per exact mono signal. It runs only on that signal's sole owner.
pub(crate) struct Tap {
    windows: Windows,
    writer: Producer<Measurement>,
    current_epoch: Arc<AtomicU64>,
    dropped: u64,
}

/// One bounded reader on the control side, shared by all management views.
pub(crate) struct Reader {
    reader: Consumer<Measurement>,
    current_epoch: Arc<AtomicU64>,
    latest: Option<Measurement>,
}

impl Reader {
    /// Retain only the newest reported value; discard in-flight values from retired epochs.
    pub(crate) fn latest(&mut self) -> Option<Measurement> {
        while let Ok(measurement) = self.reader.pop() {
            self.latest = Some(measurement);
        }
        self.current()
    }

    pub(crate) fn current(&self) -> Option<Measurement> {
        self.latest.filter(|measurement| {
            measurement.measurement_epoch == self.current_epoch.load(Ordering::Acquire)
        })
    }
}

impl Tap {
    /// Reserve both endpoints before any audio callback can use them.
    pub(crate) fn new() -> (Self, Reader) {
        Self::at_rate(NATIVE_RATE).expect("the controller's fixed mono rate has 50 ms blocks")
    }

    /// Reserve a matched pair for an exact negotiated rate and integral 50 ms windows.
    pub(crate) fn at_rate(rate: u32) -> Option<(Self, Reader)> {
        if !(8_000..=192_000).contains(&rate) || rate % 20 != 0 {
            return None;
        }
        let (writer, reader) = RingBuffer::new(QUEUE);
        let current_epoch = Arc::new(AtomicU64::new(0));
        Some((
            Self {
                windows: Windows::new(rate),
                writer,
                current_epoch: current_epoch.clone(),
                dropped: 0,
            },
            Reader {
                reader,
                current_epoch,
                latest: None,
            },
        ))
    }

    fn reset(&mut self) {
        self.windows.reset();
        self.current_epoch
            .store(self.windows.epoch, Ordering::Release);
        self.dropped = 0;
    }

    fn publish(&mut self, mut measurement: Measurement) {
        measurement.dropped_snapshots = self.dropped;
        if self.writer.push(measurement).is_err() {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Retire the existing window immediately on provider loss or malformed PCM.
    pub(crate) fn invalidate(&mut self, quality: Quality) {
        let Some((graph_generation, source_generation, end_sample, monotonic_ms)) =
            self.windows.position
        else {
            return;
        };
        self.reset();
        self.publish(Measurement {
            graph_generation,
            measurement_epoch: self.windows.epoch,
            source_generation,
            end_sample,
            monotonic_ms,
            quality,
            ..Measurement::default()
        });
    }

    /// Advance consecutive samples using their original generation and position.
    pub(crate) fn observe(&mut self, span: Span, samples: &[f32]) {
        self.observe_samples(span, samples.iter().copied());
    }

    /// Advance an exactly silent mixer contribution on the same sample clock.
    pub(crate) fn observe_silence(&mut self, span: Span, frames: usize) {
        self.observe_samples(span, std::iter::repeat_n(0.0, frames));
    }

    fn observe_samples(&mut self, span: Span, samples: impl ExactSizeIterator<Item = f32>) {
        let count = samples.len();
        if count == 0 {
            return;
        }
        let Some(next) = span.first_sample.checked_add(count as u64) else {
            self.invalidate(Quality::Fault);
            return;
        };
        let rate = u64::from(self.windows.rate);
        let ms = count as u64 * 1000 / rate;
        if self
            .windows
            .position
            .is_none_or(|(graph, source, next, last_ms)| {
                (graph, source, next)
                    != (
                        span.graph_generation,
                        span.source_generation,
                        span.first_sample,
                    )
                    || span.end_ms.saturating_sub(last_ms) > ms + 500
            })
        {
            self.reset();
        }
        self.windows.position = Some((
            span.graph_generation,
            span.source_generation,
            next,
            span.end_ms,
        ));
        for (index, sample) in samples.enumerate() {
            if !sample.is_finite() {
                self.invalidate(Quality::Fault);
                return;
            }
            self.windows.push(f64::from(sample));
            if self.windows.accumulating.samples == self.windows.block {
                let remaining = (count - index - 1) as u64 * 1000 / rate;
                let measurement = self.windows.complete(
                    span,
                    span.first_sample + index as u64 + 1,
                    span.end_ms.saturating_sub(remaining),
                );
                self.publish(measurement);
            }
        }
    }
}

#[cfg(test)]
#[path = "status_meter_tests.rs"]
mod tests;
