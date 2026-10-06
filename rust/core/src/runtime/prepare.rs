//! Configuration-to-controller preparation. All allocation and external media work is offline.

use crate::{
    config::{
        ConfigDocument, ConfigError, NodeId, ResolvedAnnouncementSettings,
        ResolvedCourtesySettings, ResolvedIdentifierSettings, ResolvedNodeSettings,
    },
    controller::{
        Announcement, ControllerControl, ControllerSettings, CourtesySettings, Identifier,
        MorseSettings, NodeController, PreparedMedia,
    },
    media::{
        FileRequest, MediaError, MediaSource, MorseSource, PreparedAudio, SpeechRequest,
        SpeechSource, StationMediaSession, ToneSource,
    },
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
    /// Existing link policy denied, invalidated, or rejected an operation.
    Link(crate::link::AdmissionError),
}
impl From<ConfigError> for RuntimeError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

pub(super) fn labels<'a>(document: &'a ConfigDocument, family: &str, node: &str) -> Vec<&'a str> {
    document
        .named_sections(family, Some(node))
        .into_iter()
        .filter_map(|name| name.rsplit(' ').next())
        .collect()
}
pub(super) fn morse(settings: &ResolvedIdentifierSettings) -> MorseSettings {
    MorseSettings {
        speed_wpm: settings.morse_speed_wpm as u32,
        frequency_hz: settings.morse_frequency_hz as f32,
        level_db: settings.morse_level_db as i8,
    }
}

fn streaming_source(
    settings: &ResolvedIdentifierSettings,
    tone: Option<ToneSource>,
    provider_gain_db: i8,
) -> MediaSource {
    MediaSource {
        file: (!settings.sound_file.is_empty()).then(|| settings.sound_file.clone().into()),
        speech: (!settings.speech_text.is_empty()).then(|| SpeechSource {
            text: settings.speech_text.clone(),
            model: settings.speech_model.clone().into(),
            speed_percent: settings.speech_speed_percent as u32,
            level_db: settings.speech_level_db as i32,
        }),
        provider_gain_db,
        tone,
        morse: (!settings.morse_text.is_empty()).then(|| MorseSource {
            text: settings.morse_text.clone(),
            speed_wpm: settings.morse_speed_wpm as u32,
            frequency_hz: settings.morse_frequency_hz as f32,
            level_db: settings.morse_level_db as i8,
        }),
    }
}

pub(super) fn streamed_status(
    station: &mut dyn StationMediaSession,
    settings: &ResolvedIdentifierSettings,
    morse_text: &str,
    speech_text: &str,
) -> Result<PreparedMedia, RuntimeError> {
    let source = MediaSource {
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
    };
    let stream = station
        .register_once(source)
        .map_err(|_| RuntimeError::Preparation)?;
    PreparedMedia::new_stream(stream).map_err(|_| RuntimeError::Preparation)
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
    PreparedMedia::new_parrot_stream(report, recording).map_err(|_| RuntimeError::Preparation)
}

fn source_with_station(
    station: &mut Box<dyn StationMediaSession>,
    settings: &ResolvedIdentifierSettings,
    tone: Option<ToneSource>,
    provider_gain_db: i8,
) -> Result<Option<PreparedMedia>, RuntimeError> {
    let source = streaming_source(settings, tone, provider_gain_db);
    if source.file.is_some()
        || source.speech.is_some()
        || source.tone.is_some()
        || source.morse.is_some()
    {
        let stream = station
            .register(source)
            .map_err(|_| RuntimeError::Preparation)?;
        return PreparedMedia::new_stream(stream)
            .map(Some)
            .map_err(|_| RuntimeError::Preparation);
    }
    Ok(None)
}

fn prepared(
    station: &mut Box<dyn StationMediaSession>,
    settings: &ResolvedIdentifierSettings,
) -> Result<Option<PreparedMedia>, RuntimeError> {
    source_with_station(station, settings, None, 0)
}

fn announcement_media(
    settings: &ResolvedAnnouncementSettings,
    base: &ResolvedIdentifierSettings,
) -> ResolvedIdentifierSettings {
    let mut output = base.clone();
    output.sound_file = settings.sound_file.clone();
    output.speech_text = settings.speech_text.clone();
    output.speech_model = settings.speech_model.clone();
    output.speech_speed_percent = settings.speech_speed_percent;
    output.speech_level_db = settings.speech_level_db;
    output.morse_text = settings.morse_text.clone();
    output.morse_speed_wpm = settings.morse_speed_wpm;
    output.morse_frequency_hz = settings.morse_frequency_hz;
    output.morse_level_db = settings.morse_level_db;
    output
}

