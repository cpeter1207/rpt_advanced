//! Modular AllStarLink account, directory, access and reporting policy.

use super::{
    jitter,
    normalize::{Fields, Layers, Rule},
};
use crate::config::{
    LinkLookupMethod,
    schema::protocol_web_url,
    settings::{DEFAULT_ASL_INTERVAL_S, DEFAULT_ASL_REGISTRATION_URL, DEFAULT_IAX_LOCAL_PORT},
};
use crate::link::PeerIdentity;
use serde_json::Value;
use std::collections::BTreeSet;
use std::num::NonZeroU64;

const INTERVAL: Rule = Rule::Object(&[("interval_s", Rule::Unsigned(30, 600))]);
pub(super) const FIELDS: Fields = &[
    ("allow_nodes", Rule::Value(node_list)),
    ("deny_nodes", Rule::Value(node_list)),
    ("directory_file", Rule::Value(directory_path)),
    ("lookup_method", Rule::Choice(&["both", "dns", "file"])),
    (
        "iax",
        Rule::Object(&[
            (
                "registration_url",
                Rule::Value(|value| endpoint(value, true)),
            ),
            ("local_port", Rule::Unsigned(1, u16::MAX as u64)),
            (
                "timing",
                Rule::Object(&[("registration_interval_s", Rule::Unsigned(30, 600))]),
            ),
        ]),
    ),
    (
        "status_post",
        Rule::Object(&[
            ("url", Rule::Value(|value| endpoint(value, false))),
            ("timing", INTERVAL),
        ]),
    ),
    ("callsign", Rule::Value(callsign)),
    ("node_number", Rule::Unsigned(1, u64::MAX)),
    ("jitter", Rule::Object(jitter::FIELDS)),
];

fn node_list(value: &Value) -> Option<Value> {
    let mut seen = BTreeSet::new();
    let mut nodes = Vec::new();
    for value in value.as_array()? {
        let node = value.as_str()?;
        if node.is_empty() || node.len() > 63 || !node.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let normalized = PeerIdentity::canonical_operator(node).unwrap_or_else(|_| node.to_owned());
        if seen.insert(normalized.clone()) {
            nodes.push(Value::String(normalized));
        }
    }
    Some(Value::Array(nodes))
}

fn directory_path(value: &Value) -> Option<Value> {
    value
        .as_str()
        .filter(|path| !path.contains('\0'))
        .map(Value::from)
}

fn callsign(value: &Value) -> Option<Value> {
    value
        .as_str()
        .filter(|text| text.len() <= 63 && !text.chars().any(char::is_control))
        .map(Value::from)
}

fn endpoint(value: &Value, https_only: bool) -> Option<Value> {
    let raw = value.as_str()?;
    if raw.is_empty() {
        return Some(Value::from(raw));
    }
    let mut url = protocol_web_url(raw, https_only)?;
    url.set_fragment(None);
    Some(Value::from(url.as_str()))
}

/// Public AllStarLink settings independent of peer audio and other protocols.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllStarLinkProfileSettings {
    /// Explicit network node number, if configured.
    pub node_number: Option<NonZeroU64>,
    /// Optional account display callsign.
    pub callsign: String,
    /// Normalized incoming allowlist. Empty permits all verified nodes.
    pub allow_nodes: Vec<String>,
    /// Normalized incoming denylist, checked before the allowlist.
    pub deny_nodes: Vec<String>,
    /// Downloaded external directory feed path. Empty leaves file lookup unavailable.
    pub directory_file: String,
    /// Source order after the separately owned static node table.
    pub lookup_method: LinkLookupMethod,
    /// Canonical HTTPS endpoint, or None when registration is disabled.
    pub registration_url: Option<String>,
    /// HTTPS registration refresh interval in seconds.
    pub registration_interval_s: u16,
    /// Local IAX listener/advertised UDP port.
    pub local_port: u16,
    /// Canonical HTTP(S) statistics endpoint, or None when reporting is disabled.
    pub status_post_url: Option<String>,
    /// Periodic status interval in seconds.
    pub status_post_interval_s: u16,
}

/// Configured ASL credential affinity. IAX uses the node even without HTTPS registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllStarLinkAccountIdentity<'a> {
    /// AllStarLink network account, independent of the local controller key.
    pub node_number: NonZeroU64,
    /// The configured registration destination, when registration is enabled.
    pub registration_url: Option<&'a str>,
}

impl AllStarLinkProfileSettings {
    /// Inspect the checked, public identity used to match an IAX private record.
    pub fn account_identity(&self) -> Option<AllStarLinkAccountIdentity<'_>> {
        Some(AllStarLinkAccountIdentity {
            node_number: self.node_number?,
            registration_url: self.registration_url.as_deref(),
        })
    }

    pub(super) fn resolve(layers: &Layers<'_>) -> Self {
        let nodes = |field| {
            layers
                .get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|node| node.as_str().expect("normalized node list").to_owned())
                .collect()
        };
        let registration_url = layers
            .text(&["iax", "registration_url"])
            .unwrap_or(DEFAULT_ASL_REGISTRATION_URL);
        Self {
            node_number: layers.number(&["node_number"]).and_then(NonZeroU64::new),
            callsign: layers.text(&["callsign"]).unwrap_or_default().to_owned(),
            allow_nodes: nodes(&["allow_nodes"]),
            deny_nodes: nodes(&["deny_nodes"]),
            directory_file: layers
                .text(&["directory_file"])
                .unwrap_or_default()
                .to_owned(),
            lookup_method: match layers.text(&["lookup_method"]) {
                Some("dns") => LinkLookupMethod::Dns,
                Some("file") => LinkLookupMethod::File,
                _ => LinkLookupMethod::default(),
            },
            registration_url: (!registration_url.is_empty()).then(|| registration_url.to_owned()),
            registration_interval_s: layers
                .number(&["iax", "timing", "registration_interval_s"])
                .map_or(DEFAULT_ASL_INTERVAL_S, |value| value as u16),
            local_port: layers
                .number(&["iax", "local_port"])
                .map_or(DEFAULT_IAX_LOCAL_PORT, |value| value as u16),
            status_post_url: layers
                .text(&["status_post", "url"])
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
            status_post_interval_s: layers
                .number(&["status_post", "timing", "interval_s"])
                .map_or(DEFAULT_ASL_INTERVAL_S, |value| value as u16),
        }
    }
}
