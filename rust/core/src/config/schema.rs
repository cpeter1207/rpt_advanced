//! Whole-document configuration validation.

use super::scope::{self, Scope, ScopeKind};
use super::{ConfigDocument, ConfigError, ConfigWarning, Resolution, parse};
use crate::access::AccessPolicy;
use crate::command::{CommandMapping, DtmfCommandMap, LinkAction};
use crate::schedule::{
    ScheduledAction, ScheduledEvent, ScheduledWindow, date_selector_valid, end_time_valid,
    start_time_valid, weekday_selector_valid,
};
use crate::template::{MessageTemplate, TemplateError};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

/// Validates section structure, typed values, and cross references.
#[derive(Clone, Copy, Debug, Default)]
pub struct Schema;

// Only documented option families reach key validation; unknown whole sections
// are diagnosed at the schema boundary instead of becoming a fictitious family.
#[derive(Clone, Copy)]
enum KnownScope {
    General,
    Identifier,
    Announcement,
    Courtesy { named: bool },
    Morse,
    Speech,
    Time,
    Template,
    Macro,
    Event,
    Permanent,
    Schedule,
}

impl KnownScope {
    fn from_kind(kind: ScopeKind) -> Option<Self> {
        Some(match kind {
            ScopeKind::General | ScopeKind::Node => Self::General,
            ScopeKind::IdentifierDefault | ScopeKind::IdentifierNode | ScopeKind::IdentifierSet => {
                Self::Identifier
            }
            ScopeKind::AnnouncementDefault
            | ScopeKind::AnnouncementNode
            | ScopeKind::AnnouncementSet => Self::Announcement,
            ScopeKind::CourtesyDefault | ScopeKind::CourtesyNode => Self::Courtesy { named: false },
            ScopeKind::CourtesySet => Self::Courtesy { named: true },
            ScopeKind::MorseDefault | ScopeKind::MorseNode => Self::Morse,
            ScopeKind::SpeechDefault | ScopeKind::SpeechNode => Self::Speech,
            ScopeKind::TimeDefault | ScopeKind::TimeNode => Self::Time,
            ScopeKind::TemplateGlobal | ScopeKind::TemplateNode => Self::Template,
            ScopeKind::MacroGlobal | ScopeKind::MacroNode => Self::Macro,
            ScopeKind::EventNode => Self::Event,
            ScopeKind::PermanentNode => Self::Permanent,
            ScopeKind::ScheduleNode => Self::Schedule,
            ScopeKind::Unknown => return None,
        })
    }
}

impl Schema {
    /// Validate a parsed document. Safely ignored input is returned as a warning.
    pub fn validate(document: &ConfigDocument) -> Result<Resolution<()>, ConfigError> {
        let mut warnings = Vec::new();
        let nodes: BTreeSet<_> = document.nodes().into_iter().collect();

        for (index, section) in document.sections().iter().enumerate() {
            let parsed = scope::parse_scope(section)
                .map_err(|()| ConfigError::structure(section, "invalid section name"))?;
            let repeated = document.sections()[..index].contains(section);
            if parsed.node.is_some_and(|node| node.len() > 63)
                || parsed.label.is_some_and(|label| label.len() > 63)
            {
                return Err(ConfigError::structure(
                    section,
                    "node or label exceeds transport limit",
                ));
            }
            if scope::is_node_scoped(parsed.kind)
                && !nodes.contains(parsed.node.unwrap_or_default())
            {
                return Err(ConfigError::structure(
                    section,
                    "scoped section references an unknown node",
                ));
            }
            if scope::is_named(parsed.kind) && repeated {
                return Err(ConfigError::structure(
                    section,
                    "duplicate named definition",
                ));
            }
            if repeated {
                continue;
            }
            let Some(known) = KnownScope::from_kind(parsed.kind) else {
                warnings.push(ConfigWarning::new(
                    document.section_line(index),
                    section,
                    "",
                    "",
                    "unknown section ignored",
                    "section ignored",
                ));
                continue;
            };
            for entry in document.section_entries(section) {
                if !key_known(known, &entry.key) {
                    warnings.push(ConfigWarning::new(
                        entry.line,
                        section,
                        &entry.key,
                        &entry.value,
                        "unknown option ignored",
                        "not configured",
                    ));
                } else if !value_valid(parsed.kind, &entry.key, &entry.value) {
                    warnings.push(ConfigWarning::new(
                        entry.line,
                        section,
                        &entry.key,
                        &entry.value,
                        "invalid option value",
                        &effective_fallback(document, parsed, &entry.key),
                    ));
                }
            }
        }

        validate_courtesies(document)?;
        validate_templates_and_macros(document)?;
        validate_events(document)?;
        validate_links(document)?;
        Ok(Resolution {
            value: (),
            warnings,
        })
    }
}

