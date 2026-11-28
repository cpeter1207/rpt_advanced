//! Public phone provider, VAD and playout settings.

use super::{
    CandidateWarning, jitter,
    normalize::{Fields, Layers, Path, Rule, timings},
};
use crate::{config::echolink, link::MailboxId};
use serde_json::Value;
use std::collections::HashSet;

const POSITIVE_MS: Rule = Rule::Unsigned(1, u32::MAX as u64);
pub(super) const FIELDS: Fields = &[
    (
        "provider",
        Rule::Object(&[
            ("protocol", Rule::Choice(&["sip", "iax2"])),
            (
                "server",
                Rule::Value(|value| echolink::host(value.as_str()?).map(Value::from)),
            ),
            ("port", Rule::Unsigned(1, u16::MAX as u64)),
            ("username", Rule::Value(username)),
        ]),
    ),
    (
        "playout_destination",
        Rule::Choice(&["local", "local_and_links"]),
    ),
    ("allow_mailboxes", Rule::Value(mailbox_list)),
    ("deny_mailboxes", Rule::Value(mailbox_list)),
    ("isolate_peers", Rule::Bool),
    (
        "timing",
        Rule::Object(&[
            ("target_latency_ms", POSITIVE_MS),
            ("maximum_latency_ms", POSITIVE_MS),
        ]),
    ),
    (
        "vad",
        Rule::Object(&[
            ("enabled", Rule::Bool),
            ("mode", Rule::Choice(&["continuous"])),
            (
                "timing",
                Rule::Object(&[("hang_ms", Rule::Unsigned(0, 32767))]),
            ),
        ]),
    ),
    ("jitter", Rule::Object(jitter::FIELDS)),
];

fn mailbox_list(value: &Value) -> Option<Value> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for entry in value.as_array()? {
        let number = entry.as_str()?;
        MailboxId::new(number).ok()?;
        if seen.insert(number) {
            entries.push(entry.clone());
        }
    }
    Some(Value::Array(entries))
}

fn resolved_mailboxes(layers: &Layers<'_>, path: Path) -> Vec<MailboxId> {
    layers
        .get(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            MailboxId::new(entry.as_str().expect("normalized mailbox string"))
                .expect("normalized mailbox number")
        })
        .collect()
}

fn username(value: &Value) -> Option<Value> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .map(Value::from)
}

/// Supported phone account protocols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneProtocol {
    /// Session Initiation Protocol.
    Sip,
    /// Inter-Asterisk eXchange protocol version 2.
    Iax2,
}

/// Where phone-originated speech can be played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhonePlayoutDestination {
    /// The local transmitter only.
    Local,
    /// The local transmitter and eligible links.
    LocalAndLinks,
}

/// Public account fields. An incomplete provider stays unconfigured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneProviderSettings {
    /// Selected protocol.
    pub protocol: Option<PhoneProtocol>,
    /// Normalized server host.
    pub server: Option<String>,
    /// Configured server port.
    pub port: Option<u16>,
    /// Exact configured username.
    pub username: Option<String>,
}

/// Stable configured endpoint and identity for phone credential matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhoneAccountIdentity<'a> {
    /// Selected protocol.
    pub protocol: PhoneProtocol,
    /// Normalized remote host.
    pub server: &'a str,
    /// Configured remote transport port.
    pub port: u16,
    /// Exact username.
    pub username: &'a str,
}

impl PhoneProviderSettings {
    /// Inspect the account/endpoint affinity once every binding field is known.
    pub fn account_identity(&self) -> Option<PhoneAccountIdentity<'_>> {
        Some(PhoneAccountIdentity {
            protocol: self.protocol?,
            server: self.server.as_deref()?,
            port: self.port?,
            username: self.username.as_deref()?,
        })
    }
}

/// Voice activity behavior after the current receive context is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadMode {
    /// Fresh speech opens until the independent closing qualifier fires.
    Kerchunk,
    /// Speech and the configured hang qualify reception.
    Continuous,
}

/// Typed VAD settings after applying the phone context defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhoneVadSettings {
    /// Apply speech qualification to a live usable caller.
    pub enabled: bool,
    /// Behavior when speech qualification is enabled.
    pub mode: VadMode,
    /// Qualification hang after speech.
    pub hang_ms: u16,
}

/// End-to-end playout budget distinct from the network jitter policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhoneLatencyTiming {
    /// Target playout delay in milliseconds.
    pub target_latency_ms: u32,
    /// Maximum playout delay in milliseconds.
    pub maximum_latency_ms: u32,
}

/// Public phone-patch instance settings. DTMF inactivity comes from the port's command settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhonePatchProfileSettings {
    /// The staged SIP or IAX2 provider binding.
    pub provider: PhoneProviderSettings,
    /// Where eligible phone speech can be played.
    pub playout_destination: PhonePlayoutDestination,
    /// Exact mailbox IDs allowed to authenticate. An empty list imposes no allow filter.
    pub allow_mailboxes: Vec<MailboxId>,
    /// Exact mailbox IDs denied even when also allowed.
    pub deny_mailboxes: Vec<MailboxId>,
    /// Keep separately attached network peers isolated during phone use.
    pub isolate_peers: bool,
    /// End-to-end playout timing.
    pub timing: PhoneLatencyTiming,
    /// Phone receive qualification.
    pub vad: PhoneVadSettings,
}

impl PhonePatchProfileSettings {
    /// Check an exact mailbox identity before granting authenticated access.
    pub fn allows_mailbox(&self, mailbox: &MailboxId) -> bool {
        !self.deny_mailboxes.contains(mailbox)
            && (self.allow_mailboxes.is_empty() || self.allow_mailboxes.contains(mailbox))
    }

    pub(super) fn resolve(layers: &Layers<'_>, warnings: &mut Vec<CandidateWarning>) -> Self {
        let (budgets, selected) = timings(
            layers,
            [
                &["timing", "target_latency_ms"],
                &["timing", "maximum_latency_ms"],
            ],
            [250, 500],
            |[target, maximum]| target <= maximum,
            warnings,
        );
        Self {
            provider: PhoneProviderSettings {
                protocol: match layers.text(&["provider", "protocol"]) {
                    Some("sip") => Some(PhoneProtocol::Sip),
                    Some("iax2") => Some(PhoneProtocol::Iax2),
                    _ => None,
                },
                server: layers.text(&["provider", "server"]).map(str::to_owned),
                port: layers.number(&["provider", "port"]).map(|port| port as u16),
                username: layers.text(&["provider", "username"]).map(str::to_owned),
            },
            playout_destination: match layers.text(&["playout_destination"]) {
                Some("local_and_links") => PhonePlayoutDestination::LocalAndLinks,
                _ => PhonePlayoutDestination::Local,
            },
            allow_mailboxes: resolved_mailboxes(layers, &["allow_mailboxes"]),
            deny_mailboxes: resolved_mailboxes(layers, &["deny_mailboxes"]),
            isolate_peers: layers.flag(&["isolate_peers"]).unwrap_or(true),
            timing: PhoneLatencyTiming {
                target_latency_ms: budgets[0][selected[0]].milliseconds,
                maximum_latency_ms: budgets[1][selected[1]].milliseconds,
            },
            vad: PhoneVadSettings {
                enabled: layers.flag(&["vad", "enabled"]).unwrap_or(true),
                mode: VadMode::Continuous,
                hang_ms: layers
                    .number(&["vad", "timing", "hang_ms"])
                    .map_or(1000, |value| value as u16),
            },
        }
    }
}
