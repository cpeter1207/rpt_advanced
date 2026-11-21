//! Prepare a command generation from the registry-bound command section and named macro members.
use super::candidate::{
    ModularCandidate, ResourceBinding, ResourceId, ResourceKind, ResourceScope,
};
use crate::{
    command::LinkAction,
    command_language::{Action, CommandLanguage, LanguageError, LanguageWarning, PreparedAction},
    site::SiteCommand,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Commands prepared for one real invoking scope; all sparse member projections retain their IDs.
pub struct ScopedCommands {
    /// Site or invoking port for this generation.
    pub scope: ResourceScope,
    /// Effective commands and bounded macro sequences.
    pub language: std::sync::Arc<CommandLanguage>,
    /// Recognized action/collector fields keyed by their actual authoring resource.
    local: BTreeMap<ResourceId, Map<String, Value>>,
    macro_owners: BTreeMap<String, ResourceBinding>,
}
impl ScopedCommands {
    /// Return accepted fields for one concrete resource. No default or inherited fields are inserted.
    pub fn local(&self, id: ResourceId) -> Option<&Map<String, Value>> {
        self.local.get(&id)
    }
    /// Resolve the same exact macro key for a schedule, nested action or DTMF binding.
    pub fn named_macro(&self, key: &str) -> Option<(ResourceBinding, &[PreparedAction])> {
        Some((
            *self.macro_owners.get(key)?,
            self.language.macros.get(key)?.as_slice(),
        ))
    }
}
impl ModularCandidate<'_> {
    /// Resolve only captured resources for the given site or port, then validate effective references.
    pub fn command_language(&self, scope: ResourceScope) -> Result<ScopedCommands, LanguageError> {
        self.command_language_with_mappings(scope, |_, _| {})
    }

    /// Also observe complete validated mapping bodies, including built-ins, without
    /// retaining a parallel JSON inventory in the live command engine.
    pub fn command_language_with_mappings(
        &self,
        scope: ResourceScope,
        mapping: impl FnMut(&str, &Value),
    ) -> Result<ScopedCommands, LanguageError> {
        let registry = self.registry();
        let site = ResourceScope::Site(registry.site_id());
        let scopes = match scope {
            ResourceScope::Site(_) if scope == site => [Some(site), None],
            ResourceScope::Port(port) if self.port_metadata(port).is_some() => {
                [Some(site), Some(scope)]
            }
            _ => return Err(LanguageError::MissingResource),
        };
        let mut bodies = [Value::Object(Map::new()), Value::Object(Map::new())];
        let mut macro_owners = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut selectors = BTreeMap::new();
        let mut embedded = false;
        let mut selector_warnings = Vec::new();
        for (i, owner) in scopes.into_iter().enumerate() {
            let Some(owner) = owner else { continue };
            let mut settings = self
                .staged(owner, ResourceKind::Command)
                .map(|r| {
                    let mut body = Map::new();
                    for (k, v) in r.fields() {
                        // The admin/regeneration/muting fields belong to their existing owners.
                        if !matches!(k.as_str(), "admin" | "regeneration" | "muting" | "macros") {
                            body.insert(k.clone(), v.clone());
                        }
                    }
                    embedded |= r.fields().contains_key("macros");
                    body
                })
                .unwrap_or_default();
            let mut macros = Map::new();
            for (binding, key) in registry.members().iter().filter(|(b, _)| b.scope == owner) {
                let Some(staged) = self.staged_member(*binding) else {
                    continue;
                };
                names.insert((binding.kind, key.as_str()));
                if matches!(binding.kind, ResourceKind::Input | ResourceKind::Sensor)
                    && let Some(field) = staged.fields().get("dtmf_id")
                {
                    if let Some(value) = field.as_str().filter(|s| {
                        !s.is_empty()
                            && !s.starts_with('0')
                            && s.bytes().all(|b| b.is_ascii_digit())
                    }) {
                        selectors.insert((binding.kind, key.as_str()), value);
                    } else {
                        selector_warnings.push(LanguageWarning::Invalid("dtmf_id"));
                    }
                }
                if binding.kind == ResourceKind::Macro {
                    macros.insert(
                        key.as_str().to_owned(),
                        Value::Object(staged.fields().clone()),
                    );
                    macro_owners.insert(key.as_str().to_owned(), *binding);
                }
            }
            settings.insert("macros".into(), Value::Object(macros));
            bodies[i] = Value::Object(settings);
        }
        let mut language = CommandLanguage::resolve_with_mappings(&bodies[0], &bodies[1], mapping)?;
        language.warnings.extend(selector_warnings);
        if embedded {
            language.warnings.push(LanguageWarning::Unknown);
        }
        for action in language.static_actions() {
            let named = match action {
                Action::Site(SiteCommand::Output { name, .. }) => {
                    Some((ResourceKind::Output, name))
                }
                Action::Site(SiteCommand::Fan { name, .. }) => Some((ResourceKind::Fan, name)),
                Action::Site(
                    SiteCommand::AlarmAcknowledge(name) | SiteCommand::AlarmReset(name),
                ) => Some((ResourceKind::Alarm, name)),
                Action::Site(SiteCommand::ResetExtrema(name)) => Some((ResourceKind::Sensor, name)),
                Action::Link(c)
                    if c.action == LinkAction::SiteStatus
                        && selectors
                            .values()
                            .filter(|&&v| v == c.node.as_str())
                            .take(2)
                            .count()
                            != 1 =>
                {
                    return Err(LanguageError::MissingResource);
                }
                _ => None,
            };
            if let Some((kind, name)) = named
                && !names.contains(&(kind, name.as_str()))
            {
                return Err(LanguageError::MissingResource);
            }
        }
        let mut local = BTreeMap::new();
        for (i, owner) in scopes.into_iter().enumerate() {
            let Some(owner) = owner else { continue };
            let mut settings = language.local[i]
                .as_object()
                .expect("resolved command layer")
                .clone();
            if let Some(Value::Object(macros)) = settings.remove("macros") {
                for (key, body) in macros {
                    let binding = registry
                        .named_binding(owner, ResourceKind::Macro, &super::PortKey::new(key))
                        .expect("registered macro");
                    if let Value::Object(fields) = body {
                        local.insert(binding.id, fields);
                    }
                }
            }
            if let Some(binding) = registry.binding(owner, ResourceKind::Command) {
                local.insert(binding.id, settings);
            }
        }
        Ok(ScopedCommands {
            scope,
            language: language.into(),
            local,
            macro_owners,
        })
    }
}