fn key_known(kind: KnownScope, key: &str) -> bool {
    match kind {
        KnownScope::General => matches!(
            key,
            "node_enabled"
                | "full_duplex"
                | "dtmf_muting"
                | "squelch_delay_ms"
                | "transmit_hang_ms"
                | "transmit_timeout_ms"
                | "timeout_lockout_ms"
                | "kerchunk_max_ms"
                | "telemetry_duck_db"
                | "courtesy_delay_ms"
                | "radio_channel"
                | "callsign"
                | "link_allow_nodes"
                | "link_deny_nodes"
                | "link_static_directory_file"
                | "link_directory_file"
                | "link_lookup_method"
                | "link_command_disconnect"
                | "link_command_monitor"
                | "link_command_transceive"
                | "link_command_remote"
                | "link_command_status"
                | "link_command_disconnect_all"
                | "link_command_last_keyed"
                | "link_command_local_monitor"
                | "link_command_disconnect_permanent"
                | "link_command_permanent_monitor"
                | "link_command_permanent_transceive"
                | "link_command_full_status"
                | "link_command_reconnect_all"
                | "link_command_permanent_local_monitor"
        ),
        KnownScope::Identifier => {
            matches!(
                key,
                "interval_ms"
                    | "priority"
                    | "first_key_only"
                    | "regardless_of_activity"
                    | "polite"
                    | "polite_maximum_wait_ms"
                    | "sound_file"
                    | "speech_text"
                    | "speech_model"
                    | "speech_speed_percent"
                    | "speech_level_db"
                    | "morse_text"
                    | "morse_speed_wpm"
                    | "morse_frequency_hz"
                    | "morse_level_db"
            )
        }
        KnownScope::Announcement => matches!(
            key,
            "interval_ms"
                | "sound_file"
                | "speech_text"
                | "speech_model"
                | "speech_speed_percent"
                | "speech_level_db"
                | "morse_text"
                | "morse_speed_wpm"
                | "morse_frequency_hz"
                | "morse_level_db"
        ),
        KnownScope::Courtesy { named } => {
            matches!(
                key,
                "sound_file"
                    | "speech_text"
                    | "speech_model"
                    | "speech_speed_percent"
                    | "morse_text"
                    | "morse_speed_wpm"
                    | "morse_frequency_hz"
                    | "tone_sequence"
                    | "level_db"
            ) || (named && matches!(key, "input" | "remote_node"))
        }
        KnownScope::Morse => {
            matches!(key, "frequency_hz" | "speed_wpm" | "level_db")
        }
        KnownScope::Speech => {
            matches!(key, "voice" | "speed_percent" | "level_db")
        }
        KnownScope::Time => key == "format",
        KnownScope::Template => key == "text",
        KnownScope::Macro => matches!(key, "action" | "target_node"),
        KnownScope::Event => matches!(key, "at" | "template" | "message" | "macro"),
        KnownScope::Permanent => key == "remote_node",
        KnownScope::Schedule => matches!(
            key,
            "remote_node"
                | "replace_permanent"
                | "days"
                | "dates"
                | "start_time"
                | "end_time"
                | "end_inactivity_ms"
        ),
    }
}

