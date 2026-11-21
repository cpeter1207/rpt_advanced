//! Public views, cross-resource checks and physical observation projection.
use super::*;

impl ScopedEquipment {
    /// Resolve the declaration supplying a canonical leaf, or None for a built-in.
    pub fn source(
        &self,
        kind: ResourceKind,
        key: &PortKey,
        path: &[&str],
    ) -> Option<ResourceBinding> {
        let slot = (kind, key.clone());
        fn leaf<'a>(fields: &'a Object, path: &[&str]) -> Option<&'a Value> {
            let (first, rest) = path.split_first()?;
            rest.iter().try_fold(fields.get(*first)?, |v, k| v.get(*k))
        }
        let value = leaf(&self.effective.get(&slot)?.1, path)?;
        self.authors
            .get(&slot)?
            .iter()
            .rev()
            .find(|b| {
                let Some(authored) = leaf(&self.local[&b.id], path) else {
                    return false;
                };
                authored == value
                    || ((value.is_f64() || authored.is_f64())
                        && value.as_f64().is_some()
                        && authored.as_f64() == value.as_f64())
            })
            .copied()
    }

    /// Select equipment that actually runs under this declaration owner. Inherited members
    /// stay available for references and views without starting an additional acquisition/policy.
    pub fn owned(&self, scope: ResourceScope) -> ResolvedSiteSettings {
        let own = |kind, name: &str| {
            let key = (kind, PortKey::new(name));
            if matches!(kind, ResourceKind::Sensor | ResourceKind::Input) {
                self.acquisition_owners
                    .get(&key)
                    .is_some_and(|binding| binding.scope == scope)
            } else {
                self.effective[&key].0.scope == scope
            }
        };
        ResolvedSiteSettings {
            sensors: self
                .settings
                .sensors
                .iter()
                .filter(|s| own(ResourceKind::Sensor, &s.name))
                .cloned()
                .collect(),
            inputs: self
                .settings
                .inputs
                .iter()
                .filter(|s| own(ResourceKind::Input, &s.name))
                .cloned()
                .collect(),
            outputs: self
                .settings
                .outputs
                .iter()
                .filter(|s| own(ResourceKind::Output, &s.name))
                .cloned()
                .collect(),
            alarms: self
                .settings
                .alarms
                .iter()
                .filter(|s| own(ResourceKind::Alarm, &s.name))
                .cloned()
                .collect(),
            fans: self
                .settings
                .fans
                .iter()
                .filter(|s| own(ResourceKind::Fan, &s.name))
                .cloned()
                .collect(),
        }
    }

    /// Physical owner of an enabled sensor or input viewed in this scope.
    pub fn acquisition_owner(&self, kind: ResourceKind, key: &PortKey) -> Option<ResourceBinding> {
        self.acquisition_owners.get(&(kind, key.clone())).copied()
    }

    /// Prepare one physical acquisition per owning source across the actual site and ports.
    /// Shared sensors poll fast enough for all enabled views. Raw cache freshness must
    /// accommodate the longest requested window; each logical sensor keeps its own window.
    pub fn acquisition_plan<'a>(
        scopes: impl IntoIterator<Item = &'a Self>,
    ) -> Result<BTreeMap<ResourceScope, ResolvedSiteSettings>, EquipmentError> {
        let views = scopes.into_iter().collect::<Vec<_>>();
        Self::validate_owners(views.iter().copied())?;
        let mut plan = views
            .iter()
            .map(|v| (v.scope, v.owned(v.scope)))
            .collect::<BTreeMap<_, _>>();
        for view in views {
            for sensor in view.settings.sensors.iter().filter(|s| s.enabled) {
                let owner =
                    view.acquisition_owners[&(ResourceKind::Sensor, PortKey::new(&sensor.name))];
                let job = plan
                    .get_mut(&owner.scope)
                    .and_then(|s| {
                        s.sensors
                            .iter_mut()
                            .find(|s| s.name == sensor.name && s.enabled)
                    })
                    .ok_or(EquipmentError::Relationship(owner))?;
                job.poll_interval_ms = job.poll_interval_ms.min(sensor.poll_interval_ms);
                job.stale_after_ms = job.stale_after_ms.max(sensor.stale_after_ms);
            }
        }
        Ok(plan)
    }

    /// Verify exclusive physical output ownership across all affected scopes. Repeated
    /// inherited references to a single declaring resource retain their one physical owner.
    pub fn validate_owners<'a>(
        all: impl IntoIterator<Item = &'a Self>,
    ) -> Result<(), EquipmentError> {
        let mut seen = BTreeMap::new();
        let mut claims = BTreeMap::new();
        let mut direct = BTreeSet::new();
        for scope in all {
            let resource = |kind, name: &str| {
                scope
                    .effective
                    .get(&(kind, PortKey::new(name)))
                    .map(|(binding, _)| *binding)
            };
            let output = |name: &str| resource(ResourceKind::Output, name);
            for sink in scope.settings.outputs.iter().filter(|o| o.enabled) {
                let binding = output(&sink.name).expect("registered output");
                let physical = (sink.endpoint.clone(), sink.interface.clone());
                if seen
                    .insert(physical, binding)
                    .is_some_and(|old| old != binding)
                {
                    return Err(EquipmentError::Relationship(binding));
                }
            }
            for fan in scope.settings.fans.iter().filter(|f| f.enabled) {
                let binding = resource(ResourceKind::Fan, &fan.name).expect("registered fan");
                let sink = output(&fan.output).ok_or(EquipmentError::Relationship(binding))?;
                if claims
                    .insert(sink.id, binding)
                    .is_some_and(|old| old != binding)
                {
                    return Err(EquipmentError::Relationship(binding));
                }
            }
            let actions = scope
                .settings
                .inputs
                .iter()
                .filter(|i| i.enabled)
                .flat_map(|i| i.on_active.iter().chain(&i.on_inactive))
                .chain(
                    scope
                        .settings
                        .alarms
                        .iter()
                        .filter(|a| a.enabled)
                        .flat_map(|a| a.on_trip.iter().chain(&a.on_clear)),
                );
            for name in actions
                .filter_map(|a| match a {
                    SiteAction::Output { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .chain(scope.command_outputs.iter().map(PortKey::as_str))
            {
                if let Some(target) = output(name) {
                    direct.insert(target.id);
                }
            }
        }
        if let Some(fan) = direct.iter().find_map(|id| claims.get(id)) {
            return Err(EquipmentError::Relationship(*fan));
        }
        Ok(())
    }

    pub(super) fn validate(
        &self,
        commands: &crate::config::resources::commands::ScopedCommands,
        capability: &impl Fn(ResourceKind, &BoundInterface) -> Option<InterfaceCapability>,
    ) -> Result<(), EquipmentError> {
        let s = &self.settings;
        let sensor = |name: &str| s.sensors.iter().any(|e| e.name == name);
        let input = |name: &str| s.inputs.iter().any(|e| e.name == name);
        let output = |name: &str| s.outputs.iter().any(|e| e.name == name);
        let error = |kind, name: &str| {
            EquipmentError::Relationship(self.effective[&(kind, PortKey::new(name))].0)
        };
        let mut fan_outputs = BTreeSet::new();
        for f in s.fans.iter().filter(|f| f.enabled) {
            let sink = s
                .outputs
                .iter()
                .find(|o| o.name == f.output)
                .ok_or_else(|| error(ResourceKind::Fan, &f.name))?;
            let can_vary = sink.interface.as_ref().is_some_and(|i| {
                capability(ResourceKind::Output, i) == Some(InterfaceCapability::DutyOutput)
            }) || sink
                .endpoint
                .as_ref()
                .is_some_and(|e| e.backend == SiteBackend::Hwmon);
            if (!f.temperature_curve.is_empty() && !can_vary)
                || !fan_outputs.insert(&f.output)
                || (f.mode != FanMode::Ptt
                    && (!sensor(&f.temperature_sensor)
                        || (f.temperature_curve.is_empty()
                            && f.on_temperature
                                .zip(f.off_temperature)
                                .is_none_or(|(on, off)| on < off))))
                || (!f.rpm_sensor.is_empty() && (!sensor(&f.rpm_sensor) || f.minimum_rpm.is_none()))
            {
                return Err(error(ResourceKind::Fan, &f.name));
            }
        }
        let action = |a: &SiteAction| match a {
            SiteAction::Macro(name) => commands.named_macro(name).is_some(),
            SiteAction::Output { name, .. } => output(name) && !fan_outputs.contains(name),
            SiteAction::Fan { name, .. } => s.fans.iter().any(|f| &f.name == name),
            SiteAction::Inhibit(_) => true,
        };
        for i in s.inputs.iter().filter(|i| i.enabled) {
            if !i.on_active.iter().chain(&i.on_inactive).all(action) {
                return Err(error(ResourceKind::Input, &i.name));
            }
        }
        for a in s.alarms.iter().filter(|a| a.enabled) {
            let valid = match a.condition {
                AlarmCondition::Above | AlarmCondition::Below => {
                    sensor(&a.source)
                        && a.trip_value
                            .zip(a.clear_value)
                            .is_some_and(|(trip, clear)| {
                                if a.condition == AlarmCondition::Above {
                                    trip >= clear
                                } else {
                                    trip <= clear
                                }
                            })
                }
                AlarmCondition::Active | AlarmCondition::Inactive => input(&a.source),
                AlarmCondition::Unavailable => sensor(&a.source) || input(&a.source),
            };
            if !valid || !a.on_trip.iter().chain(&a.on_clear).all(action) {
                return Err(error(ResourceKind::Alarm, &a.name));
            }
        }
        Ok(())
    }
}

impl ScopedEquipment {
    /// Project one immutable owner-published cache into the existing station observation
    /// input before calibration/debounce. Never query hardware during this operation.
    pub fn cached_observations(
        &self,
        cache: &BTreeMap<BoundInterface, InterfaceObservation>,
        observed: &mut crate::site::SiteObservations,
    ) {
        for sensor in self.settings.sensors.iter().filter(|s| s.enabled) {
            if let Some(source) = &sensor.interface {
                let value = cache
                    .get(source)
                    .map(InterfaceObservation::sensor)
                    .unwrap_or(Observation {
                        value: None,
                        observed_at_ms: None,
                        quality: ReadingQuality::Unavailable,
                    });
                observed.sensors.insert(sensor.name.clone(), value);
            }
        }
        for input in self.settings.inputs.iter().filter(|i| i.enabled) {
            if let Some(source) = &input.interface {
                let value = cache
                    .get(source)
                    .map(|reading| reading.input(input.active_high))
                    .unwrap_or(Observation {
                        value: None,
                        observed_at_ms: None,
                        quality: ReadingQuality::Unavailable,
                    });
                observed.inputs.insert(input.name.clone(), value);
            }
        }
    }
}

fn endpoint_fields(
    endpoint: Option<&SiteEndpoint>,
    interface: Option<&BoundInterface>,
) -> Option<Value> {
    if let Some(i) = interface {
        return Some(
            serde_json::json!({"type":"interface","node":i.key.as_str(),"property":i.property}),
        );
    }
    let e = endpoint?;
    let (typ, leaf) = match e.backend {
        SiteBackend::Gpiod => ("gpiod", "line"),
        SiteBackend::Hwmon => ("hwmon", "channel"),
        SiteBackend::Iio => ("iio", "channel"),
        SiteBackend::Cm119 | SiteBackend::Parallel => return None,
    };
    let mut body = serde_json::json!({"type":typ,"device":e.device});
    body[leaf] = Value::String(e.channel.clone());
    Some(body)
}

impl ScopedEquipment {
    pub(super) fn materialize(&mut self) {
        use serde_json::json;
        let view = &mut self.effective;
        let mut put = |kind, name: &str, fields: Value, source: Option<Value>, io: bool| {
            let target = &mut view
                .get_mut(&(kind, PortKey::new(name)))
                .expect("registered equipment")
                .1;
            target.extend(fields.as_object().expect("typed equipment fields").clone());
            if io {
                target.remove("source");
                if let Some(source) = source {
                    target.insert("source".into(), source);
                }
            }
        };
        for s in &self.settings.sensors {
            let c = match s.calibration {
                Calibration::None => json!({"mode":"none"}),
                Calibration::TwoPoint {
                    input_low,
                    input_high,
                    value_low,
                    value_high,
                } => {
                    json!({"mode":"two_point","input":{"low":input_low,"high":input_high},"value":{"low":value_low,"high":value_high}})
                }
            };
            // A disabled or falling-back calibration must not expose unusable candidate points.
            let f = json!({"enabled":s.enabled,"unit":s.unit,"precision":s.precision,"label":s.label_message_id,"timing":{"poll_interval_ms":s.poll_interval_ms,"stale_after_ms":s.stale_after_ms,"smoothing_ms":s.smoothing_ms},"calibration":c});
            put(
                ResourceKind::Sensor,
                &s.name,
                f,
                endpoint_fields(s.endpoint.as_ref(), s.interface.as_ref()),
                true,
            );
        }
        for i in &self.settings.inputs {
            put(
                ResourceKind::Input,
                &i.name,
                json!({"enabled":i.enabled,"active_level":if i.active_high{"high"}else{"low"},"timing":{"debounce_ms":i.debounce_ms}}),
                endpoint_fields(i.endpoint.as_ref(), i.interface.as_ref()),
                true,
            );
        }
        for o in &self.settings.outputs {
            put(
                ResourceKind::Output,
                &o.name,
                json!({"enabled":o.enabled,"active_level":if o.active_high{"high"}else{"low"},"startup_state":if o.startup_on{"on"}else{"off"},"safe_state":if o.safe_on{"on"}else{"off"},"timing":{"pulse_ms":o.pulse_ms}}),
                endpoint_fields(o.endpoint.as_ref(), o.interface.as_ref()),
                true,
            );
        }
        for a in &self.settings.alarms {
            put(
                ResourceKind::Alarm,
                &a.name,
                json!({"enabled":a.enabled,"source":a.source,"condition":match a.condition {AlarmCondition::Above=>"above",AlarmCondition::Below=>"below",AlarmCondition::Active=>"active",AlarmCondition::Inactive=>"inactive",AlarmCondition::Unavailable=>"unavailable"},"reading":if a.smoothed{"smoothed"}else{"current"},"latched":a.latched,"timing":{"trip_delay_ms":a.trip_delay_ms,"clear_delay_ms":a.clear_delay_ms,"repeat_interval_ms":a.repeat_interval_ms},"trip_message":a.trip_message,"clear_message":a.clear_message,"repeat_message":a.repeat_message}),
                None,
                false,
            );
        }
        for f in &self.settings.fans {
            put(
                ResourceKind::Fan,
                &f.name,
                json!({"enabled":f.enabled,"output":f.output,"mode":match f.mode {FanMode::Ptt=>"ptt",FanMode::Combined=>"combined",FanMode::Temperature=>"temperature"},"ptt_nodes":f.ptt_nodes.iter().map(PortKey::as_str).collect::<Vec<_>>(),"temperature_sensor":f.temperature_sensor,"rpm_sensor":f.rpm_sensor,"timing":{"run_on_ms":f.run_on_ms,"spinup_ms":f.spinup_ms}}),
                None,
                false,
            );
        }
    }
}