fn courtesy_media(
    station: &mut Box<dyn StationMediaSession>,
    settings: &ResolvedCourtesySettings,
    base: &ResolvedIdentifierSettings,
) -> Result<Option<PreparedMedia>, RuntimeError> {
    let mut item = base.clone();
    item.sound_file = settings.sound_file.clone();
    item.speech_text = settings.speech_text.clone();
    item.speech_model = settings.speech_model.clone();
    item.speech_speed_percent = settings.speech_speed_percent;
    item.speech_level_db = 0;
    item.morse_text = settings.morse_text.clone();
    item.morse_speed_wpm = settings.morse_speed_wpm;
    item.morse_frequency_hz = settings.morse_frequency_hz;
    item.morse_level_db = settings.level_db;
    let tone_source = (!settings.tone_sequence.is_empty()).then(|| ToneSource {
        sequence: settings.tone_sequence.clone(),
        level_db: settings.level_db as i8,
    });
    source_with_station(station, &item, tone_source, settings.level_db as i8)
}

type PreparedController = (
    NodeController,
    ControllerControl,
    ResolvedIdentifierSettings,
    Box<dyn StationMediaSession>,
);

pub(super) fn controller(
    document: &ConfigDocument,
    node: &NodeId,
    settings: &ResolvedNodeSettings,
    media: &dyn NativeMediaPreparer,
    generation: u64,
) -> Result<PreparedController, RuntimeError> {
    let status = ResolvedIdentifierSettings::resolve(document, node, None)?.value;
    let mut station = media
        .station(node.as_str(), generation)
        .map_err(|_| RuntimeError::Preparation)?;
    let mut ids = Vec::new();
    for label in labels(document, "identifier", node.as_str()) {
        let settings = ResolvedIdentifierSettings::resolve(document, node, Some(label))?.value;
        if let Some(prepared) = prepared(&mut station, &settings)? {
            ids.push(Identifier {
                media: prepared,
                interval_ms: settings.interval_ms,
                // The settings resolver bounds priority to i32::MAX.
                priority: settings.priority as i32,
                first_key_only: settings.first_key_only,
                regardless_of_activity: settings.regardless_of_activity,
                polite_maximum_wait_ms: settings.polite.then_some(settings.polite_maximum_wait_ms),
            });
        }
    }
    let mut announcements = Vec::new();
    for label in labels(document, "announcement", node.as_str()) {
        let settings = ResolvedAnnouncementSettings::resolve(document, node, Some(label))?.value;
        if let Some(prepared) = prepared(&mut station, &announcement_media(&settings, &status))? {
            announcements.push(Announcement {
                media: prepared,
                interval_ms: settings.interval_ms,
            });
        }
    }
    let mut courtesy = CourtesySettings::default();
    for label in labels(document, "courtesy", node.as_str()) {
        let settings = ResolvedCourtesySettings::resolve(document, node, label)?.value;
        if let Some(prepared) = courtesy_media(&mut station, &settings, &status)? {
            if settings.input == "receiver" {
                courtesy.receiver = Some(prepared);
            } else if settings.remote_node.is_empty() {
                courtesy.link = Some(prepared);
            } else {
                courtesy.peers.push((settings.remote_node, prepared));
            }
        }
    }
    let (controller, control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: settings.parrot_enabled,
            full_duplex: settings.full_duplex,
            hang_ms: settings.hang_ms,
            ctcss_encode_on_input: settings.ctcss_encode_on_input,
            ctcss_hang_ms: settings.ctcss_hang_ms,
            transmit_timeout_ms: settings.transmit_timeout_ms,
            timeout_lockout_ms: settings.timeout_lockout_ms,
            kerchunk_max_ms: settings.kerchunk_max_ms,
            courtesy_delay_ms: settings.courtesy_delay_ms,
            telemetry_duck_db: settings.telemetry_duck_db as i8,
            status_morse: morse(&status),
        },
        ids,
        announcements,
        courtesy,
    )
    .map_err(|_| RuntimeError::Preparation)?;
    station.start().map_err(|_| RuntimeError::Preparation)?;
    Ok((controller, control, status, station))
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
