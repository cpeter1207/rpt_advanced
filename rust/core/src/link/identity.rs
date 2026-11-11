//! Protocol-qualified peers; operator dial aliases are not their real identities.
use std::fmt;

/// Protocol namespace for a direct peer's real address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum ProtocolId {
    /// AllStarLink IAX radio link.
    AllStarLink = 1,
    /// EchoLink account/QSO.
    EchoLink = 2,
    /// Private telephone call, without a public node/dial alias.
    Phone = 3,
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
    address: PeerAddress,
}

/// A public protocol node or a private opaque call; these namespaces never alias.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PeerAddress {
    /// Canonical decimal protocol node number.
    Node(String),
    /// Locally assigned opaque phone call token.
    PhoneCall(super::PhoneCallId),
}
impl PeerIdentity {
    /// Retain a validated opaque private-call token without a public alias.
    pub fn for_phone(call: super::PhoneCallId) -> Self {
        Self {
            protocol: ProtocolId::Phone,
            address: PeerAddress::PhoneCall(call),
        }
    }
    /// Parse a real protocol identity, never a prefixed operator dial alias.
    pub fn parse(protocol: ProtocolId, node: &str) -> Result<Self, IdentityError> {
        if protocol != ProtocolId::Phone && !super::decimal_identity(node) {
            return Err(IdentityError);
        }
        let address = match protocol {
            ProtocolId::AllStarLink => PeerAddress::Node(node.to_owned()),
            ProtocolId::EchoLink => PeerAddress::Node(positive_id(node)?.to_string()),
            ProtocolId::Phone => PeerAddress::PhoneCall(node.parse()?),
        };
        Ok(Self { protocol, address })
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
        Self::from_operator(alias)
            .and_then(|identity| identity.operator_alias().ok_or(IdentityError))
    }
    /// The protocol namespace, independent of numeric collisions.
    pub fn protocol(&self) -> ProtocolId {
        self.protocol
    }
    /// Typed address; a phone is never represented as a numeric node.
    pub fn address(&self) -> &PeerAddress {
        &self.address
    }
    /// Real decimal identity when this protocol has a node namespace.
    pub fn node(&self) -> Option<&str> {
        match &self.address {
            PeerAddress::Node(node) => Some(node),
            PeerAddress::PhoneCall(_) => None,
        }
    }
    /// Opaque call identity, absent for public protocol nodes.
    pub fn call_id(&self) -> Option<super::PhoneCallId> {
        match self.address {
            PeerAddress::PhoneCall(call) => Some(call),
            PeerAddress::Node(_) => None,
        }
    }
    /// Canonical copied provider metadata spelling, never an authorization identity.
    pub fn wire_identity(&self) -> String {
        match &self.address {
            PeerAddress::Node(node) => node.clone(),
            PeerAddress::PhoneCall(call) => call.to_string(),
        }
    }
    /// Canonical dial spelling; all aliases for one EchoLink ID normalize here.
    pub fn operator_alias(&self) -> Option<String> {
        match (self.protocol, &self.address) {
            (ProtocolId::AllStarLink, PeerAddress::Node(node)) => Some(node.clone()),
            (ProtocolId::EchoLink, PeerAddress::Node(node)) => Some(format!("3{node:0>6}")),
            _ => None,
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
