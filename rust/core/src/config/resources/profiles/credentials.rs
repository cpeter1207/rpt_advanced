//! Account affinity and write-only slot ownership derived from the final typed candidate.

#[cfg(test)]
mod tests;

use super::{
    AllStarLinkProfileSettings, EchoLinkProxyIdentity, PhonePatchProfileSettings, PhoneProtocol,
    ProtocolProfile,
};
use crate::config::resources::candidate::{
    ModularCandidate, ResourceBinding, ResourceId, ResourceKind, ResourceScope,
};
use crate::config::{EchoLinkSettings, phone_mailboxes};
use crate::link::MailboxId;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt};

/// Private slot, independent of path indices in a staged patch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CredentialSlot {
    /// AllStarLink registration and IAX account.
    AllStarLink,
    /// EchoLink directory account.
    Directory,
    /// Separate proxy child and its own credential.
    Proxy,
    /// Phone provider account.
    Phone,
    /// DTMF administrative unlock digest.
    Unlock,
    /// DTMF administrative lock digest.
    Lock,
    /// Complete mailbox identity's PIN digest.
    Pin,
}
impl CredentialSlot {
    /// Durable tag of this encrypted record type.
    pub fn tag(self) -> &'static str {
        match self {
            Self::AllStarLink => "allstarlink-password",
            Self::Directory => "echolink-password",
            Self::Proxy => "proxy-password",
            Self::Phone => "phone-password",
            Self::Unlock => "admin-unlock",
            Self::Lock => "admin-lock",
            Self::Pin => "mailbox-pin",
        }
    }
}

/// Ownership is assigned by the registry; the optional normalized affinity is private-store metadata.
pub struct CredentialOwner {
    /// Stable resource and owning scope.
    pub binding: ResourceBinding,
    /// Owning parent EchoLink resource for a proxy child.
    pub parent: Option<ResourceId>,
    /// Complete account identity; absent while an account is incomplete.
    pub affinity: Option<Value>,
}
impl fmt::Debug for CredentialOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialOwner(..)")
    }
}

/// Final, registry-derived credential ownership and affinity.
pub type CredentialOwners = BTreeMap<(ResourceId, CredentialSlot), CredentialOwner>;

/// An incomplete mailbox identity or invalid set of public protocol accounts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCredentialOwner;

impl ModularCandidate<'_> {
    /// Prepare account identities without reading stored credentials.
    pub fn credential_owners(&self) -> Result<CredentialOwners, InvalidCredentialOwner> {
        use CredentialSlot::*;
        let profiles = self
            .resolve_protocol_profiles()
            .map_err(|_| InvalidCredentialOwner)?;
        let mut owners = BTreeMap::new();
        let mut accounts =
            |asl: Option<&ProtocolProfile<AllStarLinkProfileSettings>>,
             echo: Option<&ProtocolProfile<EchoLinkSettings>>,
             proxy: Option<ResourceBinding>,
             proxy_id: Option<EchoLinkProxyIdentity<'_>>,
             phone: Option<&ProtocolProfile<PhonePatchProfileSettings>>| {
                let mut put = |binding: ResourceBinding, slot, affinity, parent| {
                    owners.insert(
                        (binding.id, slot),
                        CredentialOwner {
                            binding,
                            parent,
                            affinity,
                        },
                    );
                };
                if let Some(p) = asl {
                    put(
                        p.binding,
                        AllStarLink,
                        p.settings
                            .account_identity()
                            .map(|a| json!([a.node_number.get(), a.registration_url])),
                        None,
                    );
                }
                if let Some(p) = echo {
                    put(
                        p.binding,
                        Directory,
                        p.directory_identity()
                            .map(|a| json!([a.callsign, a.node_number, a.directory_servers])),
                        None,
                    );
                }
                if let (Some(child), Some(parent)) = (proxy, echo) {
                    put(
                        child,
                        Proxy,
                        proxy_id.map(|a| json!([a.callsign, a.node_number, a.host, a.port])),
                        Some(parent.binding.id),
                    );
                }
                if let Some(p) = phone {
                    put(
                        p.binding,
                        Phone,
                        p.settings.provider.account_identity().map(|a| {
                            json!([
                                match a.protocol {
                                    PhoneProtocol::Sip => "sip",
                                    PhoneProtocol::Iax2 => "iax2",
                                },
                                a.server,
                                a.port,
                                a.username
                            ])
                        }),
                        None,
                    );
                }
            };
        let site = profiles.site_accounts();
        accounts(
            site.allstarlink.as_ref(),
            site.echolink.as_ref(),
            site.echolink_proxy,
            site.proxy_identity(),
            site.phone_patch.as_ref(),
        );
        for (_, port) in profiles.ports() {
            accounts(
                port.allstarlink.as_ref(),
                port.echolink.as_ref(),
                port.echolink_proxy,
                port.proxy_identity(),
                port.phone_patch.as_ref(),
            );
        }
        for binding in self
            .registry()
            .bindings()
            .filter(|b| b.kind == ResourceKind::Command)
        {
            for slot in [Unlock, Lock] {
                owners.insert(
                    (binding.id, slot),
                    CredentialOwner {
                        binding,
                        parent: None,
                        affinity: Some(Value::Null),
                    },
                );
            }
        }
        for (binding, number) in self
            .registry()
            .members()
            .iter()
            .filter(|(b, _)| b.kind == ResourceKind::Mailbox)
        {
            let fields = self
                .member_fields(binding.id)
                .ok_or(InvalidCredentialOwner)?;
            MailboxId::new(number.as_str()).map_err(|_| InvalidCredentialOwner)?;
            for key in ["first_name", "callsign"] {
                fields
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| phone_mailboxes::value_valid(key, s))
                    .ok_or(InvalidCredentialOwner)?;
            }
            owners.insert(
                (binding.id, Pin),
                CredentialOwner {
                    binding: *binding,
                    parent: None,
                    affinity: Some(json!([number.as_str()])),
                },
            );
        }
        Ok(owners)
    }
}

impl CredentialOwner {
    /// Authenticated payload binding; compare without trusting the index's resource claims.
    pub fn identity(&self, slot: CredentialSlot) -> Value {
        let (kind, id) = match self.binding.scope {
            ResourceScope::Site(id) => ("site", id),
            ResourceScope::Port(id) => ("port", id),
        };
        json!([
            kind,
            id.to_string(),
            self.binding.id.to_string(),
            slot.tag(),
            self.parent.map(|p| p.to_string())
        ])
    }
}
