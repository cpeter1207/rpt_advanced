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
use std::{collections::BTreeMap, sync::Arc, time::Instant};

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
pub struct Acquisition {
    devices: Vec<Device>,
}
struct Device {
    worker: DeviceWorker,
    bindings: Vec<Binding>,
}
enum Value {
    Sensor(Observation<f64>),
    Input(Observation<bool>),
    Output { status: OutputStatus, pwm: bool },
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
    /// Prepare bounded workers without performing hardware I/O on station control.
    pub fn prepare(
        settings: &BTreeMap<String, ResolvedSiteSettings>,
        backends: Arc<SiteBackends>,
        epoch: Instant,
    ) -> Result<Self, ReadingQuality> {
        let devices = bindings(settings)
            .into_values()
            .map(|bindings| {
                let settings = bindings
                    .iter()
                    .map(|binding| (binding.endpoint.clone(), binding.direction))
                    .collect();
                Ok(Device {
                    worker: DeviceWorker::start(Arc::clone(&backends), settings, epoch)?,
                    bindings,
                })
            })
            .collect::<Result<_, ReadingQuality>>()?;
        Ok(Self { devices })
    }
    /// Drain observations and submit only due operations with fresh generation permits.
    pub fn poll(
        &mut self,
        now_ms: u64,
        mut work: impl FnMut(&str) -> Option<GenerationWork>,
    ) -> BTreeMap<String, SiteObservations> {
        let mut snapshots = BTreeMap::new();
        for device in &mut self.devices {
            while let Some(reply) = device.worker.poll() {
                device.bindings[reply.index].complete(reply);
            }
            for (index, binding) in device.bindings.iter_mut().enumerate() {
                let demand = binding.demand();
                if now_ms >= binding.due_ms || demand != binding.last_attempt {
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
            .find(|binding| binding.node == node && binding.name == name)
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
        for device in &mut self.devices {
            device.worker.pause(safe);
        }
    }
    /// Drain operation permits and inspect quiescence without waiting for device I/O.
    pub fn barrier(&mut self) -> Option<Result<(), ReadingQuality>> {
        let mut ready = true;
        let mut failed = false;
        for device in &mut self.devices {
            while device.worker.poll().is_some() {}
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
