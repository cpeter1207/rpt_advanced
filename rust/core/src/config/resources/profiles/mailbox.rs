//! Public mailbox records and whole-identity inheritance.

use super::{
    CandidateWarning, ModularCandidate, PortKey, ResourceBinding, ResourceKind, ResourceScope,
};
use crate::config::{phone_mailboxes, resources::ResourceWarning};
use crate::link::MailboxId;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// A checked complete public identity, independently bound to its private PIN owner.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PublicMailbox {
    /// Actual declaring resource; a local shadow gets its own identity.
    pub binding: ResourceBinding,
    /// Registered number, preserving leading zeros.
    pub number: MailboxId,
    /// Owner-provided first name.
    pub first_name: String,
    /// Owner-provided callsign.
    pub callsign: String,
}

impl PublicMailbox {
    pub(super) fn decode(
        binding: ResourceBinding,
        number: &PortKey,
        fields: &Map<String, Value>,
    ) -> Option<Self> {
        let field = |key| {
            fields
                .get(key)?
                .as_str()
                .filter(|s| phone_mailboxes::value_valid(key, s))
                .map(str::to_owned)
        };
        Some(Self {
            binding,
            number: MailboxId::new(number.as_str()).ok()?,
            first_name: field("first_name")?,
            callsign: field("callsign")?,
        })
    }

    /// Project the complete public fields; registry metadata stays in the envelope.
    pub fn public_fields(&self) -> Map<String, Value> {
        Map::from_iter([
            ("first_name".into(), Value::String(self.first_name.clone())),
            ("callsign".into(), Value::String(self.callsign.clone())),
        ])
    }
}

/// Mailbox validation failed before private graph adoption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxError {
    /// The requested scope is absent from the candidate.
    MissingScope,
    /// A registered identity has an invalid number or incomplete profile.
    Incomplete(ResourceBinding),
}

/// Effective mailbox identities and source-bound diagnostics for an invoking scope.
pub struct ScopedMailboxes {
    /// Site and port declarations after replacing complete same-key identities.
    pub effective: BTreeMap<PortKey, PublicMailbox>,
    /// Unknown members are ignored without exposing names or values.
    pub warnings: Vec<CandidateWarning>,
}

impl ModularCandidate<'_> {
    /// Resolve complete mailbox declarations, including the site with zero ports.
    pub fn mailbox_profiles(&self, scope: ResourceScope) -> Result<ScopedMailboxes, MailboxError> {
        let site = ResourceScope::Site(self.registry().site_id());
        let port = match scope {
            ResourceScope::Site(_) if scope == site => None,
            ResourceScope::Port(id) if self.port_metadata(id).is_some() => Some(scope),
            _ => return Err(MailboxError::MissingScope),
        };
        let mut effective = BTreeMap::new();
        let mut warnings = Vec::new();
        for owner in [Some(site), port].into_iter().flatten() {
            for (binding, number) in self.registry().members().iter().filter(|(binding, _)| {
                binding.scope == owner && binding.kind == ResourceKind::Mailbox
            }) {
                let fields = self
                    .member_fields(binding.id)
                    .ok_or(MailboxError::Incomplete(*binding))?;
                let mailbox = PublicMailbox::decode(*binding, number, fields)
                    .ok_or(MailboxError::Incomplete(*binding))?;
                warnings.extend(
                    fields
                        .keys()
                        .filter(|key| {
                            !matches!(key.as_str(), "first_name" | "callsign" | "pin_hash")
                        })
                        .map(|_| CandidateWarning {
                            resource: *binding,
                            warning: ResourceWarning::UnknownField,
                        }),
                );
                effective.insert(number.clone(), mailbox);
            }
        }
        Ok(ScopedMailboxes {
            effective,
            warnings,
        })
    }
}
