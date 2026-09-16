//! Owned node settings and inherited default resolution.

use super::{ConfigDocument, ConfigError, Resolution, Schema, parse};
use crate::access::AccessPolicy;
use crate::command::{CommandMapping, DtmfCommandMap, LinkAction};
use crate::schedule::{date_selector_valid, weekday_selector_valid};
use std::collections::BTreeMap;

/// Directory lookup source selected after the local static directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkLookupMethod {
    /// Query DNS and then the external file.
    #[default]
    Both,
    /// Query DNS only.
    Dns,
    /// Query the external file only.
    File,
}

/// Fully owned node settings after general and node-scoped inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedNodeSettings {
    /// Whether the node starts.
    pub enabled: bool,
    /// Whether local receive may transmit concurrently.
    pub full_duplex: bool,
    /// Whether completed local DTMF frames are muted.
    pub dtmf_muting: bool,
    /// Transmit hang duration in milliseconds.
    pub hang_ms: u64,
    /// Continuous-source watchdog duration; zero disables it.
    pub transmit_timeout_ms: u64,
    /// Post-watchdog lockout duration.
    pub timeout_lockout_ms: u64,
    /// Kerchunk threshold in milliseconds.
    pub kerchunk_max_ms: u64,
    /// Receive-active telemetry attenuation in dB.
    pub telemetry_duck_db: i64,
    /// Unkey-to-courtesy delay in milliseconds.
    pub courtesy_delay_ms: u64,
    /// Node radio channel.
    pub channel: String,
    /// Optional callsign.
    pub callsign: String,
    /// Incoming allowlist.
    pub link_allow_nodes: String,
    /// Incoming denylist.
    pub link_deny_nodes: String,
    /// Optional local static directory path.
    pub link_static_directory_file: String,
    /// Optional external directory path.
    pub link_directory_file: String,
    /// Directory lookup policy.
    pub link_lookup_method: LinkLookupMethod,
    /// Configured DTMF command prefixes by option name.
    pub link_commands: BTreeMap<String, String>,
}

/// Owned identifier media and scheduling defaults after scope resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedIdentifierSettings {
    /// Positive identifier interval in milliseconds.
    pub interval_ms: u64,
    /// Nonnegative scheduler priority.
    pub priority: u64,
    /// Whether identification waits for the first qualifying key-up.
    pub first_key_only: bool,
    /// Whether to identify even while the node is inactive.
    pub regardless_of_activity: bool,
    /// Whether to defer identification while traffic is active.
    pub polite: bool,
    /// Maximum polite deferral in milliseconds.
    pub polite_maximum_wait_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Owned announcement settings after flat, node, and named resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAnnouncementSettings {
    /// Zero plays after each ordinary transmitter release.
    pub interval_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Resolved Morse defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMorseSettings {
    /// Tone frequency in hertz.
    pub frequency_hz: u64,
    /// Speed in words per minute.
    pub speed_wpm: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved speech defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpeechSettings {
    /// Voice model path.
    pub voice: String,
    /// Speaking speed percentage.
    pub speed_percent: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved clock-announcement defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTimeSettings {
    /// Twelve- or twenty-four-hour display.
    pub format: u64,
}
/// Resolved named courtesy assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCourtesySettings {
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain, fixed at zero because courtesy uses its common level.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain, inherited from the common courtesy level.
    pub morse_level_db: i64,
    /// Optional generated tone sequence.
    pub tone_sequence: String,
    /// Courtesy input.
    pub input: String,
    /// Optional linked peer.
    pub remote_node: String,
    /// Common courtesy level in dB.
    pub level_db: i64,
}
/// Resolved named message template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTemplateSettings {
    /// Template label.
    pub name: String,
    /// Template text.
    pub text: String,
}
/// Resolved named controller macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMacroSettings {
    /// Macro label.
    pub name: String,
    action: crate::schedule::ScheduledAction,
    /// Optional decimal target.
    pub target_node: String,
}
/// Resolved zero-time event declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEventSettings {
    /// Event label.
    pub name: String,
    /// Civil trigger.
    pub at: String,
    /// Optional template.
    pub template: String,
    /// Optional message.
    pub message: String,
    /// Optional macro.
    pub macro_name: String,
}
/// Resolved permanent direct-link declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPermanentLinkSettings {
    /// Link label.
    pub name: String,
    /// Remote decimal node.
    pub remote_node: String,
}
/// Resolved replacement-window declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedScheduleSettings {
    /// Window label.
    pub name: String,
    /// Replacement remote node.
    pub remote_node: String,
    /// Permanent label replaced by the window.
    pub replace_permanent: String,
    /// Optional weekday selector.
    pub days: String,
    /// Optional explicit-date selector.
    pub dates: String,
    /// Inclusive local start.
    pub start_time: String,
    /// Exclusive local end.
    pub end_time: String,
    /// Post-window inactivity grace in milliseconds.
    pub end_inactivity_ms: u64,
}

