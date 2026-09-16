//! Control-prepared playback and bounded status ownership transfer.

use crate::audio::{MorseRenderer, Playback};
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
    /// Prepare fixed-48-kHz normalized PCM and its Morse fallback.
    pub fn new(
        audio: Option<Vec<f32>>,
        text: &str,
        settings: MorseSettings,
    ) -> Result<Self, ControllerError> {
        if audio.as_ref().is_some_and(|pcm| {
            pcm.is_empty() || pcm.iter().any(|v| !v.is_finite() || v.abs() > 1.0)
        }) {
            return Err(ControllerError);
        }
        let available = audio.is_some() || !text.is_empty();
        Playback::new(
            audio,
            text,
            settings.speed_wpm,
            settings.frequency_hz,
            settings.level_db,
            false,
        )
        .map(|playback| Self(playback, available))
        .map_err(|_| ControllerError)
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
    settings: MorseSettings,
    outstanding: usize,
}

impl ControllerControl {
    /// Prepare and enqueue printable status, retaining input ownership on failure.
    pub fn queue_status(
        &mut self,
        text: &str,
        audio: Option<Vec<f32>>,
    ) -> Result<(), StatusRejected> {
        let valid = !text.is_empty()
            && text.len() < 128
            && audio.as_ref().is_none_or(|pcm| {
                !pcm.is_empty() && pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0)
            })
            && MorseRenderer::new(
                text,
                self.settings.speed_wpm,
                self.settings.frequency_hz,
                self.settings.level_db,
            )
            .is_ok();
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
