//! A confirmed selector is adopted on the existing lifecycle and device owners.
use super::*;
use crate::management::{
    Adoption, AdoptionOutcome, Diagnostic, Effect, Fault, Observed, OperationOwner,
    native::Prepared,
    resources::{ResourceSampler, Resources, utc_timestamp},
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
    pub(super) status_edges: OnceLock<Arc<crate::management::status::edges::EdgeQueue>>,
    state: Mutex<(u64, Observed)>,
    observing: AtomicBool,
    pub(super) reload: Mutex<Option<crate::management::HostReload>>,
}
impl ManagementState {
    pub(super) fn install_gate(&self, gate: Arc<AtomicBool>) {
        assert!(self.gate.set(gate).is_ok());
    }

    #[cfg(test)]
    pub(super) fn fixture_observed(&self, observed: Observed) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1 = observed;
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
        let mut observed = self
            .management_state
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone();
        observed.status_edges = self.management_state.status_edges.get().cloned();
        observed.clock = Some(self.clock());
        observed.status_ready = self.accepting.load(Ordering::Acquire)
            && !self.reloading.load(Ordering::Acquire)
            && self
                .management_state
                .gate
                .get()
                .is_some_and(|gate| !gate.load(Ordering::Acquire));
        if !observed.status_ready {
            observed.domain = Arc::new(crate::management::live::Domain {
                site_resources: observed.domain.site_resources.clone(),
                ..Default::default()
            });
        }
        observed
    }
    pub(super) fn start_domain_observer(self: &Arc<Self>) {
        if self.management_state.observing.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = Arc::downgrade(self);
        let job = std::thread::Builder::new()
            .name("rpt-status".into())
            .spawn(move || {
                let mut sampler = ResourceSampler::default();
                let mut resources = None;
                let mut registrations_at = None;
                while let Some(engine) = owner.upgrade() {
                    let started = Instant::now();
                    if !engine.accepting.load(Ordering::Acquire) {
                        break;
                    }
                    let sample = sampler.poll(engine.elapsed_ms(), || {
                        let clock = engine.clock();
                        utc_timestamp(clock.wall_seconds, clock.wall_subsecond_ms)
                    });
                    if let Some(sample) = sample {
                        let sample = Arc::new(sample);
                        resources = Some(Arc::clone(&sample));
                        let mut state = engine
                            .management_state
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if state.1.active_revision.is_none()
                            || engine
                                .management_state
                                .gate
                                .get()
                                .is_none_or(|gate| gate.load(Ordering::Acquire))
                        {
                            state.1.domain = Arc::new(crate::management::live::Domain {
                                site_resources: Some(sample),
                                ..Default::default()
                            });
                        }
                    }
                    let now = engine.elapsed_ms();
                    let refresh_registrations =
                        registrations_at.is_none_or(|last| now.saturating_sub(last) >= 250);
                    if refresh_registrations {
                        registrations_at = Some(now);
                    }
                    engine.refresh_domain(resources.clone(), refresh_registrations);
                    drop(engine);
                    std::thread::sleep(
                        std::time::Duration::from_millis(50).saturating_sub(started.elapsed()),
                    );
                }
            });
        match job {
            Ok(job) => self
                .jobs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(job),
            Err(_) => self
                .management_state
                .observing
                .store(false, Ordering::Release),
        }
    }

    fn refresh_domain(
        self: &Arc<Self>,
        resources: Option<Arc<Resources>>,
        refresh_registrations: bool,
    ) {
        let Ok(_call) = self.enter() else { return };
        let generation = self.revision.load(Ordering::Acquire);
        let (fault_epoch, revision, owners) = {
            let state = self
                .management_state
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(revision) = &state.1.active_revision else {
                return;
            };
            if self
                .management_state
                .gate
                .get()
                .is_none_or(|g| g.load(Ordering::Acquire))
            {
                return;
            }
            (state.0, revision.clone(), state.1.owners.clone())
        };
        let Ok(selection) = self.run(|host| Ok(host.status_selection())) else {
            return;
        };
        // Provider locks and directory observations may wait. Only this observer is occupied.
        let registrations = refresh_registrations.then(|| {
            selection
                .iter()
                .filter_map(|(&(port, account), query)| {
                    query
                        .observe(self.services, port, account)
                        .map(|result| ((port, account), result))
                })
                .collect()
        });
        let engine = Arc::clone(self);
        let _ = self.run(move |host| {
            if host.status_selection() != selection {
                return Ok(None);
            }
            let mut domain = host.live_domain(&owners, engine.elapsed_ms())?;
            domain.site_resources = resources;
            let mut state = engine
                .management_state
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if engine.current(generation)
                && state.0 == fault_epoch
                && state.1.active_revision.as_ref() == Some(&revision)
                && state.1.owners == owners
                && engine
                    .management_state
                    .gate
                    .get()
                    .is_some_and(|g| !g.load(Ordering::Acquire))
            {
                domain.registrations =
                    registrations.unwrap_or_else(|| state.1.domain.registrations.clone());
                return Ok(Some(std::mem::replace(
                    &mut state.1.domain,
                    Arc::new(domain),
                )));
            }
            Ok(None)
        });
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
                if matches!(outcome, AdoptionOutcome::Confirmed) {
                    engine.start_domain_observer();
                }
                let _ = confirmation.send(outcome);
            }) {
            Ok(job) => {
                jobs.push(job);
                pending
            }
            Err(_) => Adoption::observed(AdoptionOutcome::RecoveryRequired),
        }
    }

    fn poll_private_accounts(
        self: &Arc<Self>,
        attempt: std::num::NonZeroU64,
        reverse: bool,
    ) -> Result<(), RuntimeError> {
        loop {
            if !self.accepting.load(Ordering::Acquire) {
                return Err(RuntimeError::Rejected);
            }
            if self.run_inner(true, move |host| {
                if reverse {
                    host.restore_private_accounts(attempt)
                } else {
                    host.apply_private_accounts(attempt)
                }
            })? {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn poll_asl(
        self: &Arc<Self>,
        attempt: std::num::NonZeroU64,
        phase: u32,
    ) -> Result<(), RuntimeError> {
        loop {
            if !self.accepting.load(Ordering::Acquire) {
                return Err(RuntimeError::Rejected);
            }
            if self.run_inner(true, move |host| host.step_network_accounts(attempt, phase))? {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn restore_accounts(
        self: &Arc<Self>,
        attempt: std::num::NonZeroU64,
    ) -> Result<(), RuntimeError> {
        self.poll_asl(attempt, 3)?;
        self.poll_private_accounts(attempt, true)?;
        self.run_inner(true, move |host| {
            host.finish_private_accounts(attempt, |_, _| true)
        })?;
        self.poll_asl(attempt, 4)?;
        self.run_inner(true, move |host| host.finish_asl_selection(attempt))
    }

    pub(super) fn apply_modular(
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
        let attempt = radio.attempt();
        // The strong reservation spans native confirmation. A concurrent pump may
        // reap a genuinely abandoned ticket but cannot retire this one.
        let retained = radio.clone();
        let basis = radio.clone();
        self.run(move |host| host.verify_private_basis(&basis))?;
        let mut entered_core = false;
        let result = self
            .poll_private_accounts(attempt, false)
            .and_then(|()| self.poll_asl(attempt, 1))
            .and_then(|()| self.begin_modular_sites(site))
            .and_then(|()| self.wait_sites(false))
            .and_then(|()| {
                self.reloading
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .map_err(|_| RuntimeError::Busy)?;
                self.revision.fetch_add(1, Ordering::AcqRel);
                self.lost_digits.store(true, Ordering::Release);
                let clock = self.clock();
                let name = revision.clone();
                let installed = Arc::clone(&inputs);
                entered_core = true;
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
        // Abort uncommitted site changes or collect the committed old workers outside control.
        // A failed cleanup leaves the live selection unknown even if the radio rolled back.
        self.finish_reload_sites(result.is_ok())?;
        if result.is_err() {
            let attempted = revision.clone();
            let basis = retained.clone();
            let restored = self.run(move |host| {
                Ok(if entered_core {
                    host.modular_rollback_confirmed(&attempted)
                } else {
                    host.verify_private_basis(&basis).is_ok()
                })
            })?;
            if !restored {
                // Keep TX inhibited while the account providers reverse the failed attempt.
                self.inhibit_modular();
            }
            self.restore_accounts(attempt)?;
            return if restored {
                Ok(AdoptionOutcome::RolledBack)
            } else {
                Err(RuntimeError::Restore)
            };
        }
        loop {
            if !self.accepting.load(Ordering::Acquire) {
                return Err(RuntimeError::Rejected);
            }
            let engine = Arc::clone(self);
            let configuration = configuration.clone();
            let inputs = inputs.clone();
            let revision = revision.clone();
            let result = self.run_inner(true, move |host| {
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
                if state.0 != fault_epoch
                    || state.0 == u64::MAX
                    || !engine.accepting.load(Ordering::Acquire)
                {
                    return Err(RuntimeError::Restore);
                }
                let gate = engine
                    .management_state
                    .gate
                    .get()
                    .ok_or(RuntimeError::Rejected)?;
                // All physical and site handoffs succeeded. Release only this generation's
                // affected stations on the same serialized turn as native confirmation.
                host.runtime.check_station_confirmation(&configuration)?;
                let finish_phone = host.prepare_account_release(attempt)?;
                if !host.step_network_accounts(attempt, 2)? {
                    return Ok(false);
                }
                host.begin_account_release(attempt)?;
                if finish_phone {
                    host.finish_private_accounts(attempt, |local, mailbox| {
                        let Ok(port) = local.parse::<ResourceId>() else {
                            return false;
                        };
                        configuration
                            .scopes
                            .get(&ResourceScope::Port(port))
                            .is_some_and(|scope| {
                                scope.mailboxes.effective.values().any(|current| {
                                    Some(current.binding.id) == mailbox.binding
                                        && current.number == mailbox.id
                                        && current.first_name == mailbox.first_name
                                        && current.callsign == mailbox.callsign
                                })
                            })
                    })?;
                }
                if !host.step_network_accounts(attempt, 4)? {
                    return Ok(false);
                }
                host.runtime.confirm_station_replacements(&configuration)?;
                host.confirm_private_media();
                host.finish_asl_selection(attempt)?;
                state.1 = observed;
                gate.store(false, Ordering::Release);
                Ok(true)
            });
            match result {
                Ok(true) => return Ok(AdoptionOutcome::Confirmed),
                Ok(false) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn command_owner(
        self: &Arc<Self>,
        target: ResourceId,
        revision: &str,
        action: &PreparedAction,
    ) -> Result<OperationOwner, Fault> {
        let gate = self
            .management_state
            .gate
            .get()
            .cloned()
            .ok_or(Fault::Unavailable)?;
        let expected = revision.to_owned();
        let site_action = matches!(&action.action, rpt_advanced_core::command_language::Action::Site(command)
            if !matches!(command,rpt_advanced_core::site::SiteCommand::Inhibit(_)));
        self.run(move |host| {
            if gate.load(Ordering::Acquire) || !host.modular_revision_matches(&expected) {
                return Err(RuntimeError::Rejected);
            }
            if let Some(generation) = host.runtime.site_work(&target.to_string()) {
                if !site_action {
                    return Err(RuntimeError::Unavailable);
                }
                return Ok(OperationOwner {
                    scope: ResourceScope::Site(target),
                    generation,
                    requires_grant: false,
                });
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
        .map_err(|error| {
            if error == RuntimeError::Unavailable {
                Fault::Unavailable
            } else {
                Fault::Conflict
            }
        })
    }

    pub(crate) fn dispatch_maintenance(
        self: &Arc<Self>,
        grant: Arc<crate::management::maintenance::Grant>,
        intent: Invocation,
    ) -> Effect {
        let Some(gate) = self.management_state.gate.get().cloned() else {
            return Effect::Rejected;
        };
        grant.gate.get_or_init(|| Arc::clone(&gate));
        self.run(move |host| {
            if gate.load(Ordering::Acquire)
                || !host.modular_revision_matches(&grant.revision)
                || !grant.valid()
                || intent.context.recheck(false).is_err()
                || intent.context.generation.id() != grant.generation
                || intent.context.owner != ResourceScope::Port(grant.port)
            {
                return Ok(Effect::Rejected);
            }
            let rpt_advanced_core::command_language::Action::Maintenance(action) = intent.action
            else {
                return Ok(Effect::Rejected);
            };
            Ok(host.execute_maintenance(grant, action))
        })
        .unwrap_or(Effect::Rejected)
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

#[cfg(test)]
#[path = "lifecycle_observability_tests.rs"]
mod observability;