fn scoped<'a>(
    document: &'a ConfigDocument,
    node: &NodeId,
    family: &str,
    key: &str,
) -> Option<&'a str> {
    let node_scope = format!("{family} {}", node.as_str());
    document.lookup(key, &[node_scope.as_str(), family])
}

fn lookup_valid_scopes<T>(
    document: &ConfigDocument,
    key: &str,
    scopes: &[String],
    parse_value: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    for scope in scopes {
        if let Some(raw) = document.lookup(key, &[scope.as_str()]) {
            if let Some(value) = parse_value(raw) {
                return Some(value);
            }
        }
    }
    None
}

fn lookup_scopes(document: &ConfigDocument, key: &str, scopes: &[String]) -> Option<String> {
    scopes
        .iter()
        .find_map(|scope| document.lookup(key, &[scope.as_str()]).map(str::to_owned))
}

fn node_value(value: &str) -> Option<String> {
    (value.len() <= 63 && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| value.to_owned())
}

fn family_scopes(node: &NodeId, family: &str) -> [String; 2] {
    [format!("{family} {}", node.as_str()), family.to_owned()]
}
impl ResolvedMorseSettings {
    /// Resolve flat and node-specific Morse defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                frequency_hz: lookup_valid_scopes(
                    document,
                    "frequency_hz",
                    &family_scopes(node, "morse"),
                    |value| parse::unsigned(value, 1, u32::MAX as u64),
                )
                .unwrap_or(800),
                speed_wpm: lookup_valid_scopes(
                    document,
                    "speed_wpm",
                    &family_scopes(node, "morse"),
                    |value| parse::unsigned(value, 1, 100),
                )
                .unwrap_or(20),
                level_db: lookup_valid_scopes(
                    document,
                    "level_db",
                    &family_scopes(node, "morse"),
                    |value| parse::signed(value, -60, 0),
                )
                .unwrap_or(-6),
            },
            warnings,
        })
    }
}

impl ResolvedSpeechSettings {
    /// Resolve flat and node-specific speech defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                voice: scoped(document, node, "speech", "voice")
                    .unwrap_or("en_US-lessac-medium.onnx")
                    .to_owned(),
                speed_percent: lookup_valid_scopes(
                    document,
                    "speed_percent",
                    &family_scopes(node, "speech"),
                    |value| parse::unsigned(value, 1, 1000),
                )
                .unwrap_or(100),
                level_db: lookup_valid_scopes(
                    document,
                    "level_db",
                    &family_scopes(node, "speech"),
                    |value| parse::signed(value, -60, 0),
                )
                .unwrap_or(0),
            },
            warnings,
        })
    }
}

impl ResolvedTimeSettings {
    /// Resolve flat and node-specific time defaults.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self {
                format: lookup_valid_scopes(
                    document,
                    "format",
                    &family_scopes(node, "time"),
                    |value| {
                        matches!(value, "12" | "24")
                            .then(|| value.parse().ok())
                            .flatten()
                    },
                )
                .unwrap_or(12),
            },
            warnings,
        })
    }
}

