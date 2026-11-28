//! Prepared flat event members in the invoking command namespace.
use super::{
    PortKey,
    candidate::{
        CandidateWarning, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
        ResourceScope,
    },
    commands::ScopedCommands,
};
use crate::schedule::recurrence::{Error as CalendarError, Recurrence};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Arc};

/// Calendar or once-per-process controller activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Safe activation of the controller.
    ControllerStart,
    /// Compiled hron expression.
    Calendar(Arc<Recurrence>),
}

/// One effective macro reference in the invoking namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroRef {
    /// Stable declaring member.
    pub binding: ResourceBinding,
    /// Exact key for the shared macro executor.
    pub key: String,
}

/// Independent calendar warning with no boundary grace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// Compiled warning occurrence.
    pub at: Arc<Recurrence>,
    /// Resolved warning command.
    pub command: MacroRef,
}

/// One effective boundary, validated after inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// Trigger or captured calendar.
    pub at: Trigger,
    /// Complete macro invocation.
    pub command: MacroRef,
    /// Grace and required continuous idle time in whole minutes.
    pub politeness_minutes: u64,
    /// Warnings in declaration order.
    pub warnings: Vec<Warning>,
}

/// Complete immutable scheduled event declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Stable registry-owned declaring member.
    pub binding: ResourceBinding,
    /// Invoking owner, which may inherit the declaration.
    pub owner: ResourceScope,
    /// Required start.
    pub start: Boundary,
    /// Optional paired end.
    pub end: Option<Boundary>,
    /// Optional explicit same-instant order.
    pub sequence_number: Option<i128>,
    /// Recover an open window on safe controller startup.
    pub restart_recovery: bool,
    /// Existing permanent-group binding and exact group key.
    pub replace_permanent: Option<(ResourceBinding, PortKey)>,
}

/// Fatal, value-free errors when no valid event generation can be prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// Required effective boundary or macro is unavailable.
    Incomplete,
    /// Invalid effective relationship, including an unresolved named reference.
    Relationship,
    /// Provider unavailable or failed during preparation.
    Calendar(CalendarError),
}

/// Resolved members and normalized sparse public authoring fields.
pub struct ScopedEvents {
    pub(crate) owner: ResourceScope,
    pub(crate) port_key: Option<PortKey>,
    /// Prepared events in this invoking scope.
    pub events: Vec<Event>,
    /// Value-free diagnostics with their actual authoring resource.
    pub warnings: Vec<CandidateWarning>,
    /// Sparse recognized fields by authoring member.
    pub local: BTreeMap<ResourceId, Map<String, Value>>,
}