fn value_valid(kind: ScopeKind, key: &str, value: &str) -> bool {
    match key {
        "node_enabled"
        | "full_duplex"
        | "dtmf_muting"
        | "first_key_only"
        | "regardless_of_activity"
        | "polite" => parse::boolean(value).is_some(),
        "transmit_hang_ms"
        | "transmit_timeout_ms"
        | "timeout_lockout_ms"
        | "kerchunk_max_ms"
        | "courtesy_delay_ms"
        | "squelch_delay_ms"
        | "end_inactivity_ms" => parse::unsigned(value, 0, u64::MAX).is_some(),
        "polite_maximum_wait_ms" => parse::unsigned(value, 1, u64::MAX).is_some(),
        "interval_ms" => parse::unsigned(
            value,
            if matches!(
                kind,
                ScopeKind::IdentifierDefault | ScopeKind::IdentifierNode | ScopeKind::IdentifierSet
            ) {
                1
            } else {
                0
            },
            u64::MAX,
        )
        .is_some(),
        "priority" => parse::unsigned(value, 0, i32::MAX as u64).is_some(),
        "speech_speed_percent" | "speed_percent" => parse::unsigned(value, 1, 1000).is_some(),
        "morse_speed_wpm" | "speed_wpm" => parse::unsigned(value, 1, 100).is_some(),
        "morse_frequency_hz" | "frequency_hz" => {
            parse::unsigned(value, 1, u32::MAX as u64).is_some()
        }
        "telemetry_duck_db" | "speech_level_db" | "morse_level_db" | "level_db" => {
            parse::signed(value, -60, 0).is_some()
        }
        "format" => matches!(value, "12" | "24"),
        "callsign" => value.len() <= 63,
        "link_allow_nodes" | "link_deny_nodes" => AccessPolicy::list_valid(value),
        "link_lookup_method" => matches!(value, "both" | "dns" | "file"),
        "input" => matches!(value, "receiver" | "link"),
        "remote_node" | "target_node" => node_value_valid(value),
        "action" => ScheduledAction::from_str(value).is_ok(),
        "at" => ScheduledEvent::from_str(value).is_ok(),
        "start_time" => start_time_valid(value),
        "end_time" => end_time_valid(value),
        "days" => weekday_selector_valid(value),
        "dates" => date_selector_valid(value),
        key if key.starts_with("link_command_") => {
            DtmfCommandMap::validate(&[CommandMapping::new(value, LinkAction::Status)]).is_ok()
        }
        _ => true,
    }
}

fn node_value_valid(value: &str) -> bool {
    value.len() <= 63 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn default_value(kind: ScopeKind, key: &str) -> String {
    let value = match (kind, key) {
        (ScopeKind::General | ScopeKind::Node, "node_enabled" | "full_duplex" | "dtmf_muting") => {
            "yes"
        }
        (ScopeKind::General | ScopeKind::Node, "transmit_hang_ms") => "0",
        (ScopeKind::General | ScopeKind::Node, "transmit_timeout_ms") => "180000",
        (ScopeKind::General | ScopeKind::Node, "timeout_lockout_ms") => "30000",
        (ScopeKind::General | ScopeKind::Node, "kerchunk_max_ms") => "500",
        (ScopeKind::General | ScopeKind::Node, "telemetry_duck_db") => "-20",
        (ScopeKind::General | ScopeKind::Node, "courtesy_delay_ms") => "250",
        (ScopeKind::General | ScopeKind::Node, "squelch_delay_ms") => "0",
        (ScopeKind::General | ScopeKind::Node, "link_lookup_method") => "both",
        (_, "interval_ms")
            if matches!(
                kind,
                ScopeKind::IdentifierDefault | ScopeKind::IdentifierNode | ScopeKind::IdentifierSet
            ) =>
        {
            "600000"
        }
        (_, "interval_ms" | "priority") => "0",
        (_, "first_key_only" | "regardless_of_activity" | "polite") => "no",
        (_, "polite_maximum_wait_ms") => "60000",
        (_, "speech_speed_percent" | "speed_percent") => "100",
        (_, "speech_level_db") => "0",
        (_, "morse_speed_wpm" | "speed_wpm") => "20",
        (_, "morse_frequency_hz" | "frequency_hz") => "800",
        (_, "morse_level_db") => "-6",
        (_, "level_db")
            if matches!(
                kind,
                ScopeKind::CourtesyDefault | ScopeKind::CourtesyNode | ScopeKind::CourtesySet
            ) =>
        {
            "-20"
        }
        (_, "format") => "12",
        _ => "",
    };
    if let Some(command) = command_default(key) {
        command.to_owned()
    } else if value.is_empty() {
        "not configured".to_owned()
    } else {
        value.to_owned()
    }
}

fn command_default(key: &str) -> Option<&'static str> {
    Some(match key {
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
        _ => return None,
    })
}

