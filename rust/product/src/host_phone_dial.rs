//! Selected registration, exact-attempt PCM isolation and outgoing media ownership.
use super::*;
use crate::phone::accounts::DialAccount;
use rpt_advanced_core::link::MediaIsolationClaim;
use std::sync::{Weak, atomic::AtomicBool};

pub(super) struct Pending {
    local: String,
    account: DialAccount,
    current: Weak<AtomicBool>,
}

/// One outgoing job owns its cancellation fence and its optional isolation claim.
pub(crate) struct OutboundPhone {
    pub(crate) account: DialAccount,
    pub(crate) current: Arc<AtomicBool>,
    isolation: Option<MediaIsolationClaim>,
}

impl Host {
    pub(super) fn recheck_phone_dials(&mut self) {
        let accounts = &self.phone_accounts;
        let services = self.services;
        self.phone_dials.retain(|pending| {
            let Some(current) = pending.current.upgrade() else { return false };
            if current.load(Ordering::Acquire)
                && !accounts.registered(services, &pending.local, &pending.account.identity, pending.account.generation)
            {
                current.store(false, Ordering::Release);
            }
            true
        });
    }

    /// Select and cancel in the same serialized turn so a successor cannot be substituted.
    pub(crate) fn hangup_outgoing_phone(&mut self, local: &str) -> Result<(), RuntimeError> {
        enum Outgoing {
            Pending(Arc<AtomicBool>),
            Active(PhoneSource),
        }
        let selected = {
            let pending = self
                .phone_dials
                .iter()
                .filter(|attempt| attempt.local == local)
                .filter_map(|attempt| attempt.current.upgrade())
                .filter(|current| current.load(Ordering::Acquire))
                .map(Outgoing::Pending);
            let active = self
                .phone_calls
                .outgoing_sources(local)
                .cloned()
                .map(Outgoing::Active);
            let mut candidates = pending.chain(active);
            let selected = candidates.next().ok_or(RuntimeError::Rejected)?;
            if candidates.next().is_some() {
                return Err(RuntimeError::Rejected);
            }
            selected
        };
        let ended = match selected {
            Outgoing::Pending(current) => current.swap(false, Ordering::AcqRel),
            Outgoing::Active(source) => self.retire_phone(&source),
        };
        ended.then_some(()).ok_or(RuntimeError::Rejected)
    }

    pub(crate) fn begin_phone_dial(&mut self, local: &str) -> Result<OutboundPhone, RuntimeError> {
        self.recheck_phone_dials();
        // Limit pending workers separately from the existing bounded media call owner.
        if self.phone_dials.len() >= 16 {
            return Err(RuntimeError::Busy);
        }
        let account = self.phone_accounts.dial_selection(self.services, local).ok_or(RuntimeError::Rejected)?;
        let plan = self.radio_preparation.active_plan(local)?;
        let policy = plan.phone.as_ref().filter(|policy| {
            policy.account == account.binding && policy.confirmed.load(Ordering::Acquire)
        }).ok_or(RuntimeError::Rejected)?;
        let isolation = if policy.settings.isolate_peers {
            Some(plan.media_isolation.hold().ok_or(RuntimeError::Busy)?)
        } else {
            None
        };
        let current = Arc::new(AtomicBool::new(true));
        self.phone_dials.push(Pending { local: local.to_owned(), account: account.clone(), current: Arc::downgrade(&current) });
        Ok(OutboundPhone { account, current, isolation })
    }

    pub(crate) fn complete_phone_dial(&mut self, local: &str, attempt: OutboundPhone, io: PeerIo, clock: RuntimeClock)
        -> Result<(), RuntimeError> {
        let OutboundPhone { account, current, isolation } = attempt;
        self.phone_dials.retain(|pending| !pending.current.ptr_eq(&Arc::downgrade(&current)));
        if !current.swap(false, Ordering::AcqRel)
            || io.local() != local || io.identity().protocol() != ProtocolId::Phone
            || io.phone_account() != Some(&account.identity) || io.generations().0 != account.generation
            || io.generations().1 == 0 || !io.source_media() || io.async_owner().is_none()
        {
            return Err(RuntimeError::Rejected);
        }
        self.authorize_phone_owner(local, &account.identity, account.generation)?;
        let source = self.phone_calls.admit_outgoing(io, clock.now_ms, isolation).map_err(|_| RuntimeError::Rejected)?;
        let reply = self.phone_calls.reply(&source).expect("new phone call owns its reply");
        let attached = self.phone_calls.start_transport(&source, &reply, &mut self.peer_io)
            .map_err(|_| RuntimeError::Rejected).and_then(|()| self.refresh(local, clock));
        if attached.is_err() {
            self.retire_phone(&source);
        }
        attached
    }
}
