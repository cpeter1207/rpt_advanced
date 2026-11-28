//! Keep the selecting collector on control while the captured ASL directory resolves off-thread.
use super::{Host, HostServices, LinkEffect, RuntimeClock, RuntimeError, asl, received_effect};
use crate::directory::{ManagedDirectory, VerifiedRoute};
use rpt_advanced_core::{
    config::resources::candidate::ResourceId,
    runtime::dtmf::{DigitEvent, DigitOperation},
};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};

pub(crate) struct Lookup {
    directory: Arc<ManagedDirectory>,
    remote: String,
    answer: SyncSender<Option<VerifiedRoute>>,
}
impl Lookup {
    pub(crate) fn resolve(self, services: HostServices) {
        let proof = self.directory.resolve(services, &self.remote, None).ok();
        let _ = self.answer.send(proof);
    }
}
pub(super) struct Pending {
    station: ResourceId,
    local: String,
    directory: Arc<ManagedDirectory>,
    revision: u64,
    operation: DigitOperation,
    ticket: Arc<()>,
    work: Option<Lookup>,
    response: Receiver<Option<VerifiedRoute>>,
    receipt: Option<rpt_advanced_core::runtime::CommandReceipt>,
}
impl Host {
    pub(super) fn begin_target_selection(
        &mut self,
        local: &str,
        operation: DigitOperation,
        account: asl::Account,
        now: u64,
        receipt: Option<rpt_advanced_core::runtime::CommandReceipt>,
    ) -> Result<LinkEffect, RuntimeError> {
        let ticket = self
            .runtime
            .node(local)
            .and_then(|n| n.links().begin_command_selection(&operation, now))
            .ok_or(RuntimeError::Rejected)?;
        let (answer, response) = mpsc::sync_channel(1);
        let work = Lookup {
            directory: account.directory.clone(),
            remote: operation.command.node.clone(),
            answer,
        };
        self.target_selections.push(Pending {
            station: account.station,
            local: local.to_owned(),
            directory: account.directory,
            revision: self.runtime.revision(),
            operation,
            ticket,
            work: Some(work),
            receipt,
            response,
        });
        Ok(LinkEffect::None)
    }
    pub(crate) fn has_target_selections(&self) -> bool {
        !self.target_selections.is_empty()
    }
    pub(crate) fn cancel_target_selections(&mut self) {
        for pending in self.target_selections.drain(..) {
            if let Some(node) = self.runtime.node(&pending.local) {
                node.links()
                    .finish_command_selection(&pending.operation, &pending.ticket);
            }
            // Dropping the pending receipt rejects this macro step; the worker owns no generation.
        }
    }
    pub(crate) fn take_target_lookups(&mut self) -> Vec<Lookup> {
        self.target_selections
            .iter_mut()
            .filter_map(|p| p.work.take())
            .collect()
    }
    pub(super) fn poll_target_selections(
        &mut self,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        if self.target_selections.is_empty() {
            return Ok(());
        }
        for pending in std::mem::take(&mut self.target_selections) {
            let op = &pending.operation;
            let remote = &op.command.node;
            let current = !self.runtime.reload_pending()
                && self.runtime.revision() == pending.revision
                && self.asl_account(pending.station).is_ok_and(|a| {
                    a.permits(remote) && Arc::ptr_eq(&a.directory, &pending.directory)
                });
            let Some(node) = self.runtime.node(&pending.local) else {
                continue;
            };
            if !current
                || !node
                    .links()
                    .command_selection_current(op, &pending.ticket, clock.now_ms)
            {
                node.links().finish_command_selection(op, &pending.ticket);
                continue;
            }
            let proof = match pending.response.try_recv() {
                Ok(proof) => proof,
                Err(TryRecvError::Empty) => {
                    self.target_selections.push(pending);
                    continue;
                }
                Err(TryRecvError::Disconnected) => None,
            };
            let Some(held) = node.links().finish_command_selection(op, &pending.ticket) else {
                continue;
            };
            let verified =
                proof.is_some_and(|p| p.destination(&pending.directory, remote, None).is_some());
            // A directory miss leaves normal collection in effect for subsequent input.
            let result = self
                .runtime
                .command(&pending.local, op.clone(), clock, verified);
            if let Some(receipt) = pending.receipt {
                receipt.settle(result.is_ok());
            }
            for input in held {
                let at = match input.event {
                    DigitEvent::Digit { now_ms, .. } | DigitEvent::Unkey { now_ms } => now_ms,
                    DigitEvent::Lost => continue,
                };
                if let Some(outcome) = self
                    .runtime
                    .node(&pending.local)
                    .and_then(|n| n.links().expire_command_input(op, at))
                {
                    self.immediate(&pending.local, LinkEffect::Digit(outcome), clock)?;
                }
                if let Some(outcome) = self
                    .runtime
                    .node(&pending.local)
                    .and_then(|n| n.links().replay_command_input(op, &input))
                {
                    self.immediate(
                        &pending.local,
                        received_effect(LinkEffect::Digit(outcome), input.regeneration.clone()),
                        clock,
                    )?;
                }
            }
        }
        Ok(())
    }
}
