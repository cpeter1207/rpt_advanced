//! Read-only message definitions resolved on control, before composition admission.

use super::{ConfigDocument, ConfigError, NodeId, Resolution, Schema, parse, settings};
use crate::controller::{
    PlaybackDestination, PlaybackPolicy, PoliteBusy, PoliteDelivery, PoliteExpiry,
};
use crate::media::{MediaSource, MorseSource, SpeechSource, ToneSource};
use std::{collections::BTreeMap, str::FromStr};

/// Allowlisted source of a per-attempt value; never an executable expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingSource {
    /// Configured amateur-radio callsign, with audience-specific spelling.
    Callsign,
    /// Node identity, spoken as individual digits.
    Node,
    /// Local civil date.
    Date,
    /// Local civil time using the node's clock format.
    Time,
    /// Localized weekday.
    DayOfWeek,
    /// Localized daypart greeting.
    Greeting,
    /// Localized current link summary.
    LinkStatus,
    /// Named sensor with quality, time, and engineering units.
    Sensor(String),
    /// Named digital input with quality and time.
    Input(String),
    /// Remaining time, only for an invocation supplying a deadline.
    TimeRemaining,
}

impl BindingSource {
    /// Reject deadline bindings at callers without a deadline; no invented zero.
    pub fn available_in_context(&self, has_deadline: bool) -> bool {
        !matches!(self, Self::TimeRemaining) || has_deadline
    }
}

impl FromStr for BindingSource {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "callsign" => Self::Callsign,
            "node" => Self::Node,
            "date" => Self::Date,
            "time" => Self::Time,
            "day_of_week" => Self::DayOfWeek,
            "greeting" => Self::Greeting,
            "link_status" => Self::LinkStatus,
            "time_remaining" => Self::TimeRemaining,
            _ => {
                let (family, name) = value.split_once(':').ok_or(())?;
                if name.is_empty()
                    || name.len() > 63
                    || name.chars().any(|c| c.is_whitespace() || "[]:".contains(c))
                {
                    return Err(());
                }
                match family {
                    "sensor" => Self::Sensor(name.into()),
                    "input" => Self::Input(name.into()),
                    _ => return Err(()),
                }
            }
        })
    }
}

/// Field-wise global/node message definition; all owned state stays off audio.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMessageSettings {
    /// Ordered content, defaulting to this definition's catalog message.
    pub content: String,
    /// Configured Fluent identifier, not a DTMF-supplied string.
    pub message_id: Option<String>,
    /// Named Fluent arguments resolved afresh for each admitted playback attempt.
    pub bindings: BTreeMap<String, BindingSource>,
    /// Existing file/speech/Morse/tone source settings and inherited levels.
    pub media: MediaSource,
    /// Effective speech settings for catalog/literal segments without configured speech text.
    pub speech_defaults: super::ResolvedSpeechSettings,
    /// Effective Morse settings for every generated fallback segment.
    pub morse_defaults: super::ResolvedMorseSettings,
    /// Optional canonical positive decimal command selector.
    pub dtmf_id: Option<String>,
    /// Repeat and destination settings shared by all composed-message callers.
    pub playback: PlaybackPolicy,
    /// Polite start deadline used by scheduled or manually selected playback.
    pub delivery: PoliteDelivery,
}

pub(super) fn identifier_valid(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

pub(crate) fn selector_valid(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 127
            && !value.starts_with('0')
            && value.bytes().all(|b| b.is_ascii_digit()))
}

impl ResolvedMessageSettings {
    /// Resolve a named message using node overrides, global fields, then media defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        name: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let scopes = [
            format!("message {} {name}", node.as_str()),
            format!("message {name}"),
        ];
        if !scopes
            .iter()
            .any(|scope| document.sections().contains(scope))
        {
            return Err(ConfigError::structure(
                &scopes[0],
                "unknown message definition",
            ));
        }
        let value = Self::from_scopes(document, node, &scopes, "{message}")?;
        Ok(Resolution { value, warnings })
    }

    /// Resolve the same composition/media fields for a caller-owned scope hierarchy.
    pub(crate) fn from_scopes(
        document: &ConfigDocument,
        node: &NodeId,
        scopes: &[String],
        default_content: &str,
    ) -> Result<Self, ConfigError> {
        let text = |key| settings::lookup_scopes(document, key, scopes).unwrap_or_default();
        let optional = |key, valid: fn(&str) -> bool| {
            settings::lookup_valid_scopes(document, key, scopes, |value| {
                valid(value).then(|| value.to_owned())
            })
            .filter(|v| !v.is_empty())
        };
        let unsigned = |key, max, fallback| {
            settings::lookup_valid_scopes(document, key, scopes, |v| parse::unsigned(v, 1, max))
                .unwrap_or(fallback)
        };
        let level = |key, fallback| {
            settings::lookup_valid_scopes(document, key, scopes, |v| parse::signed(v, -60, 0))
                .unwrap_or(fallback)
        };
        let mut speech = super::ResolvedSpeechSettings::resolve(document, node)?.value;
        let mut morse = super::ResolvedMorseSettings::resolve(document, node)?.value;
        speech.voice =
            settings::lookup_scopes(document, "speech_model", scopes).unwrap_or(speech.voice);
        speech.speed_percent = unsigned("speech_speed_percent", 1000, speech.speed_percent);
        speech.level_db = level("speech_level_db", speech.level_db);
        morse.frequency_hz = unsigned("morse_frequency_hz", u32::MAX as u64, morse.frequency_hz);
        morse.speed_wpm = unsigned("morse_speed_wpm", 100, morse.speed_wpm);
        morse.level_db = level("morse_level_db", morse.level_db);
        let (content, message_id, bindings) = resolve_content(document, scopes, default_content);
        let speech_text = text("speech_text");
        let morse_text = text("morse_text");
        let tone_sequence = text("tone_sequence");
        Ok(Self {
            content,
            message_id,
            dtmf_id: message_selector(document, scopes),
            playback: playback_policy(document, scopes),
            delivery: delivery_policy(document, scopes, 0),
            bindings,
            media: MediaSource {
                file: optional("sound_file", |_| true).map(Into::into),
                speech: (!speech_text.is_empty()).then(|| SpeechSource {
                    text: speech_text,
                    model: speech.voice.clone().into(),
                    speed_percent: speech.speed_percent as u32,
                    level_db: speech.level_db as i32,
                }),
                morse: (!morse_text.is_empty()).then_some(MorseSource {
                    text: morse_text,
                    frequency_hz: morse.frequency_hz as f32,
                    speed_wpm: morse.speed_wpm as u32,
                    level_db: morse.level_db as i8,
                }),
                tone: (!tone_sequence.is_empty()).then(|| ToneSource {
                    sequence: tone_sequence,
                    level_db: level("level_db", -20) as i8,
                }),
                provider_gain_db: 0,
            },
            speech_defaults: speech,
            morse_defaults: morse,
        })
    }
}

