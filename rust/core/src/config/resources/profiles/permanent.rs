//! Checked permanent link ownership in the invoking scope.
use super::{
    CandidateWarning, ModularCandidate, PortKey, ResourceBinding, ResourceId, ResourceKind,
    ResourceScope,
    normalize::{self, Rule},
};
use crate::{config::ResolvedPermanentLinkSettings, link::PeerIdentity};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Public permanent declarations after per-field site inheritance.
pub struct PermanentGroups {
    /// Recognized fields bound to each registered authoring resource.
    pub local: BTreeMap<ResourceId, Map<String, Value>>,
    /// Effective priority-ordered routes with their declaring UUIDs.
    pub effective: BTreeMap<PortKey, (ResourceBinding, ResolvedPermanentLinkSettings)>,
    /// Value-free rejected setting diagnostics.
    pub warnings: Vec<CandidateWarning>,
}

fn nodes(raw: &Value) -> Option<Value> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for node in raw.as_array()? {
        let canonical = PeerIdentity::canonical_operator(node.as_str()?).ok()?;
        if !seen.insert(canonical.clone()) {
            return None;
        }
        values.push(Value::String(canonical));
    }
    (!values.is_empty()).then_some(Value::Array(values))
}

impl ModularCandidate<'_> {
    /// Validate required targets and exclusive ownership after merging each visible group.
    /// Caller supplies real protocol account aliases for the invoking scope.
    pub fn permanent_groups(
        &self,
        scope: ResourceScope,
        local_aliases: &BTreeSet<String>,
    ) -> Result<PermanentGroups, ResourceBinding> {
        let site = ResourceScope::Site(self.registry().site_id());
        let mut out = PermanentGroups {
            local: BTreeMap::new(),
            effective: BTreeMap::new(),
            warnings: Vec::new(),
        };
        for owner in [Some(site), (scope != site).then_some(scope)]
            .into_iter()
            .flatten()
        {
            for &(binding, ref key) in self
                .registry()
                .members()
                .iter()
                .filter(|(b, _)| b.scope == owner && b.kind == ResourceKind::PermanentGroup)
            {
                let body = self.member_fields(binding.id).ok_or(binding)?;
                let normalized = normalize::normalize(
                    binding,
                    body,
                    &[
                        ("remote_node", Rule::Value(nodes)),
                        (
                            "group_name",
                            Rule::Value(|v| {
                                v.as_str()
                                    .filter(|s| {
                                        !s.trim().is_empty()
                                            && s.len() <= 63
                                            && !s.chars().any(char::is_control)
                                    })
                                    .map(Value::from)
                            }),
                        ),
                    ],
                    &mut out.warnings,
                );
                let entry = out.effective.entry(key.clone()).or_insert_with(|| {
                    (
                        binding,
                        ResolvedPermanentLinkSettings {
                            name: key.as_str().into(),
                            remote_nodes: Vec::new(),
                            group_name: None,
                        },
                    )
                });
                entry.0 = binding;
                if let Some(nodes) = normalized.get("remote_node").and_then(Value::as_array) {
                    entry.1.remote_nodes = nodes
                        .iter()
                        .map(|n| n.as_str().expect("normalized node").to_owned())
                        .collect();
                }
                if let Some(name) = normalized.get("group_name").and_then(Value::as_str) {
                    entry.1.group_name = Some(name.into());
                }
                out.local.insert(binding.id, normalized);
            }
        }
        let mut owned = BTreeSet::new();
        for (binding, group) in out.effective.values() {
            if group.remote_nodes.is_empty()
                || group
                    .remote_nodes
                    .iter()
                    .any(|n| local_aliases.contains(n) || !owned.insert(n))
            {
                return Err(*binding);
            }
        }
        Ok(out)
    }
}
