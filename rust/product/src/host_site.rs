//! Site acquisition borrows physical owners only on serialized station control.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use rpt_advanced_core::{
    config::resources::{
        PortKey,
        candidate::{ResourceKind, ResourceScope},
        generation::RuntimeConfiguration,
    },
    runtime::dtmf::ActionPermit,
    site::{AlarmEvent, SiteCommand, SiteEffect, SiteError, SiteObservations},
};
use std::collections::BTreeMap;

/// Control-owned delivery, bounded by configured message/action identities.
#[derive(Default)]
pub(super) struct Delivery {
    revision: u64,
    pending: Vec<(String, String, AlarmEvent, Option<ActionPermit>)>,
    faults: BTreeMap<(String, String, Option<ResourceKind>), SiteError>,
    links: Vec<(String, LinkEffect)>,
}

pub(super) struct Transition {
    shutdown: bool,
    staged: bool,
    complete: bool,
}

/// Ownership metadata copied before allocating candidate workers off control.
pub(crate) struct SiteBasis {
    pub(crate) revision: u64,
    pub(crate) known: std::collections::BTreeSet<(rpt_advanced_core::config::SiteBackend, String)>,
    pub(crate) replaced: std::collections::BTreeSet<String>,
}

impl Host {
    /// Apply an authorized site request on the current control owner.
    pub(crate) fn site_command(
        &mut self,
        local: &str,
        command: rpt_advanced_core::site::SiteCommand,
        clock: RuntimeClock,
    ) -> Result<(), SiteError> {
        self.authorized_site_command(local, command, clock, None)
    }
    pub(super) fn authorized_site_command(
        &mut self,
        local: &str,
        command: SiteCommand,
        clock: RuntimeClock,
        permit: Option<&ActionPermit>,
    ) -> Result<(), SiteError> {
        if self.site_transition.is_some() || permit.is_some_and(|p| !p.current(Some(clock.now_ms)))
        {
            return Err(SiteError::Unavailable);
        }
        self.apply_equipment_command(local, command, clock, Some((local, permit)), None)
    }

    fn declaration(&self, local: &str, kind: ResourceKind, name: &str) -> String {
        let Some(domain) = self.runtime.modular_configuration() else {
            return local.to_owned();
        };
        let Ok(id) = local.parse() else {
            return local.to_owned();
        };
        let scope = if domain.registry.site_id() == id {
            ResourceScope::Site(id)
        } else {
            ResourceScope::Port(id)
        };
        domain
            .scopes
            .get(&scope)
            .and_then(|view| view.equipment.effective.get(&(kind, PortKey::new(name))))
            .map_or_else(
                || local.to_owned(),
                |(binding, _)| scope_name(binding.scope),
            )
    }

