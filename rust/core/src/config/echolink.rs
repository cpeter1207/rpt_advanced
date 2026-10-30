//! Off-audio EchoLink account resolution. Identity never inherits across accounts.
use super::{ConfigDocument, ConfigError, NodeId, Resolution, Schema, parse};
use std::{collections::BTreeSet, net::Ipv4Addr, path::PathBuf};

/// Account transport; a Proxy is the EchoLink TCP protocol, not HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoLinkTransport {
    /// IPv4 UDP audio/control and directory TCP.
    Direct,
    /// User-supplied standard EchoLink Proxy.
    Proxy,
}
impl EchoLinkTransport {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "direct" => Some(Self::Direct),
            "proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}
/// Spoken connection identity selection, subject to shared telemetry routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoLinkAnnouncement {
    /// No EchoLink connection announcement.
    Off,
    /// Real node number.
    Node,
    /// Callsign.
    Callsign,
    /// Callsign and real node number.
    Both,
}
impl EchoLinkAnnouncement {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "off" => Some(Self::Off),
            "node" => Some(Self::Node),
            "callsign" => Some(Self::Callsign),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}
/// ASL-compatible named telemetry selection; numeric legacy comments are not syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoLinkTelemetry {
    /// Disabled.
    Off,
    /// Enabled within shared routing/eligibility limits.
    On,
    /// Follow activity according to the EchoLink telemetry policy.
    Follow,
    /// On-demand telemetry.
    Demand,
}
impl EchoLinkTelemetry {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "off" => Some(Self::Off),
            "on" => Some(Self::On),
            "follow" => Some(Self::Follow),
            "demand" => Some(Self::Demand),
            _ => None,
        }
    }
}

/// Fully resolved public account settings. Passwords never belong in this record.
#[derive(Clone, Debug, PartialEq)]
pub struct EchoLinkSettings {
    /// Disabled accounts do not require a provider or credentials.
    pub enabled: bool,
    /// Node-specific normalized account callsign.
    pub callsign: String,
    /// Node-specific complete positive u32 ID; zero only when disabled/unconfigured.
    pub node_number: u32,
    /// Advertised station name, not a radio command.
    pub name: String,
    /// Advertised location.
    pub location: String,
    /// Public contact address.
    pub email: String,
    /// Station information, never synthesized as human chat.
    pub status_message: String,
    /// Decimal degrees north.
    pub latitude: f64,
    /// Decimal degrees east.
    pub longitude: f64,
    /// Advertised RF frequency in MHz.
    pub frequency_mhz: f64,
    /// Advertised tone frequency in Hz.
    pub tone_hz: f64,
    /// Advertised RF power in watts.
    pub power_w: f64,
    /// Antenna height in metres.
    pub antenna_height_m: f64,
    /// Antenna gain in dB, not an audio gain.
    pub antenna_gain_db: f64,
    /// Antenna azimuth in degrees.
    pub antenna_direction_deg: f64,
    /// Ordered directory hostnames; the protocol uses TCP 5200.
    pub directory_servers: Vec<String>,
    /// Maximum admitted stations; not a global software limit.
    pub max_stations: u32,
    /// Separate bounded unauthenticated setup budget.
    pub max_pending_handshakes: u32,
    /// Control heartbeat interval.
    pub heartbeat_interval_ms: u32,
    /// Timeout since a valid peer packet.
    pub heartbeat_timeout_ms: u32,
    /// Initial QSO setup timeout.
    pub connect_timeout_ms: u32,
    /// Per-directory operation deadline.
    pub directory_timeout_ms: u32,
    /// Login refresh period.
    pub registration_refresh_ms: u32,
    /// Directory snapshot refresh period.
    pub directory_refresh_ms: u32,
    /// Failed-directory retry period.
    pub directory_retry_ms: u32,
    /// Maximum snapshot age for authenticating a new call.
    pub directory_freshness_ms: u32,
    /// Apply once at this protocol's incoming media edge.
    pub receive_gain_db: f64,
    /// Apply once at this protocol's outgoing media edge.
    pub transmit_gain_db: f64,
    /// Connection announcement identity.
    pub announcement: EchoLinkAnnouncement,
    /// Named telemetry mode.
    pub telemetry: EchoLinkTelemetry,
    /// Allow authorized runtime telemetry selection.
    pub telemetry_dynamic: bool,
    /// Explicit transport, with no automatic fallback.
    pub transport: EchoLinkTransport,
    /// Local direct IPv4 bind address.
    pub bind_address: Ipv4Addr,
    /// Direct UDP audio port; the control port is exactly this value plus one.
    pub audio_port: u16,
    /// Remote standard Proxy host.
    pub proxy_host: String,
    /// Remote Proxy TCP port, not a UDP pair.
    pub proxy_port: u16,
    /// Normalized allow wildcards; empty permits all directory-verified calls.
    pub allow: Vec<String>,
    /// Normalized deny wildcards, evaluated before allow.
    pub deny: Vec<String>,
    /// Explicit path, or the owning host's config-directory default when absent.
    pub secrets_file: Option<PathBuf>,
}

