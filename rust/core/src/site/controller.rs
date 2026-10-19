//! Shared site policy on station control; acquisition and effect delivery stay external.

use super::{
    AlarmEvent, AlarmSnapshot, AlarmState, AlarmValue, FanOutput, FanSnapshot, FanState,
    Observation, OutputRequest, OutputSnapshot, OutputState, ReadingQuality, SensorSnapshot,
    SensorState, SiteError,
};
use crate::config::{InputSettings, NodeId, ResolvedSiteSettings, SiteAction};
use std::collections::{BTreeMap, BTreeSet};

/// Acquired observations only; policy owns calibration, debounce and timers.
#[derive(Clone, Debug, Default)]
pub struct SiteObservations {
    /// Analog sources in adapter-normalized units.
    pub sensors: BTreeMap<String, Observation<f64>>,
    /// Polarity-normalized digital snapshots; a missing entry is unavailable.
    pub inputs: BTreeMap<String, Observation<bool>>,
    /// Real output availability/capabilities from each owner. Omit a not-yet-acquired
    /// output; include unavailable once acquisition fails or a known owner is lost.
    pub outputs: BTreeMap<String, FanOutput>,
}

/// Already-authorized operation on station policy, never a hardware call.
#[derive(Clone, Debug, PartialEq)]
pub enum SiteCommand {
    /// Logical output on/off/prior-state pulse.
    Output {
        /// Configured output name.
        name: String,
        /// Logical operation, not an electrical level.
        request: OutputRequest,
    },
    /// Silence repeat notifications, not the condition or equipment actions.
    AlarmAcknowledge(String),
    /// Reset a recovered alarm; unsafe/unknown conditions reject it.
    AlarmReset(String),
    /// Clear retained analog extrema without changing the current reading.
    ResetExtrema(String),
    /// Additional manual fan demand; None restores automatic demand.
    Fan {
        /// Configured fan name.
        name: String,
        /// Additional normalized demand, or None for automatic operation.
        duty: Option<f64>,
    },
    /// Assert/release the manual protective inhibit only.
    Inhibit(bool),
}

/// Effects delivered independently of message queue admission, in policy order.
#[derive(Clone, Debug, PartialEq)]
pub enum SiteEffect {
    /// Requested logical output; submission/readback remain the owner's responsibility.
    Output {
        /// Configured output name.
        name: String,
        /// Logical requested state before hardware polarity mapping.
        on: bool,
    },
    /// Requested normalized fan duty; no assertion of physical cooling.
    Fan {
        /// Configured fan name, resolving to its exclusively claimed output.
        name: String,
        /// Requested normalized duty, 0..1.
        duty: f64,
    },
    /// Aggregate site protective inhibit, never an interlock reset.
    TransmitInhibit(bool),
    /// Existing validated controller action macro.
    Macro(String),
    /// Named local-only message queued through ordinary telemetry admission.
    Message(String),
    /// A requested policy action could not be applied; later actions still run.
    Fault {
        /// Action source whose request failed.
        target: String,
        /// Reason without an implied hardware recovery.
        error: SiteError,
    },
}

/// Current read-only control state; physical submission/readback are separate backend observations.
#[derive(Clone, Debug, Default)]
pub struct SiteSnapshot {
    /// Calibrated readings with source health, age and extrema.
    pub sensors: BTreeMap<String, SensorSnapshot>,
    /// Debounced logical inputs with their real observation time.
    pub inputs: BTreeMap<String, Observation<bool>>,
    /// Alarm state and reset eligibility.
    pub alarms: BTreeMap<String, AlarmSnapshot>,
    /// Requested logical output state and pulse deadline.
    pub outputs: BTreeMap<String, OutputSnapshot>,
    /// Requested cooling and fault state.
    pub fans: BTreeMap<String, FanSnapshot>,
    /// At least one site protection source is active.
    pub transmit_inhibited: bool,
}