/// Resolve a shared positive decimal selector, with explicit empty values clearing inheritance.
pub(crate) fn message_selector(document: &ConfigDocument, scopes: &[String]) -> Option<String> {
    settings::lookup_valid_scopes(document, "dtmf_id", scopes, |value| {
        selector_valid(value).then(|| value.to_owned())
    })
    .filter(|value| !value.is_empty())
}

/// Resolve audience and repeat fields with the same invalid-value fallback as other settings.
pub(crate) fn playback_policy(document: &ConfigDocument, scopes: &[String]) -> PlaybackPolicy {
    PlaybackPolicy {
        repeat_during_playback: settings::lookup_valid_scopes(
            document,
            "repeat_during_playback",
            scopes,
            parse::boolean,
        )
        .unwrap_or(true),
        destination: settings::lookup_valid_scopes(
            document,
            "playback_destination",
            scopes,
            |value| match value {
                "local" => Some(PlaybackDestination::Local),
                "local_and_links" => Some(PlaybackDestination::LocalAndLinks),
                _ => None,
            },
        )
        .unwrap_or_default(),
    }
}

/// Resolve start-deadline options, retaining the invoking family's documented minimum wait.
pub(crate) fn delivery_policy(
    document: &ConfigDocument,
    scopes: &[String],
    minimum_wait_ms: u64,
) -> PoliteDelivery {
    PoliteDelivery {
        maximum_wait_ms: settings::lookup_valid_scopes(
            document,
            "polite_maximum_wait_ms",
            scopes,
            |value| parse::unsigned(value, minimum_wait_ms, u64::MAX),
        )
        .unwrap_or(60_000),
        expiry: settings::lookup_valid_scopes(
            document,
            "polite_expiry",
            scopes,
            |value| match value {
                "skip" => Some(PoliteExpiry::Skip),
                "morse" => Some(PoliteExpiry::Morse),
                _ => None,
            },
        )
        .unwrap_or_default(),
        busy: settings::lookup_valid_scopes(document, "polite_expiry_busy", scopes, |value| {
            match value {
                "skip" => Some(PoliteBusy::Skip),
                "wait" => Some(PoliteBusy::Wait),
                _ => None,
            }
        })
        .unwrap_or_default(),
    }
}

/// Share only composition inheritance; ID and macro media keep their own existing precedence.
pub(super) fn resolve_content(
    document: &ConfigDocument,
    scopes: &[String],
    default_content: &str,
) -> (String, Option<String>, BTreeMap<String, BindingSource>) {
    let content = settings::lookup_valid_scopes(document, "content", scopes, |value| {
        crate::composition::syntax_valid(value).then(|| value.to_owned())
    })
    .unwrap_or_else(|| default_content.into());
    let message_id = settings::lookup_valid_scopes(document, "message_id", scopes, |value| {
        (value.is_empty() || identifier_valid(value)).then(|| value.to_owned())
    })
    .filter(|value| !value.is_empty());
    let mut bindings = BTreeMap::new();
    for entry in document
        .entries()
        .iter()
        .filter(|entry| scopes.contains(&entry.section))
    {
        if let Some(argument) = entry
            .key
            .strip_prefix("arg_")
            .filter(|name| identifier_valid(name))
        {
            if let Some(source) =
                settings::lookup_valid_scopes(document, &entry.key, scopes, |value| {
                    if value.is_empty() {
                        Some(None)
                    } else {
                        value.parse().ok().map(Some)
                    }
                })
                .flatten()
            {
                bindings.insert(argument.into(), source);
            }
        }
    }
    (content, message_id, bindings)
}

pub(super) fn content_key(key: &str) -> bool {
    matches!(key, "content" | "message_id")
        || key.strip_prefix("arg_").is_some_and(identifier_valid)
}
