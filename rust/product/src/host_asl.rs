//! The product retains registry identity, directory policy and the outstanding native receipt.
use super::Host;
use crate::{
    abi, directory::{ManagedDirectory, account_plans},
    management::native::AccountCredentials, services::HostServices, status_post::PostConfig,
};
use rpt_advanced_core::{
    config::{LinkLookupMethod, resources::{
        candidate::ResourceId, generation::RuntimeConfiguration,
        profiles::{AllStarLinkProfileSettings, JitterTiming, credentials::CredentialSlot},
    }},
    runtime::RuntimeError,
};
use std::{collections::{BTreeMap, BTreeSet}, ffi::c_void, num::NonZeroU64, ptr::{self, NonNull}, sync::Arc};

type Selection = BTreeMap<ResourceId, Account>;

#[cfg(test)]
#[path = "host_asl_tests.rs"]
mod tests;

#[derive(Clone)]
pub(crate) struct Account {
    pub(crate) station: ResourceId,
    account: ResourceId,
    generation: NonZeroU64,
    pub(crate) wire: NonZeroU64,
    settings: AllStarLinkProfileSettings,
    jitter: JitterTiming,
    version: Option<ResourceId>,
    pub(crate) directory: Arc<ManagedDirectory>,
}

impl Account {
    pub(crate) fn permits(&self, remote: &str) -> bool {
        !self.settings.deny_nodes.iter().any(|node| node == remote)
            && (self.settings.allow_nodes.is_empty() || self.settings.allow_nodes.iter().any(|node| node == remote))
    }

    pub(crate) fn identity(&self) -> abi::rptadv_asl_identity_v1 {
        abi::rptadv_asl_identity_v1 {
            station_uuid: *self.station.uuid().as_bytes(),
            account_uuid: *self.account.uuid().as_bytes(),
            wire: self.wire.get(), generation: self.generation.get(),
            udp_port: u32::from(self.settings.local_port), reserved: 0,
        }
    }

    pub(crate) fn matches(&self, id: &abi::rptadv_asl_identity_v1) -> bool {
        let selected = self.identity();
        selected.station_uuid == id.station_uuid && selected.account_uuid == id.account_uuid
            && selected.generation == id.generation && selected.wire == id.wire
            && selected.udp_port == id.udp_port && id.reserved == 0
    }

    fn retains_admission(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.directory, &previous.directory) && self.jitter == previous.jitter
    }

    fn retains_native(&self, previous: &Self) -> bool {
        self.retains_admission(previous) && self.version == previous.version
            && (self.version.is_none() || (self.settings.registration_url == previous.settings.registration_url
                && self.settings.registration_interval_s == previous.settings.registration_interval_s))
    }
}

pub(crate) fn span(bytes: &[u8]) -> abi::rptadv_peer_bytes_v1 {
    abi::rptadv_peer_bytes_v1 { data: bytes.as_ptr(), length: bytes.len() }
}

struct Stage {
    attempt: NonZeroU64,
    next: Selection,
    affected: BTreeSet<ResourceId>,
    token: Option<NonNull<c_void>>,
    started: bool,
    applied: bool,
    published: bool,
    reversed: bool,
    irreversible: bool,
    finished: bool,
}

// Only the serialized Host calls step with this token. All native socket ownership
// stays on the frontend owner. The provider retains its code through final release.
unsafe impl Send for Stage {}

#[derive(Default)]
pub(super) struct Participant {
    active: Selection,
    retiring: Vec<Arc<std::sync::atomic::AtomicBool>>,
    basis: u64,
    last: u64,
    withdrawing: bool,
    staged: Option<Stage>,
    reports: Vec<(String, String, String, u64)>,
}

