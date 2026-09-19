//! Configuration-to-controller preparation. All allocation and external media work is offline.

use crate::{
    audio::ToneSequence,
    config::{
        ConfigDocument, ConfigError, NodeId, ResolvedAnnouncementSettings,
        ResolvedCourtesySettings, ResolvedIdentifierSettings, ResolvedNodeSettings,
    },
    controller::{
        Announcement, ControllerControl, ControllerSettings, CourtesySettings, Identifier,
        MorseSettings, NodeController, PreparedMedia,
    },
    media::{Cancellation, FileRequest, MediaError, PreparedAudio, SpeechRequest},
};
use std::path::Path;

/// Native media capability composed by the adapter from decoding/synthesis and the released
/// resampling ring. Successful output must already be mono 48-kHz PCM.
pub trait NativeFilePreparer {
    /// Decode and convert a local file on a non-audio preparation owner.
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

/// Selected speech capability followed by native-rate ring conversion.
pub trait NativeSpeechPreparer {
    /// Synthesize and convert literal speech on a non-audio preparation owner.
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError>;
}

/// Complete selected media composition; neither independently selected port is optional.
pub trait NativeMediaPreparer: NativeFilePreparer + NativeSpeechPreparer {}
impl<T: NativeFilePreparer + NativeSpeechPreparer> NativeMediaPreparer for T {}

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

fn native(audio: PreparedAudio) -> Result<Vec<f32>, MediaError> {
    if audio.sample_rate_hz() != 48000 || audio.samples().iter().any(|sample| sample.abs() > 1.0) {
        return Err(MediaError::IncompatibleAdapter);
    }
    Ok(audio.samples().to_vec())
}

pub(super) fn speech(
    media: &dyn NativeMediaPreparer,
    settings: &ResolvedIdentifierSettings,
    text: &str,
) -> Result<Option<Vec<f32>>, RuntimeError> {
    if text.is_empty() {
        return Ok(None);
    }
    let cancellation = Cancellation::default();
    match media
        .speech(&SpeechRequest {
            text,
            model: Path::new(&settings.speech_model),
            speed_percent: settings.speech_speed_percent as u32,
            level_db: settings.speech_level_db as i32,
            cancellation: &cancellation,
        })
        .and_then(native)
    {
        Ok(audio) => Ok(Some(audio)),
        Err(MediaError::IncompatibleAdapter) => Err(RuntimeError::Preparation),
        Err(_) => Ok(None),
    }
}

fn source(
    media: &dyn NativeMediaPreparer,
    settings: &ResolvedIdentifierSettings,
) -> Result<Option<Vec<f32>>, RuntimeError> {
    if !settings.sound_file.is_empty() {
        let cancellation = Cancellation::default();
        match media
            .file(&FileRequest {
                path: Path::new(&settings.sound_file),
                cancellation: &cancellation,
            })
            .and_then(native)
        {
            Ok(audio) => return Ok(Some(audio)),
            Err(MediaError::IncompatibleAdapter) => return Err(RuntimeError::Preparation),
            Err(_) => {}
        }
    }
    speech(media, settings, &settings.speech_text)
}

fn prepared(
    media: &dyn NativeMediaPreparer,
    settings: &ResolvedIdentifierSettings,
) -> Result<Option<PreparedMedia>, RuntimeError> {
    let audio = source(media, settings)?;
    if audio.is_none() && settings.morse_text.is_empty() {
        return Ok(None);
    }
    PreparedMedia::new(audio, &settings.morse_text, morse(settings))
        .map(Some)
        .map_err(|_| RuntimeError::Preparation)
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
    media: &dyn NativeMediaPreparer,
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
    let mut audio = source(media, &item)?;
    if let Some(audio) = &mut audio {
        let gain = 10_f32.powf(settings.level_db as f32 / 20.0);
        for sample in audio {
            *sample *= gain;
        }
    }
    if !settings.tone_sequence.is_empty() {
        // Validate even when a higher-priority source succeeded.
        let mut tones = ToneSequence::new(&settings.tone_sequence, settings.level_db as i8)
            .map_err(|_| RuntimeError::Preparation)?;
        if audio.is_none() {
            let mut pcm = vec![0.0; tones.rendered_samples()];
            tones.render(&mut pcm);
            audio = Some(pcm);
        }
    }
    if audio.is_none() && item.morse_text.is_empty() {
        return Ok(None);
    }
    PreparedMedia::new(audio, &item.morse_text, morse(&item))
        .map(Some)
        .map_err(|_| RuntimeError::Preparation)
}

pub(super) fn controller(
    document: &ConfigDocument,
    node: &NodeId,
    settings: &ResolvedNodeSettings,
    media: &dyn NativeMediaPreparer,
) -> Result<
    (
        NodeController,
        ControllerControl,
        ResolvedIdentifierSettings,
    ),
    RuntimeError,
> {
    let status = ResolvedIdentifierSettings::resolve(document, node, None)?.value;
    let mut ids = Vec::new();
    for label in labels(document, "identifier", node.as_str()) {
        let settings = ResolvedIdentifierSettings::resolve(document, node, Some(label))?.value;
        if let Some(prepared) = prepared(media, &settings)? {
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
        if let Some(prepared) = prepared(media, &announcement_media(&settings, &status))? {
            announcements.push(Announcement {
                media: prepared,
                interval_ms: settings.interval_ms,
            });
        }
    }
    let mut courtesy = CourtesySettings::default();
    for label in labels(document, "courtesy", node.as_str()) {
        let settings = ResolvedCourtesySettings::resolve(document, node, label)?.value;
        if let Some(prepared) = courtesy_media(media, &settings, &status)? {
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
            full_duplex: settings.full_duplex,
            hang_ms: settings.hang_ms,
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
    Ok((controller, control, status))
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