impl ResolvedCourtesySettings {
    /// Resolve one named courtesy assignment.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("courtesy {} {label}", node.as_str());
        let named_scopes = vec![section.clone()];
        let courtesy_scopes = family_scopes(node, "courtesy");
        let input = document
            .lookup("input", &[section.as_str()])
            .unwrap_or("")
            .to_owned();
        let remote_node = lookup_valid_scopes(document, "remote_node", &named_scopes, node_value)
            .unwrap_or_default();
        if !matches!(input.as_str(), "receiver" | "link") {
            return Err(ConfigError::structure(
                &section,
                "courtesy input is required",
            ));
        }
        let level_db = lookup_valid_scopes(document, "level_db", &named_scopes, |value| {
            parse::signed(value, -60, 0)
        })
        .or_else(|| {
            lookup_valid_scopes(document, "level_db", &courtesy_scopes, |value| {
                parse::signed(value, -60, 0)
            })
        })
        .unwrap_or(-20);
        Ok(Resolution {
            value: Self {
                sound_file: lookup_scopes(document, "sound_file", &named_scopes)
                    .or_else(|| lookup_scopes(document, "sound_file", &courtesy_scopes))
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &named_scopes)
                    .or_else(|| lookup_scopes(document, "speech_text", &courtesy_scopes))
                    .unwrap_or_default(),
                speech_model: lookup_scopes(document, "speech_model", &named_scopes)
                    .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                    .or_else(|| lookup_scopes(document, "speech_model", &courtesy_scopes))
                    .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |value| parse::unsigned(value, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &courtesy_scopes,
                        |value| parse::unsigned(value, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: 0,
                morse_text: lookup_scopes(document, "morse_text", &named_scopes)
                    .or_else(|| lookup_scopes(document, "morse_text", &courtesy_scopes))
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |value| parse::unsigned(value, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(document, "morse_speed_wpm", &courtesy_scopes, |value| {
                        parse::unsigned(value, 1, 100)
                    })
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &named_scopes,
                    |value| parse::unsigned(value, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |value| parse::unsigned(value, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(document, "morse_frequency_hz", &courtesy_scopes, |value| {
                        parse::unsigned(value, 1, u32::MAX as u64)
                    })
                })
                .unwrap_or(800),
                morse_level_db: level_db,
                tone_sequence: lookup_scopes(document, "tone_sequence", &named_scopes)
                    .or_else(|| lookup_scopes(document, "tone_sequence", &courtesy_scopes))
                    .unwrap_or_default(),
                input,
                remote_node,
                level_db,
            },
            warnings,
        })
    }
}

impl ResolvedTemplateSettings {
    /// Resolve a global template and same-label node override.
    pub fn resolve(
        document: &ConfigDocument,
        node: Option<&NodeId>,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let mut scopes = Vec::new();
        if let Some(node) = node {
            scopes.push(format!("template {} {label}", node.as_str()));
        }
        scopes.push(format!("template {label}"));
        let text = scopes
            .iter()
            .find_map(|scope| document.lookup("text", &[scope.as_str()]))
            .unwrap_or("");
        if text.is_empty() {
            return Err(ConfigError::structure(
                &scopes[0],
                "template text is required",
            ));
        }
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                text: text.to_owned(),
            },
            warnings,
        })
    }
}

impl ResolvedMacroSettings {
    /// Validated controller operation, fixed when the macro definition is resolved.
    pub fn action(&self) -> crate::schedule::ScheduledAction {
        self.action
    }
    /// Resolve a global macro and same-label node override.
    pub fn resolve(
        document: &ConfigDocument,
        node: Option<&NodeId>,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let mut scopes = Vec::new();
        if let Some(node) = node {
            scopes.push(format!("macro {} {label}", node.as_str()));
        }
        scopes.push(format!("macro {label}"));
        let action = lookup_valid_scopes(document, "action", &scopes, |value| {
            value.parse::<crate::schedule::ScheduledAction>().ok()
        });
        let target =
            lookup_valid_scopes(document, "target_node", &scopes, node_value).unwrap_or_default();
        let action =
            action.ok_or_else(|| ConfigError::structure(&scopes[0], "macro action is required"))?;
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                action,
                target_node: target,
            },
            warnings,
        })
    }
}

