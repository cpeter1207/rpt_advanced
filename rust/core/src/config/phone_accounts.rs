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

/// Prepared analysis-only phone policy; transport/account identity is unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhoneSpeechSettings {
    /// Explicit opt-in to the shared analysis preprocessing chain.
    pub preprocessing_enabled: bool,
    /// Apply the voice bandpass before RNNoise when preprocessing is enabled.
    pub bandpass_enabled: bool,
    /// Finite lower cutoff, above zero and below the upper cutoff.
    pub bandpass_low_hz: f32,
    /// Finite upper cutoff below 24 kHz; preparation also checks source Nyquist.
    pub bandpass_high_hz: f32,
    /// Apply the existing RNNoise model when preprocessing is enabled.
    pub rnnoise_enabled: bool,
    /// Finite classifier threshold, inclusive zero through one.
    pub threshold: f32,
    /// Positive consecutive 16-ms classifier frames; no arbitrary model limit.
    pub onset_frames: u32,
    /// Requested backward voice marking, bounded by the 300-ms programme capacity.
    /// History already rendered cannot be recovered, even if this exceeds reserve.
    pub preroll_ms: u32,
    /// Future source-clock voice hold; no additional history storage.
    pub hold_ms: u32,
    /// Programme reserve, independent of backward voice marking.
    /// 86..280 ms follows 4096 output frames, 20-ms input and 300-ms capacity.
    pub reserve_ms: u32,
}
impl Default for PhoneSpeechSettings {
    fn default() -> Self {
        Self {
            preprocessing_enabled: false,
            bandpass_enabled: true,
            bandpass_low_hz: 300.0,
            bandpass_high_hz: 3000.0,
            rnnoise_enabled: true,
            threshold: 0.5,
            onset_frames: 1,
            preroll_ms: 160,
            hold_ms: 1000,
            reserve_ms: 160,
        }
    }
}
impl PhoneSpeechSettings {
    /// Resolve only this account. Schema validation reports malformed options;
    /// ignored values use the selected preset without node/general inheritance.
    pub fn resolve(document: &ConfigDocument, account: &PhoneAccountId) -> Self {
        Self::in_section(document, &format!("phone_account {}", account.as_str()))
    }
    fn in_section(document: &ConfigDocument, section: &str) -> Self {
        let field = |key| valid_field(document, section, key);
        let defaults = Self::default();
        let preprocessing_enabled = field("speech_preprocessing_enabled")
            .and_then(parse::boolean)
            .unwrap_or(false);
        let (bandpass_low_hz, bandpass_high_hz) = bandpass_pair(document, section)
            .unwrap_or((defaults.bandpass_low_hz, defaults.bandpass_high_hz));
        Self {
            preprocessing_enabled,
            bandpass_enabled: field("speech_bandpass_enabled")
                .and_then(parse::boolean)
                .unwrap_or(defaults.bandpass_enabled),
            bandpass_low_hz,
            bandpass_high_hz,
            rnnoise_enabled: field("speech_rnnoise_enabled")
                .and_then(parse::boolean)
                .unwrap_or(defaults.rnnoise_enabled),
            threshold: field("speech_threshold")
                .and_then(|value| value.parse().ok())
                .unwrap_or(if preprocessing_enabled {
                    0.75
                } else {
                    defaults.threshold
                }),
            onset_frames: field("speech_onset_frames")
                .and_then(|value| value.parse().ok())
                .unwrap_or(if preprocessing_enabled {
                    2
                } else {
                    defaults.onset_frames
                }),
            preroll_ms: field("speech_preroll_ms")
                .and_then(|value| value.parse().ok())
                .unwrap_or(defaults.preroll_ms),
            hold_ms: field("speech_hold_ms")
                .and_then(|value| value.parse().ok())
                .unwrap_or(defaults.hold_ms),
            reserve_ms: field("speech_reserve_ms")
                .and_then(|value| value.parse().ok())
                .unwrap_or(defaults.reserve_ms),
        }
    }
}

