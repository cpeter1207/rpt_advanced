//! Mailbox verification and prepared prompts on the existing serialized call owner.
use super::*;
use crate::phone::{CallerMailbox, PhoneLogin};
use rpt_advanced_core::config::resources::{
    candidate::{ResourceId, ResourceScope}, generation::RuntimeConfiguration,
    profiles::credentials::CredentialSlot,
};
use crate::{management::native::AccountCredentials, phone::accounts::MailboxChanges};
use std::num::NonZeroU64;
use rpt_advanced_core::{config::resolve_phone_mailboxes, messages::PhoneAuthenticationPrompt};

impl Host {
    pub(crate) fn authorize_phone_owner(&self, local: &str, account: &rpt_advanced_core::link::PhoneAccountId, generation: u64) -> Result<(), RuntimeError> {
        self.runtime.settings(local).ok_or(RuntimeError::MissingNode)?;
        self.phone_accounts.registered(self.services,local,account,generation).then_some(()).ok_or(RuntimeError::Rejected)
    }

    /// Phase-one input comes from the aggregate's exact final credential owner.
    /// An unused preparation can be discarded by finishing without applying it.
    pub(crate) fn prepare_private_accounts(&mut self, attempt: NonZeroU64, next: &RuntimeConfiguration,
        credentials: &AccountCredentials,
    ) -> Result<(), RuntimeError> {
        let old = self.radio_preparation.credentials.as_deref();
        let same = |id, slot| old.is_some_and(|old| old.unchanged(credentials, id, slot));
        let changes = self.runtime.modular_configuration().map_or_else(MailboxChanges::default,
            |active| MailboxChanges::between(active, next, |id| same(id, CredentialSlot::Pin)));
        self.phone_accounts.stage(attempt, self.services, &next.protocols,
            |id| credentials.get(id, CredentialSlot::Phone).map(|secret| secret.expose_bytes()),
            |id| same(id, CredentialSlot::Phone), changes)
            .map_err(|_| RuntimeError::Preparation)
    }
    pub(super) fn reap_abandoned_accounts(&mut self) -> Result<bool, RuntimeError> {
        let Some(owner) = &mut self.radio_preparation.account_owner else { return Ok(true) };
        if owner.key.upgrade().is_some() { return Ok(true); }
        if owner.started { return Ok(false); }
        if !self.echo.abandon(self.services, owner.attempt)? { return Ok(false); }
        if owner.asl {
            if !self.asl.dormant(owner.attempt) || !self.asl.step(self.services, owner.attempt, 4)? { return Ok(false); }
            self.asl.release(owner.attempt)?;
            owner.asl = false;
        }
        if owner.phone && !self.phone_accounts.abandon(owner.attempt).map_err(|_|RuntimeError::Device)? { return Ok(false); }
        self.radio_preparation.account_owner = None;
        Ok(true)
    }
    pub(crate) fn apply_private_accounts(&mut self, attempt: NonZeroU64) -> Result<bool, RuntimeError> {
        self.radio_preparation.account_owner.as_mut().filter(|owner| owner.attempt == attempt)
            .ok_or(RuntimeError::Rejected)?.started = true;
        let result = self.phone_accounts.apply(attempt).map_err(|_| RuntimeError::Device);
        #[cfg(target_os = "linux")]
        self.recheck_phone_dials();
        // Once adoption starts, changed accounts lose ingress and command grants
        // even if a registrar remains pending or the overall transaction reverses.
        self.reconcile_private_calls(|_, _| true);
        result
    }
    pub(crate) fn restore_private_accounts(&mut self, attempt: NonZeroU64) -> Result<bool, RuntimeError> {
        self.phone_accounts.restore(attempt).map_err(|_| RuntimeError::Device)
    }
    pub(crate) fn finish_private_accounts(&mut self, attempt: NonZeroU64,
        mailbox_current: impl FnMut(&str, &CallerMailbox) -> bool,
    ) -> Result<(), RuntimeError> {
        self.phone_accounts.finish(attempt).map_err(|_| RuntimeError::Device)?;
        if let Some(owner) = &mut self.radio_preparation.account_owner { owner.phone = false; }
        self.reconcile_private_calls(mailbox_current);
        Ok(())
    }
    fn reconcile_private_calls(&mut self, mut mailbox_current: impl FnMut(&str, &CallerMailbox) -> bool) {
        let accounts = &self.phone_accounts;
        let ended = self.phone_calls.retain_current(|local, source, mailbox| {
            accounts.admits(local, source.account(), source.account_generation())
                && mailbox.is_none_or(|record| {
                    let retained = record.binding.is_none_or(|binding| local.parse::<ResourceId>()
                        .is_ok_and(|port| accounts.admits_mailbox(port, binding)));
                    retained && mailbox_current(local, record)
                })
        });
        for reply in ended { self.retire_phone_source(&reply); }
    }

