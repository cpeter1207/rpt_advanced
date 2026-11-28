//! Read-only message definitions resolved on control, before composition admission.

use crate::{
    controller::{PlaybackPolicy, PoliteDelivery},
    media::MediaSource,
};
use std::{collections::BTreeMap, str::FromStr};

/// Allowlisted source of a per-attempt value; never an executable expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingSource {
    /// Configured amateur-radio callsign, with audience-specific spelling.
    Callsign,
    /// Node identity, spoken as individual digits.
    Node,
    /// One approved identity from the port profile or immutable triggering event.
    Identity(IdentitySource),
    /// Local civil date.
    Date,
    /// Local civil time using the node's clock format.
    Time,
    /// Localized weekday.
    DayOfWeek,
    /// Localized daypart greeting.
    Greeting,
    /// Localized current link summary.
    LinkStatus,
    /// Named sensor with quality, time, and engineering units.
    Sensor(String),
    /// Named digital input with quality and time.
    Input(String),
    /// Remaining time, only for an invocation supplying a deadline.
    TimeRemaining,
}

/// Public identity values accepted by native Fluent bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySource {
    /// Exact local management key.
    NodeKey,
    /// Local display name.
    NodeName,
    /// Local AllStarLink number.
    AllstarlinkNumber,
    /// Local AllStarLink callsign.
    AllstarlinkCallsign,
    /// Local EchoLink number.
    EcholinkNumber,
    /// Local EchoLink callsign.
    EcholinkCallsign,
    /// Protocol of the triggering peer.
    PeerProtocol,
    /// Number of the triggering peer.
    PeerNumber,
    /// Callsign of the triggering peer.
    PeerCallsign,
    /// Kind of the triggering event.
    EventKind,
}

use IdentitySource::*;
const STANDARD: &[(&str, &str, BindingSource)] = &[
    ("node-key", "node.key", BindingSource::Identity(NodeKey)),
    ("node-name", "node.name", BindingSource::Identity(NodeName)),
    ("node-callsign", "node.callsign", BindingSource::Callsign),
    (
        "allstarlink-node-number",
        "allstarlink.node_number",
        BindingSource::Identity(AllstarlinkNumber),
    ),
    (
        "allstarlink-callsign",
        "allstarlink.callsign",
        BindingSource::Identity(AllstarlinkCallsign),
    ),
    (
        "echolink-node-number",
        "echolink.node_number",
        BindingSource::Identity(EcholinkNumber),
    ),
    (
        "echolink-callsign",
        "echolink.callsign",
        BindingSource::Identity(EcholinkCallsign),
    ),
    (
        "peer-node-number",
        "event.peer.node_number",
        BindingSource::Identity(PeerNumber),
    ),
    (
        "peer-callsign",
        "event.peer.callsign",
        BindingSource::Identity(PeerCallsign),
    ),
    (
        "peer-protocol",
        "event.peer.protocol",
        BindingSource::Identity(PeerProtocol),
    ),
    (
        "event-kind",
        "event.kind",
        BindingSource::Identity(EventKind),
    ),
    ("date", "date", BindingSource::Date),
    ("time", "time", BindingSource::Time),
    ("day-of-week", "day_of_week", BindingSource::DayOfWeek),
    ("greeting", "greeting", BindingSource::Greeting),
    ("link-status", "link_status", BindingSource::LinkStatus),
    (
        "time-remaining",
        "time_remaining",
        BindingSource::TimeRemaining,
    ),
];

impl BindingSource {
    /// Resolve a fixed flat argument without permitting a different source under its name.
    pub fn standard_argument(name: &str) -> Option<Self> {
        STANDARD
            .iter()
            .find(|(alias, _, _)| *alias == name)
            .map(|(_, _, source)| source.clone())
    }

    /// Decode one approved JSON source path, retaining exact sensor/input member keys.
    pub fn from_public_path(path: &str) -> Option<Self> {
        if let Some((_, _, source)) = STANDARD.iter().find(|(_, name, _)| *name == path) {
            return Some(source.clone());
        }
        let (kind, name) = path.split_once(':')?;
        if name.is_empty()
            || name.chars().count() > 63
            || name.chars().any(|c| c.is_whitespace() || "[]:".contains(c))
        {
            return None;
        }
        match kind {
            "sensor" => Some(Self::Sensor(name.into())),
            "input" => Some(Self::Input(name.into())),
            _ => None,
        }
    }

    /// Reject deadline bindings at callers without a deadline; no invented zero.
    pub fn available_in_context(&self, has_deadline: bool) -> bool {
        !matches!(self, Self::TimeRemaining) || has_deadline
    }
}

impl FromStr for BindingSource {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "callsign" => Self::Callsign,
            "node" => Self::Node,
            "date" => Self::Date,
            "time" => Self::Time,
            "day_of_week" => Self::DayOfWeek,
            "greeting" => Self::Greeting,
            "link_status" => Self::LinkStatus,
            "time_remaining" => Self::TimeRemaining,
            _ => {
                let (family, name) = value.split_once(':').ok_or(())?;
                if name.is_empty()
                    || name.len() > 63
                    || name.chars().any(|c| c.is_whitespace() || "[]:".contains(c))
                {
                    return Err(());
                }
                match family {
                    "sensor" => Self::Sensor(name.into()),
                    "input" => Self::Input(name.into()),
                    _ => return Err(()),
                }
            }
        })
    }
}

/// Field-wise global/node message definition; all owned state stays off audio.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMessageSettings {
    /// Ordered content, defaulting to this definition's catalog message.
    pub content: String,
    /// Configured Fluent identifier, not a DTMF-supplied string.
    pub message_id: Option<String>,
    /// Named Fluent arguments resolved afresh for each admitted playback attempt.
    pub bindings: BTreeMap<String, BindingSource>,
    /// Existing file/speech/Morse/tone source settings and inherited levels.
    pub media: MediaSource,
    /// Effective speech settings for catalog/literal segments without configured speech text.
    pub speech_defaults: super::ResolvedSpeechSettings,
    /// Effective Morse settings for every generated fallback segment.
    pub morse_defaults: super::ResolvedMorseSettings,
    /// Optional canonical positive decimal command selector.
    pub dtmf_id: Option<String>,
    /// Repeat and destination settings shared by all composed-message callers.
    pub playback: PlaybackPolicy,
    /// Polite start deadline used by scheduled or manually selected playback.
    pub delivery: PoliteDelivery,
}

pub(crate) fn selector_valid(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 127
            && !value.starts_with('0')
            && value.bytes().all(|b| b.is_ascii_digit()))
}