impl ResolvedEventSettings {
    /// Resolve a required per-node event declaration.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("event {} {label}", node.as_str());
        let get = |key| {
            document
                .lookup(key, &[section.as_str()])
                .unwrap_or("")
                .to_owned()
        };
        let value = Self {
            name: label.to_owned(),
            at: get("at"),
            template: get("template"),
            message: get("message"),
            macro_name: get("macro"),
        };
        if value.at.is_empty() {
            return Err(ConfigError::structure(&section, "event at is required"));
        }
        Ok(Resolution { value, warnings })
    }
}

impl ResolvedPermanentLinkSettings {
    /// Resolve a required per-node permanent link.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("permanent {} {label}", node.as_str());
        let remote = document
            .lookup("remote_node", &[section.as_str()])
            .unwrap_or("");
        if remote.is_empty() {
            return Err(ConfigError::structure(
                &section,
                "permanent remote node is required",
            ));
        }
        Ok(Resolution {
            value: Self {
                name: label.to_owned(),
                remote_node: remote.to_owned(),
            },
            warnings,
        })
    }
}

impl ResolvedScheduleSettings {
    /// Resolve a required same-node replacement window.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        label: &str,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        let section = format!("schedule {} {label}", node.as_str());
        let get = |key| {
            document
                .lookup(key, &[section.as_str()])
                .unwrap_or("")
                .to_owned()
        };
        let value = Self {
            name: label.to_owned(),
            remote_node: get("remote_node"),
            replace_permanent: get("replace_permanent"),
            days: document
                .lookup("days", &[section.as_str()])
                .filter(|raw| weekday_selector_valid(raw))
                .unwrap_or("")
                .to_owned(),
            dates: document
                .lookup("dates", &[section.as_str()])
                .filter(|raw| date_selector_valid(raw))
                .unwrap_or("")
                .to_owned(),
            start_time: get("start_time"),
            end_time: get("end_time"),
            end_inactivity_ms: document
                .lookup("end_inactivity_ms", &[section.as_str()])
                .and_then(|raw| parse::unsigned(raw, 0, u64::MAX))
                .unwrap_or(0),
        };
        if value.remote_node.is_empty() {
            return Err(ConfigError::structure(
                &section,
                "schedule remote node, replacement, start time, and end time are required",
            ));
        }
        Ok(Resolution { value, warnings })
    }
}

impl ResolvedAnnouncementSettings {
    /// Resolve announcement values with named settings taking precedence.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        set: Option<&str>,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut announcement_scopes = Vec::new();
        if let Some(set) = set {
            announcement_scopes.push(format!("announcement {} {set}", node.as_str()));
        }
        announcement_scopes.push(format!("announcement {}", node.as_str()));
        announcement_scopes.push("announcement".to_owned());
        let named_count = usize::from(set.is_some());
        Ok(Resolution {
            value: Self {
                interval_ms: lookup_valid_scopes(
                    document,
                    "interval_ms",
                    &announcement_scopes,
                    |raw| parse::unsigned(raw, 0, u64::MAX),
                )
                .unwrap_or(0),
                sound_file: lookup_scopes(document, "sound_file", &announcement_scopes)
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &announcement_scopes)
                    .unwrap_or_default(),
                speech_model: lookup_scopes(
                    document,
                    "speech_model",
                    &announcement_scopes[..named_count],
                )
                .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                .or_else(|| {
                    lookup_scopes(
                        document,
                        "speech_model",
                        &announcement_scopes[named_count..],
                    )
                })
                .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: lookup_valid_scopes(
                    document,
                    "speech_level_db",
                    &announcement_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "speech"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_level_db",
                        &announcement_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(0),
                morse_text: lookup_scopes(document, "morse_text", &announcement_scopes)
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_speed_wpm",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &announcement_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_frequency_hz",
                        &announcement_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .unwrap_or(800),
                morse_level_db: lookup_valid_scopes(
                    document,
                    "morse_level_db",
                    &announcement_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "morse"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_level_db",
                        &announcement_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(-6),
            },
            warnings: schema.warnings,
        })
    }
}

