//! Registered station equipment, site-to-port inheritance and typed live interface sources.

use super::{
    CandidateWarning, ModularCandidate, PortKey, ResourceBinding, ResourceId, ResourceKind,
    ResourceScope,
    normalize::{self, Fields, Rule},
};
use crate::config::{
    AlarmCondition, AlarmSettings, Calibration, FanMode, FanSettings, InputSettings,
    OutputSettings, ResolvedSiteSettings, SensorSettings, SiteAction, SiteBackend, SiteEndpoint,
};
use crate::site::{Observation, ReadingQuality};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
type Object = Map<String, Value>;

/// Selected physical radio owner and exact property in its published interface capability set.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BoundInterface {
    /// Registered port ID; keys and file paths cannot substitute for it.
    pub port: ResourceId,
    /// Exact local port key used by the existing radio owner.
    pub key: PortKey,
    /// Checked relative RFC 6901 pointer into observed hardware, never controller configuration.
    pub property: String,
}

/// Interface capabilities offered by the prepared radio owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceCapability {
    /// Cached finite numeric observation, including its quality and acquisition time.
    Number,
    /// Cached electrical level with its acquisition time.
    Boolean,
    /// Owned digital output; complete through the existing site radio worker.
    DigitalOutput,
    /// Owned proportional output for PWM cooling demand.
    DutyOutput,
}

/// One cached hardware measurement delivered by the existing physical interface owner.
#[derive(Clone, Debug, PartialEq)]
pub enum InterfaceObservation {
    /// Analog engineering input before per-sensor calibration.
    Number(Observation<f64>),
    /// Digital physical input; converted to 0/1 for a sensor and inverted only for an active-low input.
    Boolean(Observation<bool>),
}

impl InterfaceObservation {
    /// Preserve original quality, absence and acquisition time through numeric conversion.
    pub fn sensor(&self) -> Observation<f64> {
        match self {
            Self::Number(reading) => reading.clone(),
            Self::Boolean(reading) => Observation {
                value: reading.value.map(f64::from),
                observed_at_ms: reading.observed_at_ms,
                quality: reading.quality,
            },
        }
    }

    /// Apply the configured input polarity exactly once.
    pub fn input(&self, active_high: bool) -> Observation<bool> {
        match self {
            Self::Boolean(reading) => Observation {
                value: reading.value.map(|v| v == active_high),
                observed_at_ms: reading.observed_at_ms,
                quality: reading.quality,
            },
            Self::Number(_) => Observation {
                value: None,
                observed_at_ms: None,
                quality: ReadingQuality::Unavailable,
            },
        }
    }
}

mod fields;
use fields::{ALARM, FAN, INPUT, OUTPUT, SENSOR};

/// A binding or relationship that cannot be adopted safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquipmentError {
    /// Requested site/port is absent.
    Scope,
    /// A required endpoint, reference, threshold pair or exclusive claim is unusable.
    Relationship(ResourceBinding),
}