fn valid_field<'a>(document: &'a ConfigDocument, section: &str, key: &str) -> Option<&'a str> {
    document
        .lookup(key, &[section])
        .filter(|value| value_valid(key, value))
}

fn bandpass_pair(document: &ConfigDocument, section: &str) -> Option<(f32, f32)> {
    let low = valid_field(document, section, "speech_bandpass_low_hz")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(300.0);
    let high = valid_field(document, section, "speech_bandpass_high_hz")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(3000.0);
    (low < high).then_some((low, high))
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
    /// Maximum pause within phone digit input, in milliseconds, from 1 through 60000.
    pub inter_digit_timeout_ms: u32,
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
            .field("inter_digit_timeout_ms", &self.inter_digit_timeout_ms)
            .finish_non_exhaustive()
    }
}

fn default_value(key: &str, preprocessing: bool) -> Option<&'static str> {
    match key {
        "enabled" | "register" | "speech_preprocessing_enabled" => Some("no"),
        "speech_bandpass_enabled" | "speech_rnnoise_enabled" => Some("yes"),
        "speech_threshold" => Some(if preprocessing { "0.75" } else { "0.5" }),
        "speech_onset_frames" => Some(if preprocessing { "2" } else { "1" }),
        "speech_bandpass_low_hz" => Some("300"),
        "speech_bandpass_high_hz" => Some("3000"),
        "speech_preroll_ms" | "speech_reserve_ms" => Some("160"),
        "speech_hold_ms" => Some("1000"),
        "attach_local_nodes" => Some(""),
        "protocol" => Some("iax"),
        "registration_interval_s" => Some("300"),
        "authentication_timeout_ms" => Some("30000"),
        "inter_digit_timeout_ms" => Some("10000"),
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
    unsigned(value, maximum).filter(|number| *number != 0)
}

fn unsigned(value: &str, maximum: u32) -> Option<u32> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|number| *number <= maximum)
}

fn value_valid(key: &str, value: &str) -> bool {
    match key {
        "enabled"
        | "register"
        | "speech_preprocessing_enabled"
        | "speech_bandpass_enabled"
        | "speech_rnnoise_enabled" => parse::boolean(value).is_some(),
        "speech_threshold" => value
            .parse::<f32>()
            .is_ok_and(|value| value.is_finite() && (0.0..=1.0).contains(&value)),
        "speech_onset_frames" => number(value, u32::MAX).is_some(),
        "speech_bandpass_low_hz" | "speech_bandpass_high_hz" => value
            .parse::<f32>()
            .is_ok_and(|value| value.is_finite() && value > 0.0 && value < 24000.0),
        "speech_preroll_ms" => unsigned(value, 300).is_some(),
        "speech_hold_ms" => unsigned(value, u32::MAX).is_some(),
        "speech_reserve_ms" => unsigned(value, 280).is_some_and(|value| value >= 86),
        "attach_local_nodes" => super::phone_attachment::node_list_valid(value),
        "protocol" => matches!(value, "iax" | "sip"),
        "registration_interval_s" => number(value, 65535).is_some(),
        "authentication_timeout_ms" => number(value, 120000).is_some(),
        "inter_digit_timeout_ms" => number(value, 60000).is_some(),
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
    let speech = PhoneSpeechSettings::in_section(document, section);
    let bandpass_valid = bandpass_pair(document, section).is_some();
    for entry in document.section_entries(section) {
        let fallback = default_value(&entry.key, speech.preprocessing_enabled);
        let invalid_pair = !bandpass_valid
            && matches!(
                entry.key.as_str(),
                "speech_bandpass_low_hz" | "speech_bandpass_high_hz"
            );
        if fallback.is_none() || !value_valid(&entry.key, &entry.value) || invalid_pair {
            warnings.push(ConfigWarning::new(
                entry.line,
                section,
                &entry.key,
                &entry.value,
                if invalid_pair {
                    "bandpass lower cutoff must be below upper cutoff"
                } else if fallback.is_some() {
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
            inter_digit_timeout_ms: field("inter_digit_timeout_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10000),
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
