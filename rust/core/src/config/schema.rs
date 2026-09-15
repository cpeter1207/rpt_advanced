//! Section and option schema validation.

use super::{parse, ConfigDocument, ConfigError, ConfigWarning, Resolution};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SectionKind {
    General,
    Node,
    Identifier,
    Announcement,
    Courtesy,
    Morse,
    Speech,
    Time,
    Template,
    Macro,
    Event,
    Permanent,
    Schedule,
}

/// Classify a documented section name and reject whitespace/bracket injection.
pub(crate) fn section_kind(name: &str) -> Option<SectionKind> {
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || matches!(c, '[' | ']')) {
        return None;
    }
    match name {
        "general" => return Some(SectionKind::General),
        "identifier" => return Some(SectionKind::Identifier),
        "announcement" => return Some(SectionKind::Announcement),
        "courtesy" => return Some(SectionKind::Courtesy),
        "morse" => return Some(SectionKind::Morse),
        "speech" => return Some(SectionKind::Speech),
        "time" => return Some(SectionKind::Time),
        "template" | "macro" | "event" | "permanent" | "schedule" => return None,
        _ => {}
    }
    let parts: Vec<_> = name.split(' ').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    match parts.as_slice() {
        [node] => (!node.is_empty()).then_some(SectionKind::Node),
        [prefix, _node] if matches!(*prefix, "morse" | "speech" | "time") => {
            Some(match *prefix {
                "morse" => SectionKind::Morse,
                "speech" => SectionKind::Speech,
                _ => SectionKind::Time,
            })
        }
        [prefix, _node]
            if matches!(*prefix, "identifier" | "announcement" | "courtesy") =>
        {
            Some(match *prefix {
                "identifier" => SectionKind::Identifier,
                "announcement" => SectionKind::Announcement,
                _ => SectionKind::Courtesy,
            })
        }
        [prefix, _node, _set] if matches!(*prefix, "identifier" | "announcement" | "courtesy") => {
            Some(match *prefix {
                "identifier" => SectionKind::Identifier,
                "announcement" => SectionKind::Announcement,
                _ => SectionKind::Courtesy,
            })
        }
        [prefix, _label] if matches!(*prefix, "template" | "macro") => Some(match *prefix {
            "template" => SectionKind::Template,
            _ => SectionKind::Macro,
        }),
        [prefix, _node, _label]
            if matches!(*prefix, "template" | "macro" | "event" | "permanent" | "schedule") =>
        {
            Some(match *prefix {
                "template" => SectionKind::Template,
                "macro" => SectionKind::Macro,
                "event" => SectionKind::Event,
                "permanent" => SectionKind::Permanent,
                _ => SectionKind::Schedule,
            })
        }
        _ => None,
    }
}

fn key_known(section: &str, kind: SectionKind, key: &str) -> bool {
    let node = matches!(kind, SectionKind::General | SectionKind::Node);
    let identifier = kind == SectionKind::Identifier;
    let announcement = kind == SectionKind::Announcement;
    let courtesy = kind == SectionKind::Courtesy;
    (node && matches!(
        key,
        "node_enabled" | "full_duplex" | "dtmf_muting" | "transmit_hang_ms"
            | "transmit_timeout_ms" | "timeout_lockout_ms" | "kerchunk_max_ms"
            | "telemetry_duck_db" | "courtesy_delay_ms" | "radio_channel" | "callsign"
            | "link_allow_nodes" | "link_deny_nodes" | "link_static_directory_file"
            | "link_directory_file" | "link_lookup_method" | "link_command_disconnect"
            | "link_command_monitor" | "link_command_transceive" | "link_command_remote"
            | "link_command_status" | "link_command_disconnect_all" | "link_command_last_keyed"
            | "link_command_local_monitor" | "link_command_disconnect_permanent"
            | "link_command_permanent_monitor" | "link_command_permanent_transceive"
            | "link_command_full_status" | "link_command_reconnect_all"
            | "link_command_permanent_local_monitor"
    )) || (identifier && matches!(
        key,
        "interval_ms" | "priority" | "first_key_only" | "regardless_of_activity" | "polite"
            | "polite_maximum_wait_ms" | "sound_file" | "speech_text" | "speech_model"
            | "speech_speed_percent" | "speech_level_db" | "morse_text" | "morse_speed_wpm"
            | "morse_frequency_hz" | "morse_level_db"
    )) || (announcement && matches!(
        key,
        "interval_ms" | "sound_file" | "speech_text" | "speech_model" | "speech_speed_percent"
            | "speech_level_db" | "morse_text" | "morse_speed_wpm" | "morse_frequency_hz"
            | "morse_level_db"
    )) || (courtesy && (matches!(
        key,
        "sound_file" | "speech_text" | "speech_model" | "speech_speed_percent" | "morse_text"
            | "morse_speed_wpm" | "morse_frequency_hz" | "tone_sequence" | "level_db"
    ) || (section.split(' ').count() == 3 && matches!(key, "input" | "remote_node"))))
        || matches!(kind, SectionKind::Morse) && matches!(key, "frequency_hz" | "speed_wpm" | "level_db")
        || matches!(kind, SectionKind::Speech) && matches!(key, "voice" | "speed_percent" | "level_db")
        || matches!(kind, SectionKind::Time) && key == "format"
        || matches!(kind, SectionKind::Template) && key == "text"
        || matches!(kind, SectionKind::Macro) && matches!(key, "action" | "target_node")
        || matches!(kind, SectionKind::Event) && matches!(key, "at" | "template" | "message" | "macro")
        || matches!(kind, SectionKind::Permanent) && key == "remote_node"
        || matches!(kind, SectionKind::Schedule)
            && matches!(key, "remote_node" | "replace_permanent" | "days" | "dates" | "start_time" | "end_time" | "end_inactivity_ms")
}