/// Resolved hardware, site policy and sparse public declarations for an invoking scope.
pub struct ScopedEquipment {
    /// Site or real invoking port whose logical views these are.
    pub scope: ResourceScope,
    command_outputs: BTreeSet<PortKey>,
    acquisition_owners: BTreeMap<(ResourceKind, PortKey), ResourceBinding>,
    /// Effective site policies consumed by the station service.
    pub settings: ResolvedSiteSettings,
    /// Compiled, text-only display labels for sensor observations.
    pub labels: BTreeMap<String, crate::composition::Composition>,
    /// Compiled alarm notifications keyed by the exact alarm name and transition field.
    pub alarm_messages: BTreeMap<(String, &'static str), crate::composition::Composition>,
    /// Sparse canonical fields keyed by their real authoring resource.
    pub local: BTreeMap<ResourceId, Object>,
    /// Public effective bodies with their actual owner in the shared site or local scope.
    pub effective: BTreeMap<(ResourceKind, PortKey), (ResourceBinding, Object)>,
    /// Value-free recoverable fallback events.
    pub warnings: Vec<CandidateWarning>,
    authors: BTreeMap<(ResourceKind, PortKey), Vec<ResourceBinding>>,
}

fn merge(base: &mut Object, overlay: &Object) {
    for (k, v) in overlay {
        if let Some(Value::Object(old)) = base.get_mut(k)
            && let Value::Object(new) = v
        {
            merge(old, new);
        } else {
            base.insert(k.clone(), v.clone());
        }
    }
}

fn source_fields(typ: &str, key: &str) -> bool {
    key == "type"
        || match typ {
            "interface" => matches!(key, "node" | "property"),
            "gpiod" => matches!(key, "device" | "line"),
            "hwmon" | "iio" => matches!(key, "device" | "channel"),
            _ => false,
        }
}

struct Definition<'a> {
    binding: ResourceBinding,
    name: &'a str,
    fields: &'a Object,
}
impl Definition<'_> {
    fn text(&self, k: &str) -> &str {
        self.fields
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    fn flag(&self, k: &str) -> bool {
        self.fields.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
    fn number(&self, k: &str) -> Option<f64> {
        self.fields.get(k).and_then(Value::as_f64)
    }
    fn timing(&self, k: &str, default: u64) -> u64 {
        self.fields
            .get("timing")
            .and_then(|v| v.get(k))
            .and_then(Value::as_u64)
            .unwrap_or(default)
    }
    fn selector(&self) -> Option<String> {
        self.fields
            .get("dtmf_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    fn actions(&self, k: &str) -> Vec<SiteAction> {
        self.fields
            .get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|a| {
                crate::config::site::parse_action(a.as_str().expect("normalized action"))
                    .expect("validated action")
            })
            .collect()
    }
    fn fail(&self) -> EquipmentError {
        EquipmentError::Relationship(self.binding)
    }
    fn calibration(&self) -> Option<Calibration> {
        let c = self.fields.get("calibration");
        if c.and_then(|v| v.get("mode")).and_then(Value::as_str) != Some("two_point") {
            return Some(Calibration::None);
        }
        let point = |side: &str, key: &str| c?.get(side)?.get(key)?.as_f64();
        let (input_low, input_high, value_low, value_high) = (
            point("input", "low")?,
            point("input", "high")?,
            point("value", "low")?,
            point("value", "high")?,
        );
        Calibration::two_point(input_low, input_high, value_low, value_high)
    }

    fn endpoint(
        &self,
        candidate: &ModularCandidate<'_>,
        scope: ResourceScope,
        source_scope: ResourceScope,
        capability: &impl Fn(ResourceKind, &BoundInterface) -> Option<InterfaceCapability>,
    ) -> Result<(Option<SiteEndpoint>, Option<BoundInterface>), EquipmentError> {
        let Some(source) = self.fields.get("source") else {
            return Ok((None, None));
        };
        let leaf = |k| source.get(k).and_then(Value::as_str);
        let Some(typ) = leaf("type") else {
            return Ok((None, None));
        };
        if typ == "interface" {
            let registry = candidate.registry();
            let (key, port) = if let Some(key) = leaf("node") {
                let key = PortKey::new(key);
                let port = registry.port_id(&key).ok_or(self.fail())?;
                (key, port)
            } else if let ResourceScope::Port(port) = source_scope
                && scope == source_scope
            {
                let key = registry
                    .port_keys()
                    .find_map(|(key, id)| (id == port).then_some(key))
                    .ok_or(self.fail())?;
                (key.clone(), port)
            } else {
                return Err(self.fail());
            };
            let Some(pointer) = leaf("property") else {
                return Ok((None, None));
            };
            let interface = BoundInterface {
                port,
                key,
                property: pointer.into(),
            };
            let valid = matches!(
                (self.binding.kind, capability(self.binding.kind, &interface)),
                (
                    ResourceKind::Sensor,
                    Some(InterfaceCapability::Number | InterfaceCapability::Boolean)
                ) | (ResourceKind::Input, Some(InterfaceCapability::Boolean))
                    | (
                        ResourceKind::Output,
                        Some(InterfaceCapability::DigitalOutput | InterfaceCapability::DutyOutput)
                    )
            );
            if !valid && self.flag("enabled") {
                return Err(self.fail());
            }
            return Ok((None, Some(interface)));
        }
        let (Some(device), Some(channel)) = (
            leaf("device"),
            leaf(if typ == "gpiod" { "line" } else { "channel" }),
        ) else {
            return Ok((None, None));
        };
        let backend = match typ {
            "gpiod" => SiteBackend::Gpiod,
            "hwmon" => SiteBackend::Hwmon,
            "iio" => SiteBackend::Iio,
            _ => return Err(self.fail()),
        };
        Ok((
            Some(SiteEndpoint {
                backend,
                device: device.into(),
                channel: channel.into(),
            }),
            None,
        ))
    }
}

/// Normalize source-discriminated local fields before effective relationship checks.
fn add_layer(
    effective: &mut Object,
    sparse: &mut Object,
    binding: ResourceBinding,
    warnings: &mut Vec<CandidateWarning>,
) {
    if let Some(source) = sparse.get_mut("source").and_then(Value::as_object_mut) {
        let inherited = effective
            .get("source")
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str);
        let selected = source
            .get("type")
            .and_then(Value::as_str)
            .or(inherited)
            .map(str::to_owned);
        if let Some(typ) = selected {
            if inherited.is_some_and(|old| old != typ) {
                effective.remove("source");
            }
            if typ == "gpiod"
                && let Some(line) = source.get("line")
            {
                match line.as_str().and_then(|s| s.parse::<u32>().ok()) {
                    Some(line) => {
                        source.insert("line".into(), line.to_string().into());
                    }
                    None => {
                        normalize::warn(warnings, binding, "line");
                        source.remove("line");
                    }
                }
            }
            source.retain(|k, _| {
                let active = source_fields(&typ, k);
                if !active {
                    warnings.push(CandidateWarning {
                        resource: binding,
                        warning: crate::config::resources::ResourceWarning::UnknownField,
                    });
                }
                active
            });
        }
    }
    merge(effective, sparse);
}

