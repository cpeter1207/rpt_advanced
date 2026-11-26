//! Management handoff into the serialized product control and physical owners.
use super::{
    Adoption, Diagnostic, Effect, Fault, Observed, OperationOwner, PreparedConfiguration,
    ProfileInputs, Runtime,
};
use crate::{
    configuration_private::{Credentials, RetainedCredential},
    host::RadioTicket,
    lifecycle::Engine,
};
use rpt_advanced_core::{
    command_language::{Invocation, PreparedAction},
    config::resources::{
        candidate::{ResourceBinding, ResourceId, ResourceKind},
        generation::RuntimeConfiguration,
        profiles::credentials::CredentialSlot,
    },
    runtime::RuntimeError,
};
use rptadv_peer_support::sealed_records::SecretBytes;
use std::{collections::BTreeMap, sync::Arc};

/// The private inputs actually selected by the typed site and attached port owners.
#[derive(Default)]
pub(crate) struct AccountCredentials(BTreeMap<(ResourceId, CredentialSlot), RetainedCredential>);

impl AccountCredentials {
    #[cfg(test)]
    pub(crate) fn fixture(rows: &[(ResourceId, CredentialSlot, ResourceId, &[u8])]) -> Self {
        Self(
            rows.iter()
                .map(|&(owner, slot, version, bytes)| {
                    (
                        (owner, slot),
                        RetainedCredential::fixture(owner, version, bytes),
                    )
                })
                .collect(),
        )
    }

    fn bind(
        configuration: &RuntimeConfiguration,
        final_graph: &Credentials,
    ) -> Result<Self, Fault> {
        let mut retained = BTreeMap::new();
        let mut select = |binding: ResourceBinding, slot: CredentialSlot| {
            if let Some(value) = final_graph.retain(binding.id, slot) {
                retained.insert((binding.id, slot), value);
                true
            } else {
                false
            }
        };
        // A site protocol default cannot open a network participant. A real registered
        // instance selects its own final account, even when the store inherits its bytes.
        for (_, port) in configuration
            .protocols
            .ports()
            .filter(|(_, p)| p.metadata.enabled)
        {
            if let Some(account) = &port.allstarlink {
                select(account.binding, CredentialSlot::AllStarLink);
            }
            if let Some(account) = &port.echolink
                && account.settings.enabled
            {
                select(account.binding, CredentialSlot::Directory);
                if let Some(proxy) = port.echolink_proxy {
                    select(proxy, CredentialSlot::Proxy);
                }
            }
            if let Some(account) = &port.phone_patch {
                select(account.binding, CredentialSlot::Phone);
            }
        }
        for (owner, scope) in &configuration.scopes {
            if let Some(command) = configuration
                .registry
                .binding(*owner, ResourceKind::Command)
            {
                select(command, CredentialSlot::Unlock);
                select(command, CredentialSlot::Lock);
            }
            for mailbox in scope.mailboxes.effective.values() {
                if !select(mailbox.binding, CredentialSlot::Pin) {
                    return Err(Fault::Unavailable);
                }
            }
        }
        Ok(Self(retained))
    }

    pub(crate) fn get(&self, owner: ResourceId, slot: CredentialSlot) -> Option<&SecretBytes> {
        self.0
            .get(&(owner, slot))
            .map(|credential| credential.bytes.as_ref())
    }

    pub(crate) fn matches_selection(&self, next: &Self) -> bool {
        self.0.len() == next.0.len()
            && self
                .0
                .keys()
                .all(|&(owner, slot)| self.unchanged(next, owner, slot))
    }

    pub(crate) fn version(&self, owner: ResourceId, slot: CredentialSlot) -> Option<ResourceId> {
        self.0.get(&(owner, slot)).map(RetainedCredential::version)
    }

    pub(crate) fn unchanged(&self, next: &Self, owner: ResourceId, slot: CredentialSlot) -> bool {
        let key = (owner, slot);
        match (self.0.get(&key), next.0.get(&key)) {
            (Some(old), Some(new)) => old.same_version(new),
            (None, None) => true,
            _ => false,
        }
    }
}

