//! Site acquisition borrows physical owners only on serialized station control.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use rpt_advanced_core::{
    config::NodeId,
    runtime::dtmf::DigitOperation,
    site::{SiteEffect, SiteError, SiteObservations},
};
use std::collections::BTreeMap;

/// Control-owned delivery, bounded by configured message/action identities.
#[derive(Default)]
pub(super) struct Delivery {
    revision: u64,
    pending: Vec<(String, String)>,
    faults: BTreeMap<(String, String), SiteError>,
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
        if self.site_transition.is_some() {
            return Err(SiteError::Unavailable);
        }
        let effects = self
            .runtime
            .node(local)
            .ok_or(SiteError::UnknownName)?
            .site_command(command, clock.now_ms)?;
        self.site_effects(local, effects, clock);
        Ok(())
    }
    /// Append control snapshots only; requested/submitted values never imply electrical readback.
    pub(super) fn write_site_status(
        &mut self,
        local: &str,
        text: &mut String,
    ) -> Result<(), RuntimeError> {
        use std::fmt::Write;
        let now_ms = self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let snapshot = self
            .runtime
            .node(local)
            .ok_or(RuntimeError::MissingNode)?
            .site()
            .snapshot(now_ms);
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
            let submitted = self
                .site_radio
                .output_status(local, &name)
                .or_else(|| self.site_acquisition.output_status(local, &name));
            let _ = writeln!(
                text,
                "    output {name} requested-on={} fault={} pulse-deadline-ms={:?} submitted={:?} submission-quality={:?} readback=unavailable",
                output.requested_on,
                output.fault,
                output.pulse_deadline_ms,
                submitted.as_ref().and_then(|state| state.submitted),
                submitted.map_or(
                    rpt_advanced_core::site::ReadingQuality::Unavailable,
                    |state| state.quality
                )
            );
        }
        for (name, fan) in snapshot.fans {
            let _ = writeln!(
                text,
                "    fan {name} requested-duty={} fault={} run-on-until-ms={:?}",
                fan.duty, fan.fault, fan.run_on_until_ms
            );
        }
        for ((node, target), fault) in &self.site_delivery.faults {
            if node == local {
                let _ = writeln!(text, "    site-action {target} fault={fault:?}");
            }
        }
        Ok(())
    }

    /// Drive station policy on the existing control owner, without waiting for I/O or media.
    pub(super) fn advance_sites(&mut self, clock: RuntimeClock) {
        if self.site_delivery.revision != self.runtime.revision() {
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
        let mut observations = self.poll_sites(clock.now_ms);
        let ptt = self
            .leases
            .iter()
            .map(|(name, lease)| {
                let keyed = lease
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .status
                    .transmit_keyed();
                (NodeId::new(name).expect("configured node"), keyed)
            })
            .collect();
        let names: Vec<_> = self
            .runtime
            .node_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        for local in names {
            let values = observations.remove(&local).unwrap_or_default();
            let effects =
                self.runtime
                    .node(&local)
                    .unwrap()
                    .advance_site(clock.now_ms, &values, &ptt);
            self.site_effects(&local, effects, clock);
        }
        let mut pending = std::mem::take(&mut self.site_delivery.pending);
        pending.retain(
            |(local, name)| match self.runtime.queue_named_message(local, name) {
                Ok(accepted) => !accepted,
                Err(_) => {
                    self.site_delivery
                        .faults
                        .insert((local.clone(), name.clone()), SiteError::Unavailable);
                    false
                }
            },
        );
        self.site_delivery.pending = pending;
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
    fn site_effects(&mut self, local: &str, effects: Vec<SiteEffect>, clock: RuntimeClock) {
        for effect in effects {
            let (target, result) = match effect {
                SiteEffect::Output { name, on } => {
                    let result = self.site_output(local, &name, f64::from(on));
                    (name, result)
                }
                SiteEffect::Fan { name, duty } => {
                    let output = self
                        .runtime
                        .node(local)
                        .and_then(|node| node.site().fan_output(&name))
                        .map(str::to_owned);
                    let result = output
                        .ok_or(SiteError::UnknownName)
                        .and_then(|output| self.site_output(local, &output, duty));
                    (name, result)
                }
                // RuntimeNode publishes the aggregate gate before handing effects to this host.
                SiteEffect::TransmitInhibit(_) => continue,
                SiteEffect::Macro(name) => {
                    let result = self
                        .runtime
                        .site_macro(local, &name)
                        .and_then(|command| {
                            self.runtime.command(
                                local,
                                DigitOperation::new(command, None),
                                clock,
                                false,
                            )
                        })
                        .and_then(|effect| {
                            if matches!(effect, LinkEffect::Connect(_)) {
                                self.site_delivery.links.push((local.to_owned(), effect));
                                Ok(())
                            } else {
                                self.immediate(local, effect, clock)
                            }
                        })
                        .map_err(|_| SiteError::Unavailable);
                    (name, result)
                }
                SiteEffect::Message(name) => {
                    let key = (local.to_owned(), name);
                    if !self.site_delivery.pending.contains(&key) {
                        self.site_delivery.pending.push(key);
                    }
                    continue;
                }
                SiteEffect::Fault { target, error } => (target, Err(error)),
            };
            let key = (local.to_owned(), target);
            if let Err(error) = result {
                self.site_delivery.faults.insert(key, error);
            } else {
                self.site_delivery.faults.remove(&key);
            }
        }
    }
    /// Transfer generation-protected dials to the existing off-control dispatcher.
    pub(crate) fn take_site_links(&mut self) -> Vec<(String, LinkEffect)> {
        std::mem::take(&mut self.site_delivery.links)
    }
    /// Identify radio replacements and removed nodes using the runtime's own comparison.
    pub(crate) fn site_basis(&self, document: &ConfigDocument) -> Result<SiteBasis, RuntimeError> {
        use rpt_advanced_core::config::NodeId;
        let mut replaced = std::collections::BTreeSet::new();
        let names = document.nodes();
        for name in self.runtime.node_names() {
            let next = ResolvedNodeSettings::resolve(document, &NodeId::new(name)?)?.value;
            let old = self.runtime.settings(name).expect("active node settings");
            if !names.contains(&name)
                || !next.enabled
                || old.channel != next.channel
                || old.radio != next.radio
            {
                replaced.insert(name.to_owned());
            }
        }
        Ok(SiteBasis {
            revision: self.runtime.revision(),
            known: self.site_acquisition.keys(),
            replaced,
        })
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
    /// Install owners prepared outside control after successful radio activation.
    pub(crate) fn install_sites(&mut self, acquisition: Acquisition, radio: RadioSite) {
        self.site_acquisition = acquisition;
        self.site_radio = radio;
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
        let mut readings = self.site_acquisition.poll(now_ms, |node| {
            self.runtime.node(node).and_then(|node| node.work())
        });
        let radio = self.site_radio.poll(now_ms, |owner, bank, operation| {
            let (_, lease) = self
                .leases
                .iter()
                .rev()
                .find(|(name, _)| name == owner)
                .ok_or(-7)?;
            lease
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .worker
                .as_mut()
                .ok_or(-7)?
                .site_operation(bank, operation)
        });
        for (node, values) in radio {
            let entry = readings.entry(node).or_default();
            entry.inputs.extend(values.inputs);
            entry.outputs.extend(values.outputs);
        }
        readings
    }
}
