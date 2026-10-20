//! Site acquisition borrows physical owners only on serialized station control.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use rpt_advanced_core::site::SiteObservations;
use std::collections::BTreeMap;

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
