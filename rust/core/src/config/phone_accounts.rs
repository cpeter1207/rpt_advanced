//! Provider account configuration only; transport owners must honor `enabled`.
//! Credentials are read by the host from `secrets_file`, never by this parser.
use super::{ConfigDocument, ConfigWarning, NodeId, Resolution, parse};
use crate::link::PhoneAccountId;
use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
};

/// Native phone signaling family, independent of radio-peer protocols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneAccountProtocol {
    /// General IAX2 account, not AllStarLink directory registration.
    Iax,
    /// SIP provider account.
    Sip,
}

/// A complete account definition. Disabled records must not open a transport.
#[derive(Clone, PartialEq, Eq)]
pub struct PhoneAccountSettings {
    /// Local account label, distinct from the provider username and mailbox.
    pub id: PhoneAccountId,
    /// Explicit existing local node that owns this account.
    pub local_node: NodeId,
    /// Whether the host may prepare transport resources; defaults to false.
    pub enabled: bool,
    /// Selected native signaling family.
    pub protocol: PhoneAccountProtocol,
    /// Exact host/IP and numeric nonzero port; never a credential-bearing URL.
    pub server: String,
    /// Provider authentication username; never a mailbox authorization.
    pub username: String,
    /// Host path, absolute or relative to the configuration directory, to provider secrets. No file is opened during resolution.
    pub secrets_file: String,
    /// Whether the host requests registration; defaults to false.
    pub register: bool,
    /// Registration refresh interval, in seconds, from 1 through 65535.
    pub registration_interval_s: u16,
    /// Absolute authentication timeout in milliseconds, from 1 through 120000.
    pub authentication_timeout_ms: u32,
}

impl PhoneAccountSettings {
    /// Resolve one exact account using the canonical schema/default rules, without
    /// copying unrelated records or opening its secrets file.
    pub fn resolve(document: &ConfigDocument, id: &PhoneAccountId) -> Resolution<Option<Self>> {
        resolve_section(document, &format!("phone_account {}", id.as_str()))
    }
}

impl fmt::Debug for PhoneAccountSettings {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PhoneAccountSettings")
            .field("id", &self.id)
            .field("local_node", &self.local_node)
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("register", &self.register)
            .field("registration_interval_s", &self.registration_interval_s)
            .field("authentication_timeout_ms", &self.authentication_timeout_ms)
            .finish_non_exhaustive()
    }
}

fn default_value(key: &str) -> Option<&'static str> {
    match key {
        "enabled" | "register" => Some("no"),
        "protocol" => Some("iax"),
        "registration_interval_s" => Some("300"),
        "authentication_timeout_ms" => Some("30000"),
        "local_node" | "server" | "username" | "secrets_file" => Some("account unavailable"),
        _ => None,
    }
}

fn server_valid(value: &str) -> bool {
    if let Ok(address) = value.parse::<SocketAddr>() {
        return address.port() != 0;
    }
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && (!host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
            || host.parse::<Ipv4Addr>().is_ok())
        && number(port, 65535).is_some()
}

fn number(value: &str, maximum: u32) -> Option<u32> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|number| (1..=maximum).contains(number))
}

fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "enabled" | "register" => parse::boolean(value).is_some(),
        "protocol" => matches!(value, "iax" | "sip"),
        "registration_interval_s" => number(value, 65535).is_some(),
        "authentication_timeout_ms" => number(value, 120000).is_some(),
        "local_node" => NodeId::new(value).is_ok(),
        "server" => server_valid(value),
        "secrets_file" => {
            !value.is_empty()
                && !value.contains("://")
                && value.len() <= 4096
                && !value.chars().any(char::is_control)
        }
        // Callers establish a known key; the remaining field is username.
        _ => {
            !value.is_empty()
                && value.len() <= 255
                && !value
                    .chars()
                    .any(|symbol| symbol.is_control() || symbol.is_whitespace())
        }
    }
}

/// Resolve globally named accounts without opening files, sockets or providers.
/// Missing or invalid required fields leave only that account unavailable.
/// Optional malformed values warn and retain their documented safe defaults.
pub fn resolve_phone_accounts(document: &ConfigDocument) -> Resolution<Vec<PhoneAccountSettings>> {
    let mut value = Vec::new();
    let mut warnings = Vec::new();
    for section in document.named_sections("phone_account", None) {
        let resolved = resolve_section(document, section);
        value.extend(resolved.value);
        warnings.extend(resolved.warnings);
    }
    Resolution { value, warnings }
}

pub(super) fn resolve_section(
    document: &ConfigDocument,
    section: &str,
) -> Resolution<Option<PhoneAccountSettings>> {
    let mut warnings = Vec::new();
    for entry in document.section_entries(section) {
        let fallback = default_value(&entry.key);
        if fallback.is_none() || !value_valid(&entry.key, &entry.value) {
            warnings.push(ConfigWarning::new(
                entry.line,
                section,
                &entry.key,
                &entry.value,
                if fallback.is_some() {
                    "invalid option value"
                } else {
                    "unknown option ignored"
                },
                fallback.unwrap_or("not configured"),
            ));
        }
    }
    let field = |key| {
        document
            .lookup(key, &[section])
            .filter(|value| value_valid(key, value))
    };
    let prepare = || {
        let id = PhoneAccountId::new(section.strip_prefix("phone_account ")?).ok()?;
        let local = field("local_node")?;
        if !document.nodes().contains(&local) {
            return None;
        }
        Some(PhoneAccountSettings {
            id,
            local_node: NodeId::new(local).ok()?,
            enabled: field("enabled").and_then(parse::boolean).unwrap_or(false),
            protocol: match field("protocol") {
                Some("sip") => PhoneAccountProtocol::Sip,
                _ => PhoneAccountProtocol::Iax,
            },
            server: field("server")?.into(),
            username: field("username")?.into(),
            secrets_file: field("secrets_file")?.into(),
            register: field("register").and_then(parse::boolean).unwrap_or(false),
            registration_interval_s: field("registration_interval_s")
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            authentication_timeout_ms: field("authentication_timeout_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30000),
        })
    };
    let value = prepare();
    if value.is_none() {
        warnings.push(ConfigWarning::new(0, section, "", "",
            "account requires a valid label, existing local node, server, username and secrets file path",
            "account unavailable"));
    }
    Resolution { value, warnings }
}