#[derive(Clone)]
struct InputState {
    value: Observation<bool>,
    last_sample: Option<u64>,
    pending: Option<(bool, u64)>,
}
impl InputState {
    fn new() -> Self {
        Self {
            value: Observation {
                value: None,
                observed_at_ms: None,
                quality: ReadingQuality::Unavailable,
            },
            last_sample: None,
            pending: None,
        }
    }
    fn observe(
        &mut self,
        reading: &Observation<bool>,
        settings: &InputSettings,
        now_ms: u64,
    ) -> Option<bool> {
        if reading.quality != ReadingQuality::Valid {
            self.value.quality = reading.quality;
            self.pending = None;
            return None;
        }
        let valid = reading
            .value
            .zip(reading.observed_at_ms)
            .filter(|(_, time)| {
                *time <= now_ms && self.last_sample.is_none_or(|last| *time >= last)
            });
        let Some((value, time)) = valid else {
            self.value.quality = ReadingQuality::Fault;
            self.pending = None;
            return None;
        };
        if self.last_sample == Some(time) {
            return None;
        }
        self.last_sample = Some(time);
        self.value.quality = ReadingQuality::Valid;
        if self.value.value.is_none() || self.value.value == Some(value) {
            self.value = reading.clone();
            self.pending = None;
            return None;
        }
        let since = match self.pending {
            Some((pending, since)) if pending == value => since,
            _ => time,
        };
        self.pending = Some((value, since));
        if time.saturating_sub(since) < settings.debounce_ms {
            return None;
        }
        self.pending = None;
        self.value = reading.clone();
        Some(value)
    }
}

