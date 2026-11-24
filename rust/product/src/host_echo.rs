//! Direct EchoLink's share of the existing radio adoption journal.
use super::Host;
use crate::{abi, management::native::AccountCredentials, services::HostServices};
use rpt_advanced_core::{
    config::{EchoLinkSettings, EchoLinkTransport, resources::{candidate::ResourceId,
        generation::RuntimeConfiguration, profiles::{JitterTiming, credentials::CredentialSlot}}},
    runtime::RuntimeError,
};
use std::{collections::BTreeMap, num::NonZeroU64, ptr};

#[derive(Clone, PartialEq)]
struct Selected {
    id: ResourceId,
    version: ResourceId,
    label: u64,
    settings: EchoLinkSettings,
    jitter: JitterTiming,
}
impl Selected {
    fn same_binding(&self, next: &Self) -> bool {
        self.id == next.id && self.version == next.version
            && self.settings.callsign == next.settings.callsign
            && self.settings.node_number == next.settings.node_number
            && self.settings.bind_address == next.settings.bind_address
            && self.settings.audio_port == next.settings.audio_port
    }
    fn stage(&self, host: HostServices, attempt: NonZeroU64, station: ResourceId,
        credentials: &AccountCredentials, role: u32) -> Result<(), RuntimeError> {
        if credentials.version(self.id, CredentialSlot::Directory) != Some(self.version) { return Err(RuntimeError::Preparation); }
        let secret = credentials.get(self.id, CredentialSlot::Directory).ok_or(RuntimeError::Preparation)?;
        let row = host.echo_provider().map_err(|_| RuntimeError::Preparation)?.row.unwrap();
        let code = crate::configuration::echolink::visit(&self.label.to_string(), &self.settings, |config| {
            let input = abi::rptadv_managed_echo_row_v1 {
                struct_size: size_of::<abi::rptadv_managed_echo_row_v1>() as u32, abi_version: 1,
                station_uuid: *station.uuid().as_bytes(), account_uuid: *self.id.uuid().as_bytes(),
                credential_version: *self.version.uuid().as_bytes(), local_label: self.label,
                minimum_ms: self.jitter.minimum_ms, initial_ms: self.jitter.initial_ms,
                maximum_ms: self.jitter.maximum_ms, role, settings: ptr::from_ref(config).cast(),
                directory_password: super::asl::span(secret.expose_bytes()),
            };
            unsafe { row(host.context(), attempt.get(), &input) }
        });
        (code == 0).then_some(()).ok_or(RuntimeError::Preparation)
    }
}

