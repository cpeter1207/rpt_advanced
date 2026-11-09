//! Named direct-trigger streams, independent of typed link-action macros.

use super::{
    ConfigDocument, ConfigError, ConfigWarning, NodeId, Resolution, parse,
    scope::{self, ScopeKind},
    settings::lookup_valid_scopes,
};
use crate::{
    command::{DtmfCommandMap, LinkAction},
    dtmf_macro::{self, CompiledDtmfMacro, DtmfMacroLimits},
};
use std::collections::BTreeSet;

/// Resolved operator-authored DTMF stream; credentials must never be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedDtmfMacroSettings {
    /// Descriptive label, independent of the direct trigger.
    pub name: String,
    /// Full star-prefixed command trigger.
    pub trigger: String,
    /// Require the source's existing administrative grant throughout execution.
    pub protected: bool,
    /// Explicitly terminated commands and comma pauses.
    pub commands: String,
}

pub(super) fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "trigger" => value.strip_prefix('*').is_some_and(|digits| {
            !digits.is_empty()
                && digits.len() <= 63
                && digits
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'A'..=b'D'))
        }),
        "protected" => parse::boolean(value).is_some(),
        // Syntax/credential validation requires the effective node's command map.
        "commands" => !value.trim().is_empty(),
        _ => false,
    }
}

/// Resolve inherited prepared bounds; invalid overrides retain a valid ancestor.
pub(crate) fn macro_limits(document: &ConfigDocument, node: &NodeId) -> DtmfMacroLimits {
    let scopes = [node.as_str().to_owned(), "general".into()];
    let number = |key, maximum, fallback| {
        lookup_valid_scopes(document, key, &scopes, |raw| {
            parse::unsigned(raw, 1, maximum)
        })
        .unwrap_or(fallback)
    };
    DtmfMacroLimits {
        pause_ms: number("dtmf_macro_pause_ms", 120_000, 2000),
        max_symbols: number("dtmf_macro_max_symbols", 4096, 4096) as usize,
        max_duration_ms: number("dtmf_macro_max_duration_ms", 120_000, 120_000),
    }
}

/// Resolve inherited definitions; the prepared generation admits only compiled entries.
pub fn resolve_dtmf_macros(
    document: &ConfigDocument,
    node: &NodeId,
) -> Result<Resolution<Vec<ResolvedDtmfMacroSettings>>, ConfigError> {
    let mut labels = BTreeSet::new();
    for section in document.sections() {
        let parsed = scope::parse_scope(section)
            .map_err(|()| ConfigError::structure(section, "invalid section name"))?;
        if parsed.kind == ScopeKind::DtmfMacroGlobal
            || (parsed.kind == ScopeKind::DtmfMacroNode && parsed.node == Some(node.as_str()))
        {
            labels.insert(parsed.label.unwrap());
        }
    }
    let mut value = Vec::new();
    let mut warnings = Vec::new();
    for label in labels {
        let scopes = [
            format!("dtmf_macro {} {label}", node.as_str()),
            format!("dtmf_macro {label}"),
        ];
        let get = |key| {
            lookup_valid_scopes(document, key, &scopes, |raw| {
                value_valid(key, raw).then(|| raw.to_owned())
            })
        };
        let trigger = get("trigger").unwrap_or_default();
        let commands = get("commands").unwrap_or_default();
        if trigger.is_empty() || commands.is_empty() {
            warnings.push(ConfigWarning::new(
                0,
                &scopes[0],
                "commands",
                &commands,
                "macro trigger and commands are required",
                "macro unavailable",
            ));
            continue;
        }
        value.push(ResolvedDtmfMacroSettings {
            name: label.into(),
            trigger,
            protected: get("protected")
                .and_then(|raw| parse::boolean(&raw))
                .unwrap_or(true),
            commands,
        });
    }
    // Credential classification must follow remapping. Resolve that namespace without
    // recursively calling whole-document Schema validation.
    let scopes = [node.as_str(), "general"];
    let mut mappings = DtmfCommandMap::standard().mappings().to_vec();
    for mapping in &mut mappings {
        let key = super::settings::command_keys()
            .into_iter()
            .find(|key| super::settings::command_action(key) == Some(mapping.action))
            .expect("standard DTMF action has a configuration key");
        for section in scopes {
            if let Some(raw) = document.lookup(key, &[section]).filter(|raw| {
                DtmfCommandMap::validate(&[crate::command::CommandMapping::new(
                    *raw,
                    LinkAction::Status,
                )])
                .is_ok()
            }) {
                mapping.digits = raw.into();
                break;
            }
        }
    }
    if let Ok(map) = DtmfCommandMap::new(mappings) {
        let triggers = value
            .iter()
            .map(|definition| definition.trigger.clone())
            .collect::<Vec<_>>();
        for definition in &value {
            if let Err(error) = dtmf_macro::compile(
                definition,
                macro_limits(document, node),
                &map,
                node.as_str(),
                &triggers,
            ) {
                // Only generation preparation knows the enabled shared-radio command
                // family. Avoid diagnosing its otherwise valid syntax as unavailable.
                if error.message() != "invalid or unterminated macro command stream" {
                    warnings.push(ConfigWarning::new(
                        0,
                        &format!("dtmf_macro {} {}", node.as_str(), definition.name),
                        "commands",
                        &definition.commands,
                        error.message(),
                        "macro unavailable",
                    ));
                }
            }
        }
    }
    Ok(Resolution { value, warnings })
}

/// Compile against the prepared runtime's actual enabled command family.
pub(crate) fn compile_macros(
    document: &ConfigDocument,
    node: &NodeId,
    map: &DtmfCommandMap,
) -> Result<Resolution<Vec<CompiledDtmfMacro>>, ConfigError> {
    let resolved = resolve_dtmf_macros(document, node)?;
    let triggers = resolved
        .value
        .iter()
        .map(|item| item.trigger.clone())
        .collect::<Vec<_>>();
    let mut warnings = resolved.warnings;
    let value = resolved
        .value
        .iter()
        .filter_map(|definition| {
            match dtmf_macro::compile(
                definition,
                macro_limits(document, node),
                map,
                node.as_str(),
                &triggers,
            ) {
                Ok(compiled) => Some(compiled),
                Err(error) => {
                    warnings.push(ConfigWarning::new(
                        0,
                        &format!("dtmf_macro {} {}", node.as_str(), definition.name),
                        "commands",
                        &definition.commands,
                        error.message(),
                        "macro unavailable",
                    ));
                    None
                }
            }
        })
        .collect();
    Ok(Resolution { value, warnings })
}