    pub(super) fn pump_private_input(&mut self, clock: RuntimeClock) {
        if self.runtime.modular_configuration().is_some() {
        let services=self.services;
        let accounts=&self.phone_accounts;
        let retired=self.phone_calls.retain_current(|local,source,_|
            accounts.registered(services,local,source.account(),source.account_generation()));
        for reply in retired{self.retire_phone_source(&reply);}
        }
        for (local, source, reply, symbol, received) in self.phone_calls.poll_inputs() {
            let result = self.private_digit(&local, &source, &reply, symbol, received, clock);
            if let Err(error)=result {
                let reported=reply.phone_authenticated() && self.authorize_phone_owner(&local,source.account(),source.account_generation()).is_ok()
                    && self.prepare_login_prompt(&source,&reply,match error {
                        RuntimeError::Rejected|RuntimeError::Unavailable=>PhoneAuthenticationPrompt::Unavailable,
                        _=>PhoneAuthenticationPrompt::Failed,
                    }).is_ok();
                if !reported {self.retire_phone(&source);}
            }
        }
        self.phone_calls.settle_inputs(|local, reply| self.runtime.node(local)
            .is_some_and(|node| !node.links().phone_input_active(reply)));
        let active=self.runtime.modular_configuration();
        let expired=self.phone_calls.expired_login_fields(clock.now_ms,|local|{
            let port=local.parse().ok()?;
            Some(active?.scopes.get(&ResourceScope::Port(port))?.commands.language.inactivity_timeout_ms)
        });
        for (source,reply) in expired{
            if self.prepare_login_prompt(&source,&reply,PhoneAuthenticationPrompt::Retry).is_err(){
                self.retire_phone(&source);
            }
        }
    }

    pub(super) fn private_action(&mut self,local:&str,reply:PeerReply,
        action:rpt_advanced_core::command_language::HostAction,clock:RuntimeClock)->Result<(),RuntimeError>{
        use rpt_advanced_core::{command_language::HostAction as H,phone_dtmf::PhoneDtmfAction as P};
        let source=reply.phone_source().ok_or(RuntimeError::Rejected)?.clone();
        if self.phone_calls.local(&source,&reply)!=Some(local){return Err(RuntimeError::Rejected);}
        self.authorize_phone_owner(local,source.account(),source.account_generation())?;
        match action {
            H::PhoneRetire=>self.retire_phone(&source).then_some(()).ok_or(RuntimeError::Rejected),
            H::PhoneAuthenticate(number,pin)=>self.authenticate_phone(&source,&reply,&number,&pin,clock).map(|_|()),
            H::PhoneGrammar(grammar) if self.phone_calls.mailbox(&source,&reply).is_some()=>{
                let domain=self.runtime.modular_configuration().ok_or(RuntimeError::Unavailable)?;
                let id=local.parse().map_err(|_|RuntimeError::Rejected)?;
                let port=domain.protocols.port(id).ok_or(RuntimeError::MissingNode)?;
                let profile=port.phone_patch.as_ref().ok_or(RuntimeError::Rejected)?;
                if profile.binding.id.to_string()!=source.account().as_str(){return Err(RuntimeError::Rejected);}
                // No foreign numeric spelling, mailbox ID, or unregistered remote grants
                // a destination. The called account itself grants its own local port.
                let permitted=|name:&str| name==port.key.as_str() ||
                    port.allstarlink.as_ref().and_then(|a|a.settings.node_number)
                    .is_some_and(|number|number.to_string()==name);
                match grammar.as_ref(){
                    P::Attach(target)=>{
                        if target.as_ref().is_some_and(|t|!permitted(t)){return Err(RuntimeError::Rejected);}
                        if self.phone_calls.attach_radio(&source,&reply).map_err(|_|RuntimeError::Rejected)? {
                            if let Err(error)=self.refresh(local,clock) {
                                self.phone_calls.detach_radio(&source,&reply);
                                return Err(error);
                            }
                        }
                        Ok(())
                    }
                    P::SelectTarget(target) if permitted(target)=>self.phone_calls.select_target(&source,&reply,Some(target.as_str().to_owned())).map_err(|_|RuntimeError::Rejected),
                    P::ExitCommandMode=>self.phone_calls.select_target(&source,&reply,None).map_err(|_|RuntimeError::Rejected),
                    P::NodeDigit{target,digit} if permitted(target) && self.phone_calls.targets(&source,&reply,target)=>{
                        self.private_digit(local,&source,&reply,**digit,clock.now_ms,clock)
                    }
                    // A reverse caller is never its own telephone egress. A manual
                    // key cannot override the configured receiver or its live VAD.
                    P::ManualKey|P::ManualUnkey|P::ManualUnavailable|P::TelephoneDigit(_)=>Err(RuntimeError::Unavailable),
                    _=>Err(RuntimeError::Rejected),
                }
            }
            _=>Err(RuntimeError::Rejected),
        }
    }