/// Pure shared station site policy. Called only by the existing control owner.
pub struct SiteController {
    settings: ResolvedSiteSettings,
    sensors: BTreeMap<String, SensorState>,
    inputs: BTreeMap<String, InputState>,
    outputs: BTreeMap<String, OutputState>,
    alarms: BTreeMap<String, AlarmState>,
    fans: BTreeMap<String, FanState>,
    fan_status: BTreeMap<String, FanSnapshot>,
    published_outputs: BTreeMap<String, bool>,
    inhibitors: BTreeSet<String>,
    availability: BTreeMap<String, FanOutput>,
    ptt: BTreeMap<NodeId, bool>,
}
impl SiteController {
    /// Prepare all named policy state outside audio; no device is opened here.
    pub fn new(settings: ResolvedSiteSettings) -> Self {
        Self {
            sensors: settings
                .sensors
                .iter()
                .map(|value| (value.name.clone(), SensorState::new(value.clone())))
                .collect(),
            inputs: settings
                .inputs
                .iter()
                .map(|value| (value.name.clone(), InputState::new()))
                .collect(),
            outputs: settings
                .outputs
                .iter()
                .map(|value| (value.name.clone(), OutputState::new(value.clone())))
                .collect(),
            alarms: settings
                .alarms
                .iter()
                .map(|value| (value.name.clone(), AlarmState::new(value.clone())))
                .collect(),
            fans: settings
                .fans
                .iter()
                .map(|value| (value.name.clone(), FanState::new(value.clone())))
                .collect(),
            settings,
            fan_status: BTreeMap::new(),
            published_outputs: BTreeMap::new(),
            inhibitors: BTreeSet::new(),
            availability: BTreeMap::new(),
            ptt: BTreeMap::new(),
        }
    }
    /// Replace policy only after device owners complete safe handoff. Preserve
    /// unchanged deadlines, input debounce, alarm latches and measurement history.
    /// The caller republishes the resulting aggregate inhibit/output snapshot.
    pub fn reconfigure(&mut self, settings: ResolvedSiteSettings) {
        let mut next = Self::new(settings);
        let mut retained_outputs = BTreeSet::new();
        let mut retained_inhibitors = BTreeSet::from(["manual".to_owned()]);
        for policy in &next.settings.sensors {
            if let Some(mut state) = self.sensors.remove(&policy.name) {
                state.reconfigure(policy.clone());
                next.sensors.insert(policy.name.clone(), state);
            }
        }
        for policy in &next.settings.outputs {
            if let Some(old) = self
                .settings
                .outputs
                .iter()
                .find(|old| old.name == policy.name)
            {
                if old.enabled == policy.enabled
                    && old.endpoint == policy.endpoint
                    && old.active_high == policy.active_high
                {
                    retained_outputs.insert(policy.name.clone());
                }
                if old.enabled == policy.enabled {
                    let mut state = self
                        .outputs
                        .remove(&policy.name)
                        .expect("configured output");
                    state.reconfigure(policy.clone());
                    next.outputs.insert(policy.name.clone(), state);
                }
            }
        }
        for policy in &next.settings.inputs {
            if self.settings.inputs.contains(policy) {
                next.inputs.insert(
                    policy.name.clone(),
                    self.inputs.remove(&policy.name).expect("configured input"),
                );
                retained_inhibitors.insert(format!("input:{}", policy.name));
            }
        }
        for policy in &next.settings.alarms {
            if self.settings.alarms.contains(policy) {
                next.alarms.insert(
                    policy.name.clone(),
                    self.alarms.remove(&policy.name).expect("configured alarm"),
                );
                retained_inhibitors.insert(format!("alarm:{}", policy.name));
            }
        }
        for policy in &next.settings.fans {
            if self.settings.fans.contains(policy) && retained_outputs.contains(&policy.output) {
                next.fans.insert(
                    policy.name.clone(),
                    self.fans.remove(&policy.name).expect("configured fan"),
                );
                if let Some(status) = self.fan_status.remove(&policy.name) {
                    next.fan_status.insert(policy.name.clone(), status);
                }
            }
        }
        self.published_outputs
            .retain(|name, _| retained_outputs.contains(name));
        self.availability
            .retain(|name, _| retained_outputs.contains(name));
        self.inhibitors
            .retain(|name| retained_inhibitors.contains(name));
        next.published_outputs = std::mem::take(&mut self.published_outputs);
        next.availability = std::mem::take(&mut self.availability);
        next.inhibitors = std::mem::take(&mut self.inhibitors);
        next.ptt = std::mem::take(&mut self.ptt);
        *self = next;
    }
    /// Apply one monotonic control snapshot. Timers run without waiting for I/O or speech.
    pub fn advance(
        &mut self,
        now_ms: u64,
        observations: &SiteObservations,
        ptt: &BTreeMap<NodeId, bool>,
    ) -> Vec<SiteEffect> {
        self.ptt.clone_from(ptt);
        let mut effects = Vec::new();
        for policy in &self.settings.sensors {
            if policy.enabled {
                if let Some(reading) = observations.sensors.get(&policy.name) {
                    self.sensors
                        .get_mut(&policy.name)
                        .unwrap()
                        .observe(reading.clone(), now_ms);
                }
            }
        }
        for policy in &self.settings.outputs {
            let observation = observations.outputs.get(&policy.name);
            // Pending first acquisition keeps startup demand but remains unavailable.
            // A missing previously known owner is a fault, not a new startup.
            if observation.is_some()
                || self.availability.contains_key(&policy.name)
                || !policy.enabled
            {
                self.outputs
                    .get_mut(&policy.name)
                    .unwrap()
                    .advance(now_ms, observation.is_some_and(|output| output.available));
            }
        }
        self.availability.clone_from(&observations.outputs);
        for index in 0..self.settings.outputs.len() {
            let name = self.settings.outputs[index].name.clone();
            if self.settings.outputs[index].enabled
                && !self
                    .settings
                    .fans
                    .iter()
                    .any(|fan| fan.enabled && fan.output == name)
            {
                self.publish_output(&name, &mut effects);
            }
        }
        for index in 0..self.settings.inputs.len() {
            let policy = &self.settings.inputs[index];
            if !policy.enabled {
                continue;
            }
            let missing = Observation {
                value: None,
                observed_at_ms: None,
                quality: ReadingQuality::Unavailable,
            };
            let reading = observations.inputs.get(&policy.name).unwrap_or(&missing);
            let edge = self
                .inputs
                .get_mut(&policy.name)
                .unwrap()
                .observe(reading, policy, now_ms);
            if let Some(active) = edge {
                let source = format!("input:{}", policy.name);
                let actions = if active {
                    policy.on_active.clone()
                } else {
                    policy.on_inactive.clone()
                };
                self.actions(&source, &actions, now_ms, &mut effects);
            }
        }
        for index in 0..self.settings.alarms.len() {
            let policy = &self.settings.alarms[index];
            let reading = if let Some(sensor) = self.sensors.get(&policy.source) {
                let snapshot = sensor.snapshot(now_ms);
                Observation {
                    value: (if policy.smoothed {
                        snapshot.smoothed
                    } else {
                        snapshot.current.value
                    })
                    .map(AlarmValue::Number),
                    observed_at_ms: snapshot.current.observed_at_ms,
                    quality: snapshot.current.quality,
                }
            } else if let Some(input) = self.inputs.get(&policy.source) {
                Observation {
                    value: input.value.value.map(AlarmValue::Digital),
                    observed_at_ms: input.value.observed_at_ms,
                    quality: input.value.quality,
                }
            } else {
                Observation {
                    value: None,
                    observed_at_ms: None,
                    quality: ReadingQuality::Unavailable,
                }
            };
            let name = policy.name.clone();
            let events = self
                .alarms
                .get_mut(&name)
                .unwrap()
                .advance(now_ms, &reading);
            for event in events {
                self.alarm_event(index, event, now_ms, &mut effects);
            }
        }
        self.fan_effects(now_ms, &mut effects);
        effects
    }
    fn publish_output(&mut self, name: &str, effects: &mut Vec<SiteEffect>) {
        let on = self.outputs[name].snapshot().requested_on;
        if self.published_outputs.insert(name.to_owned(), on) != Some(on) {
            effects.push(SiteEffect::Output {
                name: name.to_owned(),
                on,
            });
        }
    }
    fn inhibit(&mut self, source: &str, enabled: bool, effects: &mut Vec<SiteEffect>) {
        let old = !self.inhibitors.is_empty();
        if enabled {
            self.inhibitors.insert(source.to_owned());
        } else {
            self.inhibitors.remove(source);
        }
        let new = !self.inhibitors.is_empty();
        if old != new {
            effects.push(SiteEffect::TransmitInhibit(new));
        }
    }
    fn alarm_event(
        &mut self,
        index: usize,
        event: AlarmEvent,
        now_ms: u64,
        effects: &mut Vec<SiteEffect>,
    ) {
        let policy = &self.settings.alarms[index];
        let (actions, message) = match event {
            AlarmEvent::Trip => (policy.on_trip.clone(), policy.trip_message.clone()),
            AlarmEvent::Clear => (policy.on_clear.clone(), policy.clear_message.clone()),
            AlarmEvent::Repeat => (Vec::new(), policy.repeat_message.clone()),
        };
        let source = format!("alarm:{}", policy.name);
        self.actions(&source, &actions, now_ms, effects);
        if !message.is_empty() {
            effects.push(SiteEffect::Message(message));
        }
    }
    fn actions(
        &mut self,
        source: &str,
        actions: &[SiteAction],
        now_ms: u64,
        effects: &mut Vec<SiteEffect>,
    ) {
        for action in actions {
            let result = match action {
                SiteAction::Macro(name) => {
                    effects.push(SiteEffect::Macro(name.clone()));
                    Ok(())
                }
                SiteAction::Inhibit(enabled) => {
                    self.inhibit(source, *enabled, effects);
                    Ok(())
                }
                SiteAction::Output { name, request } => {
                    self.output(name, *request, now_ms, effects)
                }
                SiteAction::Fan { name, duty } => self.fan(name, *duty, now_ms, effects),
            };
            if let Err(error) = result {
                effects.push(SiteEffect::Fault {
                    target: source.to_owned(),
                    error,
                });
            }
        }
    }
    fn output(
        &mut self,
        name: &str,
        request: OutputRequest,
        now_ms: u64,
        effects: &mut Vec<SiteEffect>,
    ) -> Result<(), SiteError> {
        if self
            .settings
            .fans
            .iter()
            .any(|fan| fan.enabled && fan.output == name)
        {
            return Err(SiteError::Unsafe);
        }
        self.outputs
            .get_mut(name)
            .ok_or(SiteError::UnknownName)?
            .command(request, now_ms)?;
        self.publish_output(name, effects);
        Ok(())
    }
    fn sensor_value(&self, name: &str, now_ms: u64) -> Option<f64> {
        let snapshot = self.sensors.get(name)?.snapshot(now_ms);
        (snapshot.current.quality == ReadingQuality::Valid)
            .then_some(snapshot.current.value)
            .flatten()
    }
    fn fan_effects(&mut self, now_ms: u64, effects: &mut Vec<SiteEffect>) {
        for policy in &self.settings.fans {
            if !policy.enabled {
                continue;
            }
            let temperature = self.sensor_value(&policy.temperature_sensor, now_ms);
            let rpm = self.sensor_value(&policy.rpm_sensor, now_ms);
            let output = self
                .availability
                .get(&policy.output)
                .copied()
                .unwrap_or(FanOutput {
                    available: false,
                    pwm: false,
                });
            let snapshot = self.fans.get_mut(&policy.name).unwrap().advance(
                now_ms,
                &self.ptt,
                temperature,
                rpm,
                output,
            );
            if self
                .fan_status
                .get(&policy.name)
                .is_none_or(|prior| prior.duty != snapshot.duty)
            {
                effects.push(SiteEffect::Fan {
                    name: policy.name.clone(),
                    duty: snapshot.duty,
                });
            }
            self.fan_status.insert(policy.name.clone(), snapshot);
        }
    }
    fn fan(
        &mut self,
        name: &str,
        duty: Option<f64>,
        now_ms: u64,
        effects: &mut Vec<SiteEffect>,
    ) -> Result<(), SiteError> {
        if !self
            .settings
            .fans
            .iter()
            .any(|fan| fan.name == name && fan.enabled)
        {
            return Err(SiteError::Unavailable);
        }
        self.fans
            .get_mut(name)
            .expect("enabled fan state")
            .manual(duty)?;
        self.fan_effects(now_ms, effects);
        Ok(())
    }
    /// Apply an already-authorized operation. Clearing manual inhibition cannot
    /// release an alarm's inhibit or a physical interlock.
    pub fn command(
        &mut self,
        command: SiteCommand,
        now_ms: u64,
    ) -> Result<Vec<SiteEffect>, SiteError> {
        let mut effects = Vec::new();
        match command {
            SiteCommand::Output { name, request } => {
                self.output(&name, request, now_ms, &mut effects)?
            }
            SiteCommand::AlarmAcknowledge(name) => self
                .alarms
                .get_mut(&name)
                .ok_or(SiteError::UnknownName)?
                .acknowledge(),
            SiteCommand::AlarmReset(name) => {
                if let Some(event) = self
                    .alarms
                    .get_mut(&name)
                    .ok_or(SiteError::UnknownName)?
                    .reset()?
                {
                    let index = self
                        .settings
                        .alarms
                        .iter()
                        .position(|alarm| alarm.name == name)
                        .expect("configured alarm");
                    self.alarm_event(index, event, now_ms, &mut effects);
                }
            }
            SiteCommand::ResetExtrema(name) => self
                .sensors
                .get_mut(&name)
                .ok_or(SiteError::UnknownName)?
                .reset_extrema(),
            SiteCommand::Fan { name, duty } => self.fan(&name, duty, now_ms, &mut effects)?,
            SiteCommand::Inhibit(enabled) => self.inhibit("manual", enabled, &mut effects),
        }
        Ok(effects)
    }
    /// Read one coherent policy snapshot without I/O or timer advancement.
    pub fn snapshot(&self, now_ms: u64) -> SiteSnapshot {
        SiteSnapshot {
            sensors: self
                .sensors
                .iter()
                .map(|(name, state)| (name.clone(), state.snapshot(now_ms)))
                .collect(),
            inputs: self
                .inputs
                .iter()
                .map(|(name, state)| (name.clone(), state.value.clone()))
                .collect(),
            alarms: self
                .alarms
                .iter()
                .map(|(name, state)| (name.clone(), state.snapshot()))
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|(name, state)| (name.clone(), state.snapshot()))
                .collect(),
            fans: self.fan_status.clone(),
            transmit_inhibited: !self.inhibitors.is_empty(),
        }
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