fn value_valid(kind: SectionKind, key: &str, value: &str) -> bool {
    match key {
        "node_enabled" | "full_duplex" | "dtmf_muting" | "first_key_only" | "regardless_of_activity" | "polite" => parse::boolean(value).is_some(),
        "transmit_hang_ms" | "transmit_timeout_ms" | "timeout_lockout_ms" | "kerchunk_max_ms" | "courtesy_delay_ms" | "polite_maximum_wait_ms" | "end_inactivity_ms" => parse::unsigned(value, 0, u64::MAX).is_some(),
        "interval_ms" => parse::unsigned(value, if kind == SectionKind::Identifier { 1 } else { 0 }, u64::MAX).is_some(),
        "priority" => parse::unsigned(value, 0, i32::MAX as u64).is_some(),
        "speech_speed_percent" | "speed_percent" => parse::unsigned(value, 1, 1000).is_some(),
        "morse_speed_wpm" | "speed_wpm" => parse::unsigned(value, 1, 100).is_some(),
        "morse_frequency_hz" | "frequency_hz" => parse::unsigned(value, 1, u32::MAX as u64).is_some(),
        "telemetry_duck_db" | "speech_level_db" | "morse_level_db" | "level_db" => parse::signed(value, -60, 0).is_some(),
        "format" => matches!(parse::unsigned(value, 12, 24), Some(12 | 24)),
        "link_lookup_method" => matches!(value, "both" | "dns" | "file"),
        "input" => matches!(value, "receiver" | "link"),
        "remote_node" => value.is_empty() || (value.len() < 64 && value.bytes().all(|b| b.is_ascii_digit())),
        key if key.starts_with("link_command_") => value.len() <= 63 && value.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'A'..=b'D')),
        _ => true,
    }
}

/// Whole-document schema validator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Schema;

impl Schema {
    /// Validate section structure and report unknown or invalid options as recoverable warnings.
    pub fn validate(document: &ConfigDocument) -> Result<Resolution<()>, ConfigError> {
        let mut warnings = Vec::new();
        for (index, section) in document.sections().iter().enumerate() {
            let Some(kind) = section_kind(section) else {
                return Err(ConfigError::structure(section, "invalid section name"));
            };
            if index > 0 && is_named(section) && document.sections()[..index].contains(section) {
                return Err(ConfigError::structure(section, "duplicate named definition"));
            }
            for entry in document.section_entries(section) {
                if !key_known(section, kind, &entry.key) || !value_valid(kind, &entry.key, &entry.value) {
                    warnings.push(ConfigWarning::new(&entry.section, &entry.key, "using inherited default"));
                }
            }
            validate_required(document, section, kind)?;
        }
        Ok(Resolution { value: (), warnings })
    }
}

fn validate_required(document: &ConfigDocument, section: &str, kind: SectionKind) -> Result<(), ConfigError> {
    let value = |key: &str| {
        document
            .section_entries(section)
            .filter(|entry| entry.key == key)
            .last()
            .map(|entry| entry.value.as_str())
            .unwrap_or("")
    };
    match kind {
        SectionKind::Template if value("text").is_empty() => {
            Err(ConfigError::structure(section, "template text is required"))
        }
        SectionKind::Macro => {
            let action = value("action");
            if action.is_empty() {
                return Err(ConfigError::structure(section, "macro action is required"));
            }
            let needs_target = matches!(action, "connect" | "disconnect");
            if needs_target && value("target_node").is_empty() {
                return Err(ConfigError::structure(section, "macro target node is required"));
            }
            if !needs_target && !value("target_node").is_empty() {
                return Err(ConfigError::structure(section, "macro target node is not allowed"));
            }
            Ok(())
        }
        SectionKind::Event => {
            if value("at").is_empty() {
                return Err(ConfigError::structure(section, "event at is required"));
            }
            let sources = ["template", "message", "macro"]
                .into_iter()
                .filter(|key| !value(key).is_empty())
                .count();
            if sources == 0 {
                return Err(ConfigError::structure(section, "event message, template, or macro is required"));
            }
            if sources > 1 && !value("template").is_empty() && !value("message").is_empty() {
                return Err(ConfigError::structure(section, "event message and template are mutually exclusive"));
            }
            Ok(())
        }
        SectionKind::Permanent if value("remote_node").is_empty() => {
            Err(ConfigError::structure(section, "permanent remote node is required"))
        }
        SectionKind::Schedule => {
            if value("remote_node").is_empty()
                || value("replace_permanent").is_empty()
                || value("start_time").is_empty()
                || value("end_time").is_empty()
            {
                return Err(ConfigError::structure(
                    section,
                    "schedule remote node, replacement, start time, and end time are required",
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn is_named(section: &str) -> bool {
    matches!(section.split(' ').next(), Some("template" | "macro" | "event" | "permanent" | "schedule"))
}
