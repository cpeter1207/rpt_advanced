//! Control-prepared playback and bounded status ownership transfer.

#[cfg(test)]
use crate::audio::{MorseRenderer, PcmRead, ToneSequence};
use crate::audio::{PcmStreamReader, Playback};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Settings shared by control-prepared Morse fallbacks.
#[derive(Clone, Copy)]
pub struct MorseSettings {
    /// Words per minute.
    pub speed_wpm: u32,
    /// Tone frequency in Hz.
    pub frequency_hz: f32,
    /// Tone level relative to full scale.
    pub level_db: i8,
}

impl Default for MorseSettings {
    fn default() -> Self {
        Self {
            speed_wpm: 20,
            frequency_hz: 800.0,
            level_db: -6,
        }
    }
}

/// Rejected media, timing, or configuration value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerError;

/// Media validated and allocated exclusively by the control owner.
pub struct PreparedMedia(pub(super) Playback, pub(super) bool);

impl PreparedMedia {
    /// Attach a producer-backed native PCM stream to controller playback.
    pub fn new_stream(stream: Box<dyn PcmStreamReader>) -> Result<Self, ControllerError> {
        Ok(Self(Playback::new_stream(stream), true))
    }

    /// Build deterministic in-memory media for controller unit tests only.
    #[cfg(test)]
    pub fn new(
        audio: Option<Vec<f32>>,
        text: &str,
        settings: MorseSettings,
    ) -> Result<Self, ControllerError> {
        Self::new_with_tone(audio, None, text, settings)
    }

    /// Build deterministic in-memory media for controller unit tests only.
    #[cfg(test)]
    pub(crate) fn new_with_tone(
        audio: Option<Vec<f32>>,
        tone: Option<ToneSequence>,
        text: &str,
        settings: MorseSettings,
    ) -> Result<Self, ControllerError> {
        if audio.as_ref().is_some_and(|pcm| {
            pcm.is_empty() || pcm.iter().any(|v| !v.is_finite() || v.abs() > 1.0)
        }) {
            return Err(ControllerError);
        }
        let available = audio.is_some() || tone.is_some() || !text.is_empty();
        let fallback = render_morse(text, settings)?;
        let primary = if let Some(audio) = audio {
            audio
        } else if let Some(mut tone) = tone {
            render_source(&mut tone)?
        } else {
            fallback.clone()
        };
        Ok(Self(
            Playback::new_stream(Box::new(TestPcmStream {
                primary,
                fallback,
                offset: 0,
                use_fallback: false,
            })),
            available,
        ))
    }
}

/// PCM ownership returned when a status submission is invalid or queue capacity is exhausted.
#[derive(Debug)]
pub struct StatusRejected {
    /// Original caller-owned PCM, retained on every rejected submission.
    pub audio: Option<Vec<f32>>,
}

/// Lock-free qualifying-receive snapshot shared with the control owner.
#[derive(Clone)]
pub struct ActivitySnapshot(Arc<AtomicU64>);

impl ActivitySnapshot {
    pub(super) fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }
    pub(super) fn publish(&self, sample: u64) {
        self.0.store(sample.saturating_add(1), Ordering::Release);
    }
    /// Last local or linked receive sample since startup; telemetry never updates it.
    #[must_use]
    pub fn last_sample(&self) -> Option<u64> {
        self.0.load(Ordering::Acquire).checked_sub(1)
    }
}

/// Sole status producer and completed-media reclaimer; retain until audio stops.
pub struct ControllerControl {
    pending: Producer<PreparedMedia>,
    completed: Consumer<PreparedMedia>,
    #[cfg(test)]
    settings: MorseSettings,
    outstanding: usize,
}

impl ControllerControl {
    /// Enqueue producer-backed status media prepared by the station owner.
    pub fn queue_prepared_status(&mut self, media: PreparedMedia) -> bool {
        if self.outstanding == 4 {
            return false;
        }
        if self.pending.push(media).is_err() {
            return false;
        }
        self.outstanding += 1;
        true
    }

