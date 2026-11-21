//! Immutable provider preparations retained by the serialized product lifecycle.
use super::{Host, SiteBasis};
use crate::configuration_radio::{PreparedRole, PreparedStation, RadioPreparationApi};
use rpt_advanced_core::{
    config::resources::{candidate::ResourceId, generation::RuntimeConfiguration},
    runtime::RuntimeError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

#[derive(Default)]
pub(super) struct LeaseBindings {
    pub(super) active: Option<Arc<crate::services::StationBinding>>,
    pub(super) next: Option<Arc<crate::services::StationBinding>>,
}

/// A reservation of one candidate slot. Dropping every copy abandons the reservation.
#[derive(Clone)]
pub(crate) struct RadioTicket {
    key: Arc<()>,
    pub(super) base_revision: u64,
    pub(super) serial: std::num::NonZeroU64,
}

impl RadioTicket {
    pub(super) fn matches(&self, key: &Weak<()>) -> bool {
        key.ptr_eq(&Arc::downgrade(&self.key))
    }
}

pub(crate) struct RadioPlan {
    pub(crate) station: PreparedStation,
    pub(crate) incoming: PreparedRole,
    pub(super) process_incoming: bool,
}

/// Only immutable, uniquely owned SDK station and link-role preparations can enter this map.
#[derive(Default)]
pub(super) struct Plans(pub(super) BTreeMap<ResourceId, RadioPlan>);

// SAFETY: the preparation1 contract requires serialized control access and has no
// creator-thread affinity. Each box owns its copied settings and retains its DSO.
// Host stages, visits and destroys these boxes only on its logical control executor,
// including when Engine::stop removes Host before draining. No native station, live
// graph, callback context or update token is stored here. An Engine result can carry
// the reservation identity, never a prepared station or a reference to this map.
unsafe impl Send for Plans {}

#[derive(Default)]
pub(super) struct RadioPreparation {
    api: Option<RadioPreparationApi>,
    pub(super) staged: Option<(Weak<()>, Plans)>,
    pub(super) active: Plans,
    pub(super) core: Option<super::modular::CoreCandidate>,
    pub(super) adopting: Option<super::modular::Adopting>,
    pub(super) inputs: Option<Arc<crate::management::ProfileInputs>>,
    pub(super) credentials: Option<Arc<crate::management::native::AccountCredentials>>,
    pub(super) revision: Option<String>,
    pub(super) rollback: Option<String>,
}

impl RadioPreparation {
    pub(super) fn active_plan(&self, local: &str) -> Result<&RadioPlan, RuntimeError> {
        let port = local.parse().map_err(|_| RuntimeError::MissingNode)?;
        self.active.0.get(&port).ok_or(RuntimeError::MissingNode)
    }
}

impl Host {
    pub(crate) fn stage_radio_plans(
        &mut self,
        configuration: &RuntimeConfiguration,
    ) -> Result<RadioTicket, RuntimeError> {
        let owner = &mut self.radio_preparation;
        if owner
            .staged
            .as_ref()
            .is_some_and(|(key, _)| key.upgrade().is_some())
        {
            return Err(RuntimeError::Busy);
        }
        // A failed publication may have dropped its ticket off control. Release the
        // abandoned configuration here, never from the writer's destructor.
        owner.staged = None;
        owner.core = None;
        let mut plans = Plans::default();
        for (_, id) in configuration.registry.port_keys() {
            let profile = configuration
                .radio
                .port(id)
                .ok_or(RuntimeError::MissingNode)?;
            if !profile.metadata.enabled {
                continue;
            }
            if owner.api.is_none() {
                owner.api =
                    Some(RadioPreparationApi::open().map_err(|_| RuntimeError::Preparation)?);
            }
            let api = owner.api.as_ref().expect("selected station provider");
            // The SDK requires its numeric family default for its remote capability.
            // Modular DTMF admission uses the port's independently resolved literal mappings.
            let station = api
                .prepare_station(profile, "6")
                .map_err(|_| RuntimeError::Preparation)?;
            let incoming = api
                .prepare_link(profile.processing.link.as_ref())
                .map_err(|_| RuntimeError::Preparation)?;
            plans.0.insert(
                id,
                RadioPlan {
                    station,
                    incoming,
                    process_incoming: profile.processing.link.is_some(),
                },
            );
        }
        static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT_SERIAL
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_add(1),
            )
            .map_err(|_| RuntimeError::Preparation)?;
        let serial = std::num::NonZeroU64::new(serial).ok_or(RuntimeError::Preparation)?;
        let key = Arc::new(());
        owner.staged = Some((Arc::downgrade(&key), plans));
        Ok(RadioTicket {
            key,
            serial,
            base_revision: self.runtime.revision(),
        })
    }

    #[cfg(test)]
    pub(crate) fn staged_radio(
        &self,
        ticket: &RadioTicket,
        port: ResourceId,
    ) -> Result<&RadioPlan, RuntimeError> {
        self.staged_plans(ticket)?
            .0
            .get(&port)
            .ok_or(RuntimeError::MissingNode)
    }

    pub(super) fn staged_plans(&self, ticket: &RadioTicket) -> Result<&Plans, RuntimeError> {
        let (key, plans) = self
            .radio_preparation
            .staged
            .as_ref()
            .ok_or(RuntimeError::Rejected)?;
        if !key.ptr_eq(&Arc::downgrade(&ticket.key)) {
            return Err(RuntimeError::Rejected);
        }
        Ok(plans)
    }

    pub(crate) fn modular_site_basis(
        &self,
        ticket: &RadioTicket,
    ) -> Result<SiteBasis, RuntimeError> {
        if self.runtime.revision() != ticket.base_revision {
            return Err(RuntimeError::Rejected);
        }
        let proposed = self.staged_plans(ticket)?;
        let mut replaced = BTreeSet::new();
        for (port, old) in &self.radio_preparation.active.0 {
            let unchanged = match proposed.0.get(port) {
                Some(next) => old
                    .station
                    .equivalent(&next.station)
                    .map_err(|_| RuntimeError::Preparation)?,
                None => false,
            };
            if !unchanged {
                replaced.insert(port.to_string());
            }
        }
        Ok(SiteBasis {
            revision: ticket.base_revision,
            known: self.site_acquisition.keys(),
            replaced,
        })
    }
}

impl crate::lifecycle::Engine {
    pub(crate) fn prepare_radio_plans(
        self: &Arc<Self>,
        configuration: Arc<RuntimeConfiguration>,
    ) -> Result<RadioTicket, RuntimeError> {
        self.run(move |host| host.stage_radio_plans(&configuration))
    }
}
