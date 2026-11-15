//! Outgoing telephone reservations on existing serialized station control.
use super::*;
use crate::phone::OutgoingClaim;
use rpt_advanced_core::{
    config::{PhoneAccountProtocol, PhoneAccountSettings},
    link::PhoneAccountId,
};

impl Host {
    /// Prepare the existing graph cut and acknowledge exact serial owners before dialing.
    pub(crate) fn prepare_outgoing_isolation(
        &mut self,
        claim: &Arc<OutgoingClaim>,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        if !self
            .phone_calls
            .outgoing_claim(claim.local())
            .is_some_and(|current| Arc::ptr_eq(current, claim))
        {
            return Err(RuntimeError::Rejected);
        }
        if self.runtime.reload_pending() {
            return Ok(false);
        }
        self.pump(clock)?;
        let local = claim.local();
        let status = self
            .runtime
            .node(local)
            .ok_or(RuntimeError::MissingNode)?
            .status(clock.now_ms);
        let control = self
            .runtime
            .adapter_control_mut(local, status.active.ok_or(RuntimeError::Rejected)?)
            .ok_or(RuntimeError::Rejected)?;
        let started = control.isolated_call == Some(claim.id());
        if started && control.isolation_failed {
            return Err(RuntimeError::Preparation);
        }
        // Only a never-started cut without retained ordinary graph history is
        // vacuous. A required owner disappearing cannot satisfy its prior ACK.
        if !started
            && control.text_origins.is_empty()
            && status.retiring.is_none()
            && !self.peers.iter().any(|peer| peer.local == local)
        {
            return Ok(true);
        }
        if !started {
            match self.refresh(local, clock) {
                Ok(()) => {}
                Err(RuntimeError::Busy) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        let status = self
            .runtime
            .node(local)
            .ok_or(RuntimeError::MissingNode)?
            .status(clock.now_ms);
        let control = self
            .runtime
            .adapter_control_mut(local, status.active.ok_or(RuntimeError::Rejected)?)
            .ok_or(RuntimeError::Rejected)?;
        if control.isolation_failed {
            return Err(RuntimeError::Preparation);
        }
        Ok(claim.current()
            && control.isolated_call == Some(claim.id())
            && status.retiring.is_none()
            && control.redirects_complete())
    }

    /// Restore released policy on ordinary Engine control, never recursive pump
    /// or pending reload. Busy retains isolation for the next existing pump.
    pub(crate) fn reconcile_phone_isolation(&mut self, clock: RuntimeClock) {
        if self.runtime.reload_pending() {
            return;
        }
        for (local, status) in self.runtime.status(clock.now_ms) {
            if self.phone_calls.outgoing_claim(&local).is_some() {
                continue;
            }
            let isolated = status
                .active
                .and_then(|active| self.runtime.adapter_control_mut(&local, active))
                .is_some_and(|control| control.isolated_call.is_some());
            if isolated {
                match self.refresh(&local, clock) {
                    Ok(()) | Err(RuntimeError::Busy) => {}
                    Err(_) => self
                        .services
                        .scheduler_notice("phone isolation: media restore failed"),
                }
            }
        }
    }

    /// Reserve an exact call without disconnecting ordinary control transports.
    pub(crate) fn reserve_outgoing_phone(
        &mut self,
        account: PhoneAccountId,
        local: &str,
    ) -> Result<Arc<OutgoingClaim>, RuntimeError> {
        self.runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        if !self
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

impl Host {
    /// Resolve the explicit node account and literal decimal destination before reserving.
    /// Call only in the same serialized turn that admits the original RF command.
    pub(crate) fn prepare_radio_phone(
        &mut self,
        local: &str,
        number: &str,
    ) -> Result<(Arc<OutgoingClaim>, String), RuntimeError> {
        if number.is_empty() || number.len() > 63 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RuntimeError::Rejected);
        }
        let account = self
            .runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?
            .phone_outgoing_account
            .clone()
            .ok_or(RuntimeError::Rejected)?;
        let settings = PhoneAccountSettings::resolve(self.runtime.document(), &account)
            .value
            .ok_or(RuntimeError::Rejected)?;
        let destination = format!("sip:{number}@{}", settings.server);
        if destination.len() > 255 {
            return Err(RuntimeError::Rejected);
        }
        rptadv_peer_support::sip::SipUri::parse(destination.as_bytes())
            .map_err(|_| RuntimeError::Rejected)?
            .require_cleartext_routing()
            .map_err(|_| RuntimeError::Rejected)?;
        let claim = self.reserve_outgoing_phone(account, local)?;
        Ok((claim, destination))
    }

    /// Capture and cancel in one control turn, never look up a successor later.
    pub(crate) fn hangup_radio_phone(&mut self, local: &str) -> Result<(), RuntimeError> {
        let id = self
            .phone_calls
            .outgoing_claim(local)
            .ok_or(RuntimeError::Rejected)?
            .id();
        self.cancel_outgoing_phone(local, id)
    }
}

impl Host {
    /// Failed solicited RF telemetry is diagnostic, not failed telephone adoption.
    pub(crate) fn radio_phone_result(&mut self, local: &str, completed: bool) {
        if self
            .node_operation_result(
                local,
                None,
                if completed {
                    rpt_advanced_core::messages::OperationResult::Completed
                } else {
                    rpt_advanced_core::messages::OperationResult::Unavailable
                },
            )
            .is_err()
        {
            self.services
                .scheduler_notice("radio phone result preparation failed");
        }
    }
}

impl Host {
    /// Deliver local RF telephone input only to its captured answered call.
    pub(crate) fn send_radio_telephone_digit(
        &mut self,
        local: &str,
        id: u64,
        symbol: char,
    ) -> Result<(), RuntimeError> {
        self.phone_calls
            .send_radio_telephone_digit(local, id, symbol)
            .map_err(|_| RuntimeError::Rejected)
    }
}