impl Participant {
    pub(super) fn prepare(&mut self, services: HostServices, attempt: NonZeroU64,
        domain: &RuntimeConfiguration, credentials: &AccountCredentials) -> Result<(), RuntimeError> {
        if self.staged.is_some() { return Err(RuntimeError::Busy); }
        let mut next = Selection::new();
        let mut wires = BTreeSet::new();
        for plan in account_plans(domain) {
            if plan.settings.local_port == 0 || !wires.insert(plan.wire) {
                return Err(RuntimeError::Preparation);
            }
            let old = self.active.get(&plan.station).filter(|a| a.account == plan.account
                && a.wire == plan.wire && a.settings.local_port == plan.settings.local_port);
            let generation = old.map_or(attempt, |old| old.generation);
            let directory = old.filter(|a| a.settings.directory_file == plan.settings.directory_file
                && a.settings.lookup_method == plan.settings.lookup_method && a.settings.allow_nodes == plan.settings.allow_nodes
                && a.settings.deny_nodes == plan.settings.deny_nodes)
                .map(|a| a.directory.clone()).unwrap_or_else(|| Arc::new(ManagedDirectory {
                    station: *plan.station.uuid().as_bytes(), account: *plan.account.uuid().as_bytes(),
                    generation, method: match plan.settings.lookup_method {
                        LinkLookupMethod::Both => 0, LinkLookupMethod::Dns => 1, LinkLookupMethod::File => 2,
                    },
                    external: plan.settings.directory_file.clone(),
                    registered: Arc::new(BTreeMap::new()),
                }));
            let version = plan.settings.registration_url.as_ref().and_then(|_| {
                credentials.version(plan.account, CredentialSlot::AllStarLink)
            });
            next.insert(plan.station, Account { station: plan.station, account: plan.account,
                generation, wire: plan.wire, settings: plan.settings.clone(), jitter: plan.jitter.timing(), version, directory });
        }
        let affected = self.active.keys().chain(next.keys()).copied().filter(|key| {
            !next.get(key).zip(self.active.get(key)).is_some_and(|(a, b)| a.retains_admission(b))
        }).collect();
        let retain = next.len() == self.active.len() && next.iter().all(|(key, a)| {
            self.active.get(key).is_some_and(|previous| a.retains_native(previous))
        });
        let token = if retain { None } else {
            let group = services.asl_provider().map_err(|_| RuntimeError::Preparation)?;
            let rows = next.iter().map(|(id, a)| {
                let registrar = a.version.zip(credentials.get(a.account, CredentialSlot::AllStarLink));
                abi::rptadv_asl_account_v1 {
                    struct_size: size_of::<abi::rptadv_asl_account_v1>() as u32, abi_version: 1, id: a.identity(),
                    jitter_min_ms: a.jitter.minimum_ms, jitter_initial_ms: a.jitter.initial_ms, jitter_max_ms: a.jitter.maximum_ms,
                    registration_interval_s: u32::from(a.settings.registration_interval_s),
                    registration_url: span(a.settings.registration_url.as_deref().unwrap_or("").as_bytes()),
                    record_version: registrar.map_or([0; 16], |(version, _)| *version.uuid().as_bytes()),
                    registration_credential: span(registrar.map_or(&[][..], |(_, secret)| secret.expose_bytes())),
                    admission_changed: u32::from(!self.active.get(id).is_some_and(|old| Arc::ptr_eq(&a.directory, &old.directory))),
                }
            }).collect::<Vec<_>>();
            let mut token = ptr::null_mut();
            let code = unsafe { group.prepare.unwrap()(services.context(), self.basis, attempt.get(),
                rows.as_ptr(), rows.len(), &crate::lifecycle::asl::ADMISSION, &mut token) };
            if code != 0 { return Err(RuntimeError::Preparation); }
            Some(NonNull::new(token).ok_or(RuntimeError::Preparation)?)
        };
        self.last = attempt.get();
        self.staged = Some(Stage { attempt, next, affected, token, started: false, applied: false, published: false, reversed: false,
            irreversible: false, finished: false });
        Ok(())
    }

    fn stage(&mut self, attempt: NonZeroU64) -> Result<&mut Stage, RuntimeError> {
        self.staged.as_mut().filter(|s| s.attempt == attempt).ok_or(RuntimeError::Rejected)
    }

