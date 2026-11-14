//! Exact-source private digits and prepared replies on serialized station control.
use super::*;
use crate::phone::LoginInput;
use rpt_advanced_core::{
    messages::{OperationResult, PhoneAuthenticationPrompt as Prompt},
    phone_dtmf::{PhoneDtmfAction, PhoneDtmfGrammar, PhoneDtmfOptions},
};

impl Host {
    pub(super) fn current_private_phone(
        &self,
        local: &str,
        reply: &PeerReply,
    ) -> Result<(), RuntimeError> {
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?;
        (reply.phone_authenticated() && self.phone_calls.local(source, reply) == Some(local))
            .then_some(())
            .ok_or(RuntimeError::Rejected)
    }
    /// Deliver only to the original caller; preparation failure never becomes public telemetry.
    pub(crate) fn private_phone_failure(&mut self, reply: &PeerReply) -> Result<(), RuntimeError> {
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?.clone();
        let local = self
            .phone_calls
            .local(&source, reply)
            .ok_or(RuntimeError::Rejected)?
            .to_owned();
        self.current_private_phone(&local, reply)?;
        let result = self.prepare_login_prompt(&source, reply, Prompt::Failed);
        if result.is_err() {
            self.retire_phone(&source);
        }
        result
    }
    /// Mirror only a typed, credential-free node outcome. An explicit caller
    /// remains exact-source; RF-origin outcomes go only to currently attached calls.
    pub(crate) fn node_operation_result(
        &mut self,
        local: &str,
        reply: Option<&PeerReply>,
        outcome: OperationResult,
    ) -> Result<(), RuntimeError> {
        let targets = if let Some(reply) = reply {
            self.current_private_phone(local, reply)?;
            vec![(
                reply.phone_source().ok_or(RuntimeError::Rejected)?.clone(),
                reply.clone(),
            )]
        } else {
            self.phone_calls.attachments(local)
        };
        #[cfg(test)]
        super::tests::phone::before_operation_result();
        for (source, target) in targets {
            // Attachment enumeration and explicit requests both retain exact call authority.
            let delivery = self
                .phone_calls
                .local(&source, &target)
                .ok_or(RuntimeError::Rejected)
                .and_then(|bound| self.current_private_phone(bound, &target))
                .and_then(|()| {
                    self.runtime
                        .prepare_phone_operation_result(local, outcome, &target)
                })
                .and_then(|stream| {
                    self.phone_calls
                        .queue_reply(&source, &target, stream)
                        .map_err(|_| RuntimeError::Rejected)
                });
            if let Err(error) = delivery {
                self.retire_phone(&source);
                if reply.is_some() {
                    return Err(error);
                }
                self.services
                    .scheduler_notice("node operation result: private preparation failed");
            }
        }
        if self.runtime.queue_operation_result(local, outcome).is_err() {
            self.services
                .scheduler_notice("node operation result: local RF preparation failed");
        }
        Ok(())
    }
    pub(super) fn start_private_phone(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
    ) -> Result<(), RuntimeError> {
        let local = self
            .phone_calls
            .local(source, reply)
            .ok_or(RuntimeError::Rejected)?;
        let map = self
            .runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?
            .command_map()
            .map_err(|_| RuntimeError::Preparation)?;
        let grammar = PhoneDtmfGrammar::new(
            &map,
            PhoneDtmfOptions {
                default_command_target: Some(local.to_owned()),
                ..PhoneDtmfOptions::default()
            },
        )
        .map_err(|_| RuntimeError::Preparation)?;
        self.phone_calls
            .start(source, reply, grammar, &mut self.peer_io)
            .map_err(|_| RuntimeError::Preparation)
    }
    pub(super) fn pump_private_phones(&mut self, clock: RuntimeClock) {
        // Bound total work even if a concurrent provider refills its 64-event queue.
        for _ in 0..8192 {
            let Some((source, reply, digit, now_ms)) = self.phone_calls.event() else {
                break;
            };
            #[cfg(test)]
            super::tests::phone::before_event();
            if reply.ended() {
                self.retire_phone(&source);
                continue;
            }
            let result = if reply.phone_authenticated() {
                self.phone_calls
                    .actions(&source, &reply, digit, now_ms)
                    .map_err(|_| RuntimeError::Rejected)
                    .and_then(|actions| self.private_phone_actions(&source, &reply, actions, clock))
            } else {
                self.phone_calls
                    .login_digit(&source, &reply, digit, now_ms)
                    .map_err(|_| RuntimeError::Rejected)
                    .and_then(|input| match input {
                        Some(LoginInput::Password) => {
                            self.prepare_login_prompt(&source, &reply, Prompt::Password)
                        }
                        Some(LoginInput::Credentials { mailbox, pin }) => self
                            .authenticate_phone(&source, &reply, &mailbox, &pin, clock)
                            .map(|_| ()),
                        Some(LoginInput::Retry) => {
                            self.prepare_login_prompt(&source, &reply, Prompt::Retry)
                        }
                        None => Ok(()),
                    })
            };
            if result.is_err() {
                self.retire_phone(&source);
            }
        }
        for (source, reply) in self.phone_calls.login_timeouts(clock.now_ms) {
            if self
                .prepare_login_prompt(&source, &reply, Prompt::Retry)
                .is_err()
            {
                self.retire_phone(&source);
            }
        }
        for (source, reply, action) in self.phone_calls.grammar_timeouts(clock.now_ms) {
            if self
                .private_phone_actions(&source, &reply, vec![action], clock)
                .is_err()
            {
                self.retire_phone(&source);
            }
        }
        self.phone_calls.transfer_replies();
        self.peer_io.reap();
    }
    /// Called only by the ordinary Engine pump, never pending-reload servicing.
    pub(crate) fn reconcile_private_collections(&mut self, clock: RuntimeClock) {
        let runtime = &mut self.runtime;
        self.phone_calls
            .reconcile_collections(|local, source, reply| {
                runtime
                    .node(local)?
                    .links()
                    .phone_collection_pending(source, reply)
            });
        // Run only on ordinary Engine control, not recursively inside pump or
        // pending reload. The old generation is already revoked by its owner.
        if !self.runtime.reload_pending() {
            for local in self.phone_calls.media_refresh_nodes() {
                if self.refresh_inputs(&local, clock, true).is_err() {
                    self.services
                        .scheduler_notice("phone attachment: media preparation failed");
                    for (source, reply) in self.phone_calls.attachments(&local) {
                        self.phone_calls.detach_media(&source, &reply);
                    }
                }
            }
        }
    }
    /// Prepare a typed graph attachment while retaining the existing private
    /// call owner, current attachment policy and proven egress timing.
    pub fn attach_phone(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        target: &str,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let local = self
            .phone_calls
            .local(source, reply)
            .ok_or(RuntimeError::Rejected)?
            .to_owned();
        self.phone_calls
            .mailbox(source, reply)
            .ok_or(RuntimeError::Rejected)?;
        self.authorize_phone_account(source.account(), &local)?;
        if self.runtime.settings(target).is_none()
            || !self
                .runtime
                .document()
                .phone_local_attachment_allowed(source.account(), target)
        {
            return Err(RuntimeError::Rejected);
        }
        if !self
            .phone_calls
            .attach(source, reply, target)
            .map_err(|_| RuntimeError::Rejected)?
        {
            return Ok(());
        }
        if let Err(error) = self.refresh(target, clock) {
            self.phone_calls.detach_media(source, reply);
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn private_phone_actions(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        actions: Vec<PhoneDtmfAction>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        for action in actions {
            let local = self
                .phone_calls
                .local(source, reply)
                .ok_or(RuntimeError::Rejected)?
                .to_owned();
            let unavailable = match action {
                PhoneDtmfAction::Attach(target) => {
                    // Invalid source authority never receives even a denial.
                    self.phone_calls
                        .mailbox(source, reply)
                        .ok_or(RuntimeError::Rejected)?;
                    let target = target.as_deref().unwrap_or(&local);
                    let permitted = self
                        .authorize_phone_account(source.account(), &local)
                        .is_ok()
                        && self.runtime.settings(target).is_some()
                        && self
                            .runtime
                            .document()
                            .phone_local_attachment_allowed(source.account(), target);
                    if !permitted {
                        self.prepare_login_prompt(source, reply, Prompt::Failed)?;
                    } else {
                        let outcome = if self.attach_phone(source, reply, target, clock).is_ok() {
                            OperationResult::Completed
                        } else {
                            OperationResult::Unavailable
                        };
                        self.node_operation_result(&local, Some(reply), outcome)?;
                    }
                    false
                }
                PhoneDtmfAction::SelectTarget(target) => {
                    if target == local {
                        false
                    } else {
                        self.phone_calls.cancel_grammar(source, reply);
                        true
                    }
                }
                PhoneDtmfAction::NodeDigit { target, digit } if target == local => {
                    let node = self.runtime.node(&local).ok_or(RuntimeError::MissingNode)?;
                    if let Some(outcome) =
                        node.links()
                            .phone_digit(source, digit, clock.now_ms, reply.clone())
                    {
                        if matches!(outcome, DigitOutcome::RemoteCandidate(_)) {
                            // Legacy remote selection has no private-origin/reply profile.
                            node.links().clear_source_input(reply);
                            self.phone_calls.cancel_grammar(source, reply);
                            true
                        } else {
                            self.immediate(&local, LinkEffect::Digit(outcome), clock)
                                .is_err()
                        }
                    } else {
                        false
                    }
                }
                PhoneDtmfAction::ExitCommandMode => {
                    self.runtime
                        .node(&local)
                        .ok_or(RuntimeError::MissingNode)?
                        .links()
                        .clear_source_input(reply);
                    false
                }
                _ => true,
            };
            if unavailable {
                self.node_operation_result(&local, Some(reply), OperationResult::Unavailable)?;
            }
        }
        Ok(())
    }
}
