//! Mailbox verification and prepared prompts on the existing serialized call owner.
use super::*;
use crate::{
    link::private_phone::Digit,
    phone::{CallerMailbox, PhoneLogin},
};
use crate::{management::native::AccountCredentials, phone::accounts::MailboxChanges};
use rpt_advanced_core::config::resources::{
    candidate::{ResourceId, ResourceScope},
    generation::RuntimeConfiguration,
    profiles::credentials::CredentialSlot,
};
use rpt_advanced_core::messages::PhoneAuthenticationPrompt;
use std::num::NonZeroU64;

impl Host {
    pub(super) fn current_phone_reply(
        &self,
        local: &str,
        reply: &PeerReply,
    ) -> Result<(), RuntimeError> {
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?;
        (reply.phone_authenticated() && self.phone_calls.local(source, reply) == Some(local))
            .then_some(())
            .ok_or(RuntimeError::Rejected)
    }

    pub(super) fn phone_link_event(
        &mut self,
        local: &str,
        remote: &str,
        connected: bool,
        echo_callsign: Option<&str>,
        reply: &PeerReply,
    ) -> Result<bool, RuntimeError> {
        self.current_phone_reply(local, reply)?;
        let caller = reply.phone_source().ok_or(RuntimeError::Rejected)?;
        if let Some(stream) =
            self.runtime
                .prepare_phone_link_event(local, remote, connected, echo_callsign, reply)?
        {
            self.queue_phone_reply(caller, reply, stream)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(super) fn phone_operation_result(
        &mut self,
        local: &str,
        reply: &PeerReply,
        result: OperationResult,
    ) -> Result<(), RuntimeError> {
        self.current_phone_reply(local, reply)?;
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?;
        let stream = self
            .runtime
            .prepare_phone_operation_result(local, result, reply)?;
        self.queue_phone_reply(source, reply, stream)
    }

    pub(crate) fn authorize_phone_owner(
        &self,
        local: &str,
        account: &rpt_advanced_core::link::PhoneAccountId,
        generation: u64,
    ) -> Result<(), RuntimeError> {
        self.runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        self.phone_accounts
            .registered(self.services, local, account, generation)
            .then_some(())
            .ok_or(RuntimeError::Rejected)
    }

    /// Phase-one input comes from the aggregate's exact final credential owner.
    /// An unused preparation can be discarded by finishing without applying it.
    pub(crate) fn prepare_private_accounts(
        &mut self,
        attempt: NonZeroU64,
        next: &RuntimeConfiguration,
        credentials: &AccountCredentials,
    ) -> Result<(), RuntimeError> {
        let old = self.radio_preparation.credentials.as_deref();
        let same = |id, slot| old.is_some_and(|old| old.unchanged(credentials, id, slot));
        let changes = self
            .runtime
            .modular_configuration()
            .map_or_else(MailboxChanges::default, |active| {
                MailboxChanges::between(active, next, |id| same(id, CredentialSlot::Pin))
            });
        self.phone_accounts
            .stage(
                attempt,
                self.services,
                &next.protocols,
                |id| {
                    credentials
                        .get(id, CredentialSlot::Phone)
                        .map(|secret| secret.expose_bytes())
                },
                |id| same(id, CredentialSlot::Phone),
                changes,
            )
            .map_err(|_| RuntimeError::Preparation)
    }
    pub(super) fn reap_abandoned_accounts(&mut self) -> Result<bool, RuntimeError> {
        let Some(owner) = &mut self.radio_preparation.account_owner else {
            return Ok(true);
        };
        if owner.key.upgrade().is_some() {
            return Ok(true);
        }
        if owner.started {
            return Ok(false);
        }
        if !self.echo.abandon(self.services, owner.attempt)? {
            return Ok(false);
        }
        if owner.asl {
            if !self.asl.dormant(owner.attempt)
                || !self.asl.step(self.services, owner.attempt, 4)?
            {
                return Ok(false);
            }
            self.asl.release(owner.attempt)?;
            owner.asl = false;
        }
        if owner.phone
            && !self
                .phone_accounts
                .abandon(owner.attempt)
                .map_err(|_| RuntimeError::Device)?
        {
            return Ok(false);
        }
        self.radio_preparation.account_owner = None;
        Ok(true)
    }
    pub(crate) fn apply_private_accounts(
        &mut self,
        attempt: NonZeroU64,
    ) -> Result<bool, RuntimeError> {
        self.radio_preparation
            .account_owner
            .as_mut()
            .filter(|owner| owner.attempt == attempt)
            .ok_or(RuntimeError::Rejected)?
            .started = true;
        let result = self
            .phone_accounts
            .apply(attempt)
            .map_err(|_| RuntimeError::Device);
        #[cfg(target_os = "linux")]
        self.recheck_phone_dials();
        // Once adoption starts, changed accounts lose ingress and command grants
        // even if a registrar remains pending or the overall transaction reverses.
        self.reconcile_private_calls(|_, _| true);
        result
    }
    pub(crate) fn restore_private_accounts(
        &mut self,
        attempt: NonZeroU64,
    ) -> Result<bool, RuntimeError> {
        self.phone_accounts
            .restore(attempt)
            .map_err(|_| RuntimeError::Device)
    }
    pub(crate) fn finish_private_accounts(
        &mut self,
        attempt: NonZeroU64,
        mailbox_current: impl FnMut(&str, &CallerMailbox) -> bool,
    ) -> Result<(), RuntimeError> {
        self.phone_accounts
            .finish(attempt)
            .map_err(|_| RuntimeError::Device)?;
        if let Some(owner) = &mut self.radio_preparation.account_owner {
            owner.phone = false;
        }
        self.reconcile_private_calls(mailbox_current);
        Ok(())
    }
    fn reconcile_private_calls(
        &mut self,
        mut mailbox_current: impl FnMut(&str, &CallerMailbox) -> bool,
    ) {
        let accounts = &self.phone_accounts;
        let ended = self.phone_calls.retain_current(|local, source, mailbox| {
            accounts.admits(local, source.account(), source.account_generation())
                && mailbox.is_none_or(|record| {
                    let retained = record.binding.is_none_or(|binding| {
                        local
                            .parse::<ResourceId>()
                            .is_ok_and(|port| accounts.admits_mailbox(port, binding))
                    });
                    retained && mailbox_current(local, record)
                })
        });
        for reply in ended {
            self.retire_phone_source(&reply);
        }
    }

    pub(super) fn pump_private_input(&mut self, clock: RuntimeClock) {
        if self.runtime.modular_configuration().is_some() {
            let services = self.services;
            let accounts = &self.phone_accounts;
            let retired = self.phone_calls.retain_current(|local, source, _| {
                accounts.registered(
                    services,
                    local,
                    source.account(),
                    source.account_generation(),
                )
            });
            for reply in retired {
                self.retire_phone_source(&reply);
            }
        }
        for (local, source, reply, digit) in self.phone_calls.poll_inputs() {
            let result = self.private_digit(&local, &source, &reply, digit, clock);
            if let Err(error) = result {
                let reported = reply.phone_authenticated()
                    && self
                        .authorize_phone_owner(
                            &local,
                            source.account(),
                            source.account_generation(),
                        )
                        .is_ok()
                    && self
                        .prepare_login_prompt(
                            &source,
                            &reply,
                            match error {
                                RuntimeError::Rejected | RuntimeError::Unavailable => {
                                    PhoneAuthenticationPrompt::Unavailable
                                }
                                _ => PhoneAuthenticationPrompt::Failed,
                            },
                        )
                        .is_ok();
                if !reported {
                    self.retire_phone(&source);
                }
            }
        }
        self.phone_calls.settle_inputs(|local, reply| {
            self.runtime
                .node(local)
                .is_some_and(|node| !node.links().phone_input_active(reply))
        });
        let active = self.runtime.modular_configuration();
        let expired = self
            .phone_calls
            .expired_login_fields(clock.now_ms, |local| {
                let port = local.parse().ok()?;
                Some(
                    active?
                        .scopes
                        .get(&ResourceScope::Port(port))?
                        .commands
                        .language
                        .inactivity_timeout_ms,
                )
            });
        for (source, reply) in expired {
            if self
                .prepare_login_prompt(&source, &reply, PhoneAuthenticationPrompt::Retry)
                .is_err()
            {
                self.retire_phone(&source);
            }
        }
    }

    pub(super) fn private_action(
        &mut self,
        local: &str,
        reply: PeerReply,
        action: rpt_advanced_core::command_language::HostAction,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        use rpt_advanced_core::{
            command_language::HostAction as H, phone_dtmf::PhoneDtmfAction as P,
        };
        let source = reply.phone_source().ok_or(RuntimeError::Rejected)?.clone();
        if self.phone_calls.local(&source, &reply) != Some(local) {
            return Err(RuntimeError::Rejected);
        }
        self.authorize_phone_owner(local, source.account(), source.account_generation())?;
        match action {
            H::PhoneRetire => self
                .retire_phone(&source)
                .then_some(())
                .ok_or(RuntimeError::Rejected),
            H::PhoneAuthenticate(number, pin) => self
                .authenticate_phone(&source, &reply, &number, &pin, clock)
                .map(|_| ()),
            H::PhoneGrammar(grammar) => {
                let domain = self
                    .runtime
                    .modular_configuration()
                    .ok_or(RuntimeError::Unavailable)?;
                let id = local.parse().map_err(|_| RuntimeError::Rejected)?;
                let port = domain.protocols.port(id).ok_or(RuntimeError::MissingNode)?;
                let profile = port.phone_patch.as_ref().ok_or(RuntimeError::Rejected)?;
                let mailbox = self
                    .phone_calls
                    .mailbox(&source, &reply)
                    .ok_or(RuntimeError::Rejected)?;
                if profile.binding.id.to_string() != source.account().as_str()
                    || !profile.settings.allows_mailbox(&mailbox.id)
                {
                    return Err(RuntimeError::Rejected);
                }
                match grammar.as_ref() {
                    P::Attach(None) => self
                        .phone_calls
                        .attach_radio(&source, &reply)
                        .map_err(|_| RuntimeError::Rejected),
                    // Historical grammar actions confer no routing or keying authority.
                    _ => Err(RuntimeError::Unavailable),
                }
            }
            _ => Err(RuntimeError::Rejected),
        }
    }

    fn private_digit(
        &mut self,
        local: &str,
        source: &PhoneSource,
        reply: &PeerReply,
        digit: Digit,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        self.authorize_phone_owner(local, source.account(), source.account_generation())?;
        if self.phone_calls.local(source, reply) != Some(local) {
            return Err(RuntimeError::Rejected);
        }
        if reply.phone_authenticated() {
            let node = self.runtime.node(local).ok_or(RuntimeError::MissingNode)?;
            let links = node.links();
            let mut ready = links.expire_connection_digits(reply, digit.received);
            if let Some(outcome) =
                links.phone_digit(source, digit.symbol, digit.received, reply.clone())
            {
                ready.push(outcome);
            }
            for outcome in ready {
                self.immediate(local, LinkEffect::Digit(outcome), clock)?;
            }
            return Ok(());
        }
        let active = self
            .runtime
            .modular_configuration()
            .ok_or(RuntimeError::Rejected)?;
        let port = local.parse().map_err(|_| RuntimeError::Rejected)?;
        let selection = active
            .scopes
            .get(&ResourceScope::Port(port))
            .ok_or(RuntimeError::Rejected)?;
        match self
            .phone_calls
            .login_digit(
                source,
                reply,
                digit,
                selection.commands.language.inactivity_timeout_ms,
            )
            .map_err(|_| RuntimeError::Rejected)?
        {
            Some(crate::phone::LoginField::Password) => {
                self.prepare_login_prompt(source, reply, PhoneAuthenticationPrompt::Password)?;
            }
            Some(crate::phone::LoginField::Submitted(number, pin)) => {
                self.authenticate_phone(source, reply, &number, &pin, clock)?;
            }
            Some(crate::phone::LoginField::Expired) => {
                self.prepare_login_prompt(source, reply, PhoneAuthenticationPrompt::Retry)?;
            }
            Some(crate::phone::LoginField::Retry) => {
                // Account and mailbox existence are intentionally undisclosed.
                self.authenticate_phone(source, reply, "", "", clock)?;
            }
            None => {}
        }
        Ok(())
    }
    /// Verify the currently configured mailbox/PIN against the exact admitted call.
    /// PINs remain borrowed, unlogged and absent from replies, node parsers and grants.
    pub fn authenticate_phone(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        mailbox: &str,
        pin: &str,
        clock: RuntimeClock,
    ) -> Result<PhoneLogin, RuntimeError> {
        let active = self
            .runtime
            .modular_configuration()
            .ok_or(RuntimeError::Rejected)?;
        let result = {
            let local = self
                .phone_calls
                .local(source, reply)
                .ok_or(RuntimeError::Rejected)?;
            self.authorize_phone_owner(local, source.account(), source.account_generation())?;
            let port: ResourceId = local.parse().map_err(|_| RuntimeError::Rejected)?;
            let selection = active
                .scopes
                .get(&ResourceScope::Port(port))
                .ok_or(RuntimeError::Rejected)?;
            let profile = active
                .protocols
                .port(port)
                .and_then(|port| port.phone_patch.as_ref())
                .ok_or(RuntimeError::Rejected)?;
            let accounts = &self.phone_accounts;
            let credentials = self
                .radio_preparation
                .credentials
                .as_deref()
                .ok_or(RuntimeError::Rejected)?;
            self.phone_calls.authenticate_scoped(
                source,
                reply,
                &selection.mailboxes,
                |record| {
                    let id = record.binding.id;
                    if !accounts.admits_mailbox(port, id)
                        || !profile.settings.allows_mailbox(&record.number)
                    {
                        return None;
                    }
                    credentials.get(id, CredentialSlot::Pin)
                },
                (mailbox, pin),
                clock.now_ms,
            )
        }
        .map_err(|_| RuntimeError::Rejected)?;
        #[cfg(test)]
        if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
            let count = if matches!(result, PhoneLogin::Authenticated) {
                &crate::fixture::PHONE_MAILBOX_VERIFIED
            } else {
                &crate::fixture::PHONE_MAILBOX_REJECTED
            };
            count.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(target_os = "linux")]
        self.collect_phone_status_edges(clock.now_ms);
        let prompt = match result {
            PhoneLogin::Authenticated => PhoneAuthenticationPrompt::Authenticated,
            PhoneLogin::Retry => PhoneAuthenticationPrompt::Retry,
            PhoneLogin::Closed => {
                self.retire_phone(source);
                return Ok(result);
            }
        };
        if let Err(error) = self.prepare_login_prompt(source, reply, prompt) {
            self.retire_phone(source);
            return Err(error);
        }
        if result == PhoneLogin::Authenticated
            && self.phone_calls.attach_radio(source, reply).is_err()
        {
            self.retire_phone(source);
            return Err(RuntimeError::Rejected);
        }
        Ok(result)
    }

    /// Coalesce calls on each port and retry busy generations on later turns.
    pub(super) fn poll_phone_attachments(&mut self, clock: RuntimeClock) {
        for (local, mut pending) in self.phone_calls.pending_radio_attachments() {
            self.exclude_active_phone_redirects(&local, clock.now_ms, &mut pending);
            if pending.is_empty() {
                continue;
            }
            match self.try_refresh(&local, clock) {
                Ok(false) => continue,
                Ok(true) => self.exclude_active_phone_redirects(&local, clock.now_ms, &mut pending),
                Err(_) => {}
            }
            if !pending.is_empty() {
                for (source, reply) in self.phone_calls.fail_radio_attachments(&local, &pending) {
                    if self
                        .prepare_login_prompt(&source, &reply, PhoneAuthenticationPrompt::Failed)
                        .is_err()
                    {
                        self.retire_phone(&source);
                    }
                }
            }
        }
    }

    /// An active adapter already owns these exact redirects until each call worker acknowledges.
    fn exclude_active_phone_redirects(
        &mut self,
        local: &str,
        now_ms: u64,
        pending: &mut std::collections::BTreeSet<usize>,
    ) {
        if let Some(node) = self.runtime.node(local)
            && let Some(generation) = node.status(now_ms).active
            && let Some(control) = node.adapter_control_mut(generation)
        {
            for route in &control.phones {
                pending.remove(&route.grant().identity());
            }
        }
    }

    /// Return only the configured profile of an authenticated, current call.
    pub fn phone_mailbox(&self, source: &PhoneSource, reply: &PeerReply) -> Option<&CallerMailbox> {
        self.phone_calls.mailbox(source, reply)
    }
    pub(super) fn prepare_login_prompt(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        prompt: PhoneAuthenticationPrompt,
    ) -> Result<(), RuntimeError> {
        let local = self
            .phone_calls
            .local(source, reply)
            .ok_or(RuntimeError::Rejected)?
            .to_owned();
        let stream = self
            .runtime
            .prepare_private_phone_prompt(&local, prompt, reply)?;
        self.phone_calls
            .queue_login_prompt(source, reply, stream, prompt)
            .map_err(|_| RuntimeError::Rejected)
    }
}
