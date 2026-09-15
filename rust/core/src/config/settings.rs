//! Owned node settings and inherited default resolution.

use super::{parse, ConfigDocument, ConfigError, NodeId, Resolution, Schema};
use std::collections::BTreeMap;

/// Directory lookup source selected after the local static directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkLookupMethod {
    /// Query DNS and then the external file.
    #[default]
    Both,
    /// Query DNS only.
    Dns,
    /// Query the external file only.
    File,
}

/// Fully owned node settings after general and node-scoped inheritance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedNodeSettings {
    /// Whether the node starts.
    pub enabled: bool,
    /// Whether local receive may transmit concurrently.
    pub full_duplex: bool,
    /// Whether completed local DTMF frames are muted.
    pub dtmf_muting: bool,
    /// Transmit hang duration in milliseconds.
    pub hang_ms: u64,
    /// Continuous-source watchdog duration; zero disables it.
    pub transmit_timeout_ms: u64,
    /// Post-watchdog lockout duration.
    pub timeout_lockout_ms: u64,
    /// Kerchunk threshold in milliseconds.
    pub kerchunk_max_ms: u64,
    /// Receive-active telemetry attenuation in dB.
    pub telemetry_duck_db: i64,
    /// Unkey-to-courtesy delay in milliseconds.
    pub courtesy_delay_ms: u64,
    /// Node radio channel.
    pub channel: String,
    /// Optional callsign.
    pub callsign: String,
    /// Incoming allowlist.
    pub link_allow_nodes: String,
    /// Incoming denylist.
    pub link_deny_nodes: String,
    /// Optional local static directory path.
    pub link_static_directory_file: String,
    /// Optional external directory path.
    pub link_directory_file: String,
    /// Directory lookup policy.
    pub link_lookup_method: LinkLookupMethod,
    /// Configured DTMF command prefixes by option name.
    pub link_commands: BTreeMap<String, String>,
}

impl ResolvedNodeSettings {
    /// Resolve general defaults and an exact node override, returning warnings for recoverable values.
    pub fn resolve(document: &ConfigDocument, node: &NodeId) -> Result<Resolution<Self>, ConfigError> {
        let schema = Schema::validate(document)?;
        let mut value = Self::defaults(node);
        let scopes = ["general".to_owned(), node.as_str().to_owned()];
        macro_rules! text {
            ($field:ident, $key:literal) => {
                if let Some(raw) = lookup(document, $key, &scopes) {
                    value.$field = raw.to_owned();
                }
            };
        }
        macro_rules! boolean {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, parse::boolean) {
                    value.$field = parsed;
                }
            };
        }
        macro_rules! number {
            ($field:ident, $key:literal) => {
                if let Some(parsed) = lookup_valid(document, $key, &scopes, |raw| parse::unsigned(raw, 0, u64::MAX)) {
                    value.$field = parsed;
                }
            };
        }
        boolean!(enabled, "node_enabled");
        boolean!(full_duplex, "full_duplex");
        boolean!(dtmf_muting, "dtmf_muting");
        number!(hang_ms, "transmit_hang_ms");
        number!(transmit_timeout_ms, "transmit_timeout_ms");
        number!(timeout_lockout_ms, "timeout_lockout_ms");
        number!(kerchunk_max_ms, "kerchunk_max_ms");
        number!(courtesy_delay_ms, "courtesy_delay_ms");
        if let Some(parsed) = lookup_valid(document, "telemetry_duck_db", &scopes, |raw| parse::signed(raw, -60, 0)) {
            value.telemetry_duck_db = parsed;
        }
        text!(channel, "radio_channel");
        text!(callsign, "callsign");
        text!(link_allow_nodes, "link_allow_nodes");
        text!(link_deny_nodes, "link_deny_nodes");
        text!(link_static_directory_file, "link_static_directory_file");
        text!(link_directory_file, "link_directory_file");
        if let Some(parsed) = lookup_valid(document, "link_lookup_method", &scopes, |raw| match raw {
                "dns" => LinkLookupMethod::Dns,
                "file" => LinkLookupMethod::File,
                "both" => LinkLookupMethod::Both,
                _ => return None,
            }) {
            value.link_lookup_method = parsed;
        }
        for key in command_keys() {
            if let Some(raw) = lookup(document, key, &scopes) {
                value.link_commands.insert(key.to_owned(), raw.to_owned());
            }
        }
        Ok(Resolution { value, warnings: schema.warnings })
    }

    fn defaults(node: &NodeId) -> Self {
        Self {
            enabled: true,
            full_duplex: true,
            dtmf_muting: true,
            hang_ms: 0,
            transmit_timeout_ms: 180_000,
            timeout_lockout_ms: 30_000,
            kerchunk_max_ms: 500,
            telemetry_duck_db: -20,
            courtesy_delay_ms: 250,
            channel: node.as_str().to_owned(),
            callsign: String::new(),
            link_allow_nodes: String::new(),
            link_deny_nodes: String::new(),
            link_static_directory_file: String::new(),
            link_directory_file: String::new(),
            link_lookup_method: LinkLookupMethod::Both,
            link_commands: command_keys().into_iter().map(|key| (key.to_owned(), default_command(key))).collect(),
        }
    }
}

fn lookup<'a>(document: &'a ConfigDocument, key: &str, scopes: &[String; 2]) -> Option<&'a str> {
    document.lookup(key, &[scopes[0].as_str(), scopes[1].as_str()])
}

fn lookup_valid<T>(
    document: &ConfigDocument,
    key: &str,
    scopes: &[String; 2],
    parse_value: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    for scope in scopes {
        if let Some(raw) = document.lookup(key, &[scope.as_str()]) {
            if let Some(value) = parse_value(raw) {
                return Some(value);
            }
        }
    }
    None
}

fn command_keys() -> [&'static str; 14] {
    [
        "link_command_disconnect", "link_command_monitor", "link_command_transceive", "link_command_remote",
        "link_command_status", "link_command_disconnect_all", "link_command_last_keyed", "link_command_local_monitor",
        "link_command_disconnect_permanent", "link_command_permanent_monitor", "link_command_permanent_transceive",
        "link_command_full_status", "link_command_reconnect_all", "link_command_permanent_local_monitor",
    ]
}

fn default_command(key: &str) -> String {
    match key {
        "link_command_disconnect" => "1",
        "link_command_monitor" => "2",
        "link_command_transceive" => "3",
        "link_command_remote" => "4",
        "link_command_status" => "70",
        "link_command_disconnect_all" => "806",
        "link_command_last_keyed" => "72",
        "link_command_local_monitor" => "75",
        "link_command_disconnect_permanent" => "811",
        "link_command_permanent_monitor" => "812",
        "link_command_permanent_transceive" => "813",
        "link_command_full_status" => "73",
        "link_command_reconnect_all" => "816",
        _ => "818",
    }
    .to_owned()
}

/// A validated node identity with the transport's 63-byte limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Construct an identity containing no whitespace/brackets and at most 63 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty() || value.len() > 63 || value.chars().any(|c| c.is_whitespace() || matches!(c, '[' | ']')) {
            return Err(ConfigError::structure(&value, "invalid node identity"));
        }
        Ok(Self(value))
    }

    /// Borrow the identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
