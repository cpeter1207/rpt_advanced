//! Mailbox login is private to the call. Post-login digits never revisit this state.
use rpt_advanced_core::messages::PhoneAuthenticationPrompt;
use zeroize::Zeroizing;

pub(super) struct Collector {
    number: String,
    pin: Zeroizing<String>,
    password: bool,
    invalid: bool,
    received: Option<u64>,
}
impl Default for Collector {
    fn default() -> Self {
        Self {
            number: String::with_capacity(32),
            pin: Zeroizing::new(String::with_capacity(32)),
            password: false,
            invalid: false,
            received: None,
        }
    }
}
pub(crate) enum LoginField {
    Password,
    Submitted(String, Zeroizing<String>),
    Retry,
    Expired,
}
impl Collector {
    /// Only an untouched field waits for its exact prompt to finish.
    pub(super) fn waiting_for_prompt(&self, prompt: PhoneAuthenticationPrompt) -> bool {
        !self.invalid
            && match prompt {
                PhoneAuthenticationPrompt::Login | PhoneAuthenticationPrompt::Retry => {
                    !self.password && self.number.is_empty()
                }
                PhoneAuthenticationPrompt::Password => self.password && self.pin.is_empty(),
                _ => false,
            }
    }

    pub(super) fn await_prompt(&mut self) {
        self.received = None;
    }

    pub(super) fn prompt_played(&mut self, at: u64) {
        self.received = Some(at);
    }

    pub(super) fn expired(&mut self, now: u64, timeout: u64) -> bool {
        if self
            .received
            .is_some_and(|last| now.saturating_sub(last) >= timeout)
        {
            *self = Self::default();
            true
        } else {
            false
        }
    }
    pub(super) fn digit(&mut self, symbol: char, received: u64) -> Option<LoginField> {
        if symbol == '#' {
            if self.password || self.invalid || self.number.is_empty() {
                let old = std::mem::take(self);
                if old.invalid {
                    return Some(LoginField::Retry);
                }
                return Some(LoginField::Submitted(old.number, old.pin));
            }
            self.password = true;
            self.received = Some(received);
            return Some(LoginField::Password);
        }
        self.received = Some(received);
        let field: &mut String = if self.password {
            &mut self.pin
        } else {
            &mut self.number
        };
        if !self.invalid && symbol.is_ascii_digit() && field.len() < 32 {
            field.push(symbol);
        } else {
            self.invalid = true;
        }
        None
    }
}
