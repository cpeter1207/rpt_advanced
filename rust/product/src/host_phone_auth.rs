//! Mailbox verification and prepared prompts on the existing serialized call owner.
use super::*;
use crate::phone::PhoneLogin;
use rpt_advanced_core::{
    config::{PhoneMailbox, resolve_phone_mailboxes},
    messages::PhoneAuthenticationPrompt,
};

impl Host {
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
        let records = resolve_phone_mailboxes(self.runtime.document()).value;
        let result = self
            .phone_calls
            .authenticate(source, reply, &records, mailbox, pin, clock.now_ms)
            .map_err(|_| RuntimeError::Rejected)?;
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
    pub fn phone_mailbox(&self, source: &PhoneSource, reply: &PeerReply) -> Option<&PhoneMailbox> {
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
