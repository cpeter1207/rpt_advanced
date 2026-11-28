//! Configuration-to-controller preparation. All allocation and external media work is offline.

#[path = "prepare_modular.rs"]
mod modular;
pub(super) use modular::controller as modular_controller;

use crate::{
    config::{ConfigError, ResolvedIdentifierSettings},
    controller::{
        Announcement, ControllerControl, ControllerSettings, CourtesySettings, Identifier,
        MessagePreparation, MorseSettings, NodeController, PreparedMedia,
    },
    media::{
        FileRequest, MediaError, MediaSource, MorseSource, PreparedAudio, SpeechRequest,
        SpeechSource, StationMediaSession,
    },
    messages::{Message, MessageForms},
    schedule::{CivilTime, Weekday},
};

/// Decode a configured file for adapters that expose a non-streaming control API.
pub trait NativeFilePreparer {
    /// Decode the complete file away from an audio callback.
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

/// Synthesize speech for adapters that expose a non-streaming control API.
pub trait NativeSpeechPreparer {
    /// Synthesize the complete utterance away from an audio callback.
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

/// Required per-generation producer for every telemetry PCM source.
pub trait NativeMediaPreparer: NativeFilePreparer + NativeSpeechPreparer {
    /// Create the station producer before any telemetry is registered for this generation.
    fn station(
        &self,
        node: &str,
        generation: u64,
    ) -> Result<Box<dyn StationMediaSession>, MediaError>;
}

/// A preparation or lifecycle failure leaves the previous runtime configuration published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// Invalid document or scoped settings.
    Config(ConfigError),
    /// A required controller, template, route, or adapter cannot be prepared safely.
    Preparation,
    /// Callback adoption, work, or detachment is still pending.
    Busy,
    /// Device activation failed, but all previous leases were restored.
    Device,
    /// At least one previous device could not be restored; that node remains RF-safe/stopped.
    Restore,
    /// Required current civil time is unavailable.
    Clock,
    /// No running node has the requested identity.
    MissingNode,
    /// A message queue is full or a copied reservation is stale.
    Rejected,
    /// The requested domain operation has no available owner or adapter.
    Unavailable,
    /// Existing link policy denied, invalidated, or rejected an operation.
    Link(crate::link::AdmissionError),
}
impl From<ConfigError> for RuntimeError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

pub(super) fn streamed_status(
    station: &mut dyn StationMediaSession,
    settings: &ResolvedIdentifierSettings,
    morse_text: &str,
    speech_text: &str,
) -> Result<PreparedMedia, RuntimeError> {
    PreparedMedia::new_stream(status_stream(station, settings, morse_text, speech_text)?)
        .map_err(|_| RuntimeError::Preparation)
}

/// Prepare one existing status producer for either controller or call-private playback.
pub(super) fn status_stream(
    station: &mut dyn StationMediaSession,
    settings: &ResolvedIdentifierSettings,
    morse_text: &str,
    speech_text: &str,
) -> Result<Box<dyn crate::audio::PcmStreamReader>, RuntimeError> {
    let source = status_source(settings, morse_text, speech_text);
    station
        .register_once(source)
        .map_err(|_| RuntimeError::Preparation)
}

/// Preserve the existing status speech settings and complete Morse substitute in a recipe.
pub(super) fn status_source(
    settings: &ResolvedIdentifierSettings,
    morse_text: &str,
    speech_text: &str,
) -> MediaSource {
    MediaSource {
        file: None,
        speech: (!speech_text.is_empty()).then(|| SpeechSource {
            text: speech_text.to_owned(),
            model: settings.speech_model.clone().into(),
            speed_percent: settings.speech_speed_percent as u32,
            level_db: settings.speech_level_db as i32,
        }),
        provider_gain_db: 0,
        tone: None,
        morse: Some(MorseSource {
            text: morse_text.to_owned(),
            speed_wpm: settings.morse_speed_wpm as u32,
            frequency_hz: settings.morse_frequency_hz as f32,
            level_db: settings.morse_level_db as i8,
        }),
    }
}

/// Prepare an optional streamed level report followed by native-rate retained receive PCM.
pub(super) fn streamed_parrot(
    station: &mut dyn StationMediaSession,
    settings: &ResolvedIdentifierSettings,
    speech_text: &str,
    recording: PreparedAudio,
) -> Result<PreparedMedia, RuntimeError> {
    let report = if speech_text.is_empty() || settings.speech_model.is_empty() {
        None
    } else {
        station
            .register_once(MediaSource {
                file: None,
                speech: Some(SpeechSource {
                    text: speech_text.to_owned(),
                    model: settings.speech_model.clone().into(),
                    speed_percent: settings.speech_speed_percent as u32,
                    level_db: settings.speech_level_db as i32,
                }),
                provider_gain_db: 0,
                tone: None,
                morse: None,
            })
            .ok()
    };
    let recording = station
        .register_prepared(recording)
        .map_err(|_| RuntimeError::Preparation)?;
    Ok(PreparedMedia::new_parrot_stream(report, recording))
}

type PreparedController = (
    NodeController,
    ControllerControl,
    ResolvedIdentifierSettings,
    Box<dyn StationMediaSession>,
    super::composition::MessageRecipes,
);

fn set_timeout_media(
    controller: &mut NodeController,
    station: &mut dyn StationMediaSession,
    timeout_ms: u64,
    status: &ResolvedIdentifierSettings,
    catalog: &crate::messages::MessageCatalog,
    civil: Option<CivilTime>,
) -> Result<(), RuntimeError> {
    if timeout_ms != 0 {
        // Duration month lengths use the captured date when available. Ordinary runtimes
        // without calendar scheduling retain a deterministic date instead of requiring a clock.
        let local = match civil {
            Some(local) => local,
            None => CivilTime::new(2000, 1, 1, Weekday::Saturday, 0, 0)
                .expect("fixed fallback calendar date is valid"),
        };
        let timed_out = catalog
            .format_timeout(timeout_ms.div_ceil(1000), local)
            .map_err(|_| RuntimeError::Preparation)?;
        let cleared = catalog
            .format(&Message::RepeaterTimeoutCleared)
            .map_err(|_| RuntimeError::Preparation)?;
        let mut register = |forms: MessageForms| {
            let stream = station
                .register(status_source(status, &forms.morse, &forms.tts))
                .map_err(|_| RuntimeError::Preparation)?;
            PreparedMedia::new_stream(stream).map_err(|_| RuntimeError::Preparation)
        };
        controller.set_timeout_media([register(timed_out)?, register(cleared)?]);
    }
    Ok(())
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
