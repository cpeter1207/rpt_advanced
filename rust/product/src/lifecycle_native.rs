//! Checked modular transitions use the existing serialized station and physical owners.
use super::Engine;
use crate::{
    host::{RadioTicket, SiteBasis},
    site::{
        acquisition::{Acquisition, PreparedAcquisition},
        backends::SiteBackends,
        radio::RadioSite,
    },
};
use rpt_advanced_core::{
    config::resources::{candidate::ResourceScope, generation::RuntimeConfiguration},
    runtime::RuntimeError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

/// Dormant candidates for the existing equipment barrier. The writer retains these
/// workers off control until the real host accepts them or they are discarded.
pub(crate) struct PreparedSite {
    revision: u64,
    replaced: BTreeSet<String>,
    acquisition: PreparedAcquisition,
    radio: RadioSite,
}

impl Engine {
    pub(crate) fn prepare_modular_sites(
        self: &Arc<Self>,
        configuration: &RuntimeConfiguration,
        ticket: &RadioTicket,
    ) -> Result<PreparedSite, RuntimeError> {
        let ticket = ticket.clone();
        let SiteBasis {
            revision,
            known,
            replaced,
        } = self.run(move |host| host.modular_site_basis(&ticket))?;
        // Each physical definition is bound only by the scoped acquisition plan.
        // Full inherited equipment views are for evaluating the invoking scope's policy.
        let settings: BTreeMap<_, _> = configuration
            .acquisition
            .iter()
            .map(|(scope, settings)| {
                let (ResourceScope::Site(id) | ResourceScope::Port(id)) = scope;
                (id.to_string(), settings.clone())
            })
            .collect();
        let providers = Arc::new(SiteBackends::load_scopes(settings.values())?);
        let acquisition = Acquisition::prepare_reload(&settings, providers, self.origin, known)
            .map_err(|_| RuntimeError::Preparation)?;
        let radio = RadioSite::prepare(&settings).map_err(|_| RuntimeError::Preparation)?;
        Ok(PreparedSite {
            revision,
            replaced,
            acquisition,
            radio,
        })
    }

    pub(crate) fn begin_modular_sites(
        self: &Arc<Self>,
        plan: PreparedSite,
    ) -> Result<(), RuntimeError> {
        let PreparedSite {
            revision,
            replaced,
            acquisition,
            radio,
        } = plan;
        let held = Arc::new(Mutex::new((Some(acquisition), Some(radio))));
        let candidate = Arc::clone(&held);
        // Rejected late work can release its extra reference on the executor.
        // The writer still owns the original and joins unused dormant workers here.
        self.run(move |host| {
            let mut pair = candidate.lock().unwrap_or_else(|error| error.into_inner());
            let (acquisition, radio) = &mut *pair;
            host.begin_site_reload(revision, acquisition, radio, &replaced)
        })
    }
}