impl ModularCandidate<'_> {
    /// Prepare the registered station owners and their references. The capability lookup
    /// borrows the generation's already prepared interface inventory and never opens a device.
    pub fn equipment_profiles(
        &self,
        scope: ResourceScope,
        capability: impl Fn(ResourceKind, &BoundInterface) -> Option<InterfaceCapability>,
        commands: &crate::config::resources::commands::ScopedCommands,
        messages: &super::public_messages::ScopedMessages,
    ) -> Result<ScopedEquipment, EquipmentError> {
        let site = ResourceScope::Site(self.registry().site_id());
        let port = match scope {
            ResourceScope::Site(_) if scope == site => None,
            ResourceScope::Port(id) if self.port_metadata(id).is_some() => Some(scope),
            _ => return Err(EquipmentError::Scope),
        };
        if commands.scope != scope || messages.scope != scope {
            return Err(EquipmentError::Scope);
        }
        let mut result = ScopedEquipment {
            scope,
            acquisition_owners: BTreeMap::new(),
            command_outputs: commands
                .language
                .static_actions()
                .filter_map(|a| match a {
                    crate::command_language::Action::Site(crate::site::SiteCommand::Output {
                        name,
                        ..
                    }) => Some(PortKey::new(name)),
                    _ => None,
                })
                .collect(),
            settings: Default::default(),
            labels: BTreeMap::new(),
            alarm_messages: BTreeMap::new(),
            local: BTreeMap::new(),
            effective: BTreeMap::new(),
            warnings: Vec::new(),
            authors: BTreeMap::new(),
        };
        let mut unsafe_calibration = BTreeSet::new();
        let mut site_acquisitions = BTreeSet::new();
        let mut authors = BTreeMap::<(ResourceKind, PortKey), Vec<ResourceBinding>>::new();
        for owner in [Some(site), port].into_iter().flatten() {
            for (binding, key) in self
                .registry()
                .members()
                .iter()
                .filter(|(b, _)| b.scope == owner)
            {
                let schema = match binding.kind {
                    ResourceKind::Sensor => SENSOR,
                    ResourceKind::Input => INPUT,
                    ResourceKind::Output => OUTPUT,
                    ResourceKind::Alarm => ALARM,
                    ResourceKind::Fan => FAN,
                    _ => continue,
                };
                if key.as_str().is_empty()
                    || key.as_str().chars().count() > 63
                    || key
                        .as_str()
                        .chars()
                        .any(|c| c.is_whitespace() || "[]:".contains(c))
                {
                    return Err(EquipmentError::Relationship(*binding));
                }
                let fields = self
                    .member_fields(binding.id)
                    .ok_or(EquipmentError::Relationship(*binding))?;
                let mut sparse =
                    normalize::normalize(*binding, fields, schema, &mut result.warnings);
                let target = (binding.kind, key.clone());
                let (origin, base) = result
                    .effective
                    .entry(target.clone())
                    .or_insert((*binding, Object::new()));
                let inherited = base.clone();
                add_layer(base, &mut sparse, *binding, &mut result.warnings);
                if binding.kind == ResourceKind::Sensor {
                    let definition = Definition {
                        binding: *binding,
                        name: key.as_str(),
                        fields: base,
                    };
                    if definition.calibration().is_none() && sparse.contains_key("calibration") {
                        normalize::warn(&mut result.warnings, *binding, "calibration");
                        sparse.remove("calibration");
                        match inherited.get("calibration") {
                            Some(c) => {
                                base.insert("calibration".into(), c.clone());
                            }
                            None => {
                                base.remove("calibration");
                                if !authors.contains_key(&target) {
                                    unsafe_calibration.insert(target.clone());
                                }
                            }
                        }
                    }
                    // After an unsafe declaration, only a checked, explicitly selected mode
                    // opts the child back into acquisition (including deliberate mode=none).
                    if sparse
                        .get("calibration")
                        .and_then(|c| c.get("mode"))
                        .is_some()
                    {
                        unsafe_calibration.remove(&target);
                    }
                    let definition = Definition {
                        binding: *binding,
                        name: key.as_str(),
                        fields: base,
                    };
                    if definition.timing("stale_after_ms", 5000)
                        <= definition.timing("poll_interval_ms", 1000)
                        && sparse
                            .get("timing")
                            .is_some_and(|t| t.get("stale_after_ms").is_some())
                    {
                        normalize::warn(&mut result.warnings, *binding, "stale_after_ms");
                        sparse["timing"]
                            .as_object_mut()
                            .expect("normalized timing")
                            .remove("stale_after_ms");
                        let timing = base
                            .get_mut("timing")
                            .expect("merged timing")
                            .as_object_mut()
                            .expect("normalized timing");
                        match inherited
                            .get("timing")
                            .and_then(|t| t.get("stale_after_ms"))
                        {
                            Some(value) => {
                                timing.insert("stale_after_ms".into(), value.clone());
                            }
                            None => {
                                timing.remove("stale_after_ms");
                            }
                        }
                    }
                }
                if matches!(
                    binding.kind,
                    ResourceKind::Sensor | ResourceKind::Input | ResourceKind::Output
                ) && sparse.contains_key("source")
                    && inherited.contains_key("source")
                {
                    let current = Definition {
                        binding: *binding,
                        name: key.as_str(),
                        fields: base,
                    };
                    let previous = Definition {
                        binding: *origin,
                        name: key.as_str(),
                        fields: &inherited,
                    };
                    let owner = if sparse["source"].get("type").is_some() {
                        binding.scope
                    } else {
                        origin.scope
                    };
                    let complete =
                        |endpoint| matches!(endpoint, Ok((Some(_), _)) | Ok((_, Some(_))));
                    if !complete(current.endpoint(self, scope, owner, &capability))
                        && complete(previous.endpoint(self, scope, origin.scope, &capability))
                    {
                        normalize::warn(&mut result.warnings, *binding, "source");
                        sparse.remove("source");
                        base.insert("source".into(), inherited["source"].clone());
                    }
                }
                if owner == site
                    && base.get("enabled").and_then(Value::as_bool) == Some(true)
                    && !unsafe_calibration.contains(&target)
                {
                    site_acquisitions.insert(binding.id);
                }
                // A fan override claims the sink only when it supplies a policy field.
                // Its only object-valued group is timing; an empty group supplies no leaf.
                if binding.kind != ResourceKind::Fan
                    || sparse
                        .values()
                        .any(|v| v.as_object().is_none_or(|group| !group.is_empty()))
                {
                    *origin = *binding;
                }
                result.local.insert(binding.id, sparse);
                authors.entry(target).or_default().push(*binding);
            }
        }
        let mut selectors = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut interfaces = BTreeSet::new();
        let mut external = BTreeSet::new();
        for ((kind, key), (binding, fields)) in &result.effective {
            let def = Definition {
                binding: *binding,
                name: key.as_str(),
                fields,
            };
            let history = &authors[&(*kind, key.clone())];
            let site_field = |field: &str| {
                history
                    .iter()
                    .filter_map(|b| result.local.get(&b.id)?.get(field))
                    .next()
            };
            let mut enabled =
                def.flag("enabled") && !unsafe_calibration.contains(&(*kind, key.clone()));
            let is_io = matches!(
                kind,
                ResourceKind::Sensor | ResourceKind::Input | ResourceKind::Output
            );
            let source_scope = history
                .iter()
                .rev()
                .find(|b| {
                    result.local[&b.id]
                        .get("source")
                        .and_then(Value::as_object)
                        .is_some_and(|s| s.contains_key("type"))
                })
                .map_or(binding.scope, |b| b.scope);
            let (endpoint, interface) = if is_io {
                def.endpoint(self, scope, source_scope, &capability)?
            } else {
                (None, None)
            };
            if is_io && enabled && endpoint.is_none() && interface.is_none() {
                normalize::warn(&mut result.warnings, *binding, "source");
                enabled = false;
            }
            if matches!(kind, ResourceKind::Sensor | ResourceKind::Input) && enabled {
                let mut acquisition = *binding;
                if let [parent, _] = history.as_slice() {
                    let previous = Definition {
                        binding: *parent,
                        name: key.as_str(),
                        fields: &result.local[&parent.id],
                    };
                    if site_acquisitions.contains(&parent.id)
                        && previous
                            .endpoint(self, scope, parent.scope, &capability)
                            .is_ok_and(|(e, i)| e == endpoint && i == interface)
                    {
                        acquisition = *parent;
                    }
                }
                result
                    .acquisition_owners
                    .insert((*kind, key.clone()), acquisition);
            }
            if matches!(kind, ResourceKind::Sensor | ResourceKind::Input)
                && (!names.insert(key.clone())
                    || def.selector().is_some_and(|id| !selectors.insert(id)))
            {
                return Err(def.fail());
            }
            match kind {
                ResourceKind::Sensor => {
                    let poll = def.timing("poll_interval_ms", 1000);
                    let stale = def.timing("stale_after_ms", 5000);
                    let stale_after_ms = if stale > poll {
                        stale
                    } else {
                        normalize::warn(&mut result.warnings, *binding, "stale_after_ms");
                        history
                            .iter()
                            .rev()
                            .filter_map(|b| {
                                result.local[&b.id]
                                    .get("timing")?
                                    .get("stale_after_ms")?
                                    .as_u64()
                            })
                            .find(|v| *v > poll)
                            .unwrap_or_else(|| 5000.max(poll + 1))
                    };
                    let calibration = def
                        .calibration()
                        .or_else(|| {
                            normalize::warn(&mut result.warnings, *binding, "calibration");
                            let parent = site_field("calibration")?;
                            let fallback =
                                Object::from_iter([("calibration".into(), parent.clone())]);
                            Definition {
                                fields: &fallback,
                                ..def
                            }
                            .calibration()
                        })
                        .unwrap_or_else(|| {
                            enabled = false;
                            Calibration::None
                        });
                    let label = def.text("label");
                    if !label.is_empty() {
                        result.labels.insert(
                            def.name.into(),
                            messages.compile_text_only(label).map_err(|_| def.fail())?,
                        );
                    }
                    result.settings.sensors.push(SensorSettings {
                        name: def.name.into(),
                        enabled,
                        endpoint,
                        interface,
                        label_message_id: label.into(),
                        unit: def.text("unit").into(),
                        precision: fields.get("precision").and_then(Value::as_u64).unwrap_or(1)
                            as u8,
                        poll_interval_ms: poll,
                        stale_after_ms,
                        smoothing_ms: def.timing("smoothing_ms", 0),
                        calibration,
                        dtmf_id: def.selector(),
                    });
                }
                ResourceKind::Input => result.settings.inputs.push(InputSettings {
                    name: def.name.into(),
                    enabled,
                    endpoint,
                    interface,
                    active_high: def.text("active_level") != "low",
                    debounce_ms: def.timing("debounce_ms", 50),
                    dtmf_id: def.selector(),
                    on_active: def.actions("on_active"),
                    on_inactive: def.actions("on_inactive"),
                }),
                ResourceKind::Output => {
                    if enabled
                        && (interface
                            .as_ref()
                            .is_some_and(|i| !interfaces.insert(i.clone()))
                            || endpoint
                                .as_ref()
                                .is_some_and(|e| !external.insert(e.clone())))
                    {
                        return Err(def.fail());
                    }
                    result.settings.outputs.push(OutputSettings {
                        name: def.name.into(),
                        enabled,
                        endpoint,
                        interface,
                        active_high: def.text("active_level") != "low",
                        startup_on: def.text("startup_state") == "on",
                        safe_on: def.text("safe_state") == "on",
                        pulse_ms: def.timing("pulse_ms", 500),
                    });
                }
                ResourceKind::Alarm => {
                    let condition = match def.text("condition") {
                        "above" => AlarmCondition::Above,
                        "below" => AlarmCondition::Below,
                        "active" => AlarmCondition::Active,
                        "inactive" => AlarmCondition::Inactive,
                        _ => AlarmCondition::Unavailable,
                    };
                    let (trip_message, clear_message, repeat_message) = (
                        def.text("trip_message"),
                        def.text("clear_message"),
                        def.text("repeat_message"),
                    );
                    for (phase, content) in [
                        ("trip_message", trip_message),
                        ("clear_message", clear_message),
                        ("repeat_message", repeat_message),
                    ]
                    .into_iter()
                    .filter(|(_, s)| !s.is_empty())
                    {
                        let recipe = messages
                            .compile_inline(content, crate::composition::MorseRequirement::Optional)
                            .map_err(|_| def.fail())?;
                        result
                            .alarm_messages
                            .insert((def.name.into(), phase), recipe);
                    }
                    result.settings.alarms.push(AlarmSettings {
                        name: def.name.into(),
                        enabled,
                        source: def.text("source").into(),
                        condition,
                        trip_value: def.number("trip_value"),
                        clear_value: def.number("clear_value"),
                        smoothed: def.text("reading") == "smoothed",
                        trip_delay_ms: def.timing("trip_delay_ms", 0),
                        clear_delay_ms: def.timing("clear_delay_ms", 0),
                        repeat_interval_ms: def.timing("repeat_interval_ms", 0),
                        latched: def.flag("latched"),
                        on_trip: def.actions("on_trip"),
                        on_clear: def.actions("on_clear"),
                        trip_message: trip_message.into(),
                        clear_message: clear_message.into(),
                        repeat_message: repeat_message.into(),
                    });
                }
                ResourceKind::Fan => {
                    let ptt_nodes =
                        if let Some(list) = fields.get("ptt_nodes").and_then(Value::as_array) {
                            list.iter()
                                .map(|v| PortKey::new(v.as_str().expect("normalized port")))
                                .collect()
                        } else if let ResourceScope::Port(port) = binding.scope {
                            self.registry()
                                .port_keys()
                                .filter_map(|(k, id)| (id == port).then_some(k.clone()))
                                .collect()
                        } else {
                            Vec::new()
                        };
                    if ptt_nodes
                        .iter()
                        .any(|key| self.registry().port_id(key).is_none())
                    {
                        return Err(def.fail());
                    }
                    let mode = match def.text("mode") {
                        "combined" => FanMode::Combined,
                        "temperature" => FanMode::Temperature,
                        _ => FanMode::Ptt,
                    };
                    let curve = fields
                        .get("temperature_curve")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|row| {
                            (
                                row["temperature"].as_f64().unwrap(),
                                row["duty"].as_f64().unwrap(),
                            )
                        })
                        .collect();
                    result.settings.fans.push(FanSettings {
                        name: def.name.into(),
                        enabled,
                        output: def.text("output").into(),
                        mode,
                        ptt_nodes,
                        run_on_ms: def.timing("run_on_ms", 120000),
                        spinup_ms: def.timing("spinup_ms", 1000),
                        temperature_sensor: def.text("temperature_sensor").into(),
                        on_temperature: def.number("on_temperature"),
                        off_temperature: def.number("off_temperature"),
                        temperature_curve: curve,
                        rpm_sensor: def.text("rpm_sensor").into(),
                        minimum_rpm: def.number("minimum_rpm"),
                    });
                }
                _ => unreachable!("only equipment present"),
            }
        }
        result.authors = authors;
        result.validate(commands, &capability)?;
        result.materialize();
        Ok(result)
    }
}

mod resolved;

#[cfg(test)]
mod tests;
