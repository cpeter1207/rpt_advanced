//! Core and physical preparations share one reservation until actual aggregate adoption.
use super::{
    Audio, Device, GenerationControl, Host, Lease, StationAudio, TextExecutor, media_worker,
    native::{Plans, RadioTicket},
    prepared,
};
use crate::{
    management::native::AccountCredentials, services::StationBinding, worker::LocalRingUpdate,
};
use rpt_advanced_core::{
    config::resources::{
        candidate::{ResourceId, ResourceKind, ResourceScope},
        generation::RuntimeConfiguration,
        profiles::credentials::CredentialSlot,
    },
    runtime::{PreparedConfigurationTransition, RuntimeClock, RuntimeError},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub(super) struct CoreCandidate {
    core: PreparedConfigurationTransition<Audio, GenerationControl>,
    text: TextExecutor,
    bindings: BTreeMap<ResourceId, (Arc<Mutex<Lease>>, Arc<StationBinding>)>,
}

pub(super) struct Adopting {
    revision: String,
    plans: Plans,
    credentials: Arc<AccountCredentials>,
    inputs: Arc<crate::management::ProfileInputs>,
}

impl Host {
    pub(crate) fn prepare_core(
        &mut self,
        ticket: &RadioTicket,
        configuration: Arc<RuntimeConfiguration>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        // Attached accounts requiring a network participant must enter the same
        // adoption barrier as the RF device. Until a frontend provides a real
        // participant, keep the incumbent selected configuration running.
        if configuration.protocols.ports().any(|(_, port)| {
            port.metadata.enabled
                && (port
                    .allstarlink
                    .as_ref()
                    .is_some_and(|account| account.settings.account_identity().is_some())
                    || port
                        .echolink
                        .as_ref()
                        .is_some_and(|account| account.settings.enabled)
                    || port.phone_patch.as_ref().is_some_and(|account| {
                        account.settings.provider.account_identity().is_some()
                    }))
        }) {
            return Err(RuntimeError::Preparation);
        }
        let basis = self.modular_site_basis(ticket)?;
        let services = self.services;
        let epoch = self.epoch;
        let owner = &mut self.radio_preparation;
        let (key, plans) = owner.staged.as_ref().ok_or(RuntimeError::Rejected)?;
        if !ticket.matches(key) || owner.core.is_some() {
            return Err(RuntimeError::Rejected);
        }
        let mut text = TextExecutor::prepare(plans.0.len().max(1), self.media.clone())
            .map_err(|_| RuntimeError::Preparation)?;
        let previous_lease = |name: &str| {
            self.leases
                .iter()
                .rev()
                .find(|(local, _)| local == name)
                .map(|(_, lease)| lease)
        };
        let mut fresh_leases = BTreeMap::new();
        let core = self.runtime.prepare_configuration(
            configuration,
            &self.media,
            |port, settings| {
                let name = port.to_string();
                let plan = plans.0.get(&port).ok_or(RuntimeError::MissingNode)?;
                let mut candidate = prepared(
                    &name,
                    settings,
                    &self.peers,
                    media_worker(&mut self.media_workers, &name)?,
                    &mut text,
                    services,
                    StationAudio::Modular(plan),
                )?;
                let previous = previous_lease(&name)
                    .map(|lease| lease.lock().unwrap_or_else(|e| e.into_inner()));
                if let Some(worker) = previous.as_ref().and_then(|lease| lease.worker.as_ref()) {
                    let target = settings.squelch_delay_ms.max(worker.minimum_delay_ms());
                    if worker.squelch_delay_ms() != target {
                        candidate.control.local_update = Some(
                            LocalRingUpdate::prepare(target)
                                .map_err(|_| RuntimeError::Preparation)?,
                        );
                        candidate.control.awaiting_local = true;
                    }
                }
                Ok((candidate, basis.replaced.contains(&name)))
            },
            |port, _| {
                // The aggregate requests a new device after it evicts stopped incumbents.
                // Its new NodeHost must register its own callback pair on this lease.
                let lease = Arc::new(Mutex::new(Lease {
                    binding: Default::default(),
                    quiesced: false,
                    quiescing: false,
                    owners: None,
                    worker: None,
                    epoch,
                    status: crate::worker::RadioStatus::with_epoch(epoch),
                }));
                fresh_leases.insert(port, Arc::clone(&lease));
                Ok(Box::new(Device {
                    name: port.to_string(),
                    lease,
                    services,
                }))
            },
            clock,
        )?;
        let mut bindings = BTreeMap::new();
        for (id, plan) in &plans.0 {
            let lease = fresh_leases
                .remove(id)
                .or_else(|| previous_lease(&id.to_string()).cloned())
                .ok_or(RuntimeError::Device)?;
            let binding = services
                .prepare_station_binding(&plan.station, ticket.serial)
                .map_err(|_| RuntimeError::Preparation)?;
            bindings.insert(*id, (lease, Arc::new(binding)));
        }
        owner.core = Some(CoreCandidate {
            core,
            text,
            bindings,
        });
        Ok(())
    }

    pub(crate) fn bind_core_credentials(
        &mut self,
        ticket: &RadioTicket,
        domain: &RuntimeConfiguration,
        credentials: AccountCredentials,
    ) -> Result<Arc<AccountCredentials>, RuntimeError> {
        self.staged_plans(ticket)?;
        let transition = &mut self
            .radio_preparation
            .core
            .as_mut()
            .ok_or(RuntimeError::Rejected)?
            .core;
        for (port, profile) in domain.protocols.ports().filter(|(_, p)| p.metadata.enabled) {
            let scope = ResourceScope::Port(port);
            let binding = domain
                .registry
                .binding(scope, ResourceKind::Command)
                .or_else(|| {
                    if profile.metadata.inherit_from
                        != rpt_advanced_core::config::resources::ScopeParent::Site
                    {
                        return None;
                    }
                    domain.registry.binding(
                        ResourceScope::Site(domain.registry.site_id()),
                        ResourceKind::Command,
                    )
                });
            let phc = |slot| {
                binding
                    .and_then(|b| credentials.get(b.id, slot))
                    .map(|bytes| {
                        std::str::from_utf8(bytes.expose_bytes())
                            .map_err(|_| RuntimeError::Preparation)
                    })
                    .transpose()
            };
            transition.bind_admin(
                port,
                phc(CredentialSlot::Unlock)?,
                phc(CredentialSlot::Lock)?,
            )?;
        }
        // A fully authenticated final graph may retain the existing zeroizing allocation
        // only when the bound record versions and selected owner set are identical.
        Ok(match self.radio_preparation.credentials.as_ref() {
            Some(active) if active.matches_selection(&credentials) => Arc::clone(active),
            _ => Arc::new(credentials),
        })
    }

    pub(crate) fn begin_core_adoption(
        &mut self,
        ticket: RadioTicket,
        revision: String,
        credentials: Arc<AccountCredentials>,
        inputs: Arc<crate::management::ProfileInputs>,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        self.staged_plans(&ticket)?;
        let CoreCandidate {
            core,
            text,
            bindings,
        } = self
            .radio_preparation
            .core
            .take()
            .ok_or(RuntimeError::Rejected)?;
        if self.runtime.revision() != ticket.base_revision
            || self.radio_preparation.adopting.is_some()
        {
            return Err(RuntimeError::Rejected);
        }
        for (port, (lease, plan)) in &bindings {
            lease.lock().unwrap_or_else(|e| e.into_inner()).binding.next = Some(Arc::clone(plan));
            if !self.leases.iter().any(|(_, l)| Arc::ptr_eq(l, lease)) {
                self.leases.push((port.to_string(), Arc::clone(lease)));
            }
        }
        let (_, plans) = self
            .radio_preparation
            .staged
            .take()
            .expect("retained ticket");
        self.radio_preparation.adopting = Some(Adopting {
            revision,
            plans,
            credentials,
            inputs,
        });
        self.radio_preparation.rollback = None;
        self.reload_failure = None;
        self.reload_text = Some(text);
        if let Err(error) = self.runtime.adopt_configuration(core, clock) {
            self.reload_text = None;
            self.core_adoption_error(&error);
            self.prune_leases();
            return Err(error);
        }
        self.poll_reload(clock)
    }

    pub(super) fn core_adoption_error(&mut self, error: &RuntimeError) {
        if let Some(Adopting { revision, .. }) = &self.radio_preparation.adopting {
            if *error == RuntimeError::Device
                && !self.runtime.reload_pending()
                && self.runtime.revision() == self.activated_revision
            {
                self.radio_preparation.rollback = Some(revision.clone());
            }
            self.discard_physical_plans();
        }
    }

    pub(crate) fn modular_revision_matches(&self, revision: &str) -> bool {
        self.radio_preparation.revision.as_deref() == Some(revision)
            && self.runtime.revision() == self.activated_revision
            && !self.runtime.reload_pending()
    }

    pub(crate) fn modular_rollback_confirmed(&self, revision: &str) -> bool {
        self.radio_preparation.rollback.as_deref() == Some(revision)
            && !self.runtime.reload_pending()
    }

    pub(crate) fn observed_configuration(
        &mut self,
        revision: &str,
        inputs: &Arc<crate::management::ProfileInputs>,
    ) -> Result<crate::management::Observed, RuntimeError> {
        if !self
            .radio_preparation
            .inputs
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, inputs))
            || self.radio_preparation.revision.as_deref() != Some(revision)
            || self.runtime.reload_pending()
            || self.activated_revision != self.runtime.revision()
        {
            return Err(RuntimeError::Rejected);
        }
        let domain = self
            .runtime
            .modular_configuration()
            .cloned()
            .ok_or(RuntimeError::Rejected)?;
        let mut observed = crate::management::Observed {
            active_revision: Some(revision.to_owned()),
            ..Default::default()
        };
        for (_, port) in domain.registry.port_keys() {
            if let Some(work) = self
                .runtime
                .node(&port.to_string())
                .and_then(|node| node.command_work())
            {
                observed.owners.insert(port, work.id());
            }
        }
        Ok(observed)
    }

    pub(super) fn discard_physical_plans(&mut self) {
        self.radio_preparation.adopting = None;
        for (_, lease) in &self.leases {
            lease.lock().unwrap_or_else(|e| e.into_inner()).binding.next = None;
        }
    }

    pub(super) fn publish_physical_plans(&mut self) {
        if let Some(Adopting {
            revision,
            plans,
            credentials,
            inputs,
        }) = self.radio_preparation.adopting.take()
        {
            for (_, lease) in &self.leases {
                let mut lease = lease.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(plan) = lease.binding.next.take() {
                    lease.binding.active = Some(plan);
                }
            }
            self.radio_preparation.inputs = Some(inputs);
            self.radio_preparation.active = plans;
            self.radio_preparation.credentials = Some(credentials);
            self.radio_preparation.revision = Some(revision);
        }
    }
}
