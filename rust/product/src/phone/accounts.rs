//! Private call admission uses only a provider-confirmed account selection.
use crate::{Error, abi, services::HostServices};
use rpt_advanced_core::{
    config::resources::{
        candidate::{ResourceId, ResourceKind, ResourceScope},
        profiles::{PhoneProtocol, PhoneProviderSettings, ResolvedProtocolProfiles},
    },
    link::PhoneAccountId,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    mem::size_of,
    num::NonZeroU64,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

/// A private-record or declaring-owner change is fenced from the first apply
/// turn until the selected aggregate is published. This contains no PIN data.
#[derive(Default)]
pub(crate) struct MailboxChanges(BTreeSet<(ResourceId, ResourceId)>);

impl MailboxChanges {
    #[cfg(target_os = "linux")]
    pub(crate) fn between(
        old: &rpt_advanced_core::config::resources::generation::RuntimeConfiguration,
        new: &rpt_advanced_core::config::resources::generation::RuntimeConfiguration,
        mut unchanged: impl FnMut(ResourceId) -> bool,
    ) -> Self {
        let mut changed = BTreeSet::new();
        for (scope, selection) in &old.scopes {
            let ResourceScope::Port(port) = *scope else { continue };
            let next = new.scopes.get(scope);
            for (number, mailbox) in &selection.mailboxes.effective {
                let same = next.and_then(|next| next.mailboxes.effective.get(number))
                    .is_some_and(|next| next.binding == mailbox.binding && next.number == mailbox.number);
                if !same || !unchanged(mailbox.binding.id) {
                    changed.insert((port, mailbox.binding.id));
                }
            }
        }
        Self(changed)
    }
}

/// Last fully confirmed native selection, retained by the serialized Host.
#[derive(Default)]
pub(crate) struct Accounts(BTreeMap<PhoneAccountId, Account>);
struct Account {
    port: ResourceId,
    binding: ResourceId,
    local: String,
    provider: PhoneProviderSettings,
    generation: NonZeroU64,
}

/// Copy of the exact live SIP selection used by one bounded outgoing job.
#[derive(Clone)]
pub(crate) struct DialAccount {
    pub(crate) port: ResourceId,
    pub(crate) binding: ResourceId,
    pub(crate) identity: PhoneAccountId,
    pub(crate) generation: u64,
}

impl Account {
    fn registered(&self,services:HostServices)->bool{
        let Ok(group)=services.phone_provider() else{return false};
        let mut status=abi::rptadv_phone_account_status_v1 {
            struct_size:size_of::<abi::rptadv_phone_account_status_v1>() as u32,
            abi_version:1,generation:0,state:0,reserved:0,
        };
        let result=unsafe{group.observe.unwrap()(services.context(),self.port.uuid().as_bytes().as_ptr(),
            self.binding.uuid().as_bytes().as_ptr(),self.generation.get(),&mut status)};
        result==0 && status.struct_size as usize>=size_of::<abi::rptadv_phone_account_status_v1>()
            && status.abi_version==1 && status.reserved==0 && status.generation==self.generation.get() && status.state==2
    }
}

impl Accounts {
    pub(crate) fn admits(&self, local: &str, identity: &PhoneAccountId, generation: u64) -> bool {
        self.0.get(identity).is_some_and(|owner| {
            owner.local == local && owner.generation.get() == generation
        })
    }

    /// Private records are lent from the final authenticated store graph. The
    /// C boundary copies their bytes during prepare; this plan owns none.
    pub(crate) fn prepare<'a>(
        &self,
        services: HostServices,
        profiles: &ResolvedProtocolProfiles,
        mut credential: impl FnMut(ResourceId) -> Option<&'a [u8]>,
        mut unchanged: impl FnMut(ResourceId) -> bool,
    ) -> Result<Prepared, Error> {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let mut next = Accounts::default();
        let mut rows = Vec::new();
        for (port, public) in profiles.ports().filter(|(_, port)| port.metadata.enabled) {
            let Some(phone) = &public.phone_patch else { continue };
            let binding = phone.binding;
            if binding.scope != ResourceScope::Port(port) || binding.kind != ResourceKind::PhonePatch {
                return Err(Error::Admission);
            }
            let Some(endpoint) = phone.settings.provider.account_identity() else { continue };
            let Some(secret) = credential(binding.id) else { continue };
            let label = PhoneAccountId::new(&binding.id.to_string()).map_err(|_| Error::Admission)?;
            let generation = match self.0.get(&label) {
                Some(old) if old.port == port && old.provider == phone.settings.provider && unchanged(binding.id) => old.generation,
                _ => {
                    let prior = LAST.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                        .map_err(|_| Error::Reservation)?;
                    NonZeroU64::new(prior + 1).ok_or(Error::Reservation)?
                }
            };
            let timing = phone.jitter.timing();
            let span = |bytes: &[u8]| abi::rptadv_peer_bytes_v1 {
                data: bytes.as_ptr(),
                length: bytes.len(),
            };
            rows.push(abi::rptadv_phone_account_v1 {
                struct_size: size_of::<abi::rptadv_phone_account_v1>() as u32,
                abi_version: 1,
                port_uuid: *port.uuid().as_bytes(),
                account_uuid: *binding.id.uuid().as_bytes(),
                generation: generation.get(),
                protocol: match endpoint.protocol { PhoneProtocol::Sip => 1, PhoneProtocol::Iax2 => 2 },
                port: u32::from(endpoint.port),
                server: span(endpoint.server.as_bytes()),
                username: span(endpoint.username.as_bytes()),
                credential: span(secret),
                jitter_minimum_ms: timing.minimum_ms,
                jitter_initial_ms: timing.initial_ms,
                jitter_maximum_ms: timing.maximum_ms,
                reserved: 0,
            });
            if next.0.insert(label, Account {
                port, binding: binding.id, local: port.to_string(),
                provider: phone.settings.provider.clone(), generation,
            }).is_some() {
                return Err(Error::Admission);
            }
        }
        let token = if rows.is_empty() && self.0.is_empty() {
            None
        } else {
            let group = services.phone_provider()?;
            if !next.0.is_empty() { crate::services::phone_media::PhoneClock::prepare(services)?; }
            let mut token = ptr::null_mut();
            // The registry and credential owner outlive all borrowed spans in rows.
            let code = unsafe { group.prepare.unwrap()(services.context(), rows.as_ptr(), rows.len(), &mut token) };
            if code != 0 { return Err(Error::Admission); }
            Some(NonNull::new(token).ok_or(Error::Admission)?)
        };
        Ok(Prepared { services, token, next, phase: Phase::Prepared, mailboxes: MailboxChanges::default() })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase { Prepared, Applying, Failed, Applied, Restoring, Restored }

pub(crate) struct Prepared {
    services: HostServices,
    token: Option<NonNull<c_void>>,
    next: Accounts,
    phase: Phase,
    mailboxes: MailboxChanges,
}

// The frontend contract allows logical serialized ownership on a migrating
// control executor. Native reactor-affine objects stay on their original owner.
unsafe impl Send for Prepared {}

impl Prepared {
    /// Publish no account to ingress until the actual frontend and all account
    /// observations agree on the exact expected native generations.
    pub(crate) fn apply(&mut self) -> Result<bool, Error> {
        match self.phase {
            Phase::Applied => return Ok(true),
            Phase::Prepared | Phase::Applying => self.phase = Phase::Applying,
            Phase::Failed | Phase::Restoring | Phase::Restored => return Err(Error::Admission),
        }
        if let Some(token) = self.token {
            let group = self.services.phone_provider()?;
            match unsafe { group.apply.unwrap()(self.services.context(), token.as_ptr()) } {
                0 => {}
                1 => return Ok(false),
                _ => { self.phase = Phase::Failed; return Err(Error::Operation); }
            }
            if self.next.0.values().any(|account|!account.registered(self.services)){
                self.phase=Phase::Failed;
                return Err(Error::Operation);
            }
        }
        self.phase = Phase::Applied;
        Ok(true)
    }

    /// Keep the token for every pending/indeterminate reverse.
    pub(crate) fn restore(&mut self) -> Result<bool, Error> {
        if self.phase == Phase::Restored { return Ok(true); }
        self.phase = Phase::Restoring;
        if let Some(token) = self.token {
            let group = self.services.phone_provider()?;
            match unsafe { group.restore.unwrap()(self.services.context(), token.as_ptr()) } {
                0 => {}
                1 => return Ok(false),
                _ => return Err(Error::Operation),
            }
        }
        self.phase = Phase::Restored;
        Ok(true)
    }

    /// The aggregate calls this only after native apply/restore and its own
    /// terminal selection. Any refused release keeps both the ticket and map.
    pub(crate) fn finish(&mut self, active: &mut Accounts) -> Result<(), Error> {
        self.ready_to_finish(active)?;
        if let Some(token) = self.token {
            let group = self.services.phone_provider()?;
            if unsafe { group.release.unwrap()(self.services.context(), token.as_ptr()) } != 0 {
                return Err(Error::Operation);
            }
            self.token = None;
        }
        if self.phase == Phase::Applied {
            *active = std::mem::take(&mut self.next);
            self.phase = Phase::Restored;
        }
        Ok(())
    }

    fn ready_to_finish(&self, active: &Accounts) -> Result<(), Error> {
        if matches!(self.phase, Phase::Applying | Phase::Failed | Phase::Restoring) { return Err(Error::Reservation); }
        let selected=match self.phase {
            Phase::Applied=>Some(&self.next),Phase::Restored=>Some(&*active),_=>None,
        };
        if selected.is_some_and(|rows|rows.0.values().any(|owner|!owner.registered(self.services))) {
            return Err(Error::Operation);
        }
        Ok(())
    }
}

/// Serialized authority and its single retained transition. The product never
/// lends an opaque transaction to a future or drops it on a failed callback.
#[derive(Default)]
pub(crate) struct Participant {
    active: Accounts,
    pending: Option<(NonZeroU64, Prepared)>,
    transitioning: bool,
    held_accounts: BTreeSet<PhoneAccountId>,
    held_mailboxes: MailboxChanges,
    stopping: bool,
    stop_withdrawal: bool,
}
impl Participant {
    pub(crate) fn dial_selection(&self, services: HostServices, local: &str) -> Option<DialAccount> {
        let (identity, owner) = self.active.0.iter().find(|(identity, owner)| {
            owner.local == local
                && owner.provider.account_identity().is_some_and(|endpoint| endpoint.protocol == PhoneProtocol::Sip)
                && self.registered(services, local, identity, owner.generation.get())
        })?;
        Some(DialAccount {
            port: owner.port,
            binding: owner.binding,
            identity: identity.clone(),
            generation: owner.generation.get(),
        })
    }

    pub(crate) fn ready_to_finish(&self, attempt: NonZeroU64) -> Result<(), Error> {
        let (_, staged) = self.pending.as_ref().filter(|(serial, _)| *serial == attempt).ok_or(Error::Admission)?;
        staged.ready_to_finish(&self.active)
    }
    pub(crate) fn stage<'s>(&mut self, attempt: NonZeroU64, services: HostServices,
        profiles: &ResolvedProtocolProfiles, credential: impl FnMut(ResourceId) -> Option<&'s [u8]>,
        unchanged: impl FnMut(ResourceId) -> bool, mailboxes: MailboxChanges) -> Result<(), Error> {
        if self.stopping { return Err(Error::Reservation); }
        if let Some((old, prior)) = self.pending.as_ref() {
            if *old == attempt {
                return if prior.phase == Phase::Prepared { Ok(()) } else { Err(Error::Reservation) };
            }
            // A new prepare may replace an unused reservation, never one that
            // is applying, restoring or retained after an uncertain outcome.
            self.discard(*old)?;
        }
        let mut staged = self.active.prepare(services, profiles, credential, unchanged)?;
        staged.mailboxes = mailboxes;
        self.pending = Some((attempt, staged));
        Ok(())
    }
    fn pending(&mut self, attempt: NonZeroU64) -> Result<&mut Prepared, Error> {
        self.pending.as_mut().filter(|(id, _)| *id == attempt)
            .map(|(_, value)| value).ok_or(Error::Admission)
    }
    pub(crate) fn apply(&mut self, attempt: NonZeroU64) -> Result<bool, Error> {
        self.pending(attempt)?;
        self.transitioning = true;
        self.pending(attempt)?.apply()
    }
    pub(crate) fn restore(&mut self, attempt: NonZeroU64) -> Result<bool, Error> {
        self.pending(attempt)?.restore()
    }
    pub(crate) fn finish(&mut self, attempt: NonZeroU64) -> Result<(), Error> {
        let (id, prepared) = self.pending.as_mut().ok_or(Error::Admission)?;
        if *id != attempt { return Err(Error::Admission); }
        let publishing = prepared.phase == Phase::Applied;
        prepared.finish(&mut self.active)?;
        if publishing {
            self.held_accounts.clear();
            self.held_mailboxes.0.clear();
        }
        self.pending.take();
        self.transitioning = false;
        Ok(())
    }
    pub(crate) fn registered(&self,services:HostServices,local:&str,identity:&PhoneAccountId,generation:u64)->bool{
        self.admits(local,identity,generation) && self.active.0.get(identity).is_some_and(|account|account.registered(services))
    }
    pub(crate) fn admits_mailbox(&self, port: ResourceId, binding: ResourceId) -> bool {
        !self.stopping && !self.held_mailboxes.0.contains(&(port,binding))
            && (!self.transitioning || self.pending.as_ref()
            .is_some_and(|(_, pending)| !pending.mailboxes.0.contains(&(port, binding))))
    }

    pub(crate) fn admits(&self, local: &str, account: &PhoneAccountId, generation: u64) -> bool {
        !self.stopping && !self.held_accounts.contains(account) && self.active.admits(local, account, generation)
            && (!self.transitioning || self.pending.as_ref()
                .is_some_and(|(_, staged)| staged.next.admits(local, account, generation)))
    }
}

impl Participant {
    /// A deallocated aggregate ticket can no longer be published. Confirm any
    /// necessary reversal before returning its exact native reservation.
    pub(crate) fn abandon(&mut self, attempt: NonZeroU64) -> Result<bool, Error> {
        if self.pending(attempt)?.phase == Phase::Prepared {
            self.discard(attempt)?;
        } else {
            // The aggregate never confirmed this attempt. Restore the actual native
            // accounts for reuse, retaining affected admission fences until a
            // subsequent exact aggregate confirmation.
            let (_, prepared) = self.pending.as_ref().ok_or(Error::Reservation)?;
            self.held_accounts.extend(self.active.0.iter()
                .filter(|(id, owner)| !prepared.next.admits(&owner.local, id, owner.generation.get()))
                .map(|(id, _)| id.clone()));
            self.held_mailboxes.0.extend(prepared.mailboxes.0.iter().copied());
            if !self.restore(attempt)? { return Ok(false); }
            self.finish(attempt)?;
        }
        Ok(true)
    }

    /// Discard only an unactivated reservation for its real staging attempt.
    /// Preserve a busy or failed native ticket until explicit restore/finish.
    pub(crate) fn discard(&mut self, attempt: NonZeroU64) -> Result<(), Error> {
        let value = self.pending(attempt)?;
        if value.phase != Phase::Prepared { return Err(Error::Reservation); }
        self.finish(attempt)
    }
}

impl Participant {
    /// First reverse any in-flight aggregate choice, then use the same genuine
    /// native participant to withdraw all selected accounts. False retains all
    /// tokens/DSOs for the next Engine::stop poll.
    pub(crate) fn stop(&mut self, services: HostServices) -> Result<bool, Error> {
        self.stopping = true;
        if let Some((serial, staged)) = self.pending.as_ref() {
            let serial = *serial;
            let phase = staged.phase;
            if self.stop_withdrawal && !matches!(phase, Phase::Failed | Phase::Restoring | Phase::Restored) {
                if !self.apply(serial)? { return Ok(false); }
                self.finish(serial)?;
                return Ok(true);
            }
            self.stop_withdrawal = false;
            if phase == Phase::Prepared {
                self.discard(serial)?;
            } else {
                if !self.restore(serial)? { return Ok(false); }
                self.finish(serial)?;
            }
        }
        if self.active.0.is_empty() { return Ok(true); }
        // No adoption can run after stopping is latched, and the old ticket
        // above has been consumed. The stop marker never escapes to providers.
        let serial = NonZeroU64::MIN;
        self.pending = Some((serial, self.active.prepare(services, &ResolvedProtocolProfiles::default(), |_| None, |_| false)?));
        self.stop_withdrawal = true;
        if !self.apply(serial)? { return Ok(false); }
        self.finish(serial)?;
        Ok(true)
    }
}
