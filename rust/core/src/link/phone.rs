//! Private call identity and generation fences, independent of radio node IDs.
use super::IdentityError;
use std::{fmt, str::FromStr};

/// Opaque nonzero 128-bit token assigned locally, never a provider's claimed node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhoneCallId([u8; 16]);
impl PhoneCallId {
    /// Retain a caller-generated opaque token; all-zero is reserved as invalid.
    pub fn new(bytes: [u8; 16]) -> Result<Self, IdentityError> {
        if bytes == [0; 16] {
            return Err(IdentityError);
        }
        Ok(Self(bytes))
    }
}
impl FromStr for PhoneCallId {
    type Err = IdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 32 {
            return Err(IdentityError);
        }
        let hex = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(IdentityError),
        };
        let mut bytes = [0; 16];
        for (output, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            *output = (hex(pair[0])? << 4) | hex(pair[1])?;
        }
        Self::new(bytes)
    }
}
impl fmt::Display for PhoneCallId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(output, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Configured provider-account identity, distinct from its username or a mailbox.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhoneAccountId(String);
impl PhoneAccountId {
    /// Validate a bounded local account label; credentials never belong here.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(IdentityError);
        }
        Ok(Self(value.into()))
    }
    /// Local account label, never an authentication proof.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Configured mailbox number; login never supplies a node-administration grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxId(String);
impl MailboxId {
    /// Validate the exact bounded mailbox number, retaining leading zeros.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        if value.is_empty() || value.len() > 32 || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(IdentityError);
        }
        Ok(Self(value.into()))
    }
    /// Exact configured number, independent of provider and node identities.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Actual ingress plus provider/call generations retained through dispatch and replies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhoneSource {
    account: PhoneAccountId,
    account_generation: u64,
    call: PhoneCallId,
    call_generation: u64,
    ingress: u64,
}
impl PhoneSource {
    /// Bind copied metadata to a product-assigned actual ingress, never claimed text.
    pub fn new(
        account: PhoneAccountId,
        account_generation: u64,
        call: PhoneCallId,
        call_generation: u64,
        ingress: u64,
    ) -> Result<Self, IdentityError> {
        if account_generation == 0 || call_generation == 0 || ingress == 0 {
            return Err(IdentityError);
        }
        Ok(Self {
            account,
            account_generation,
            call,
            call_generation,
            ingress,
        })
    }
    /// Configured provider account owning this call.
    pub fn account(&self) -> &PhoneAccountId {
        &self.account
    }
    /// Account lifetime, independent of any node operating generation.
    pub fn account_generation(&self) -> u64 {
        self.account_generation
    }
    /// Opaque private call token, without a public dial alias.
    pub fn call_id(&self) -> PhoneCallId {
        self.call
    }
    /// Exact provider call lifetime.
    pub fn call_generation(&self) -> u64 {
        self.call_generation
    }
    /// Product-assigned actual ingress lifetime; never supplied by a remote origin.
    pub fn ingress(&self) -> u64 {
        self.ingress
    }
}
