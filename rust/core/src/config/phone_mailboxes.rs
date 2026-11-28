//! Mailbox credential and public identity validation.
use argon2::{Argon2, Params, PasswordVerifier, password_hash::PasswordHash};
/// Verify bounded telephone digits against the selected Argon2id PHC.
/// Only the private owner supplies the PHC; neither value is retained.
pub fn verify_mailbox_pin(encoded: &str, pin: &str) -> bool {
    (4..=32).contains(&pin.len())
        && pin.bytes().all(|byte| byte.is_ascii_digit())
        && bounded_hash(encoded).is_some_and(|hash| {
            Argon2::default()
                .verify_password(pin.as_bytes(), &hash)
                .is_ok()
        })
}

/// Check the bounded salted Argon2id PHC contract before retaining a mailbox PIN digest.
pub fn valid_mailbox_pin_hash(value: &str) -> bool {
    bounded_hash(value).is_some()
}

fn bounded_hash(value: &str) -> Option<PasswordHash<'_>> {
    if value.len() > 256 {
        return None;
    }
    PasswordHash::new(value).ok().filter(|hash| {
        hash.algorithm.as_str() == "argon2id"
            && hash.version == Some(19)
            && hash.salt.is_some_and(|salt| {
                let mut decoded = [0; 64];
                salt.decode_b64(&mut decoded)
                    .is_ok_and(|salt| salt.len() >= 16)
            })
            && hash.hash.is_some_and(|output| output.len() == 32)
            && Params::try_from(hash).is_ok_and(|params| {
                (19_456..=65_536).contains(&params.m_cost())
                    && (2..=5).contains(&params.t_cost())
                    && (1..=4).contains(&params.p_cost())
            })
    })
}

pub(super) fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "pin_hash" => valid_mailbox_pin_hash(value),
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