    pub(super) fn step(&mut self, services: HostServices, attempt: NonZeroU64, phase: u32) -> Result<bool, RuntimeError> {
        let stage = self.stage(attempt)?;
        if phase == 1 && stage.applied { return Ok(true); }
        if phase == 2 && stage.published { return Ok(true); }
        if phase == 3 && stage.reversed { return Ok(true); }
        if phase == 3 && stage.irreversible { return Err(RuntimeError::Restore); }
        if phase == 1 { stage.started = true; }
        if stage.finished { return if phase == 4 { Ok(true) } else { Err(RuntimeError::Rejected) }; }
        if let Some(token) = stage.token {
            let group = services.asl_provider().map_err(|_| RuntimeError::Device)?;
            match unsafe { group.step.unwrap()(services.context(), token.as_ptr(), phase) } {
                0 => {}
                1 => return Ok(false),
                _ => return Err(RuntimeError::Device),
            }
        }
        match phase {
            1 => stage.applied = true,
            2 => stage.published = true,
            3 => { stage.published = false; stage.reversed = true; stage.applied = false; }
            4 => {
                stage.finished = true;
                if stage.token.take().is_some() && stage.published { self.basis = attempt.get(); }
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) fn dormant(&self, attempt: NonZeroU64) -> bool {
        self.staged.as_ref().is_some_and(|s| s.attempt == attempt && !s.started && !s.irreversible)
    }

    pub(super) fn release(&mut self, attempt: NonZeroU64) -> Result<(), RuntimeError> {
        let stage = self.stage(attempt)?;
        if !stage.finished { return Err(RuntimeError::Rejected); }
        let stage = self.staged.take().expect("checked transaction");
        if stage.published { self.active = stage.next; }
        self.retiring.clear();
        Ok(())
    }

    /// Stop drives the same reservation from Host after the ordinary peer owner drained.
    pub(super) fn stop(&mut self, services: HostServices) -> Result<bool, RuntimeError> {
        if let Some(stage) = self.staged.as_ref() {
            let attempt = stage.attempt;
            if self.withdrawing {
                if !stage.published && (!self.step(services, attempt, 1)? || !self.step(services, attempt, 2)?) {
                    return Ok(false);
                }
            } else if stage.started && !stage.irreversible && !self.step(services, attempt, 3)? {
                return Ok(false);
            }
            if !self.step(services, attempt, 4)? { return Ok(false); }
            self.release(attempt)?;
        }
        if self.active.is_empty() { return Ok(true); }
        let attempt = self.last.checked_add(1).and_then(NonZeroU64::new).ok_or(RuntimeError::Rejected)?;
        let group = services.asl_provider().map_err(|_| RuntimeError::Device)?;
        let mut token = ptr::null_mut();
        let result = unsafe { group.prepare.unwrap()(services.context(), self.basis, attempt.get(),
            ptr::null(), 0, &crate::lifecycle::asl::ADMISSION, &mut token) };
        if result != 0 { return Err(RuntimeError::Device); }
        self.last = attempt.get();
        self.withdrawing = true;
        self.staged = Some(Stage { attempt, affected: self.active.keys().copied().collect(), next: Selection::new(),
            token: Some(NonNull::new(token).ok_or(RuntimeError::Device)?), started: false, applied: false, published: false,
            reversed: false, irreversible: false, finished: false });
        Ok(false)
    }
}

impl Host {
    pub(crate) fn prepare_account_release(&mut self, attempt: NonZeroU64) -> Result<bool, RuntimeError> {
        let owner = self.radio_preparation.account_owner.as_ref().filter(|owner| owner.attempt == attempt)
            .ok_or(RuntimeError::Rejected)?;
        if owner.phone { self.phone_accounts.ready_to_finish(attempt).map_err(|_| RuntimeError::Device)?; }
        Ok(owner.phone)
    }

    pub(crate) fn begin_account_release(&mut self, attempt: NonZeroU64) -> Result<(), RuntimeError> {
        let stage = self.asl.staged.as_ref().filter(|s| s.attempt == attempt).ok_or(RuntimeError::Rejected)?;
        if !stage.irreversible {
            for peer in &self.peers {
                if peer.control.text_origin().protocol != rpt_advanced_core::link::ProtocolId::AllStarLink as u32 { continue; }
                let Ok(port) = peer.local.parse::<ResourceId>() else { continue };
                if !stage.affected.contains(&port) { continue; }
                let retain = stage.next.get(&port).zip(self.asl.active.get(&port)).is_some_and(|(new, old)| {
                    new.generation == old.generation && new.permits(&peer.remote)
                });
                if !retain { self.asl.retiring.push(peer.control.request_retirement()); }
            }
            self.asl.stage(attempt)?.irreversible = true;
        }
        Ok(())
    }
    pub(crate) fn asl_identity(&self, id: &abi::rptadv_asl_identity_v1) -> Result<Account, RuntimeError> {
        let account = self.asl.active.values().find(|a| a.matches(id)).ok_or(RuntimeError::Rejected)?;
        self.asl_account(account.station)
    }

    pub(crate) fn asl_account(&self, station: ResourceId) -> Result<Account, RuntimeError> {
        if self.asl.staged.as_ref().is_some_and(|s| s.started && s.affected.contains(&station)) {
            return Err(RuntimeError::Rejected);
        }
        self.asl.active.get(&station).cloned().ok_or(RuntimeError::Rejected)
    }

    pub(crate) fn step_asl(&mut self, attempt: NonZeroU64, phase: u32) -> Result<bool, RuntimeError> {
        if phase == 4 && !self.asl.retiring.is_empty() {
            self.asl.stage(attempt)?;
            self.peer_io.reap();
            self.asl.retiring.retain(|session| self.peer_io.owns_session(session));
            if !self.asl.retiring.is_empty() { return Ok(false); }
        }
        let ready = self.asl.step(self.services, attempt, phase)?;
        if !ready && matches!(phase, 1 | 3) {
            let stage = self.asl.stage(attempt)?;
            for peer in &self.peers {
                if peer.local.parse().is_ok_and(|id: ResourceId| stage.affected.contains(&id)) {
                    peer.control.wake().notify();
                }
            }
        }
        Ok(ready)
    }

    pub(crate) fn finish_asl_selection(&mut self, attempt: NonZeroU64) -> Result<(), RuntimeError> {
        self.echo.release(attempt)?;
        self.asl.release(attempt)?;
        self.radio_preparation.account_owner = None;
        let reports = self.asl.active.values().filter_map(|a| Some((a.station.to_string(), a.wire.to_string(),
            a.settings.status_post_url.clone()?, u64::from(a.settings.status_post_interval_s)))).collect::<Vec<_>>();
        if self.asl.reports != reports {
            self.status_post.configure_bound(reports.iter().map(|(station, node, url, seconds)| {
                (station.clone(), PostConfig { node: node.clone(), url: url.clone(), interval_seconds: *seconds })
            }));
            self.asl.reports = reports;
        }
        Ok(())
    }
}
