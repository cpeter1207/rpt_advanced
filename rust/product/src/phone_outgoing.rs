//! Exact pending telephone intent; no fabricated provider call or media owner.
use super::*;
use rpt_advanced_core::link::PhoneAccountId;
use std::sync::atomic::AtomicBool;

static NEXT_CLAIM: AtomicU64 = AtomicU64::new(1);

/// One cancellable call-time reservation, retained by serialized control and its dial job.
pub(crate) struct OutgoingClaim {
    id: u64,
    account: PhoneAccountId,
    local: String,
    current: AtomicBool,
    bypass: AtomicBool,
}
impl OutgoingClaim {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
    pub(crate) fn account(&self) -> &PhoneAccountId {
        &self.account
    }
    pub(crate) fn local(&self) -> &str {
        &self.local
    }
    pub(crate) fn current(&self) -> bool {
        self.current.load(Ordering::Acquire)
    }
    /// Exact-call latch; neither unlock expiry nor another call inherits it.
    pub(crate) fn bypassed(&self) -> bool {
        self.bypass.load(Ordering::Acquire)
    }
    pub(crate) fn set_bypass(&self, enabled: bool) {
        self.bypass.store(enabled, Ordering::Release);
    }
    pub(crate) fn cancel(&self) {
        self.current.store(false, Ordering::Release);
    }
}
impl PhoneCalls {
    /// Reserve capacity before external dialing; target policy is checked by Host.
    pub(crate) fn reserve_outgoing(
        &mut self,
        account: PhoneAccountId,
        local: &str,
    ) -> Result<Arc<OutgoingClaim>, Error> {
        if self.calls.len() + self.pending_outgoing.len() >= self.maximum_calls
            || self.outgoing_reserved(local)
            || self
                .calls
                .iter()
                .any(|call| call.attachment.as_deref() == Some(local))
        {
            return Err(Error::Reservation);
        }
        self.pending_outgoing
            .try_reserve(1)
            .map_err(|_| Error::Allocation)?;
        let claim = Arc::new(OutgoingClaim {
            id: next_ingress(&NEXT_CLAIM)?,
            account,
            local: local.to_owned(),
            current: AtomicBool::new(true),
            bypass: AtomicBool::new(false),
        });
        self.pending_outgoing.push(Arc::clone(&claim));
        Ok(claim)
    }
    /// Adopt only the exact pending claim and the provider's real PHONE identity.
    pub(crate) fn adopt_outgoing(
        &mut self,
        claim: &Arc<OutgoingClaim>,
        io: PeerIo,
    ) -> Result<PhoneSource, Error> {
        let index = self
            .pending_outgoing
            .iter()
            .position(|pending| Arc::ptr_eq(pending, claim))
            .filter(|_| claim.current())
            .ok_or(Error::Admission)?;
        if io.local() != claim.local() || io.phone_account() != Some(claim.account()) {
            return Err(Error::Admission);
        }
        let source = self.admit_call(io, 0, Some(Arc::clone(claim)))?;
        self.pending_outgoing.remove(index);
        Ok(source)
    }
    /// Release an exact local claim, returning an active reply for graph cleanup.
    pub(crate) fn cancel_outgoing(
        &mut self,
        local: &str,
        id: u64,
    ) -> Result<Option<PeerReply>, Error> {
        if let Some(index) = self
            .pending_outgoing
            .iter()
            .position(|claim| claim.id == id && claim.local == local)
        {
            self.pending_outgoing.remove(index).cancel();
            return Ok(None);
        }
        let source = self
            .calls
            .iter()
            .find(|call| {
                call.outgoing
                    .as_ref()
                    .is_some_and(|claim| claim.id == id && claim.local == local)
            })
            .map(|call| call.source.clone())
            .ok_or(Error::Admission)?;
        Ok(self.retire(&source))
    }
    /// Borrow the exact current node-local intent during serialized command admission.
    pub(crate) fn outgoing_claim(&self, local: &str) -> Option<&Arc<OutgoingClaim>> {
        self.pending_outgoing
            .iter()
            .find(|claim| claim.local == local)
            .or_else(|| {
                self.calls
                    .iter()
                    .filter_map(|call| call.outgoing.as_ref())
                    .find(|claim| claim.local == local)
            })
            .filter(|claim| claim.current())
    }
    /// Held through pending and active control ownership. Exact cancellation first
    /// revokes media, then releases this intent; provider teardown may finish later.
    pub(crate) fn outgoing_reserved(&self, local: &str) -> bool {
        self.pending_outgoing
            .iter()
            .any(|claim| claim.local == local)
            || self.calls.iter().any(|call| {
                call.outgoing
                    .as_ref()
                    .is_some_and(|claim| claim.local == local)
            })
    }
}
