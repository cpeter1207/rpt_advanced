//! Protocol-qualified peers; operator dial aliases are not their real identities.
use std::fmt;

/// Protocol namespace for a direct peer's real node number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum ProtocolId {
    /// AllStarLink IAX radio link.
    AllStarLink = 1,
    /// EchoLink account/QSO.
    EchoLink = 2,
}

/// An invalid real identity or reserved operator alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityError;
impl fmt::Display for IdentityError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("invalid peer identity")
    }
}
impl std::error::Error for IdentityError {}

/// Canonical identity, scoped by its owner to a local node and connection generation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerIdentity {
    protocol: ProtocolId,
    node: String,
}
impl PeerIdentity {
    /// Parse a real protocol identity, never a prefixed operator dial alias.
    pub fn parse(protocol: ProtocolId, node: &str) -> Result<Self, IdentityError> {
        if !super::decimal_identity(node) {
            return Err(IdentityError);
        }
        let node = match protocol {
            ProtocolId::AllStarLink => node.to_owned(),
            ProtocolId::EchoLink => positive_id(node)?.to_string(),
        };
        Ok(Self { protocol, node })
    }
    /// Resolve the reserved EchoLink dial range before any AllStar directory lookup.
    pub fn from_operator(alias: &str) -> Result<Self, IdentityError> {
        if alias.starts_with('3') && alias.len() >= 7 {
            Self::parse(ProtocolId::EchoLink, &EchoAlias::parse(alias)?.to_string())
        } else {
            Self::parse(ProtocolId::AllStarLink, alias)
        }
    }
    /// Canonical control/configuration key before duplicate, access or retry
    /// comparisons. AllStar spellings are retained; Echo aliases are reversible.
    pub fn canonical_operator(alias: &str) -> Result<String, IdentityError> {
        Self::from_operator(alias).map(|identity| identity.operator_alias())
    }
    /// The protocol namespace, independent of numeric collisions.
    pub fn protocol(&self) -> ProtocolId {
        self.protocol
    }
    /// Real canonical decimal identity, without the operator prefix.
    pub fn node(&self) -> &str {
        &self.node
    }
    /// Canonical dial spelling; all aliases for one EchoLink ID normalize here.
    pub fn operator_alias(&self) -> String {
        match self.protocol {
            ProtocolId::AllStarLink => self.node.clone(),
            ProtocolId::EchoLink => format!("3{:0>6}", self.node),
        }
    }
}

/// Reversible ASL-style EchoLink alias: `3` plus at least six real-ID digits.
pub struct EchoAlias;
impl EchoAlias {
    /// Reject malformed/zero/overflowing aliases without falling through to ASL.
    pub fn parse(digits: &str) -> Result<u32, IdentityError> {
        let real = digits.strip_prefix('3').ok_or(IdentityError)?;
        if !(6..=10).contains(&real.len()) {
            return Err(IdentityError);
        }
        positive_id(real)
    }
    /// Format a previously validated positive ID; zero produces an invalid alias.
    pub fn format(id: u32) -> String {
        format!("3{id:06}")
    }
}

fn positive_id(digits: &str) -> Result<u32, IdentityError> {
    // Both callers establish a nonempty identity before this numeric check.
    if !digits.bytes().all(|digit| digit.is_ascii_digit()) {
        return Err(IdentityError);
    }
    digits
        .parse::<u32>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or(IdentityError)
}
