//! Source-owned asynchronous commands for the shared physical radio owner.

use super::*;
use rpt_advanced_core::{
    remote_base_command::{RemoteCommand, RemoteReplyPart},
    runtime::dtmf::DigitOperation,
};

pub(super) struct Pending {
    local: String,
    revision: u64,
    operation: DigitOperation,
    status: crate::worker::RadioStatus,
    ticket: crate::services::RemoteTicket,
}

impl Host {
    pub(super) fn submit_remote(
        &mut self,
        local: &str,
        operation: DigitOperation,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let revision = self.runtime.revision();
        if !self.remote_request_current(local, revision, &operation, clock.now_ms) {
            return Err(RuntimeError::Rejected);
        }
        if let Some(reply) = operation
            .reply
            .as_ref()
            .filter(|reply| reply.phone_source().is_some())
        {
            // Ended identities still consume a slot until the actual receipt is dropped.
            let private_pending = || {
                self.remote_pending.iter().filter_map(|pending| {
                    pending
                        .operation
                        .reply
                        .as_ref()
                        .filter(|source| source.phone_source().is_some())
                })
            };
            if private_pending().count() >= self.phone_calls.maximum_calls()
                || private_pending().any(|source| source == reply)
            {
                return self.deliver_remote_reply(
                    local,
                    revision,
                    &operation,
                    &crate::remote_reply::failure(1).parts,
                    clock.now_ms,
                );
            }
        }
        let request = operation.remote.ok_or(RuntimeError::Rejected)?.wire();
        let prepared = (|| {
            let (_, lease) = self
                .leases
                .iter()
                .rev()
                .find(|(name, _)| name == local)
                .ok_or(-7)?;
            let mut lease = lease.lock().unwrap_or_else(|error| error.into_inner());
            let status = lease.status.clone();
            let active = operation.reply.as_ref().map_or_else(
                || status.remote_command_source_active(),
                |reply| reply.active(),
            );
            let ticket = lease.worker.as_mut().ok_or(-7)?.remote_submit(
                &request,
                operation.reply.is_none(),
                active,
            )?;
            Ok((status, ticket))
        })();
        match prepared {
            Ok((status, ticket)) => {
                self.remote_pending.push(Pending {
                    local: local.to_owned(),
                    revision,
                    operation,
                    status,
                    ticket,
                });
                Ok(())
            }
            Err(code) => self.deliver_remote_reply(
                local,
                revision,
                &operation,
                &crate::remote_reply::failure(code).parts,
                clock.now_ms,
            ),
        }
    }

    pub(super) fn pump_remote(&mut self, clock: RuntimeClock) {
        for mut pending in std::mem::take(&mut self.remote_pending) {
            let valid = self.remote_request_current(
                &pending.local,
                pending.revision,
                &pending.operation,
                clock.now_ms,
            );
            let active = pending.operation.reply.as_ref().map_or_else(
                || pending.status.remote_command_source_active(),
                |reply| reply.active(),
            );
            if !valid {
                continue;
            } // Ticket drop revokes this exact pending source.
            let completion = match pending
                .ticket
                .source_state(active, true)
                .and_then(|()| pending.ticket.take())
            {
                Ok(Some(text)) => crate::remote_reply::parse(
                    &text,
                    pending.operation.remote.expect("admitted remote command"),
                ),
                Ok(None) => {
                    self.remote_pending.push(pending);
                    continue;
                }
                Err(code) => Ok(crate::remote_reply::failure(code)),
            };
            let completion = completion.unwrap_or_else(|_| crate::remote_reply::failure(-1003));
            if completion.successful {
                self.runtime.complete_remote_command(
                    &pending.local,
                    &pending.operation,
                    clock.now_ms,
                );
            }
            // Media/queue failure does not undo successful physical work or
            // suppress completions from other radios. Normal telemetry is bounded.
            let _ = self.deliver_remote_reply(
                &pending.local,
                pending.revision,
                &pending.operation,
                &completion.parts,
                clock.now_ms,
            );
        }
    }
    fn remote_request_current(
        &mut self,
        local: &str,
        revision: u64,
        operation: &DigitOperation,
        now_ms: u64,
    ) -> bool {
        if !self
            .runtime
            .remote_command_authorized(local, revision, operation, now_ms)
        {
            return false;
        }
        let Some(reply) = operation
            .reply
            .as_ref()
            .filter(|reply| reply.phone_source().is_some())
        else {
            return true;
        };
        let source = reply.phone_source().expect("private source");
        matches!(operation.remote, Some(RemoteCommand::Status { .. }))
            && self.current_private_phone(local, reply).is_ok()
            && self.phone_calls.mailbox(source, reply).is_some()
            && self
                .authorize_phone_account(source.account(), local)
                .is_ok()
    }

    fn deliver_remote_reply(
        &mut self,
        local: &str,
        revision: u64,
        operation: &DigitOperation,
        parts: &[RemoteReplyPart],
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        if let Some(reply) = operation
            .reply
            .as_ref()
            .filter(|reply| reply.phone_source().is_some())
        {
            // Native source/take callbacks can retire a caller. Revalidate after them,
            // before registering a producer; queue_reply rechecks the exact owner again.
            if !self.remote_request_current(local, revision, operation, now_ms) {
                return Err(RuntimeError::Rejected);
            }
            let source = reply.phone_source().expect("private source");
            let stream = self
                .runtime
                .prepare_phone_remote_reply(local, parts, reply)?;
            self.phone_calls
                .queue_reply(source, reply, stream)
                .map_err(|_| RuntimeError::Rejected)
        } else {
            self.runtime
                .queue_remote_reply(local, parts, operation.reply.clone(), now_ms)
        }
    }
}
