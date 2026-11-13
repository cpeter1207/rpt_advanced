//! Exact-source private digits and prepared replies on serialized station control.
use super::*;
use crate::phone::LoginInput;
use rpt_advanced_core::{
    messages::PhoneAuthenticationPrompt as Prompt,
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
    pub(crate) fn private_phone_result(
        &mut self,
        reply: &PeerReply,
        completed: bool,
    ) -> Result<(), RuntimeError> {
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?.clone();
        let local = self
            .phone_calls
            .local(&source, reply)
            .ok_or(RuntimeError::Rejected)?
            .to_owned();
        self.current_private_phone(&local, reply)?;
        let result = self.prepare_login_prompt(
            &source,
            reply,
            if completed {
                Prompt::Completed
            } else {
                Prompt::Failed
            },
        );
        if result.is_err() {
            self.retire_phone(&source);
        }
        result
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
    pub(crate) fn reconcile_private_collections(&mut self) {
        let runtime = &mut self.runtime;
        self.phone_calls
            .reconcile_collections(|local, source, reply| {
                runtime
                    .node(local)?
                    .links()
                    .phone_collection_pending(source, reply)
            });
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
                    self.prepare_login_prompt(
                        source,
                        reply,
                        if permitted {
                            Prompt::Unavailable
                        } else {
                            Prompt::Failed
                        },
                    )?;
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
                self.prepare_login_prompt(source, reply, Prompt::Unavailable)?;
            }
        }
        Ok(())
    }
}