    fn private_digit(&mut self, local: &str, source: &PhoneSource, reply: &PeerReply,
        symbol: char, received: u64, clock: RuntimeClock) -> Result<(), RuntimeError> {
        self.authorize_phone_owner(local, source.account(), source.account_generation())?;
        if self.phone_calls.local(source, reply) != Some(local) { return Err(RuntimeError::Rejected); }
        if reply.phone_authenticated() {
            let node = self.runtime.node(local).ok_or(RuntimeError::MissingNode)?;
            let links = node.links();
            let mut ready = links.expire_connection_digits(reply, received);
            if let Some(outcome) = links.phone_digit(source, symbol, received, reply.clone()) {
                ready.push(outcome);
            }
            for outcome in ready { self.immediate(local, LinkEffect::Digit(outcome), clock)?; }
            return Ok(());
        }
        let active = self.runtime.modular_configuration().ok_or(RuntimeError::Rejected)?;
        let port = local.parse().map_err(|_| RuntimeError::Rejected)?;
        let selection = active.scopes.get(&ResourceScope::Port(port)).ok_or(RuntimeError::Rejected)?;
        match self.phone_calls.login_digit(source, reply, symbol, received, selection.commands.language.inactivity_timeout_ms)
            .map_err(|_| RuntimeError::Rejected)? {
            Some(crate::phone::LoginField::Password)=>{
                self.prepare_login_prompt(source,reply,PhoneAuthenticationPrompt::Password)?;
            }
            Some(crate::phone::LoginField::Submitted(number, pin)) => {
                self.authenticate_phone(source, reply, &number, &pin, clock)?;
            }
            Some(crate::phone::LoginField::Expired) => {
                self.prepare_login_prompt(source,reply,PhoneAuthenticationPrompt::Retry)?;
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
        let result = if let Some(active) = self.runtime.modular_configuration() {
            let local = self.phone_calls.local(source, reply).ok_or(RuntimeError::Rejected)?;
            self.authorize_phone_owner(local, source.account(), source.account_generation())?;
            let port: ResourceId = local.parse().map_err(|_| RuntimeError::Rejected)?;
            let selection = active.scopes.get(&ResourceScope::Port(port)).ok_or(RuntimeError::Rejected)?;
            let accounts = &self.phone_accounts;
            let credentials = self.radio_preparation.credentials.as_deref().ok_or(RuntimeError::Rejected)?;
            self.phone_calls.authenticate_scoped(source, reply, &selection.mailboxes,
                |id| accounts.admits_mailbox(port, id).then(|| credentials.get(id, CredentialSlot::Pin)).flatten(),
                (mailbox, pin), clock.now_ms)
        } else {
            let records = resolve_phone_mailboxes(self.runtime.document()).value;
            self.phone_calls.authenticate(source, reply, &records, mailbox, pin, clock.now_ms)
        }.map_err(|_| RuntimeError::Rejected)?;
        #[cfg(test)]
        if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
            let count=if matches!(result,PhoneLogin::Authenticated) { &crate::fixture::PHONE_MAILBOX_VERIFIED }
                else { &crate::fixture::PHONE_MAILBOX_REJECTED };
            count.fetch_add(1,Ordering::Relaxed);
        }
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
        Ok(result)
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
            .queue_reply(source, reply, stream)
            .map_err(|_| RuntimeError::Rejected)
    }
}