pub(super) fn default_value(key: &str) -> Option<&'static str> {
    Some(match key {
        "enabled" => "no",
        "callsign" | "node_number" | "name" | "location" | "email" | "status_message" | "allow"
        | "deny" | "proxy_host" | "secrets_file" => "",
        "latitude"
        | "longitude"
        | "frequency_mhz"
        | "tone_hz"
        | "power_w"
        | "antenna_height_m"
        | "antenna_gain_db"
        | "antenna_direction_deg" => "0",
        "directory_servers" => "servers.echolink.org,backup.echolink.org",
        "max_stations" | "max_pending_handshakes" => "20",
        "heartbeat_interval_ms" | "directory_timeout_ms" => "10000",
        "heartbeat_timeout_ms" => "100000",
        "connect_timeout_ms" => "30000",
        "registration_refresh_ms" => "300000",
        "directory_refresh_ms" => "240000",
        "directory_retry_ms" => "20000",
        "directory_freshness_ms" => "900000",
        "receive_gain_db" => "-3",
        "transmit_gain_db" => "3",
        "announcement" => "node",
        "telemetry" => "demand",
        "telemetry_dynamic" => "yes",
        "transport" => "direct",
        "bind_address" => "0.0.0.0",
        "audio_port" => "5198",
        "proxy_port" => "8100",
        _ => return None,
    })
}
fn finite(raw: &str) -> Option<f64> {
    raw.parse::<f64>().ok().filter(|number| number.is_finite())
}
fn host(raw: &str) -> Option<String> {
    if raw.len() > 253 || raw.contains(char::is_whitespace) {
        return None;
    }
    url::Host::parse(raw).ok().map(|host| host.to_string())
}
fn hosts(raw: &str) -> Option<Vec<String>> {
    raw.split(',').map(|entry| host(entry.trim())).collect()
}
fn patterns(raw: &str) -> Option<Vec<String>> {
    if raw.is_empty() {
        return Some(Vec::new());
    }
    raw.split(',')
        .map(|entry| {
            let entry = entry.trim();
            (!entry.is_empty()
                && entry.len() <= 64
                && entry
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-/*_?".contains(&byte)))
            .then(|| entry.to_ascii_uppercase())
        })
        .collect()
}
fn callsign(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > 15 {
        return false;
    }
    if let Some(conference) = raw
        .strip_prefix('*')
        .and_then(|body| body.strip_suffix('*'))
    {
        return !conference.is_empty()
            && conference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte));
    }
    let upper = raw.to_ascii_uppercase();
    let base = upper
        .strip_suffix("-L")
        .or_else(|| upper.strip_suffix("-R"))
        .unwrap_or(&upper);
    base.bytes().any(|byte| byte.is_ascii_alphabetic())
        && base.bytes().any(|byte| byte.is_ascii_digit())
        && base
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'/')
}
/// The schema checks its known-key list first; resolution supplies fixed keys.
pub(super) fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "enabled" | "telemetry_dynamic" => parse::boolean(value).is_some(),
        "callsign" => callsign(value),
        "node_number"
        | "max_stations"
        | "max_pending_handshakes"
        | "heartbeat_interval_ms"
        | "heartbeat_timeout_ms"
        | "connect_timeout_ms"
        | "directory_timeout_ms"
        | "registration_refresh_ms"
        | "directory_refresh_ms"
        | "directory_retry_ms"
        | "directory_freshness_ms" => parse::unsigned(value, 1, u32::MAX.into()).is_some(),
        "audio_port" => parse::unsigned(value, 1, 65534).is_some(),
        "proxy_port" => parse::unsigned(value, 1, 65535).is_some(),
        "transport" => EchoLinkTransport::parse(value).is_some(),
        "announcement" => EchoLinkAnnouncement::parse(value).is_some(),
        "telemetry" => EchoLinkTelemetry::parse(value).is_some(),
        "bind_address" => value.parse::<Ipv4Addr>().is_ok(),
        "proxy_host" => value.is_empty() || host(value).is_some(),
        "directory_servers" => hosts(value).is_some(),
        "allow" | "deny" => patterns(value).is_some(),
        "latitude" => finite(value).is_some_and(|number| (-90.0..=90.0).contains(&number)),
        "longitude" => finite(value).is_some_and(|number| (-180.0..=180.0).contains(&number)),
        "antenna_direction_deg" => {
            finite(value).is_some_and(|number| (0.0..=360.0).contains(&number))
        }
        "receive_gain_db" | "transmit_gain_db" => {
            finite(value).is_some_and(|number| (-60.0..=60.0).contains(&number))
        }
        "frequency_mhz" | "tone_hz" | "power_w" | "antenna_height_m" => {
            finite(value).is_some_and(|number| number >= 0.0)
        }
        "antenna_gain_db" => finite(value).is_some(),
        "name" => value.len() <= 240 && !value.chars().any(char::is_control),
        "location" | "email" | "status_message" => {
            value.len() <= 4096 && !value.chars().any(char::is_control)
        }
        "secrets_file" => value.len() <= 4096 && !value.chars().any(char::is_control),
        _ => unreachable!("known EchoLink configuration parameter"),
    }
}
fn raw<'a>(document: &'a ConfigDocument, node: &NodeId, key: &str) -> &'a str {
    let section = format!("echolink {}", node.as_str());
    let identity = matches!(key, "callsign" | "node_number");
    [section.as_str(), "echolink"]
        .into_iter()
        .take(if identity { 1 } else { 2 })
        .find_map(|scope| {
            document
                .lookup(key, &[scope])
                .filter(|value| value_valid(key, value))
        })
        .unwrap_or_else(|| default_value(key).expect("known account option"))
}
impl EchoLinkSettings {
    /// Resolve shared defaults and node overrides, retaining all schema warnings.
    pub fn resolve(
        document: &ConfigDocument,
        node: &NodeId,
    ) -> Result<Resolution<Self>, ConfigError> {
        let warnings = Schema::validate(document)?.warnings;
        Ok(Resolution {
            value: Self::from_document(document, node)?,
            warnings,
        })
    }
    fn from_document(document: &ConfigDocument, node: &NodeId) -> Result<Self, ConfigError> {
        let text = |key| raw(document, node, key);
        let number = |key| text(key).parse::<u32>().expect("validated account number");
        let scalar = |key| finite(text(key)).expect("validated account scalar");
        let settings = Self {
            enabled: parse::boolean(text("enabled")).unwrap(),
            callsign: text("callsign").to_ascii_uppercase(),
            node_number: text("node_number").parse().unwrap_or(0),
            name: text("name").into(),
            location: text("location").into(),
            email: text("email").into(),
            status_message: text("status_message").into(),
            latitude: scalar("latitude"),
            longitude: scalar("longitude"),
            frequency_mhz: scalar("frequency_mhz"),
            tone_hz: scalar("tone_hz"),
            power_w: scalar("power_w"),
            antenna_height_m: scalar("antenna_height_m"),
            antenna_gain_db: scalar("antenna_gain_db"),
            antenna_direction_deg: scalar("antenna_direction_deg"),
            directory_servers: hosts(text("directory_servers")).unwrap(),
            max_stations: number("max_stations"),
            max_pending_handshakes: number("max_pending_handshakes"),
            heartbeat_interval_ms: number("heartbeat_interval_ms"),
            heartbeat_timeout_ms: number("heartbeat_timeout_ms"),
            connect_timeout_ms: number("connect_timeout_ms"),
            directory_timeout_ms: number("directory_timeout_ms"),
            registration_refresh_ms: number("registration_refresh_ms"),
            directory_refresh_ms: number("directory_refresh_ms"),
            directory_retry_ms: number("directory_retry_ms"),
            directory_freshness_ms: number("directory_freshness_ms"),
            receive_gain_db: scalar("receive_gain_db"),
            transmit_gain_db: scalar("transmit_gain_db"),
            announcement: EchoLinkAnnouncement::parse(text("announcement")).unwrap(),
            telemetry: EchoLinkTelemetry::parse(text("telemetry")).unwrap(),
            telemetry_dynamic: parse::boolean(text("telemetry_dynamic")).unwrap(),
            transport: EchoLinkTransport::parse(text("transport")).unwrap(),
            bind_address: text("bind_address").parse().unwrap(),
            audio_port: number("audio_port") as u16,
            proxy_host: if text("proxy_host").is_empty() {
                String::new()
            } else {
                host(text("proxy_host")).unwrap()
            },
            proxy_port: number("proxy_port") as u16,
            allow: patterns(text("allow")).unwrap(),
            deny: patterns(text("deny")).unwrap(),
            secrets_file: (!text("secrets_file").is_empty())
                .then(|| PathBuf::from(text("secrets_file"))),
        };
        if settings.enabled
            && (settings.callsign.is_empty()
                || settings.node_number == 0
                || (settings.transport == EchoLinkTransport::Proxy
                    && settings.proxy_host.is_empty())
                || settings
                    .max_stations
                    .checked_add(settings.max_pending_handshakes)
                    .is_none())
        {
            return Err(ConfigError::structure(
                &format!("echolink {}", node.as_str()),
                "enabled account needs its own valid identity, transport and bounded setup budget",
            ));
        }
        Ok(settings)
    }
}
pub(super) fn validate_all(document: &ConfigDocument) -> Result<(), ConfigError> {
    if !document
        .sections()
        .iter()
        .any(|section| section == "echolink" || section.starts_with("echolink "))
    {
        return Ok(());
    }
    let mut accounts: Vec<EchoLinkSettings> = Vec::new();
    let mut callsigns = BTreeSet::new();
    let mut numbers = BTreeSet::new();
    for name in document.nodes() {
        let node = NodeId::new(name)?;
        if parse::boolean(raw(document, &node, "enabled")) != Some(true) {
            continue;
        }
        let account = EchoLinkSettings::from_document(document, &node)?;
        if !callsigns.insert(account.callsign.clone()) || !numbers.insert(account.node_number) {
            return Err(ConfigError::structure(
                "echolink",
                "duplicate enabled account identity",
            ));
        }
        for previous in &accounts {
            let conflict = match (account.transport, previous.transport) {
                (EchoLinkTransport::Direct, EchoLinkTransport::Direct) => {
                    (account.bind_address.is_unspecified()
                        || previous.bind_address.is_unspecified()
                        || account.bind_address == previous.bind_address)
                        && account.audio_port <= previous.audio_port + 1
                        && previous.audio_port <= account.audio_port + 1
                }
                (EchoLinkTransport::Proxy, EchoLinkTransport::Proxy) => {
                    account.proxy_host == previous.proxy_host
                        && account.proxy_port == previous.proxy_port
                }
                _ => false,
            };
            if conflict {
                return Err(ConfigError::structure(
                    "echolink",
                    "enabled accounts share an ambiguous transport endpoint",
                ));
            }
        }
        accounts.push(account);
    }
    Ok(())
}