impl ResolvedIdentifierSettings {
    /// Resolve flat, node, and named identifier values with the most-specific value first.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
        set: Option<&str>,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut identifier_scopes = Vec::new();
        if let Some(set) = set {
            identifier_scopes.push(format!("identifier {} {set}", node.as_str()));
        }
        identifier_scopes.push(format!("identifier {}", node.as_str()));
        identifier_scopes.push("identifier".to_owned());
        let named_count = usize::from(set.is_some());
        let number = |key, default, minimum| {
            lookup_valid_scopes(document, key, &identifier_scopes, |raw| {
                parse::unsigned(raw, minimum, u64::MAX)
            })
            .unwrap_or(default)
        };
        let boolean = |key, default| {
            lookup_valid_scopes(document, key, &identifier_scopes, parse::boolean)
                .unwrap_or(default)
        };
        Ok(Resolution {
            value: Self {
                interval_ms: number("interval_ms", 600_000, 1),
                priority: lookup_valid_scopes(document, "priority", &identifier_scopes, |raw| {
                    parse::unsigned(raw, 0, i32::MAX as u64)
                })
                .unwrap_or(0),
                first_key_only: boolean("first_key_only", false),
                regardless_of_activity: boolean("regardless_of_activity", false),
                polite: boolean("polite", false),
                polite_maximum_wait_ms: number("polite_maximum_wait_ms", 60_000, 1),
                sound_file: lookup_scopes(document, "sound_file", &identifier_scopes)
                    .unwrap_or_default(),
                speech_text: lookup_scopes(document, "speech_text", &identifier_scopes)
                    .unwrap_or_default(),
                speech_model: lookup_scopes(
                    document,
                    "speech_model",
                    &identifier_scopes[..named_count],
                )
                .or_else(|| lookup_scopes(document, "voice", &family_scopes(node, "speech")))
                .or_else(|| {
                    lookup_scopes(document, "speech_model", &identifier_scopes[named_count..])
                })
                .unwrap_or_else(|| "en_US-lessac-medium.onnx".to_owned()),
                speech_speed_percent: lookup_valid_scopes(
                    document,
                    "speech_speed_percent",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 1000),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_percent",
                        &family_scopes(node, "speech"),
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_speed_percent",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 1000),
                    )
                })
                .unwrap_or(100),
                speech_level_db: lookup_valid_scopes(
                    document,
                    "speech_level_db",
                    &identifier_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "speech"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speech_level_db",
                        &identifier_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(0),
                morse_text: lookup_scopes(document, "morse_text", &identifier_scopes)
                    .unwrap_or_default(),
                morse_speed_wpm: lookup_valid_scopes(
                    document,
                    "morse_speed_wpm",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, 100),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "speed_wpm",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_speed_wpm",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, 100),
                    )
                })
                .unwrap_or(20),
                morse_frequency_hz: lookup_valid_scopes(
                    document,
                    "morse_frequency_hz",
                    &identifier_scopes[..named_count],
                    |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "frequency_hz",
                        &family_scopes(node, "morse"),
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_frequency_hz",
                        &identifier_scopes[named_count..],
                        |raw| parse::unsigned(raw, 1, u32::MAX as u64),
                    )
                })
                .unwrap_or(800),
                morse_level_db: lookup_valid_scopes(
                    document,
                    "morse_level_db",
                    &identifier_scopes[..named_count],
                    |raw| parse::signed(raw, -60, 0),
                )
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "level_db",
                        &family_scopes(node, "morse"),
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .or_else(|| {
                    lookup_valid_scopes(
                        document,
                        "morse_level_db",
                        &identifier_scopes[named_count..],
                        |raw| parse::signed(raw, -60, 0),
                    )
                })
                .unwrap_or(-6),
            },
            warnings: schema.warnings,
        })
    }
}