/// Bounded candidate whose immutable provider boxes stay on serialized control.
pub(crate) struct Prepared {
    pub(crate) revision: String,
    pub(crate) configuration: Arc<RuntimeConfiguration>,
    pub(crate) inputs: Arc<ProfileInputs>,
    pub(crate) fault_epoch: u64,
    pub(crate) radio: RadioTicket,
    pub(crate) site: crate::lifecycle::PreparedSite,
    pub(crate) credentials: Option<Arc<AccountCredentials>>,
}

/// The existing process control owner supplies actual adoption, inhibition and receipts.
pub(crate) struct NativeRuntime {
    engine: Arc<Engine>,
    #[cfg(test)]
    pub(crate) final_inspection_fixture: std::sync::Mutex<Option<fn()>>,
}
impl NativeRuntime {
    pub(crate) fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            #[cfg(test)]
            final_inspection_fixture: Default::default(),
        }
    }
}
impl Runtime for NativeRuntime {
    type Prepared = Prepared;
    #[cfg(test)]
    fn before_final_inspection(&self) {
        if let Some(checkpoint) = self.final_inspection_fixture.lock().unwrap().take() {
            checkpoint();
        }
    }

    fn prepare(
        &self,
        candidate: &PreparedConfiguration,
        revision: &str,
    ) -> Result<Prepared, Fault> {
        let configuration = Arc::clone(candidate.runtime());
        let epoch = self.engine.fault_epoch();
        let radio = self
            .engine
            .prepare_radio_plans(Arc::clone(&configuration))
            .map_err(fault)?;
        let token = radio.clone();
        let domain = configuration.clone();
        let clock = self.engine.clock();
        self.engine
            .run(move |host| host.prepare_core(&token, domain, clock))
            .map_err(fault)?;
        let site = self
            .engine
            .prepare_modular_sites(&configuration, &radio)
            .map_err(fault)?;
        Ok(Prepared {
            revision: revision.to_owned(),
            configuration,
            fault_epoch: epoch,
            inputs: Arc::clone(candidate.inputs()),
            radio,
            site,
            credentials: None,
        })
    }

    fn prepare_credentials(
        &self,
        prepared: &mut Prepared,
        final_graph: &Credentials,
    ) -> Result<(), Fault> {
        let graph = AccountCredentials::bind(&prepared.configuration, final_graph)?;
        let ticket = prepared.radio.clone();
        let configuration = Arc::clone(&prepared.configuration);
        prepared.credentials = Some(
            self.engine
                .run(move |host| host.bind_core_credentials(&ticket, &configuration, graph))
                .map_err(fault)?,
        );
        Ok(())
    }

    fn permits_save(&self, revision: &str) -> bool {
        self.engine.permits_modular_save(revision)
    }

    fn adopt(&self, prepared: Prepared) -> Adoption {
        // Only the physical/control owner can report a confirmed rollback.
        // A provider error by itself cannot establish that the old generation resumed.
        self.engine.adopt_modular(prepared)
    }

    fn inhibit_uncertain(&self, _revision: Option<&str>, _cause: Fault) {
        self.engine.inhibit_modular();
    }

    fn diagnostic(&self, event: Diagnostic) {
        self.engine.management_diagnostic(event);
    }

    fn observed(&self) -> Observed {
        self.engine.modular_observed()
    }

    fn operation_owner(
        &self,
        target: ResourceId,
        revision: &str,
        action: &PreparedAction,
    ) -> Result<OperationOwner, Fault> {
        self.engine.command_owner(target, revision, action)
    }

    fn maintenance(&self, grant: Arc<super::maintenance::Grant>, intent: Invocation) -> Effect {
        self.engine.dispatch_maintenance(grant, intent)
    }
    fn clear_maintenance(&self, grant: Arc<super::maintenance::Grant>) -> Result<(), Fault> {
        self.engine
            .run(move |host| {
                host.clear_maintenance(&grant)
                    .map_err(|_| RuntimeError::Unavailable)
            })
            .map_err(fault)
    }
    fn dispatch(&self, revision: &str, invocation: Invocation) -> Effect {
        self.engine.dispatch_registered(revision, invocation)
    }
}

fn fault(error: RuntimeError) -> Fault {
    match error {
        RuntimeError::Busy => Fault::Conflict,
        RuntimeError::Config(_) => Fault::Invalid,
        _ => Fault::Unavailable,
    }
}