fn effective_fallback(document: &ConfigDocument, scope: Scope<'_>, key: &str) -> String {
    let mut candidates: Vec<(String, &str)> = Vec::new();
    let node = scope.node.unwrap_or_default();
    let label = scope.label.unwrap_or_default();
    match scope.kind {
        ScopeKind::Node => candidates.push(("general".to_owned(), key)),
        ScopeKind::IdentifierNode => candidates.push(("identifier".to_owned(), key)),
        ScopeKind::IdentifierSet => {
            add_media_defaults(&mut candidates, key, node);
            candidates.push((format!("identifier {node}"), key));
            candidates.push(("identifier".to_owned(), key));
        }
        ScopeKind::AnnouncementNode => candidates.push(("announcement".to_owned(), key)),
        ScopeKind::AnnouncementSet => {
            add_media_defaults(&mut candidates, key, node);
            candidates.push((format!("announcement {node}"), key));
            candidates.push(("announcement".to_owned(), key));
        }
        ScopeKind::CourtesyNode => candidates.push(("courtesy".to_owned(), key)),
        ScopeKind::CourtesySet => {
            add_media_defaults(&mut candidates, key, node);
            candidates.push((format!("courtesy {node}"), key));
            candidates.push(("courtesy".to_owned(), key));
        }
        ScopeKind::MorseNode => candidates.push(("morse".to_owned(), key)),
        ScopeKind::SpeechNode => candidates.push(("speech".to_owned(), key)),
        ScopeKind::TimeNode => candidates.push(("time".to_owned(), key)),
        ScopeKind::MacroNode => candidates.push((format!("macro {label}"), key)),
        _ => {}
    }
    for (section, candidate_key) in candidates {
        if let Some(value) = document.lookup(candidate_key, &[section.as_str()]) {
            if value_valid(scope.kind, key, value) {
                return value.to_owned();
            }
        }
    }
    default_value(scope.kind, key)
}

fn add_media_defaults<'a>(candidates: &mut Vec<(String, &'a str)>, key: &'a str, node: &str) {
    let family_key = match key {
        "speech_speed_percent" => Some(("speech", "speed_percent")),
        "speech_level_db" => Some(("speech", "level_db")),
        "morse_speed_wpm" => Some(("morse", "speed_wpm")),
        "morse_frequency_hz" => Some(("morse", "frequency_hz")),
        "morse_level_db" => Some(("morse", "level_db")),
        _ => None,
    };
    if let Some((family, family_key)) = family_key {
        candidates.push((format!("{family} {node}"), family_key));
        candidates.push((family.to_owned(), family_key));
    }
}

fn valid_value<'a>(
    document: &'a ConfigDocument,
    section: &str,
    kind: ScopeKind,
    key: &str,
) -> Option<&'a str> {
    document
        .lookup(key, &[section])
        .filter(|value| value_valid(kind, key, value))
}