impl ResolvedNodeSettings {
    /// Build the effective command map, rejecting unknown option names or invalid prefixes.
    pub fn command_map(&self) -> Result<DtmfCommandMap, crate::command::CommandMapError> {
        DtmfCommandMap::new(
            self.link_commands
                .iter()
                .map(|(key, digits)| {
                    command_action(key)
                        .map(|action| CommandMapping::new(digits, action))
                        .ok_or(crate::command::CommandMapError::UnknownAction)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
    /// Resolve general defaults and an exact node override, returning warnings for recoverable values.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut value = Self::defaults(node);
        let scopes = [node.as_str().to_owned(), "general".to_owned()];
        macro_rules! text {
            ($field:ident, $key:literal) => {
                if let Some(raw) = lookup(document, $key, &scopes) {
                    value.$field = raw.to_owned();
                }
            };
        }
        macro_rules! boolean {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, parse::boolean) {
                    value.$field = parsed;
                }
            };
        }
        macro_rules! number {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, |raw| {
                    parse::unsigned(raw, 0, u64::MAX)
                }) {
                    value.$field = parsed;
                }
            };
        }
        boolean!(enabled, "node_enabled");
        boolean!(full_duplex, "full_duplex");
        boolean!(dtmf_muting, "dtmf_muting");
        number!(hang_ms, "transmit_hang_ms");
        number!(transmit_timeout_ms, "transmit_timeout_ms");
        number!(timeout_lockout_ms, "timeout_lockout_ms");
        number!(kerchunk_max_ms, "kerchunk_max_ms");
        number!(courtesy_delay_ms, "courtesy_delay_ms");
        if let Some(parsed) = lookup_valid(document, "telemetry_duck_db", &scopes, |raw| {
            parse::signed(raw, -60, 0)
        }) {
            value.telemetry_duck_db = parsed;
        }
        text!(channel, "radio_channel");
        if let Some(raw) = lookup_valid(document, "callsign", &scopes, |raw| {
            (raw.len() <= 63).then(|| raw.to_owned())
        }) {
            value.callsign = raw;
        }
        if let Some(raw) = lookup_valid(document, "link_allow_nodes", &scopes, |raw| {
            AccessPolicy::list_valid(raw).then(|| raw.to_owned())
        }) {
            value.link_allow_nodes = raw;
        }
        if let Some(raw) = lookup_valid(document, "link_deny_nodes", &scopes, |raw| {
            AccessPolicy::list_valid(raw).then(|| raw.to_owned())
        }) {
            value.link_deny_nodes = raw;
        }
        text!(link_static_directory_file, "link_static_directory_file");
        text!(link_directory_file, "link_directory_file");
        if let Some(parsed) = lookup_valid(document, "link_lookup_method", &scopes, |raw| match raw
        {
            "dns" => Some(LinkLookupMethod::Dns),
            "file" => Some(LinkLookupMethod::File),
            "both" => Some(LinkLookupMethod::Both),
            _ => None,
        }) {
            value.link_lookup_method = parsed;
        }
        for key in command_keys()
            .into_iter()
            .filter(|key| key.starts_with("link_command_"))
        {
            if let Some(raw) = lookup_valid(document, key, &scopes, |raw| {
                DtmfCommandMap::validate(&[CommandMapping::new(raw, LinkAction::Status)])
                    .is_ok()
                    .then(|| raw.to_owned())
            }) {
                value.link_commands.insert(key.to_owned(), raw);
            }
        }
        value
            .command_map()
            .map_err(|error| ConfigError::structure(node.as_str(), &error.to_string()))?;
        Ok(Resolution {
            value,
            warnings: schema.warnings,
        })
    }

    fn defaults(node: &NodeId) -> Self {
        Self {
            enabled: true,
            full_duplex: true,
            dtmf_muting: true,
            hang_ms: 0,
            transmit_timeout_ms: 180_000,
            timeout_lockout_ms: 30_000,
            kerchunk_max_ms: 500,
            telemetry_duck_db: -20,
            courtesy_delay_ms: 250,
            channel: node.as_str().to_owned(),
            callsign: String::new(),
            link_allow_nodes: String::new(),
            link_deny_nodes: String::new(),
            link_static_directory_file: String::new(),
            link_directory_file: String::new(),
            link_lookup_method: LinkLookupMethod::Both,
            link_commands: command_keys()
                .into_iter()
                .map(|key| (key.to_owned(), default_command(key)))
                .collect(),
        }
    }
}

