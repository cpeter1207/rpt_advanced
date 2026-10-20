//! Bounded polling and output acknowledgements shared by configured station nodes.
use super::{
    backends::{
        SiteBackends,
        endpoint::{Direction, Sample},
    },
    worker::{DeviceWorker, Outcome, Reply},
};
use rpt_advanced_core::{
    config::{ResolvedSiteSettings, SiteBackend, SiteEndpoint},
    runtime::GenerationWork,
    site::{FanOutput, Observation, ReadingQuality, SiteObservations},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

/// Logical demand and driver submission, never independent electrical readback.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputStatus {
    /// Latest normalized request.
    pub requested: f64,
    /// Last successfully submitted normalized value and monotonic time.
    pub submitted: Option<(f64, u64)>,
    /// Current owner/write health.
    pub quality: ReadingQuality,
}

/// One serial acquisition owner per configured external physical device.
#[derive(Default)]
pub struct Acquisition {
    devices: Vec<Device>,
    replacement: Option<Replacement>,
    auto_resume: bool,
}
type DeviceKey = (SiteBackend, String);
/// Inert candidate whose new worker allocation/failed cleanup occurs outside control.
pub struct PreparedAcquisition {
    known: BTreeSet<DeviceKey>,
    replacement: Replacement,
}
struct Replacement {
    bindings: BTreeMap<DeviceKey, Vec<Binding>>,
    fresh: BTreeMap<DeviceKey, Device>,
    prepared: bool,
}
struct Device {
    key: DeviceKey,
    worker: DeviceWorker,
    bindings: Vec<Binding>,
    paused: bool,
}
#[derive(Clone)]
enum Value {
    Sensor(Observation<f64>),
    Input(Observation<bool>),
    Output { status: OutputStatus, pwm: bool },
}
impl Device {
    fn prepare(
        key: DeviceKey,
        bindings: Vec<Binding>,
        backends: Arc<SiteBackends>,
        epoch: Instant,
    ) -> Result<Self, ReadingQuality> {
        Ok(Self {
            key,
            worker: DeviceWorker::start(backends, physical(&bindings), epoch)?,
            bindings,
            paused: false,
        })
    }
}
fn physical(bindings: &[Binding]) -> Vec<(SiteEndpoint, Direction)> {
    bindings
        .iter()
        .map(|binding| (binding.endpoint.clone(), binding.direction))
        .collect()
}
/// Keep observations and live intent only when their source and logical identity survive.
fn transfer(previous: &[Binding], next: &mut [Binding]) {
    for binding in next {
        if let Some(old) = previous.iter().find(|old| {
            old.node == binding.node
                && old.name == binding.name
                && old.endpoint == binding.endpoint
                && old.direction.same_io(binding.direction)
        }) {
            binding.value = old.value.clone();
            binding.observed = old.observed;
            binding.last_attempt = old.last_attempt;
            binding.due_ms = old.due_ms;
        }
    }
}
struct Binding {
    node: String,
    name: String,
    endpoint: SiteEndpoint,
    direction: Direction,
    interval_ms: u64,
    stale_ms: u64,
    due_ms: u64,
    last_attempt: Option<f64>,
    observed: bool,
    value: Value,
}
fn unavailable<T>() -> Observation<T> {
    Observation {
        value: None,
        observed_at_ms: None,
        quality: ReadingQuality::Unavailable,
    }
}
impl Binding {
    fn demand(&self) -> Option<f64> {
        match &self.value {
            Value::Output { status, .. } => Some(status.requested),
            _ => None,
        }
    }
    fn complete(&mut self, reply: Reply) {
        if !reply.work.is_current() {
            self.due_ms = 0;
            return;
        }
        self.observed = true;
        let outcome = match (&mut self.value, reply.outcome) {
            (Value::Sensor(reading), Ok(Outcome::Reading(Sample::Number(value)))) => {
                *reading = Observation {
                    value: Some(value),
                    observed_at_ms: Some(reply.completed_at_ms),
                    quality: ReadingQuality::Valid,
                };
                return;
            }
            (Value::Input(reading), Ok(Outcome::Reading(Sample::Digital(value)))) => {
                *reading = Observation {
                    value: Some(value),
                    observed_at_ms: Some(reply.completed_at_ms),
                    quality: ReadingQuality::Valid,
                };
                return;
            }
            (Value::Output { status, pwm }, Ok(Outcome::Submitted { pwm: capable })) => {
                status.submitted = self
                    .last_attempt
                    .map(|value| (value, reply.completed_at_ms));
                status.quality = ReadingQuality::Valid;
                *pwm = capable;
                return;
            }
            (_, Err(error)) => error,
            _ => ReadingQuality::Fault,
        };
        match &mut self.value {
            Value::Sensor(reading) => reading.quality = outcome,
            Value::Input(reading) => reading.quality = outcome,
            Value::Output { status, .. } => status.quality = outcome,
        }
    }
    fn snapshot(&self, snapshot: &mut SiteObservations, now_ms: u64) {
        match &self.value {
            Value::Sensor(reading) => {
                snapshot
                    .sensors
                    .insert(self.name.clone(), freshness(reading, now_ms, self.stale_ms));
            }
            Value::Input(reading) => {
                snapshot
                    .inputs
                    .insert(self.name.clone(), freshness(reading, now_ms, self.stale_ms));
            }
            Value::Output { status, pwm } if self.observed => {
                snapshot.outputs.insert(
                    self.name.clone(),
                    FanOutput {
                        available: status.quality == ReadingQuality::Valid,
                        pwm: *pwm,
                    },
                );
            }
            _ => {}
        }
    }
}
fn freshness<T: Clone>(reading: &Observation<T>, now_ms: u64, stale_ms: u64) -> Observation<T> {
    let mut reading = reading.clone();
    if reading.quality == ReadingQuality::Valid
        && reading
            .observed_at_ms
            .is_some_and(|time| now_ms.saturating_sub(time) >= stale_ms)
    {
        reading.quality = ReadingQuality::Stale;
    }
    reading
}

