//! Source-owned asynchronous commands for the shared physical radio owner.

use super::*;
use rpt_advanced_core::runtime::dtmf::DigitOperation;

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
        if !self
            .runtime
            .remote_command_authorized(local, revision, &operation, clock.now_ms)
        {
            return Err(RuntimeError::Rejected);
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
            Err(code) => self.runtime.queue_remote_reply(
                local,
                &crate::remote_reply::failure(code).parts,
                operation.reply,
                clock.now_ms,
            ),
        }
    }

    pub(super) fn pump_remote(&mut self, clock: RuntimeClock) {
        for mut pending in std::mem::take(&mut self.remote_pending) {
            let valid = self.runtime.remote_command_authorized(
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
            let _ = self.runtime.queue_remote_reply(
                &pending.local,
                &completion.parts,
                pending.operation.reply,
                clock.now_ms,
            );
        }
    }
}