fn lookup<'a>(document: &'a ConfigDocument, key: &str, scopes: &[String; 2]) -> Option<&'a str> {
    document.lookup(key, &[scopes[0].as_str(), scopes[1].as_str()])
}

fn lookup_valid<T>(
    document: &ConfigDocument,
    key: &str,
    scopes: &[String; 2],
    parse_value: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    for scope in scopes {
        if let Some(raw) = document.lookup(key, &[scope.as_str()]) {
            if let Some(value) = parse_value(raw) {
                return Some(value);
            }
        }
    }
    None
}

fn command_keys() -> [&'static str; 16] {
    [
        "link_command_disconnect",
        "link_command_monitor",
        "link_command_transceive",
        "link_command_remote",
        "link_command_status",
        "link_command_disconnect_all",
        "link_command_last_keyed",
        "link_command_local_monitor",
        "link_command_disconnect_permanent",
        "link_command_permanent_monitor",
        "link_command_permanent_transceive",
        "link_command_full_status",
        "link_command_reconnect_all",
        "link_command_permanent_local_monitor",
        "fixed_10",
        "fixed_722",
    ]
}

fn default_command(key: &str) -> String {
    match key {
        "link_command_disconnect" => "1",
        "link_command_monitor" => "2",
        "link_command_transceive" => "3",
        "link_command_remote" => "4",
        "link_command_status" => "70",
        "link_command_disconnect_all" => "806",
        "link_command_last_keyed" => "72",
        "link_command_local_monitor" => "75",
        "link_command_disconnect_permanent" => "811",
        "link_command_permanent_monitor" => "812",
        "link_command_permanent_transceive" => "813",
        "link_command_full_status" => "73",
        "link_command_reconnect_all" => "816",
        "link_command_permanent_local_monitor" => "818",
        "fixed_10" => "10",
        _ => "722",
    }
    .to_owned()
}

fn command_action(key: &str) -> Option<LinkAction> {
    Some(match key {
        "link_command_disconnect" => LinkAction::Disconnect,
        "link_command_monitor" => LinkAction::Monitor,
        "link_command_transceive" => LinkAction::Transceive,
        "link_command_remote" => LinkAction::Command,
        "link_command_status" => LinkAction::Status,
        "link_command_disconnect_all" => LinkAction::DisconnectAll,
        "link_command_last_keyed" => LinkAction::LastKeyed,
        "link_command_local_monitor" => LinkAction::LocalMonitor,
        "link_command_disconnect_permanent" => LinkAction::DisconnectPermanent,
        "link_command_permanent_monitor" => LinkAction::PermanentMonitor,
        "link_command_permanent_transceive" => LinkAction::PermanentTransceive,
        "link_command_full_status" => LinkAction::FullStatus,
        "link_command_reconnect_all" => LinkAction::ReconnectAll,
        "link_command_permanent_local_monitor" => LinkAction::PermanentLocalMonitor,
        "fixed_10" => LinkAction::DisconnectNonPermanentAll,
        "fixed_722" => LinkAction::Time,
        _ => return None,
    })
}

/// A validated node identity with the transport's 63-byte limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Construct an identity containing no whitespace/brackets and at most 63 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 63
            || value
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '[' | ']'))
        {
            return Err(ConfigError::structure(&value, "invalid node identity"));
        }
        Ok(Self(value))
    }

    /// Borrow the identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
