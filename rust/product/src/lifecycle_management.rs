//! A confirmed selector is adopted on the existing lifecycle and device owners.
use super::*;
use crate::management::{
    Adoption, AdoptionOutcome, Diagnostic, Effect, Fault, Observed, OperationOwner,
    native::Prepared,
};
use rpt_advanced_core::{
    command_language::{Invocation, PreparedAction},
    config::resources::candidate::{ResourceId, ResourceScope},
};
use std::sync::OnceLock;

/// The worker updates observations only after the serialized owner and its site barrier finish.
/// The independent atomic gate reaches TX even if an executor submission is rejected.
#[derive(Default)]
pub(super) struct ManagementState {
    gate: OnceLock<Arc<AtomicBool>>,
    state: Mutex<(u64, Observed)>,
    pub(super) reload: Mutex<Option<crate::management::HostReload>>,
}
impl ManagementState {
    pub(super) fn install_gate(&self, gate: Arc<AtomicBool>) {
        assert!(self.gate.set(gate).is_ok());
    }
}
impl Engine {
    pub(crate) fn fault_epoch(&self) -> u64 {
        self.management_state
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
    }

    pub(crate) fn permits_modular_save(&self, revision: &str) -> bool {
        let state = self
            .management_state
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.accepting.load(Ordering::Acquire)
            && self
                .management_state
                .gate
                .get()
                .is_some_and(|gate| !gate.load(Ordering::Acquire))
            && state.1.active_revision.as_deref() == Some(revision)
    }

    pub(crate) fn inhibit_modular(&self) {
        let mut state = self
            .management_state
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.0 = state.0.saturating_add(1);
        if let Some(gate) = self.management_state.gate.get() {
            gate.store(true, Ordering::Release);
        }
        // Storage trust is independent of the last observation confirmed by the physical owner.
    }
    pub(crate) fn modular_observed(&self) -> Observed {
        self.management_state
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone()
    }
    pub(crate) fn management_diagnostic(&self, event: Diagnostic) {
        self.services
            .scheduler_notice(&format!("management {event:?}"));
    }

    pub(crate) fn adopt_modular(self: &Arc<Self>, prepared: Prepared) -> Adoption {
        let Ok(call) = self.enter() else {
            return Adoption::observed(AdoptionOutcome::RecoveryRequired);
        };
        let (pending, confirmation) = Adoption::pending();
        let engine = Arc::clone(self);
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        match std::thread::Builder::new()
            .name("rpt-config-adoption".into())
            .spawn(move || {
                let _call = call;
                let outcome = engine.apply_modular(prepared).unwrap_or_else(|_| {
                    engine
                        .management_state
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .1 = Observed::default();
                    AdoptionOutcome::RecoveryRequired
                });
                let _ = confirmation.send(outcome);
            }) {
            Ok(job) => {
                jobs.push(job);
                pending
            }
            Err(_) => Adoption::observed(AdoptionOutcome::RecoveryRequired),
        }
    }

    fn apply_modular(
        self: &Arc<Self>,
        prepared: Prepared,
    ) -> Result<AdoptionOutcome, RuntimeError> {
        let Prepared {
            revision,
            configuration,
            inputs,
            radio,
            site,
            credentials,
            fault_epoch,
        } = prepared;
        let credentials = credentials.ok_or(RuntimeError::Rejected)?;
        self.begin_modular_sites(site)?;
        let result = self.wait_sites(false).and_then(|()| {
            self.reloading
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| RuntimeError::Busy)?;
            self.revision.fetch_add(1, Ordering::AcqRel);
            self.lost_digits.store(true, Ordering::Release);
            let clock = self.clock();
            let name = revision.clone();
            let installed = Arc::clone(&inputs);
            let mut ready = self.run_inner(true, move |host| {
                host.begin_core_adoption(radio, name, credentials, installed, clock)
            })?;
            while !ready {
                if !self.accepting.load(Ordering::Acquire) {
                    return Err(RuntimeError::Rejected);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
                let clock = self.clock();
                ready = self.run_inner(true, move |host| host.poll_reload(clock))?;
            }
            Ok(())
        });
        let physical_success = result.is_ok();
        // Abort uncommitted site changes or collect the committed old workers outside control.
        let terminal = self.finish_reload(result);
        if terminal.is_err() {
            let attempted = revision.clone();
            let restored = self.run(move |host| Ok(host.modular_rollback_confirmed(&attempted)))?;
            if !physical_success && restored && terminal == Err(RuntimeError::Device) {
                return Ok(AdoptionOutcome::RolledBack);
            }
            // An inconclusive physical transition invalidates our live observation.
            return Err(RuntimeError::Restore);
        }
        let engine = Arc::clone(self);
        self.run(move |host| {
            if !host
                .runtime
                .modular_configuration()
                .is_some_and(|active| Arc::ptr_eq(active, &configuration))
            {
                return Err(RuntimeError::Rejected);
            }
            let observed = host.observed_configuration(&revision, &inputs)?;
            let mut state = engine
                .management_state
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.1 = observed;
            if state.0 != fault_epoch
                || state.0 == u64::MAX
                || !engine.accepting.load(Ordering::Acquire)
            {
                return Ok(AdoptionOutcome::RecoveryRequired);
            }
            let gate = engine
                .management_state
                .gate
                .get()
                .ok_or(RuntimeError::Rejected)?;
            // All physical and site handoffs succeeded. Release only this generation's
            // affected stations on the same serialized turn as native confirmation.
            host.runtime.confirm_station_replacements(&configuration)?;
            gate.store(false, Ordering::Release);
            Ok(AdoptionOutcome::Confirmed)
        })
    }

    pub(crate) fn command_owner(
        self: &Arc<Self>,
        target: ResourceId,
        revision: &str,
        _action: &PreparedAction,
    ) -> Result<OperationOwner, Fault> {
        let gate = self
            .management_state
            .gate
            .get()
            .cloned()
            .ok_or(Fault::Unavailable)?;
        let expected = revision.to_owned();
        self.run(move |host| {
            if gate.load(Ordering::Acquire) || !host.modular_revision_matches(&expected) {
                return Err(RuntimeError::Rejected);
            }
            let generation = host
                .runtime
                .node(&target.to_string())
                .and_then(|node| node.command_work())
                .ok_or(RuntimeError::MissingNode)?;
            Ok(OperationOwner {
                scope: ResourceScope::Port(target),
                generation,
                requires_grant: false,
            })
        })
        .map_err(|_| Fault::Conflict)
    }

    pub(crate) fn dispatch_registered(
        self: &Arc<Self>,
        expected: &str,
        invocation: Invocation,
    ) -> Effect {
        let Some(gate) = self.management_state.gate.get().cloned() else {
            return Effect::Rejected;
        };
        let epoch = self.revision.load(Ordering::Acquire);
        let expected = expected.to_owned();
        let clock = self.clock();
        let result = self.run(move |host| {
            if gate.load(Ordering::Acquire) || !host.modular_revision_matches(&expected) {
                return Err(RuntimeError::Rejected);
            }
            host.runtime.management_invocation(invocation, clock)
        });
        match result {
            Ok((local, effect)) => {
                if self.effect(local, effect, epoch).is_ok() {
                    Effect::Admitted
                } else {
                    Effect::Indeterminate
                }
            }
            Err(_) => Effect::Rejected,
        }
    }
}