    /// Prepare and enqueue printable status, retaining input ownership on failure.
    #[cfg(test)]
    pub fn queue_status(
        &mut self,
        text: &str,
        audio: Option<Vec<f32>>,
    ) -> Result<(), StatusRejected> {
        let valid = !text.is_empty()
            && text.len() < 128
            && MorseRenderer::new(
                text,
                self.settings.speed_wpm,
                self.settings.frequency_hz,
                self.settings.level_db,
            )
            .is_ok()
            && audio.as_ref().is_none_or(|pcm| {
                !pcm.is_empty() && pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0)
            });
        if !valid || self.outstanding == 4 {
            return Err(StatusRejected { audio });
        }
        // Validation above guarantees construction; preparation and allocation stay here.
        let media = PreparedMedia::new(audio, text, self.settings).expect("validated status media");
        self.pending.push(media).ok();
        self.outstanding += 1;
        Ok(())
    }

    /// Reclaim completed media on the control owner; dropping each item releases its PCM.
    pub fn reclaim(&mut self) -> impl Iterator<Item = PreparedMedia> + '_ {
        std::iter::from_fn(|| {
            let media = self.completed.pop().ok()?;
            self.outstanding -= 1;
            Some(media)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Status,
    Courtesy(usize),
    Identifier(usize),
    Announcement(usize),
}

pub(super) struct TelemetryPlanner {
    pub active: Option<Source>,
    pub status: Option<PreparedMedia>,
    pending: Consumer<PreparedMedia>,
    completed: Producer<PreparedMedia>,
    pub gain: f32,
    duck_gain: f32,
}

impl TelemetryPlanner {
    pub fn new(settings: MorseSettings, duck_db: i8) -> (Self, ControllerControl) {
        #[cfg(not(test))]
        let _ = settings;
        let (producer, pending) = RingBuffer::new(4);
        let (completed, consumer) = RingBuffer::new(4);
        (
            Self {
                active: None,
                status: None,
                pending,
                completed,
                gain: 1.0,
                duck_gain: 10_f32.powf(f32::from(duck_db) / 20.0),
            },
            ControllerControl {
                pending: producer,
                completed: consumer,
                #[cfg(test)]
                settings,
                outstanding: 0,
            },
        )
    }
    pub fn status_pending(&self) -> bool {
        self.status.is_some() || !self.pending.is_empty()
    }
    pub fn start_status(&mut self) {
        self.status = self.pending.pop().ok();
        self.active = self.status.as_ref().map(|_| Source::Status);
    }
    pub fn finish_status(&mut self) {
        self.status.take().into_iter().for_each(|media| {
            // Admission permits at most four statuses across pending, active and completed.
            // Owning this status proves the four-slot completed queue has room.
            let _ = self.completed.push(media);
        });
    }
    pub fn gain_step(&mut self, receive: bool) -> f32 {
        let target = if receive { self.duck_gain } else { 1.0 };
        self.gain = if self.gain > target {
            (self.gain - 1.0 / 480.0).max(target)
        } else {
            (self.gain + 1.0 / 4800.0).min(target)
        };
        self.gain
    }
}

#[cfg(test)]
struct TestPcmStream {
    primary: Vec<f32>,
    fallback: Vec<f32>,
    offset: usize,
    use_fallback: bool,
}

#[cfg(test)]
impl PcmStreamReader for TestPcmStream {
    fn start(&mut self) {
        self.offset = 0;
    }
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        let source = if self.use_fallback {
            &self.fallback
        } else {
            &self.primary
        };
        let count = output.len().min(source.len().saturating_sub(self.offset));
        output[..count].copy_from_slice(&source[self.offset..self.offset + count]);
        self.offset += count;
        if count == 0 {
            PcmRead::Finished
        } else if self.offset == source.len() {
            PcmRead::FinalSamples(count)
        } else {
            PcmRead::Samples(count)
        }
    }
    fn select_morse_fallback(&mut self) -> bool {
        self.use_fallback = true;
        self.offset = 0;
        !self.fallback.is_empty()
    }
}

#[cfg(test)]
fn render_morse(text: &str, settings: MorseSettings) -> Result<Vec<f32>, ControllerError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut source = MorseRenderer::new(
        text,
        settings.speed_wpm,
        settings.frequency_hz,
        settings.level_db,
    )
    .map_err(|_| ControllerError)?;
    render_source(&mut source)
}

#[cfg(test)]
fn render_source(source: &mut impl crate::audio::AudioSource) -> Result<Vec<f32>, ControllerError> {
    let mut result = Vec::new();
    let mut block = [0.0; 2048];
    loop {
        let count = source.render(&mut block);
        result.try_reserve(count).map_err(|_| ControllerError)?;
        result.extend_from_slice(&block[..count]);
        if count < block.len() {
            return Ok(result);
        }
    }
}