/// Resolve external-device grouping only; CM119/parallel bindings reuse radio owners.
fn bindings(
    settings: &BTreeMap<String, ResolvedSiteSettings>,
) -> BTreeMap<(SiteBackend, String), Vec<Binding>> {
    let mut devices: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut add = |node: &str,
                   name: &str,
                   endpoint: &SiteEndpoint,
                   direction,
                   interval_ms,
                   stale_ms,
                   value| {
        if matches!(endpoint.backend, SiteBackend::Cm119 | SiteBackend::Parallel) {
            return;
        }
        devices
            .entry((endpoint.backend, endpoint.device.clone()))
            .or_default()
            .push(Binding {
                node: node.into(),
                name: name.into(),
                endpoint: endpoint.clone(),
                direction,
                interval_ms,
                stale_ms,
                due_ms: 0,
                last_attempt: None,
                observed: false,
                value,
            });
    };
    for (node, settings) in settings {
        for sensor in settings.sensors.iter().filter(|value| value.enabled) {
            if let Some(endpoint) = &sensor.endpoint {
                add(
                    node,
                    &sensor.name,
                    endpoint,
                    Direction::Input { active_high: true },
                    sensor.poll_interval_ms,
                    sensor.stale_after_ms,
                    Value::Sensor(unavailable()),
                );
            }
        }
        for input in settings.inputs.iter().filter(|value| value.enabled) {
            if let Some(endpoint) = &input.endpoint {
                // Digital acquisition follows the existing 10 ms station cadence. A blocked
                // read uses the same five-second freshness bound as default analog sources.
                add(
                    node,
                    &input.name,
                    endpoint,
                    Direction::Input {
                        active_high: input.active_high,
                    },
                    10,
                    5000,
                    Value::Input(unavailable()),
                );
            }
        }
        for output in settings.outputs.iter().filter(|value| value.enabled) {
            if let Some(endpoint) = &output.endpoint {
                add(
                    node,
                    &output.name,
                    endpoint,
                    Direction::Output {
                        active_high: output.active_high,
                        startup_on: output.startup_on,
                        safe_on: output.safe_on,
                    },
                    1000,
                    5000,
                    Value::Output {
                        status: OutputStatus {
                            requested: f64::from(output.startup_on),
                            submitted: None,
                            quality: ReadingQuality::Unavailable,
                        },
                        pwm: false,
                    },
                );
            }
        }
    }
    devices
}
impl Acquisition {
    /// Copy current identities before preparing a candidate outside station control.
    pub fn keys(&self) -> BTreeSet<DeviceKey> {
        self.devices
            .iter()
            .map(|device| device.key.clone())
            .collect()
    }
    /// Allocate only new, paused owners outside control; no operation is submitted here.
    pub fn prepare_reload(
        settings: &BTreeMap<String, ResolvedSiteSettings>,
        backends: Arc<SiteBackends>,
        epoch: Instant,
        known: BTreeSet<DeviceKey>,
    ) -> Result<PreparedAcquisition, ReadingQuality> {
        let mut bindings = bindings(settings);
        let mut fresh = BTreeMap::new();
        let keys: Vec<_> = bindings
            .keys()
            .filter(|key| !known.contains(*key))
            .cloned()
            .collect();
        for key in keys {
            let entries = bindings.remove(&key).expect("selected candidate key");
            let mut device = Device::prepare(key.clone(), entries, Arc::clone(&backends), epoch)?;
            // No operations have been submitted; a new owner cannot touch hardware
            // until commit, old-owner retirement, and explicit resume.
            device.worker.pause(false);
            device.paused = true;
            fresh.insert(key, device);
        }
        Ok(PreparedAcquisition {
            known,
            replacement: Replacement {
                bindings,
                fresh,
                prepared: false,
            },
        })
    }
    /// Fence changed owners; return rejected preparation for disposal outside control.
    /// Also drain operations for radio nodes whose generation cannot retain old permits.
    pub fn begin(
        &mut self,
        candidate: PreparedAcquisition,
        replaced: &BTreeSet<String>,
    ) -> Result<(), PreparedAcquisition> {
        if self.replacement.is_some()
            || self.devices.iter().any(|device| device.paused)
            || self.keys() != candidate.known
        {
            return Err(candidate);
        }
        for device in &mut self.devices {
            let next = candidate.replacement.bindings.get(&device.key);
            if next.is_none_or(|next| physical(next) != physical(&device.bindings))
                || device
                    .bindings
                    .iter()
                    .any(|binding| replaced.contains(&binding.node))
            {
                device.worker.pause(next.is_none());
                device.paused = true;
            }
        }
        self.auto_resume = false;
        self.replacement = Some(candidate.replacement);
        Ok(())
    }
    /// Prepare retained-device bindings after their pause barrier.
    pub fn stage(&mut self) -> bool {
        if self.barrier() != Some(Ok(())) {
            return false;
        }
        let Some(replacement) = &mut self.replacement else {
            return false;
        };
        if replacement.prepared {
            return false;
        }
        for device in self.devices.iter_mut().filter(|device| device.paused) {
            if let Some(bindings) = replacement.bindings.get(&device.key) {
                assert!(device.worker.stage(physical(bindings)));
            }
        }
        replacement.prepared = true;
        true
    }
    /// Publish bindings after successful runtime publication; return safely retired owners.
    pub fn commit(&mut self) -> Result<Vec<DeviceWorker>, ReadingQuality> {
        if self.barrier() != Some(Ok(()))
            || !self
                .replacement
                .as_ref()
                .is_some_and(|replacement| replacement.prepared)
        {
            return Err(ReadingQuality::Fault);
        }
        let mut replacement = self.replacement.take().expect("validated replacement");
        let mut retained = Vec::new();
        let mut retired = Vec::new();
        for mut device in self.devices.drain(..) {
            if let Some(mut bindings) = replacement.bindings.remove(&device.key) {
                transfer(&device.bindings, &mut bindings);
                if device.paused {
                    assert!(device.worker.commit());
                }
                device.bindings = bindings;
                retained.push(device);
            } else {
                retired.push(device.worker);
            }
        }
        retained.extend(replacement.fresh.into_values());
        self.devices = retained;
        Ok(retired)
    }
    /// Cancel a candidate; return unused new owners for disposal outside control.
    pub fn abort(&mut self) -> Vec<DeviceWorker> {
        let retired = self
            .replacement
            .take()
            .map(|replacement| {
                replacement
                    .fresh
                    .into_values()
                    .map(|device| device.worker)
                    .collect()
            })
            .unwrap_or_default();
        for device in self.devices.iter_mut().filter(|device| device.paused) {
            device.worker.pause(false);
            // A preparation may already have applied the old safe state.
            // Restore retained policy demand after the abort barrier, not startup.
            for binding in &mut device.bindings {
                binding.due_ms = 0;
            }
        }
        self.auto_resume = true;
        retired
    }
    /// Resume acknowledged owners after commit or abort without reopening them.
    pub fn resume(&mut self) -> bool {
        if self.replacement.is_some() {
            return false;
        }
        self.auto_resume = true;
        for device in self.devices.iter_mut().filter(|device| device.paused) {
            if device.worker.resume() {
                device.paused = false;
            }
        }
        self.devices.iter().all(|device| !device.paused)
    }
    /// Prepare bounded workers without performing hardware I/O on station control.
    pub fn prepare(
        settings: &BTreeMap<String, ResolvedSiteSettings>,
        backends: Arc<SiteBackends>,
        epoch: Instant,
    ) -> Result<Self, ReadingQuality> {
        let devices = bindings(settings)
            .into_iter()
            .map(|(key, bindings)| Device::prepare(key, bindings, Arc::clone(&backends), epoch))
            .collect::<Result<_, ReadingQuality>>()?;
        Ok(Self {
            devices,
            ..Self::default()
        })
    }
    /// Drain observations and submit only due operations with fresh generation permits.
    pub fn poll(
        &mut self,
        now_ms: u64,
        mut work: impl FnMut(&str) -> Option<GenerationWork>,
    ) -> BTreeMap<String, SiteObservations> {
        let mut snapshots = BTreeMap::new();
        if self.auto_resume {
            self.resume();
        }
        for device in &mut self.devices {
            while let Some(reply) = device.worker.poll() {
                device.bindings[reply.index].complete(reply);
            }
            for (index, binding) in device.bindings.iter_mut().enumerate() {
                if let Value::Output { status, .. } = &mut binding.value {
                    if status.quality == ReadingQuality::Valid
                        && status
                            .submitted
                            .is_some_and(|(_, at)| now_ms.saturating_sub(at) >= binding.stale_ms)
                    {
                        status.quality = ReadingQuality::Stale;
                    }
                }
                let demand = binding.demand();
                if !device.paused && (now_ms >= binding.due_ms || demand != binding.last_attempt) {
                    if let Some(work) = work(&binding.node) {
                        if device.worker.submit(index, demand, work) {
                            binding.due_ms = now_ms.saturating_add(binding.interval_ms);
                            binding.last_attempt = demand;
                        }
                    }
                }
                binding.snapshot(snapshots.entry(binding.node.clone()).or_default(), now_ms);
            }
        }
        snapshots
    }
    /// Update the bounded latest demand; reject unknown/nonfinite/out-of-range requests.
    pub fn output(&mut self, node: &str, name: &str, demand: f64) -> Result<(), ReadingQuality> {
        if !demand.is_finite() || !(0.0..=1.0).contains(&demand) {
            return Err(ReadingQuality::Fault);
        }
        if let Some(Value::Output { status, .. }) = self
            .devices
            .iter_mut()
            .flat_map(|device| &mut device.bindings)
            .find(|binding| {
                binding.node == node
                    && binding.name == name
                    && matches!(binding.value, Value::Output { .. })
            })
            .map(|binding| &mut binding.value)
        {
            status.requested = demand;
            return Ok(());
        }
        Err(ReadingQuality::Unavailable)
    }
    /// Inspect requested/submitted state without inventing physical readback.
    pub fn output_status(&self, node: &str, name: &str) -> Option<OutputStatus> {
        self.devices
            .iter()
            .flat_map(|device| &device.bindings)
            .find_map(|binding| match &binding.value {
                Value::Output { status, .. } if binding.node == node && binding.name == name => {
                    Some(status.clone())
                }
                _ => None,
            })
    }
    /// Fence ordinary work and optionally require confirmed safe output submissions.
    pub fn pause(&mut self, safe: bool) {
        self.auto_resume = false;
        for device in &mut self.devices {
            device.worker.pause(safe);
            device.paused = true;
        }
    }
    /// Drain operation permits and inspect quiescence without waiting for device I/O.
    pub fn barrier(&mut self) -> Option<Result<(), ReadingQuality>> {
        let mut ready = true;
        let mut failed = false;
        for device in self.devices.iter_mut().filter(|device| device.paused) {
            while let Some(reply) = device.worker.poll() {
                device.bindings[reply.index].complete(reply);
            }
            match device.worker.barrier() {
                None => ready = false,
                Some(Err(_)) => failed = true,
                Some(Ok(())) => {}
            }
        }
        ready.then_some(if failed {
            Err(ReadingQuality::Fault)
        } else {
            Ok(())
        })
    }
    /// Join safely retired device workers only outside station control/audio callbacks.
    pub fn finish(&mut self) -> bool {
        if self.barrier() != Some(Ok(())) {
            return false;
        }
        self.devices.iter_mut().all(|device| device.worker.finish())
    }
}