type Selection = BTreeMap<ResourceId, Selected>;
struct Stage {
    attempt: NonZeroU64,
    next: Selection,
    started: bool,
    applied: bool,
    published: bool,
    reversed: bool,
    finished: bool,
}
#[derive(Default)]
pub(super) struct Participant {
    active: Selection, staged: Option<Stage>, basis: u64, label: u64, retiring: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Participant {
    pub(super) fn accepts(&self, local: &str) -> bool {
        let Ok(id) = local.parse::<ResourceId>() else {return false};
        self.active.get(&id).is_some_and(|old| self.staged.as_ref().is_none_or(|s|
            !s.started || s.next.get(&id) == Some(old)))
    }
    pub(super) fn prepare(&mut self, host: HostServices, attempt: NonZeroU64, domain: &RuntimeConfiguration,
        credentials: &AccountCredentials, prior: Option<&AccountCredentials>) -> Result<(), RuntimeError> {
        if self.staged.is_some() { return Err(RuntimeError::Busy); }
        if attempt.get() > (u64::MAX - 1) / 2 { return Err(RuntimeError::Preparation); }
        let mut next = Selection::new();
        for (station, port) in domain.protocols.ports().filter(|(_, p)| p.metadata.enabled) {
            let Some(profile) = port.echolink.as_ref().filter(|p| p.settings.enabled) else { continue };
            if profile.settings.transport != EchoLinkTransport::Direct { return Err(RuntimeError::Preparation); }
            let id = profile.binding.id;
            let version = credentials.version(id, CredentialSlot::Directory).ok_or(RuntimeError::Preparation)?;
            let mut settings = profile.settings.clone();
            settings.proxy_host.clear();
            settings.proxy_port = 0;
            let mut selected = Selected { id, version, settings, jitter: profile.jitter.timing(), label: 0 };
            selected.label = if let Some(old) = self.active.get(&station).filter(|old| old.same_binding(&selected)) {
                old.label
            } else {
                self.label = self.label.checked_add(1).ok_or(RuntimeError::Preparation)?;
                self.label
            };
            next.insert(station, selected);
        }
        if next == self.active { return Ok(()); }
        let group = host.echo_provider().map_err(|_| RuntimeError::Preparation)?;
        if unsafe { group.begin.unwrap()(host.context(), self.basis, attempt.get()) } != 0 {
            return Err(RuntimeError::Preparation);
        }
        self.staged = Some(Stage { attempt, next, started: false, applied: false, published: false, reversed: false, finished: false });
        let next = &self.staged.as_ref().unwrap().next;
        for (station, old) in &self.active {
            if next.get(station).is_none_or(|new| !old.same_binding(new)) {
                old.stage(host, attempt, *station, prior.ok_or(RuntimeError::Preparation)?, 3)?;
            }
        }
        for (station, row) in next {
            let role = match self.active.get(station) {
                Some(old) if old == row => continue,
                Some(old) if old.same_binding(row) => 2,
                Some(_) => 4,
                None => 1,
            };
            row.stage(host, attempt, *station, credentials, role)?;
        }
        Ok(())
    }
    fn step(&mut self, host: HostServices, attempt: NonZeroU64, phase: u32) -> Result<bool, RuntimeError> {
        let Some(s) = &mut self.staged else { return Ok(true) };
        if s.attempt != attempt { return Err(RuntimeError::Rejected); }
        let done = match phase {
            1 => &mut s.applied, 2 => &mut s.published, 3 => &mut s.reversed, 4 => &mut s.finished,
            _ => return Err(RuntimeError::Rejected),
        };
        if *done { return Ok(true); }
        let group = host.echo_provider().map_err(|_| RuntimeError::Device)?;
        match unsafe { group.step.unwrap()(host.context(), attempt.get(), phase) } {
            0 => { *done = true; Ok(true) }, 1 => Ok(false), _ => Err(RuntimeError::Device),
        }
    }
    pub(super) fn release(&mut self, attempt: NonZeroU64) -> Result<(), RuntimeError> {
        if let Some(s) = &self.staged {
            if s.attempt != attempt || !s.finished { return Err(RuntimeError::Rejected); }
            let s = self.staged.take().unwrap();
            if s.published && !s.reversed { self.active = s.next; self.basis = attempt.get(); }
        }
        Ok(())
    }
    pub(super) fn abandon(&mut self, host: HostServices, attempt: NonZeroU64) -> Result<bool, RuntimeError> {
        if !self.step(host, attempt, 4)? { return Ok(false); }
        self.release(attempt)?;
        Ok(true)
    }
    pub(super) fn stop(&self, host: HostServices) -> bool {
        host.echo_provider().map_or(self.staged.is_none() && self.active.is_empty(),
            |ops| unsafe { ops.shutdown.unwrap()(host.context()) == 0 })
    }
}
impl Host {
    pub(crate) fn step_network_accounts(&mut self, attempt: NonZeroU64, phase: u32) -> Result<bool, RuntimeError> {
        if phase == 1 {
            if let Some(stage) = &mut self.echo.staged {
                if stage.attempt != attempt { return Err(RuntimeError::Rejected); }
                if !stage.started {
                    // Only displaced native bindings lose QSOs. Live policy exchanges retain their serial media owners.
                    for peer in &self.peers {
                        if peer.control.text_origin().protocol != rpt_advanced_core::link::ProtocolId::EchoLink as u32 {continue;}
                        let Ok(id) = peer.local.parse::<ResourceId>() else {continue};
                        if self.echo.active.get(&id).is_some_and(|old| stage.next.get(&id).is_none_or(|new| !old.same_binding(new))) {
                            self.echo.retiring.push(peer.control.request_retirement());
                        }
                    }
                    stage.started=true;
                }
            }
            if !self.echo.retiring.is_empty() {
                self.peer_io.reap();
                self.echo.retiring.retain(|p| self.peer_io.owns_session(p));
                if !self.echo.retiring.is_empty() { return Ok(false); }
            }
        }
        if phase == 3 {
            if !self.echo.step(self.services, attempt, phase)? { return Ok(false); }
            return self.step_asl(attempt, phase);
        }
        if !self.step_asl(attempt, phase)? { return Ok(false); }
        self.echo.step(self.services, attempt, phase)
    }
}
