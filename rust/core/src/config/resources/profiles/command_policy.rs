//! Prepare the public command fields used by the grant, PCM and tone owners.

use super::normalize::{Fields, Rule, normalize_fields};
use crate::config::{
    ResolvedDtmfRegenerationSettings,
    resources::candidate::{
        CandidateWarning, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
        ResourceScope,
    },
};
use serde_json::{Map, Value, json};

const FIELDS: Fields = &[
    ("muting", Rule::Bool),
    (
        "admin",
        Rule::Object(&[(
            "timing",
            Rule::Object(&[("timeout_ms", Rule::Unsigned(1, u64::MAX))]),
        )]),
    ),
    (
        "regeneration",
        Rule::Object(&[
            ("enabled", Rule::Bool),
            (
                "timing",
                Rule::Object(&[
                    ("duration_ms", Rule::Unsigned(1, 1000)),
                    ("gap_ms", Rule::Unsigned(1, 1000)),
                ]),
            ),
            (
                "level_dbfs",
                Rule::Value(|v| {
                    v.as_i64()
                        .filter(|n| (-60..=0).contains(n))
                        .map(Value::from)
                }),
            ),
        ]),
    ),
];

/// Effective command policy for a site or port with sparse public source layers.
pub struct ScopedCommandPolicy {
    /// Mute detected local keypad frames at the PCM owner.
    pub muting: bool,
    /// Idle lifetime of a live administration grant.
    pub admin_timeout_ms: u64,
    /// Settings for regeneration on the local transmitter.
    pub regeneration: ResolvedDtmfRegenerationSettings,
    /// Value-free diagnostics tied to the actual authoring resource.
    pub warnings: Vec<CandidateWarning>,
    layers: Vec<(ResourceBinding, Map<String, Value>)>,
}

impl ScopedCommandPolicy {
    /// Get recognized sparse authoring data without materializing inherited settings.
    pub fn local(&self, id: ResourceId) -> Option<&Map<String, Value>> {
        self.layers
            .iter()
            .find(|(b, _)| b.id == id)
            .map(|(_, fields)| fields)
    }

    fn leaf(&self, path: &[&str]) -> Option<(ResourceBinding, &Value)> {
        let (first, rest) = path.split_first()?;
        self.layers.iter().rev().find_map(|(b, fields)| {
            let value = rest
                .iter()
                .try_fold(fields.get(*first)?, |v, k| v.get(*k))?;
            Some((*b, value))
        })
    }

    /// Return the authoring binding for an effective leaf, or None for a built-in.
    pub fn source(&self, path: &[&str]) -> Option<ResourceBinding> {
        self.leaf(path).map(|(binding, _)| binding)
    }

    /// Serialize only typed effective public leaves for a command HAL representation.
    pub fn effective_fields(&self) -> Map<String, Value> {
        Map::from_iter([
            ("muting".into(), json!(self.muting)),
            (
                "admin".into(),
                json!({"timing":{"timeout_ms":self.admin_timeout_ms}}),
            ),
            (
                "regeneration".into(),
                json!({
                    "enabled":self.regeneration.enabled,
                    "level_dbfs":self.regeneration.level_dbfs,
                    "timing":{"duration_ms":self.regeneration.duration_ms,"gap_ms":self.regeneration.gap_ms}
                }),
            ),
        ])
    }
}

impl ModularCandidate<'_> {
    /// Resolve policy in the real site namespace or an existing port's inherited namespace.
    pub fn command_policy(&self, scope: ResourceScope) -> Option<ScopedCommandPolicy> {
        let site = ResourceScope::Site(self.registry().site_id());
        let port = match scope {
            ResourceScope::Site(_) if scope == site => None,
            ResourceScope::Port(id) if self.port_metadata(id).is_some() => Some(scope),
            _ => return None,
        };
        let mut warnings = Vec::new();
        let layers = [Some(site), port]
            .into_iter()
            .flatten()
            .filter_map(|owner| {
                let section = self.staged(owner, ResourceKind::Command)?;
                let binding = section.binding();
                let fields = section
                    .fields()
                    .iter()
                    .filter(|(key, _)| FIELDS.iter().any(|(field, _)| key.as_str() == *field));
                Some((
                    binding,
                    normalize_fields(binding, fields, FIELDS, &mut warnings),
                ))
            })
            .collect();
        let mut policy = ScopedCommandPolicy {
            muting: true,
            admin_timeout_ms: 300_000,
            regeneration: ResolvedDtmfRegenerationSettings::default(),
            layers,
            warnings,
        };
        let value = |path: &[&str]| policy.leaf(path).map(|(_, v)| v);
        let regeneration = &policy.regeneration;
        let muting = value(&["muting"])
            .and_then(Value::as_bool)
            .unwrap_or(policy.muting);
        let timeout = value(&["admin", "timing", "timeout_ms"])
            .and_then(Value::as_u64)
            .unwrap_or(policy.admin_timeout_ms);
        policy.regeneration = ResolvedDtmfRegenerationSettings {
            enabled: value(&["regeneration", "enabled"])
                .and_then(Value::as_bool)
                .unwrap_or(regeneration.enabled),
            duration_ms: value(&["regeneration", "timing", "duration_ms"])
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(regeneration.duration_ms),
            gap_ms: value(&["regeneration", "timing", "gap_ms"])
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(regeneration.gap_ms),
            level_dbfs: value(&["regeneration", "level_dbfs"])
                .and_then(Value::as_i64)
                .map(|v| v as i8)
                .unwrap_or(regeneration.level_dbfs),
        };
        policy.muting = muting;
        policy.admin_timeout_ms = timeout;
        Some(policy)
    }
}
