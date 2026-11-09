//! Prepared mailbox identities and bounded Argon2id PIN verification, off audio.
use super::{ConfigDocument, ConfigWarning, Resolution};
use crate::link::MailboxId;
use argon2::{Argon2, Params, PasswordVerifier, password_hash::PasswordHash};
use std::fmt;

/// One configured mailbox; provider authentication never substitutes for its PIN.
#[derive(Clone, PartialEq, Eq)]
pub struct PhoneMailbox {
    /// Exact mailbox number, retaining leading zeros.
    pub id: MailboxId,
    /// Configured first name, never supplied by a calling party.
    pub first_name: String,
    /// Configured callsign, independent of provider identity.
    pub callsign: String,
    pin_hash: String,
}
impl fmt::Debug for PhoneMailbox {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PhoneMailbox")
            .field("id", &self.id)
            .field("first_name", &self.first_name)
            .field("callsign", &self.callsign)
            .finish_non_exhaustive()
    }
}
impl PhoneMailbox {
    /// Verify 4–32 telephone digits against the prepared salted digest.
    /// This memory-hard operation belongs only to serialized control, never audio.
    pub fn verify_pin(&self, pin: &str) -> bool {
        (4..=32).contains(&pin.len())
            && pin.bytes().all(|byte| byte.is_ascii_digit())
            && PasswordHash::new(&self.pin_hash).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(pin.as_bytes(), &hash)
                    .is_ok()
            })
    }
}

fn valid_hash(value: &str) -> bool {
    if value.len() > 256 {
        return false;
    }
    PasswordHash::new(value).is_ok_and(|hash| {
        hash.algorithm.as_str() == "argon2id"
            && hash.version == Some(19)
            && hash.salt.is_some_and(|salt| {
                let mut decoded = [0; 64];
                salt.decode_b64(&mut decoded)
                    .is_ok_and(|salt| salt.len() >= 16)
            })
            && hash.hash.is_some_and(|output| output.len() == 32)
            && Params::try_from(&hash).is_ok_and(|params| {
                (19_456..=65_536).contains(&params.m_cost())
                    && (2..=5).contains(&params.t_cost())
                    && (1..=4).contains(&params.p_cost())
            })
    })
}

// Schema rejects unknown keys before calling this helper. The resolver calls
// only first_name, callsign and pin_hash; the remaining known case is first_name.
pub(super) fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "pin_hash" => valid_hash(value),
        "callsign" => {
            !value.is_empty()
                && value.len() <= 16
                && value
                    .bytes()
                    .all(|symbol| symbol.is_ascii_alphanumeric() || b"/-".contains(&symbol))
        }
        _ => {
            !value.is_empty()
                && value.len() <= 64
                && value.chars().all(|symbol| !symbol.is_control())
        }
    }
}

/// Prepare global mailbox records. An incomplete/invalid record remains unavailable;
/// no plaintext, empty PIN, or weaker digest can become a permissive fallback.
pub fn resolve_phone_mailboxes(document: &ConfigDocument) -> Resolution<Vec<PhoneMailbox>> {
    let mut value = Vec::new();
    let mut warnings = Vec::new();
    for section in document.named_sections("mailbox", None) {
        let id = section
            .strip_prefix("mailbox ")
            .and_then(|id| MailboxId::new(id).ok());
        let field = |key| {
            document
                .lookup(key, &[section])
                .filter(|value| value_valid(key, value))
        };
        match (id, field("first_name"), field("callsign"), field("pin_hash")) {
            (Some(id), Some(first_name), Some(callsign), Some(pin_hash)) => value.push(PhoneMailbox {
                id,
                first_name: first_name.into(),
                callsign: callsign.into(),
                pin_hash: pin_hash.into(),
            }),
            _ => warnings.push(ConfigWarning::new(0, section, "", "",
                "mailbox requires a valid number, first name, callsign and salted Argon2id PIN hash",
                "mailbox unavailable")),
        }
    }
    Resolution { value, warnings }
}