    fn apply_equipment_command(
        &mut self,
        local: &str,
        command: SiteCommand,
        clock: RuntimeClock,
        caller: Option<(&str, Option<&ActionPermit>)>,
        action_source: Option<&str>,
    ) -> Result<(), SiteError> {
        let target = match &command {
            SiteCommand::Output { name, .. } => self.declaration(local, ResourceKind::Output, name),
            SiteCommand::Fan { name, .. } => self.declaration(local, ResourceKind::Fan, name),
            SiteCommand::AlarmAcknowledge(name) | SiteCommand::AlarmReset(name) => {
                self.declaration(local, ResourceKind::Alarm, name)
            }
            // Extrema and per-port protection are logical view state.
            _ => local.to_owned(),
        };
        let actuator = match &command {
            SiteCommand::Fan { name, .. } => Some((name.clone(), ResourceKind::Fan)),
            SiteCommand::Output { name, .. } => Some((name.clone(), ResourceKind::Output)),
            _ => None,
        };
        let effects = self
            .runtime
            .equipment_command(&target, command, clock.now_ms)
            .inspect_err(|error| {
                if let Some(source) = action_source {
                    self.site_delivery
                        .faults
                        .insert((local.to_owned(), source.to_owned(), None), *error);
                }
            })?;
        let delivery = self.site_effects(&target, effects, clock, caller);
        if self.runtime.modular_configuration().is_none() {
            // Legacy station callers inspect the separate delivery result in status.
            return Ok(());
        }
        delivery?;
        if let Some((name, kind)) = actuator
            && let Some(error) = self.site_delivery.faults.get(&(target, name, Some(kind)))
        {
            return Err(*error);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(super) fn live_site_status(
        &mut self,
        result: &mut crate::management::live::Domain,
        clock: RuntimeClock,
    ) {
        use rpt_advanced_core::site::ReadingQuality;
        use serde_json::json;
        let label = |quality| match quality {
            ReadingQuality::Valid => "valid",
            ReadingQuality::Stale => "stale",
            ReadingQuality::Fault => "fault",
            ReadingQuality::Unavailable => "unavailable",
        };
        let Some(domain) = self.runtime.modular_configuration().cloned() else {
            return;
        };
        let snapshots: BTreeMap<_, _> = domain
            .scopes
            .keys()
            .filter_map(|scope| {
                let site = self.runtime.site_controller(&scope_name(*scope))?;
                Some((*scope, site.snapshot(clock.now_ms)))
            })
            .collect();
        for (scope, view) in &domain.scopes {
            if !snapshots.contains_key(scope) {
                continue;
            }
            let node = self.runtime.node(&scope_name(*scope));
            for ((kind, key), (binding, _)) in &view.equipment.effective {
                let owner = match kind {
                    ResourceKind::Alarm | ResourceKind::Output | ResourceKind::Fan => binding.scope,
                    _ => *scope,
                };
                let Some(s) = snapshots.get(&owner) else {
                    continue;
                };
                let name = key.as_str();
                let observation = match kind {
                    ResourceKind::Sensor => s.sensors.get(name).map(|r| json!({
                        "value":r.current.value,"quality":label(r.current.quality),"observed_at_ms":r.current.observed_at_ms,
                        "smoothed":r.smoothed,"minimum":r.minimum.map(|v|v.value),"maximum":r.maximum.map(|v|v.value),
                        "minimum_at_ms":r.minimum.map(|v|v.observed_at_ms),"maximum_at_ms":r.maximum.map(|v|v.observed_at_ms)})),
                    ResourceKind::Input => s.inputs.get(name).map(|r| json!({
                        "value":r.value,"quality":label(r.quality),"observed_at_ms":r.observed_at_ms})),
                    ResourceKind::Alarm => s.alarms.get(name).map(|r| json!({
                        "active":r.active,"acknowledged":r.acknowledged,"unsafe":r.unsafe_condition,"reset_ready":r.reset_ready})),
                    ResourceKind::Fan => s.fans.get(name).map(|r| json!({
                        "requested_duty":r.duty,"fault":r.fault,"run_on_until_ms":r.run_on_until_ms})),
                    ResourceKind::Output => s.outputs.get(name).map(|r| {
                        let local=scope_name(owner);
                        let submitted=self.site_radio.output_status(&local,name)
                            .or_else(|| self.site_acquisition.output_status(&local,name));
                        json!({"requested_on":r.requested_on,"fault":r.fault,"pulse_deadline_ms":r.pulse_deadline_ms,
                            "submitted":submitted.as_ref().and_then(|s|s.submitted).map(|(level,at)|json!({"level":level,"observed_at_ms":at})),
                            "submission_quality":label(submitted.map_or(ReadingQuality::Unavailable,|s|s.quality)),
                            "readback":null})
                    }),
                    _ => None,
                };
                if let Some(mut fields) = observation {
                    fields["key"] = json!(name);
                    if *kind == ResourceKind::Sensor
                        && let Some(label) =
                            view.sensor_display_name(name, s, clock, node.as_deref())
                    {
                        fields["name"] = json!(label);
                    }
                    result.resources.insert((*scope, binding.id), fields);
                }
            }
        }
    }

    /// Append control snapshots only; requested/submitted values never imply electrical readback.
    pub(super) fn write_site_status(
        &mut self,
        local: &str,
        text: &mut String,
    ) -> Result<(), RuntimeError> {
        use std::fmt::Write;
        let now_ms = self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let mut snapshot = self
            .runtime
            .site_controller(local)
            .ok_or(RuntimeError::MissingNode)?
            .snapshot(now_ms);
        if let Some(domain) = self.runtime.modular_configuration().cloned()
            && let Ok(id) = local.parse()
            && let Some(view) = domain.scopes.get(&ResourceScope::Port(id))
        {
            for ((kind, key), (binding, _)) in &view.equipment.effective {
                if binding.scope == ResourceScope::Port(id)
                    || !matches!(
                        kind,
                        ResourceKind::Alarm | ResourceKind::Output | ResourceKind::Fan
                    )
                {
                    continue;
                }
                let Some(owner) = self.runtime.site_controller(&scope_name(binding.scope)) else {
                    continue;
                };
                let state = owner.snapshot(now_ms);
                let key = key.as_str();
                match kind {
                    ResourceKind::Alarm => {
                        if let Some(value) = state.alarms.get(key) {
                            snapshot.alarms.insert(key.to_owned(), *value);
                        }
                    }
                    ResourceKind::Output => {
                        if let Some(value) = state.outputs.get(key) {
                            snapshot.outputs.insert(key.to_owned(), *value);
                        }
                    }
                    ResourceKind::Fan => {
                        if let Some(value) = state.fans.get(key) {
                            snapshot.fans.insert(key.to_owned(), *value);
                        }
                    }
                    _ => {}
                }
            }
        }
        let age = |observed: Option<u64>| observed.map(|time| now_ms.saturating_sub(time));
        let _ = writeln!(
            text,
            "  site transmit-inhibited={}",
            snapshot.transmit_inhibited
        );
        for (name, sensor) in snapshot.sensors {
            let _ = writeln!(
                text,
                "    sensor {name} value={:?} quality={:?} age-ms={:?} smoothed={:?} minimum={:?} maximum={:?}",
                sensor.current.value,
                sensor.current.quality,
                age(sensor.current.observed_at_ms),
                sensor.smoothed,
                sensor.minimum,
                sensor.maximum
            );
        }
        for (name, input) in snapshot.inputs {
            let _ = writeln!(
                text,
                "    input {name} value={:?} quality={:?} age-ms={:?}",
                input.value,
                input.quality,
                age(input.observed_at_ms)
            );
        }
        for (name, alarm) in snapshot.alarms {
            let _ = writeln!(
                text,
                "    alarm {name} active={} acknowledged={} unsafe={:?} reset-ready={}",
                alarm.active, alarm.acknowledged, alarm.unsafe_condition, alarm.reset_ready
            );
        }
        for (name, output) in snapshot.outputs {
            let owner = self.declaration(local, ResourceKind::Output, &name);
            let submitted = self
                .site_radio
                .output_status(&owner, &name)
                .or_else(|| self.site_acquisition.output_status(&owner, &name));
            let _ = writeln!(
                text,
                "    output {name} requested-on={} fault={} pulse-deadline-ms={:?} submitted={:?} submission-quality={:?} readback=unavailable requested-duty={:?}",
                output.requested_on,
                output.fault,
                output.pulse_deadline_ms,
                submitted.as_ref().and_then(|state| state.submitted),
                submitted.as_ref().map_or(
                    rpt_advanced_core::site::ReadingQuality::Unavailable,
                    |state| state.quality
                ),
                submitted.as_ref().map(|state| state.requested)
            );
        }
        for (name, fan) in snapshot.fans {
            let _ = writeln!(
                text,
                "    fan {name} requested-duty={} fault={} run-on-until-ms={:?}",
                fan.duty, fan.fault, fan.run_on_until_ms
            );
        }
        for ((node, target, _), fault) in &self.site_delivery.faults {
            if node == local {
                let _ = writeln!(text, "    site-action {target} fault={fault:?}");
            }
        }
        Ok(())
    }

    /// Drive station policy on the existing control owner, without waiting for I/O or media.
    pub(super) fn advance_sites(&mut self, clock: RuntimeClock) {
        if self.site_delivery.revision != self.runtime.revision() {
            if self.runtime.modular_configuration().is_some() {
                for (scope, name, kind) in self.site_delivery.faults.keys() {
                    if let Some(kind) = kind
                        && let Some(site) = self.runtime.site_controller(scope)
                    {
                        site.retry_actuator_delivery(*kind, name);
                    }
                }
            }
            self.site_delivery = Delivery {
                revision: self.runtime.revision(),
                ..Default::default()
            };
        }
        // Reload fences physical owners individually; unrelated protection and
        // elapsed-time policy must continue while a changed owner acknowledges.
        if self
            .site_transition
            .as_ref()
            .is_some_and(|change| change.shutdown)
        {
            return;
        }
        let observations = self.poll_sites(clock.now_ms);
        let domain = self.runtime.modular_configuration().cloned();
        let mut ptt = BTreeMap::new();
        let names: Vec<_> = self
            .runtime
            .node_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        if let Some(domain) = &domain {
            for (key, port) in domain.registry.port_keys() {
                if let Some(keyed) = self.live_ptt(&port.to_string(), clock.now_ms) {
                    ptt.insert(key.clone(), keyed);
                }
            }
            let scope = ResourceScope::Site(domain.registry.site_id());
            let site = scope_name(scope);
            let values = scope_observations(domain, scope, &observations);
            let effects = self
                .runtime
                .advance_equipment(&site, clock.now_ms, &values, &ptt);
            let _ = self.site_effects(&site, effects, clock, None);
        } else {
            for name in &names {
                if let Some(keyed) = self.live_ptt(name, clock.now_ms) {
                    ptt.insert(PortKey::new(name), keyed);
                }
            }
        }
        for local in names {
            let values = if let Some(domain) = &domain {
                let port = local.parse().expect("registered station UUID");
                scope_observations(domain, ResourceScope::Port(port), &observations)
            } else {
                observations.get(&local).cloned().unwrap_or_default()
            };
            let effects = self
                .runtime
                .advance_equipment(&local, clock.now_ms, &values, &ptt);
            let _ = self.site_effects(&local, effects, clock, None);
        }
        let mut pending = std::mem::take(&mut self.site_delivery.pending);
        pending.retain(|(local, name, event, permit)| {
            match if permit
                .as_ref()
                .is_some_and(|p| !p.current(Some(clock.now_ms)))
            {
                Err(RuntimeError::Rejected)
            } else {
                self.runtime.queue_alarm_message(local, name, *event)
            } {
                Ok(accepted) => !accepted,
                Err(_) => {
                    self.site_delivery
                        .faults
                        .insert((local.clone(), name.clone(), None), SiteError::Unavailable);
                    false
                }
            }
        });
        self.site_delivery.pending = pending;
    }
    fn live_ptt(&self, name: &str, now_ms: u64) -> Option<bool> {
        let lease = live_station(
            &self.leases,
            name,
            self.runtime.modular_configuration().is_some(),
        )?;
        lease.status.transmit_observation(now_ms)
    }

    fn site_output(&mut self, local: &str, name: &str, demand: f64) -> Result<(), SiteError> {
        let result = if self.site_radio.output_status(local, name).is_some() {
            self.site_radio.output(local, name, demand)
        } else {
            self.site_acquisition.output(local, name, demand)
        };
        result.map_err(|_| SiteError::Unavailable)
    }
    /// Apply equipment effects once; only coalesced notifications wait for queue space.
    fn site_effects(
        &mut self,
        local: &str,
        effects: Vec<SiteEffect>,
        clock: RuntimeClock,
        caller: Option<(&str, Option<&ActionPermit>)>,
    ) -> Result<(), SiteError> {
        let (radio, permit) = caller.unwrap_or((local, None));
        let mut outcome = Ok(());
        for effect in effects {
            let actuator = match &effect {
                SiteEffect::Fan { .. } => Some(ResourceKind::Fan),
                SiteEffect::Output { .. } => Some(ResourceKind::Output),
                _ => None,
            };
            let (target, result) = match effect {
                SiteEffect::Equipment { source, command } => {
                    if let Err(error) =
                        self.apply_equipment_command(local, command, clock, caller, Some(&source))
                    {
                        outcome = Err(error);
                    }
                    continue;
                }
                SiteEffect::Output { name, on } => {
                    let result = self.site_output(local, &name, f64::from(on));
                    (name, result)
                }
                SiteEffect::Fan { name, duty } => {
                    let output = self
                        .runtime
                        .site_controller(local)
                        .and_then(|site| site.fan_output(&name))
                        .map(str::to_owned);
                    let result = output.ok_or(SiteError::UnknownName).and_then(|output| {
                        let owner = self.declaration(local, ResourceKind::Output, &output);
                        self.site_output(&owner, &output, duty)
                    });
                    (name, result)
                }
                // RuntimeNode publishes the aggregate gate before handing effects to this host.
                SiteEffect::TransmitInhibit(_) => continue,
                SiteEffect::Macro(name) => {
                    let result = self
                        .runtime
                        .equipment_macro(radio, &name, permit, clock)
                        .and_then(|effect| {
                            if matches!(effect, LinkEffect::Connect(_)) {
                                self.site_delivery.links.push((radio.to_owned(), effect));
                                Ok(())
                            } else {
                                self.immediate(radio, effect, clock)
                            }
                        })
                        .map_err(|_| SiteError::Unavailable);
                    (name, result)
                }
                SiteEffect::Message { alarm, event } if self.runtime.node(radio).is_some() => {
                    self.defer_alarm(radio, &alarm, event, permit);
                    continue;
                }
                SiteEffect::Message { alarm, .. } => (alarm, Err(SiteError::Unavailable)),
                SiteEffect::Fault { target, error } => (target, Err(error)),
            };
            let key = (local.to_owned(), target, actuator);
            if let Err(error) = result {
                self.site_delivery.faults.insert(key, error);
                outcome = Err(error);
            } else {
                self.site_delivery.faults.remove(&key);
            }
        }
        outcome
    }
    fn defer_alarm(
        &mut self,
        radio: &str,
        alarm: &str,
        event: AlarmEvent,
        permit: Option<&ActionPermit>,
    ) {
        let pending = &mut self.site_delivery.pending;
        if !pending
            .iter()
            .any(|(station, name, phase, _)| station == radio && name == alarm && *phase == event)
        {
            pending.push((radio.to_owned(), alarm.to_owned(), event, permit.cloned()));
        }
    }

    pub(super) fn defer_link(&mut self, local: &str, effect: LinkEffect) {
        self.site_delivery.links.push((local.to_owned(), effect));
    }
    /// Transfer generation-protected dials to the existing off-control dispatcher.
    pub(crate) fn take_site_links(&mut self) -> Vec<(String, LinkEffect)> {
        std::mem::take(&mut self.site_delivery.links)
    }
    /// Fence prepared site bindings before replacing controller or radio ownership.
    pub(crate) fn begin_site_reload(
        &mut self,
        revision: u64,
        acquisition: &mut Option<crate::site::acquisition::PreparedAcquisition>,
        radio: &mut Option<RadioSite>,
        replaced: &std::collections::BTreeSet<String>,
    ) -> Result<(), RuntimeError> {
        if self.runtime.revision() != revision
            || self.site_transition.is_some()
            || acquisition.is_none()
            || radio.is_none()
        {
            return Err(RuntimeError::Rejected);
        }
        if let Err(candidate) = self
            .site_acquisition
            .begin(acquisition.take().unwrap(), replaced)
        {
            *acquisition = Some(candidate);
            return Err(RuntimeError::Busy);
        }
        assert!(self.site_radio.begin(radio.take().unwrap(), replaced));
        self.site_transition = Some(Transition {
            shutdown: false,
            staged: false,
            complete: false,
        });
        Ok(())
    }
    /// Advance safe/release acknowledgments, never wait on control.
    pub(crate) fn site_barrier(&mut self, now_ms: u64) -> Result<bool, RuntimeError> {
        self.poll_sites(now_ms);
        if !self.site_ready()? {
            return Ok(false);
        }
        let transition = self
            .site_transition
            .as_mut()
            .ok_or(RuntimeError::Rejected)?;
        if !transition.staged {
            if !transition.shutdown {
                assert!(self.site_acquisition.stage());
            }
            assert!(self.site_radio.stage());
            transition.staged = true;
            self.poll_sites(now_ms);
        }
        self.site_ready()
    }
    fn site_ready(&mut self) -> Result<bool, RuntimeError> {
        let mut ready = true;
        for state in [self.site_acquisition.barrier(), self.site_radio.barrier()] {
            match state {
                None => ready = false,
                Some(Ok(())) => {}
                Some(Err(_)) => return Err(RuntimeError::Device),
            }
        }
        Ok(ready)
    }
    /// Restore live demand if controller publication did not happen.
    pub(crate) fn abort_sites(&mut self) -> Vec<crate::site::worker::DeviceWorker> {
        if self.site_transition.take().is_some() {
            self.site_radio.abort();
            self.site_retired.extend(self.site_acquisition.abort());
        }
        std::mem::take(&mut self.site_retired)
    }
    /// Transfer prepared ownership at runtime publication, before any new-generation polling.
    pub(super) fn commit_sites(&mut self) -> Result<(), RuntimeError> {
        if self
            .site_transition
            .as_ref()
            .is_some_and(|change| !change.shutdown)
        {
            self.site_retired.extend(
                self.site_acquisition
                    .commit()
                    .map_err(|_| RuntimeError::Device)?,
            );
            assert!(self.site_radio.commit());
            self.site_transition = None;
        }
        Ok(())
    }
    /// Resume only after the caller has retired removed device workers off control.
    pub(crate) fn resume_sites(&mut self) -> bool {
        self.site_acquisition.resume()
    }
    /// Confirm safe levels and release site pins before any radio reservation is destroyed.
    pub(crate) fn site_shutdown(&mut self, now_ms: u64) -> Result<bool, RuntimeError> {
        if self.site_transition.is_none() {
            self.site_acquisition.pause(true);
            assert!(
                self.site_radio
                    .begin(RadioSite::default(), &Default::default())
            );
            self.site_transition = Some(Transition {
                shutdown: true,
                staged: false,
                complete: false,
            });
        }
        if !self
            .site_transition
            .as_ref()
            .is_some_and(|change| change.shutdown)
        {
            return Err(RuntimeError::Busy);
        }
        if self
            .site_transition
            .as_ref()
            .is_some_and(|change| change.complete)
        {
            return Ok(true);
        }
        if !self.site_barrier(now_ms)? {
            return Ok(false);
        }
        assert!(self.site_radio.commit());
        self.site_transition.as_mut().unwrap().complete = true;
        Ok(true)
    }
    /// Move quiesced device owners out of control before their threads are joined.
    pub(crate) fn take_sites(&mut self) -> (Acquisition, Vec<crate::site::worker::DeviceWorker>) {
        (
            std::mem::take(&mut self.site_acquisition),
            std::mem::take(&mut self.site_retired),
        )
    }
    /// Collect bounded site results without waiting for physical I/O.
    pub(crate) fn poll_sites(&mut self, now_ms: u64) -> BTreeMap<String, SiteObservations> {
        let mut readings = self
            .site_acquisition
            .poll(now_ms, |node| self.runtime.equipment_work(node));
        let modular = self.runtime.modular_configuration().is_some();
        let radio = self.site_radio.poll(now_ms, |owner, bank, operation| {
            live_station(&self.leases, owner, modular)
                .ok_or(-7)?
                .worker
                .as_mut()
                .ok_or(-7)?
                .site_operation(bank, operation)
        });
        for (node, values) in radio {
            let entry = readings.entry(node).or_default();
            entry.sensors.extend(values.sensors);
            entry.inputs.extend(values.inputs);
            entry.outputs.extend(values.outputs);
        }
        readings
    }
}

/// Use the committed live reservation; a stopped or merely staged same-name lease has no sample.
pub(super) fn live_station<'a>(
    leases: &'a [(String, Arc<Mutex<Lease>>)],
    name: &str,
    modular: bool,
) -> Option<std::sync::MutexGuard<'a, Lease>> {
    leases
        .iter()
        .rev()
        .filter(|(key, _)| key == name)
        .find_map(|(_, lease)| {
            let lease = lease.lock().unwrap_or_else(|error| error.into_inner());
            #[cfg(target_os = "linux")]
            if modular && lease.binding.active.is_none() {
                return None;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = modular;
            (lease.worker.is_some() && !lease.quiescing && !lease.quiesced).then_some(lease)
        })
}

fn scope_name(scope: ResourceScope) -> String {
    let (ResourceScope::Site(id) | ResourceScope::Port(id)) = scope;
    id.to_string()
}

/// Borrow each declaring cache without consuming it. Calibration/debounce remain local policy.
fn scope_observations(
    domain: &RuntimeConfiguration,
    scope: ResourceScope,
    physical: &BTreeMap<String, SiteObservations>,
) -> SiteObservations {
    let view = &domain.scopes[&scope].equipment;
    let mut result = SiteObservations::default();
    for sensor in view.settings.sensors.iter().filter(|s| s.enabled) {
        if let Some(binding) =
            view.acquisition_owner(ResourceKind::Sensor, &PortKey::new(&sensor.name))
            && let Some(value) = physical
                .get(&scope_name(binding.scope))
                .and_then(|p| p.sensors.get(&sensor.name))
        {
            result.sensors.insert(sensor.name.clone(), value.clone());
        }
    }
    for input in view.settings.inputs.iter().filter(|i| i.enabled) {
        if let Some(binding) =
            view.acquisition_owner(ResourceKind::Input, &PortKey::new(&input.name))
            && let Some(value) = physical
                .get(&scope_name(binding.scope))
                .and_then(|p| p.inputs.get(&input.name))
            && let Some(owner) = domain.acquisition[&binding.scope]
                .inputs
                .iter()
                .find(|i| i.name == input.name)
        {
            let mut logical = value.clone();
            if owner.active_high != input.active_high {
                logical.value = logical.value.map(|v| !v);
            }
            result.inputs.insert(input.name.clone(), logical);
        }
    }
    for output in &view.settings.outputs {
        let binding = view.effective[&(ResourceKind::Output, PortKey::new(&output.name))].0;
        if let Some(value) = physical
            .get(&scope_name(binding.scope))
            .and_then(|p| p.outputs.get(&output.name))
        {
            result.outputs.insert(output.name.clone(), *value);
        }
    }
    result
}