fn validate_courtesies(document: &ConfigDocument) -> Result<(), ConfigError> {
    let mut assignments: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    for section in unique_sections(document) {
        let parsed = scope::parse_scope(section).expect("section grammar checked");
        if parsed.kind != ScopeKind::CourtesySet {
            continue;
        }
        let input = valid_value(document, section, parsed.kind, "input").unwrap_or_default();
        if input.is_empty() {
            return Err(ConfigError::structure(
                section,
                "courtesy input is required",
            ));
        }
        let remote = valid_value(document, section, parsed.kind, "remote_node").unwrap_or_default();
        if input != "link" && !remote.is_empty() {
            return Err(ConfigError::structure(
                section,
                "courtesy remote node requires link input",
            ));
        }
        let identity = if input == "receiver" {
            ("receiver".to_owned(), String::new())
        } else {
            ("link".to_owned(), remote.to_owned())
        };
        let node_assignments = assignments.entry(parsed.node.unwrap()).or_default();
        if node_assignments.contains(&identity) {
            return Err(ConfigError::structure(
                section,
                "duplicate courtesy input assignment",
            ));
        }
        node_assignments.push(identity);
    }
    Ok(())
}

fn validate_templates_and_macros(document: &ConfigDocument) -> Result<(), ConfigError> {
    for section in unique_sections(document) {
        let parsed = scope::parse_scope(section).expect("section grammar checked");
        match parsed.kind {
            ScopeKind::TemplateGlobal | ScopeKind::TemplateNode => {
                let inherited;
                let text = if let Some(text) = document.lookup("text", &[section]) {
                    text
                } else if parsed.kind == ScopeKind::TemplateNode {
                    inherited = format!("template {}", parsed.label.unwrap());
                    document
                        .lookup("text", &[inherited.as_str()])
                        .unwrap_or_default()
                } else {
                    ""
                };
                if text.is_empty() {
                    return Err(ConfigError::structure(section, "template text is required"));
                }
                validate_template(section, text)?;
            }
            ScopeKind::MacroGlobal | ScopeKind::MacroNode => {
                let global = format!("macro {}", parsed.label.unwrap());
                let scopes = if parsed.kind == ScopeKind::MacroNode {
                    [section, global.as_str()]
                } else {
                    [section, section]
                };
                let action = lookup_valid(document, "action", &scopes, parsed.kind);
                let target =
                    lookup_valid(document, "target_node", &scopes, parsed.kind).unwrap_or_default();
                let Some(action) = action else {
                    return Err(ConfigError::structure(section, "macro action is required"));
                };
                if matches!(action, "connect" | "disconnect") && target.is_empty() {
                    return Err(ConfigError::structure(
                        section,
                        "macro target node is required",
                    ));
                }
                if matches!(action, "disconnect_all" | "reconnect_all") && !target.is_empty() {
                    return Err(ConfigError::structure(
                        section,
                        "macro target node is not allowed",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn lookup_valid<'a>(
    document: &'a ConfigDocument,
    key: &str,
    sections: &[&str],
    kind: ScopeKind,
) -> Option<&'a str> {
    sections.iter().find_map(|section| {
        document
            .lookup(key, &[*section])
            .filter(|value| value_valid(kind, key, value))
    })
}

fn validate_events(document: &ConfigDocument) -> Result<(), ConfigError> {
    for section in unique_sections(document) {
        let parsed = scope::parse_scope(section).expect("section grammar checked");
        if parsed.kind != ScopeKind::EventNode {
            continue;
        }
        let raw_at = document.lookup("at", &[section]).unwrap_or_default();
        if raw_at.is_empty() {
            return Err(ConfigError::structure(section, "event at is required"));
        }
        if ScheduledEvent::from_str(raw_at).is_err() {
            return Err(ConfigError::structure(section, "invalid event time"));
        }
        let template = document.lookup("template", &[section]).unwrap_or_default();
        let message = document.lookup("message", &[section]).unwrap_or_default();
        let macro_name = document.lookup("macro", &[section]).unwrap_or_default();
        if !message.is_empty() && !template.is_empty() {
            return Err(ConfigError::structure(
                section,
                "event message and template are mutually exclusive",
            ));
        }
        if message.is_empty() && template.is_empty() && macro_name.is_empty() {
            return Err(ConfigError::structure(
                section,
                "event message, template, or macro is required",
            ));
        }
        if !message.is_empty() {
            validate_template(section, message)?;
        }
        if !template.is_empty()
            && !named_visible(document, "template", parsed.node.unwrap(), template)
        {
            return Err(ConfigError::structure(
                section,
                "event references an unknown template",
            ));
        }
        if !macro_name.is_empty()
            && !named_visible(document, "macro", parsed.node.unwrap(), macro_name)
        {
            return Err(ConfigError::structure(
                section,
                "event references an unknown macro",
            ));
        }
    }
    Ok(())
}

fn named_visible(document: &ConfigDocument, family: &str, node: &str, label: &str) -> bool {
    let local = format!("{family} {node} {label}");
    let global = format!("{family} {label}");
    document.sections().contains(&local) || document.sections().contains(&global)
}

fn validate_links(document: &ConfigDocument) -> Result<(), ConfigError> {
    let mut permanent: BTreeMap<&str, BTreeMap<&str, (&str, &str)>> = BTreeMap::new();
    let mut remotes: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for section in unique_sections(document) {
        let parsed = scope::parse_scope(section).expect("section grammar checked");
        if parsed.kind != ScopeKind::PermanentNode {
            continue;
        }
        let remote = valid_value(document, section, parsed.kind, "remote_node").unwrap_or_default();
        if remote.is_empty() {
            return Err(ConfigError::structure(
                section,
                "permanent remote node is required",
            ));
        }
        let node = parsed.node.unwrap();
        if remote == node {
            return Err(ConfigError::structure(
                section,
                "configured link cannot target its local node",
            ));
        }
        if !remotes.entry(node).or_default().insert(remote) {
            return Err(ConfigError::structure(
                section,
                "duplicate configured link remote node",
            ));
        }
        permanent
            .entry(node)
            .or_default()
            .insert(parsed.label.unwrap(), (remote, section));
    }

    for section in unique_sections(document) {
        let parsed = scope::parse_scope(section).expect("section grammar checked");
        if parsed.kind != ScopeKind::ScheduleNode {
            continue;
        }
        let required = |key| valid_value(document, section, parsed.kind, key).unwrap_or_default();
        let remote = required("remote_node");
        let replacement = required("replace_permanent");
        let start = required("start_time");
        let end = required("end_time");
        if remote.is_empty() || replacement.is_empty() || start.is_empty() || end.is_empty() {
            return Err(ConfigError::structure(
                section,
                "schedule remote node, replacement, start time, and end time are required",
            ));
        }
        let node = parsed.node.unwrap();
        if remote == node {
            return Err(ConfigError::structure(
                section,
                "configured link cannot target its local node",
            ));
        }
        let Some((primary_remote, _)) =
            permanent.get(node).and_then(|links| links.get(replacement))
        else {
            return Err(ConfigError::structure(
                section,
                "schedule references an unknown permanent link",
            ));
        };
        if remote == *primary_remote {
            return Err(ConfigError::structure(
                section,
                "schedule replacement must select another node",
            ));
        }
        let days = valid_value(document, section, parsed.kind, "days").unwrap_or_default();
        let dates = valid_value(document, section, parsed.kind, "dates").unwrap_or_default();
        if ScheduledWindow::parse(
            (!days.is_empty()).then_some(days),
            (!dates.is_empty()).then_some(dates),
            start,
            end,
        )
        .is_err()
        {
            return Err(ConfigError::structure(section, "invalid schedule window"));
        }
        if !remotes.entry(node).or_default().insert(remote) {
            return Err(ConfigError::structure(
                section,
                "duplicate configured link remote node",
            ));
        }
    }
    Ok(())
}

fn unique_sections(document: &ConfigDocument) -> impl Iterator<Item = &str> {
    document
        .sections()
        .iter()
        .enumerate()
        .filter(|(index, section)| !document.sections()[..*index].contains(section))
        .map(|(_, section)| section.as_str())
}

fn validate_template(section: &str, text: &str) -> Result<(), ConfigError> {
    match MessageTemplate::parse(text) {
        Ok(_) => Ok(()),
        Err(TemplateError::InvalidSyntax) => {
            Err(ConfigError::structure(section, "invalid message template"))
        }
        Err(TemplateError::OutputTooLong) => Err(ConfigError::structure(
            section,
            "scheduled message exceeds maximum output",
        )),
    }
}