type CompiledWarnings = Vec<(Arc<Recurrence>, String)>;
#[derive(Default)]
struct SparseBoundary {
    at: Option<Trigger>,
    command: Option<String>,
    minutes: u64,
    warnings: CompiledWarnings,
    invalid_at: Option<CalendarError>,
}
#[derive(Default)]
struct Inherited {
    start: SparseBoundary,
    end: Option<SparseBoundary>,
    sequence: Option<i128>,
    recovery: bool,
    group: Option<PortKey>,
}
struct Layer<'a> {
    binding: ResourceBinding,
    zone: Option<&'a str>,
    warnings: &'a mut Vec<CandidateWarning>,
}
fn name(v: &Value) -> Option<&str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 63)
}
impl Layer<'_> {
    fn warn(&mut self, warning: super::ResourceWarning) {
        self.warnings.push(CandidateWarning {
            resource: self.binding,
            warning,
        })
    }
    fn invalid(&mut self, field: &'static str) {
        self.warn(super::ResourceWarning::InvalidValue(field))
    }
    fn warning_list(&mut self, v: &Value) -> Result<Option<CompiledWarnings>, EventError> {
        let Some(list) = v.as_array() else {
            return Ok(None);
        };
        let mut result = Vec::with_capacity(list.len());
        for elem in list {
            let Some(row) = elem.as_object() else {
                return Ok(None);
            };
            for _ in row.keys().filter(|k| !matches!(k.as_str(), "at" | "macro")) {
                self.warn(super::ResourceWarning::UnknownField);
            }
            let (Some(at), Some(key)) = (
                row.get("at").and_then(Value::as_str),
                row.get("macro").and_then(name),
            ) else {
                return Ok(None);
            };
            match Recurrence::compile(at, self.zone) {
                Ok(compiled) => result.push((Arc::new(compiled), key.to_owned())),
                Err(CalendarError::Provider) => {
                    return Err(EventError::Calendar(CalendarError::Provider));
                }
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(result))
    }
    fn boundary(
        &mut self,
        start: bool,
        fields: &Map<String, Value>,
        out: &mut SparseBoundary,
    ) -> Result<Map<String, Value>, EventError> {
        let mut accepted = Map::new();
        for (k, v) in fields {
            match k.as_str() {
                "at" => {
                    if start && v.as_str() == Some("controller_start") {
                        out.at = Some(Trigger::ControllerStart)
                    } else {
                        let result = v
                            .as_str()
                            .ok_or(CalendarError::Text)
                            .and_then(|s| Recurrence::compile(s, self.zone));
                        match result {
                            Ok(s) => out.at = Some(Trigger::Calendar(Arc::new(s))),
                            Err(CalendarError::Provider) => {
                                return Err(EventError::Calendar(CalendarError::Provider));
                            }
                            Err(err) => {
                                out.invalid_at = Some(err);
                                self.invalid("at");
                                continue;
                            }
                        }
                    }
                }
                "macro" => {
                    if let Some(s) = name(v) {
                        out.command = Some(s.to_owned())
                    } else {
                        self.invalid("macro");
                        continue;
                    }
                }
                "politeness_minutes" => {
                    if let Some(n) = v.as_u64() {
                        out.minutes = n
                    } else {
                        self.invalid("politeness_minutes");
                        continue;
                    }
                }
                "warnings" => {
                    if let Some(list) = self.warning_list(v)? {
                        let sparse = list
                            .iter()
                            .map(|(at, key)| serde_json::json!({"at":at.source(),"macro":key}))
                            .collect();
                        accepted.insert(k.clone(), Value::Array(sparse));
                        out.warnings = list;
                        continue;
                    } else {
                        self.invalid("warnings");
                        continue;
                    }
                }
                _ => {
                    self.warn(super::ResourceWarning::UnknownField);
                    continue;
                }
            }
            accepted.insert(k.clone(), v.clone());
        }
        Ok(accepted)
    }
    fn event(
        &mut self,
        fields: &Map<String, Value>,
        out: &mut Inherited,
    ) -> Result<Map<String, Value>, EventError> {
        let mut local = Map::new();
        for (k, v) in fields {
            match k.as_str() {
                "start" | "end" => {
                    let start = k == "start";
                    let Some(fields) = v.as_object() else {
                        self.invalid(if start { "start" } else { "end" });
                        continue;
                    };
                    let boundary = if start {
                        &mut out.start
                    } else {
                        out.end.get_or_insert_with(Default::default)
                    };
                    local.insert(k.clone(), self.boundary(start, fields, boundary)?.into());
                    continue;
                }
                "sequence_number" => {
                    if let Some(n) = v
                        .as_i64()
                        .map(i128::from)
                        .or_else(|| v.as_u64().map(i128::from))
                    {
                        out.sequence = Some(n)
                    } else {
                        self.invalid("sequence_number");
                        continue;
                    }
                }
                "restart_recovery" => {
                    if let Some(b) = v.as_bool() {
                        out.recovery = b
                    } else {
                        self.invalid("restart_recovery");
                        continue;
                    }
                }
                "replace_permanent" => {
                    if let Some(s) = name(v) {
                        out.group = Some(PortKey::new(s))
                    } else {
                        self.invalid("replace_permanent");
                        continue;
                    }
                }
                _ => {
                    self.warn(super::ResourceWarning::UnknownField);
                    continue;
                }
            }
            local.insert(k.clone(), v.clone());
        }
        Ok(local)
    }
}
impl SparseBoundary {
    fn resolve(
        self,
        commands: &ScopedCommands,
        macros: &BTreeMap<&str, ResourceBinding>,
    ) -> Result<Boundary, EventError> {
        let resolve = |key: String| {
            let (binding, _) = commands.named_macro(&key).ok_or(EventError::Relationship)?;
            if macros.get(key.as_str()) != Some(&binding) {
                return Err(EventError::Relationship);
            }
            Ok(MacroRef { binding, key })
        };
        Ok(Boundary {
            at: self.at.ok_or_else(|| {
                self.invalid_at
                    .map(EventError::Calendar)
                    .unwrap_or(EventError::Incomplete)
            })?,
            command: resolve(self.command.ok_or(EventError::Incomplete)?)?,
            politeness_minutes: self.minutes,
            warnings: self
                .warnings
                .into_iter()
                .map(|(at, key)| {
                    Ok(Warning {
                        at,
                        command: resolve(key)?,
                    })
                })
                .collect::<Result<_, EventError>>()?,
        })
    }
}
impl ModularCandidate<'_> {
    /// Prepare the exact effective events for one invoking scope with a single captured host zone.
    pub fn event_profiles(
        &self,
        commands: &ScopedCommands,
        zone: Option<&str>,
    ) -> Result<ScopedEvents, EventError> {
        crate::schedule::recurrence::provider_ready().map_err(EventError::Calendar)?;
        let registry = self.registry();
        let site = ResourceScope::Site(registry.site_id());
        let scope = commands.scope;
        if scope != site
            && !matches!(scope,ResourceScope::Port(port) if self.port_metadata(port).is_some())
        {
            return Err(EventError::Relationship);
        }
        let port_key = match scope {
            ResourceScope::Port(id) => Some(
                registry
                    .port_keys()
                    .find(|(_, registered)| *registered == id)
                    .ok_or(EventError::Relationship)?
                    .0
                    .clone(),
            ),
            ResourceScope::Site(_) => None,
        };
        let mut result = ScopedEvents {
            owner: scope,
            port_key,
            events: Vec::new(),
            warnings: Vec::new(),
            local: BTreeMap::new(),
        };
        let mut inherited = BTreeMap::new();
        let mut groups = BTreeMap::new();
        let mut macros = BTreeMap::new();
        for owner in std::iter::once(site).chain((scope != site).then_some(scope)) {
            for &(binding, ref key) in registry.members().iter().filter(|(b, _)| b.scope == owner) {
                let Some(resource) = self.staged_member(binding) else {
                    continue;
                };
                if binding.kind == ResourceKind::Macro {
                    macros.insert(key.as_str(), binding);
                }
                if binding.kind == ResourceKind::PermanentGroup {
                    groups.insert(key.clone(), binding);
                }
                if binding.kind != ResourceKind::Event {
                    continue;
                }
                let (declaration, settings) = inherited
                    .entry(key.clone())
                    .or_insert_with(|| (binding, Inherited::default()));
                *declaration = binding;
                let mut layer = Layer {
                    binding,
                    zone,
                    warnings: &mut result.warnings,
                };
                result
                    .local
                    .insert(binding.id, layer.event(resource.fields(), settings)?);
            }
        }
        for (_, (binding, e)) in inherited {
            let start = e.start.resolve(commands, &macros)?;
            let end = e.end.map(|b| b.resolve(commands, &macros)).transpose()?;
            if matches!(start.at, Trigger::ControllerStart) && end.is_some()
                || end.is_none() && (e.recovery || e.group.is_some())
            {
                return Err(EventError::Relationship);
            }
            let replace_permanent = e
                .group
                .map(|key| {
                    let b = groups.get(&key).copied().ok_or(EventError::Relationship)?;
                    Ok((b, key))
                })
                .transpose()?;
            result.events.push(Event {
                binding,
                owner: scope,
                start,
                end,
                sequence_number: e.sequence,
                restart_recovery: e.recovery,
                replace_permanent,
            });
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resources::candidate::{CapturedResource, ResourceRegistry};
    use serde_json::json;
    fn id(n: u8) -> ResourceId {
        format!("00000000-0000-4000-8000-{n:012}").parse().unwrap()
    }
    fn resolve(site_event: Value, port_event: Value) -> Result<ScopedEvents, EventError> {
        let site = ResourceScope::Site(id(1));
        let port = ResourceScope::Port(id(2));
        let mut docs = vec![];
        let mut members = vec![];
        for (n, scope, kind, key, value) in [
            (
                1,
                site,
                ResourceKind::Metadata,
                None,
                json!({"name":"site","inherit_from":null}),
            ),
            (
                2,
                port,
                ResourceKind::Metadata,
                None,
                json!({"name":"port","inherit_from":"site"}),
            ),
            (
                3,
                site,
                ResourceKind::Macro,
                Some("notice"),
                json!({"actions":[{"action":"announce_time"}]}),
            ),
            (4, site, ResourceKind::Event, Some("a/b"), site_event),
            (5, port, ResourceKind::Event, Some("a/b"), port_event),
        ] {
            let binding = ResourceBinding {
                id: id(n),
                scope,
                kind,
            };
            let mut doc = if kind == ResourceKind::Event {
                value
            } else {
                json!({"configuration":value})
            };
            doc["id"] = json!(id(n).to_string());
            doc["kind"] = json!(kind.as_str());
            if let Some(key) = key {
                members.push((binding, PortKey::new(key)));
                doc["key"] = json!(key);
            }
            docs.push((binding, serde_json::to_vec(&doc).unwrap()));
        }
        let reg =
            ResourceRegistry::with_members(id(1), &[(id(2), PortKey::new("port"))], &[], &members)
                .unwrap();
        let captured = docs
            .iter()
            .map(|(binding, json)| CapturedResource {
                binding: *binding,
                json,
            })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(&reg, &captured).unwrap();
        let commands = candidate.command_language(port).unwrap();
        candidate.event_profiles(&commands, None)
    }
    #[test]
    fn resolves_flat_named_events_with_per_field_inherited_fallback() {
        let port = ResourceScope::Port(id(2));
        let result=resolve(json!({"start":{"at":"every day at 0:00 in UTC","macro":"notice","politeness_minutes":2},"sequence_number":9}),json!({"start":{"at":"invalid calendar","politeness_minutes":1},"sequence_number":false,"extra":9})).unwrap();
        assert_eq!(result.events.len(), 1);
        let event = &result.events[0];
        assert_eq!(event.binding.id, id(5));
        assert_eq!(event.owner, port);
        assert_eq!(event.start.command.binding.id, id(3));
        assert_eq!(event.sequence_number, Some(9));
        assert_eq!(event.start.politeness_minutes, 1);
        assert_eq!(
            result.local[&id(5)],
            json!({"start":{"politeness_minutes":1}})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(result.warnings.len(), 3);
    }

    #[test]
    fn validates_required_relationships_after_site_port_inheritance() {
        let site = json!({"start":{"at":"every day at 00:00 in UTC","macro":"notice"},"end":{"at":"every day at 01:00 in UTC","macro":"notice"}});
        assert!(matches!(
            resolve(site.clone(), json!({"start":{"at":"controller_start"}})),
            Err(EventError::Relationship)
        ));
        assert!(matches!(
            resolve(
                json!({"start":{"at":"controller_start","macro":"notice"}}),
                json!({"restart_recovery":true})
            ),
            Err(EventError::Relationship)
        ));
        assert!(matches!(
            resolve(site.clone(), json!({"end":{"macro":"absent"}})),
            Err(EventError::Relationship)
        ));
        assert!(matches!(
            resolve(site, json!({"replace_permanent":"absent"})),
            Err(EventError::Relationship)
        ));
        assert!(matches!(
            resolve(
                json!({"start":{"at":"every day at 00:00","macro":"notice"}}),
                json!({})
            ),
            Err(EventError::Calendar(CalendarError::Zone))
        ));
        assert!(matches!(
            resolve(json!({"end":{}}), json!({})),
            Err(EventError::Incomplete)
        ));
    }
    #[test]
    fn warnings_replace_as_an_array_without_inheriting_boundary_grace() {
        let site = json!({"start":{"at":"controller_start","macro":"notice","politeness_minutes":2,"warnings":[{"at":"every day at 00:05 in UTC","macro":"notice"}]}});
        let p=resolve(site.clone(),json!({"start":{"warnings":[{"at":"every day at 00:06 in UTC","macro":"notice","unused":true}]}})).unwrap();
        assert_eq!(p.events[0].start.warnings.len(), 1);
        assert_eq!(
            p.events[0].start.warnings[0].at.source(),
            "every day at 00:06 in UTC"
        );
        assert_eq!(p.local[&id(5)]["start"]["warnings"][0].get("unused"), None);
        let empty = resolve(site.clone(), json!({"start":{"warnings":[]}})).unwrap();
        assert!(empty.events[0].start.warnings.is_empty());
        let malformed = resolve(
            site,
            json!({"start":{"warnings":[{"at":"controller_start","macro":"notice"}]}}),
        )
        .unwrap();
        assert_eq!(malformed.events[0].start.warnings.len(), 1);
        assert!(malformed.local[&id(5)]["start"].get("warnings").is_none());
    }
}
