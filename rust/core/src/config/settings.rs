//! Typed station and media settings shared by the resolved resource profiles.
use super::ConfigError;

pub(super) const DEFAULT_ASL_REGISTRATION_URL: &str = "https://register.allstarlink.org/";
pub(super) const DEFAULT_ASL_INTERVAL_S: u16 = 60;
pub(super) const DEFAULT_IAX_LOCAL_PORT: u16 = 4569;

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

/// Local-transmitter DTMF regeneration after general and node-scoped inheritance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedDtmfRegenerationSettings {
    /// Regenerate addressed idle link digits on the local transmitter only.
    pub enabled: bool,
    /// Tone duration in milliseconds, 1 through 1000.
    pub duration_ms: u32,
    /// Following silence in milliseconds, 1 through 1000.
    pub gap_ms: u32,
    /// Combined peak of both tone components, -60 through 0 dBFS.
    pub level_dbfs: i8,
}

impl Default for ResolvedDtmfRegenerationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            duration_ms: 200,
            gap_ms: 50,
            level_dbfs: -12,
        }
    }
}

/// Fully owned node settings after general and node-scoped inheritance.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedNodeSettings {
    /// Whether the node starts.
    pub enabled: bool,
    /// Whether local receive may transmit concurrently.
    pub full_duplex: bool,
    /// Whether completed local DTMF frames are muted.
    pub dtmf_muting: bool,
    /// Local transmitter signaling generated from eligible addressed peer digits.
    pub dtmf_regeneration: ResolvedDtmfRegenerationSettings,
    /// Whether this node records and replays receive bursts after unkey.
    pub parrot_enabled: bool,
    /// Extra receive-to-transmit delay used for squelch-tail and DTMF lookback.
    pub squelch_delay_ms: u64,
    /// Interval for publishing receive/transmit meter snapshots.
    pub status_snapshot_interval_ms: u64,
    /// Transmit hang duration in milliseconds.
    pub hang_ms: u64,
    /// Enable CTCSS only for received traffic or command-response telemetry.
    pub ctcss_encode_on_input: bool,
    /// Received-traffic CTCSS hang in milliseconds, no longer than transmit hang.
    pub ctcss_hang_ms: u64,
    /// Continuous-source watchdog duration; zero disables it.
    pub transmit_timeout_ms: u64,
    /// Kerchunk threshold in milliseconds.
    pub kerchunk_max_ms: u64,
    /// Receive-active telemetry attenuation in dB.
    pub telemetry_duck_db: i64,
    /// Unkey-to-courtesy delay in milliseconds.
    pub courtesy_delay_ms: u64,
    /// Node radio channel.
    pub channel: String,
    /// Selected Fluent locale, inherited from general settings.
    pub language: String,
    /// Optional callsign.
    pub callsign: String,
    /// Optional AllStarLink-compatible status reporting URL; empty disables reporting.
    pub statpost_url: String,
    /// Status reporting interval in seconds.
    pub statpost_time: u64,
    /// ASL3 HTTPS node-registration endpoint; empty disables registration.
    pub iax_registration_url: String,
    /// HTTPS registration refresh interval in seconds.
    pub iax_registration_interval_s: u64,
    /// Locally advertised IAX2 UDP port.
    pub iax_local_port: u16,
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
    /// Idle timeout for an unlocked DTMF administration session.
    pub dtmf_admin_timeout_ms: u64,
}

/// Owned identifier media and scheduling defaults after scope resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedIdentifierSettings {
    /// Optional manual message selector after ID-scope inheritance.
    pub dtmf_id: Option<String>,
    /// Audience and traffic policy for this configured ID.
    pub playback: crate::controller::PlaybackPolicy,
    /// Start deadline for a manually selected attempt; automatic ID timing is unchanged.
    pub delivery: crate::controller::PoliteDelivery,
    /// Ordered content, retaining the existing media chain when absent.
    pub content: String,
    /// Optional localized message embedded by this identifier.
    pub message_id: Option<String>,
    /// Per-attempt values supplied to the identifier's catalog message.
    pub bindings: std::collections::BTreeMap<String, super::BindingSource>,
    /// Positive identifier interval in milliseconds.
    pub interval_ms: u64,
    /// Nonnegative scheduler priority.
    pub priority: u64,
    /// Whether identification waits for the first qualifying key-up.
    pub first_key_only: bool,
    /// Whether to identify even while the node is inactive.
    pub regardless_of_activity: bool,
    /// Whether to defer identification while traffic is active.
    pub polite: bool,
    /// Maximum polite deferral in milliseconds.
    pub polite_maximum_wait_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Owned announcement settings after flat, node, and named resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAnnouncementSettings {
    /// Zero plays after each ordinary transmitter release.
    pub interval_ms: u64,
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain in dB.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain in dB.
    pub morse_level_db: i64,
}

/// Resolved Morse defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMorseSettings {
    /// Tone frequency in hertz.
    pub frequency_hz: u64,
    /// Speed in words per minute.
    pub speed_wpm: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved speech defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpeechSettings {
    /// Voice model path.
    pub voice: String,
    /// Speaking speed percentage.
    pub speed_percent: u64,
    /// Level in dB.
    pub level_db: i64,
}
/// Resolved clock-announcement defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTimeSettings {
    /// Twelve- or twenty-four-hour display.
    pub format: u64,
}
/// Resolved named courtesy assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCourtesySettings {
    /// Configured sound-file path.
    pub sound_file: String,
    /// Configured speech text.
    pub speech_text: String,
    /// Piper voice model path.
    pub speech_model: String,
    /// Piper speaking speed percentage.
    pub speech_speed_percent: u64,
    /// Piper gain, fixed at zero because courtesy uses its common level.
    pub speech_level_db: i64,
    /// Configured Morse fallback text.
    pub morse_text: String,
    /// Morse speed in words per minute.
    pub morse_speed_wpm: u64,
    /// Morse tone frequency in hertz.
    pub morse_frequency_hz: u64,
    /// Morse gain, inherited from the common courtesy level.
    pub morse_level_db: i64,
    /// Optional generated tone sequence.
    pub tone_sequence: String,
    /// Courtesy input.
    pub input: String,
    /// Optional linked peer.
    pub remote_node: String,
    /// Common courtesy level in dB.
    pub level_db: i64,
}
/// Resolved permanent direct-link declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPermanentLinkSettings {
    /// Link label.
    pub name: String,
    /// Ordered remote decimal nodes, with index zero at highest priority.
    pub remote_nodes: Vec<String>,
    /// Optional display name for this permanent-link group.
    pub group_name: Option<String>,
}
/// Existing DTMF administrator PHC policy, independent of mailbox PIN cost limits.
pub fn valid_admin_hash(value: &str) -> bool {
    argon2::password_hash::PasswordHash::new(value)
        .is_ok_and(|hash| hash.algorithm.as_str() == "argon2id")
}

/// A validated node identity with the transport's 63-byte limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Construct an identity containing no whitespace/brackets and at most 63 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 63
            || value
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '[' | ']'))
        {
            return Err(ConfigError::structure(&value, "invalid node identity"));
        }
        Ok(Self(value))
    }

    /// Borrow the identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
