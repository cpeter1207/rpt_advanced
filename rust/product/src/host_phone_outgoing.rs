//! Outgoing telephone reservations on existing serialized station control.
use super::*;
use crate::phone::OutgoingClaim;
use rpt_advanced_core::{
    config::{PhoneAccountProtocol, PhoneAccountSettings},
    link::PhoneAccountId,
};

impl Host {
    /// The exact outgoing intent temporarily owns ordinary publication/retry admission.
    pub(crate) fn outgoing_phone_reserved(&self, local: &str) -> bool {
        self.phone_calls.outgoing_reserved(local)
    }

    /// First milestone requires a peer-free initiating node for the exact call.
    /// Inspect retained owners too; a closing transport is not an empty node.
    pub(crate) fn reserve_outgoing_phone(
        &mut self,
        account: PhoneAccountId,
        local: &str,
    ) -> Result<Arc<OutgoingClaim>, RuntimeError> {
        self.runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        if self.peers.iter().any(|peer| peer.local == local)
            || !self
                .runtime
                .document()
                .phone_local_attachment_allowed(&account, local)
            || !PhoneAccountSettings::resolve(self.runtime.document(), &account)
                .value
                .is_some_and(|settings| {
                    settings.enabled && settings.protocol == PhoneAccountProtocol::Sip
                })
        {
            return Err(RuntimeError::Rejected);
        }
        self.phone_calls
            .reserve_outgoing(account, local)
            .map_err(|_| RuntimeError::Rejected)
    }
}

impl Host {
    /// Adopt an answered call only through its exact still-current reservation.
    pub(crate) fn adopt_outgoing_phone(
        &mut self,
        claim: &Arc<OutgoingClaim>,
        io: PeerIo,
        clock: RuntimeClock,
    ) -> Result<PhoneSource, RuntimeError> {
        let local = claim.local();
        if !claim.current()
            || self.peers.iter().any(|peer| peer.local == local)
            || self.runtime.settings(local).is_none()
            || !self
                .runtime
                .document()
                .phone_local_attachment_allowed(claim.account(), local)
        {
            return Err(RuntimeError::Rejected);
        }
        let source = self
            .phone_calls
            .adopt_outgoing(claim, io)
            .map_err(|_| RuntimeError::Rejected)?;
        let reply = self
            .phone_calls
            .reply(&source)
            .ok_or(RuntimeError::Rejected)?;
        let result = self
            .phone_calls
            .start_outgoing(&source, &reply, &mut self.peer_io)
            .and_then(|()| self.phone_calls.attach(&source, &reply, local))
            .map_err(|_| RuntimeError::Preparation)
            .and_then(|_| self.refresh(local, clock));
        if let Err(error) = result {
            self.retire_phone(&source);
            return Err(error);
        }
        Ok(source)
    }
}

impl Host {
    /// Revoke an exact pending/active claim and its existing source collectors.
    pub(crate) fn cancel_outgoing_phone(
        &mut self,
        local: &str,
        id: u64,
    ) -> Result<(), RuntimeError> {
        if let Some(reply) = self
            .phone_calls
            .cancel_outgoing(local, id)
            .map_err(|_| RuntimeError::Rejected)?
        {
            Self::retire_phone_source(&mut self.runtime, &reply);
        }
        Ok(())
    }
}
