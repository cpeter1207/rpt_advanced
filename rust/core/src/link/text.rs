//! Protocol-neutral human text, separated from authenticated protocol control.
use super::{Protocol, identity};
use std::sync::Arc;

/// Authenticated transport/generation identity. A message's claimed sender is not authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextOrigin {
    /// Prepared station generation; retired generations cannot produce speech.
    pub station_generation: u64,
    /// Unique connection generation, even when a peer number is reused.
    pub connection_generation: u64,
    /// Authenticated ingress peer, not the relayed sender token.
    pub peer: String,
    /// Protocol adapter identity; AllStarLink is 1.
    pub protocol: u32,
}
/// Original protocol audience, retained for local admission and forwarding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextAudience {
    /// Protocol broadcast, encoded as destination 0 in ASL.
    Broadcast,
    /// Directed station message, which may need transit forwarding.
    Node(String),
}
/// Validated human content. Body is literal text, never a command or template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanMessage {
    /// Authenticated connection owner used for authorization and cancellation.
    pub origin: TextOrigin,
    /// Claimed original station, for attribution/ASL flood exclusions only.
    pub source: String,
    /// Original destination, independent of ingress.
    pub audience: TextAudience,
    /// Literal UTF-8 body. Local synthesis bounds do not limit forwarding.
    pub body: Arc<str>,
}
/// A recognized control frame, a supported human form, or an ignored payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassifiedText {
    /// Strict existing signaling path; never synthesized.
    Control(Protocol),
    /// Human message eligible for separate routing/speech admission.
    Human(HumanMessage),
    /// Unknown or malformed text; never guessed to be chat.
    Unknown,
}

/// Classify canonical ASL text without executing its body or trusting its sender.
///
/// Pinned ASL b6cc1cf sender/receiver use `M FROM TO BODY`; the obsolete wiki
/// reverses the header labels. Accept a missing or single terminal NUL, but
/// reject hidden NUL suffixes rather than reproducing C-string truncation.
pub fn classify_asl(origin: TextOrigin, bytes: &[u8]) -> ClassifiedText {
    if let Some(control) = Protocol::parse(bytes) {
        return ClassifiedText::Control(control);
    }
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    let human = (|| {
        let text = std::str::from_utf8(bytes).ok()?;
        if text.chars().any(char::is_control) {
            return None;
        }
        let rest = text.strip_prefix("M ")?.trim_start_matches(' ');
        let (source, rest) = rest.split_once(' ')?;
        let (destination, body) = rest.trim_start_matches(' ').split_once(' ')?;
        let body = body.trim_start_matches(' ');
        // Match ASL's minimum two body bytes, with canonical existing identities.
        if !identity(source) || !identity(destination) || body.len() < 2 {
            return None;
        }
        Some(HumanMessage {
            origin,
            source: source.to_owned(),
            audience: if destination == "0" {
                TextAudience::Broadcast
            } else {
                TextAudience::Node(destination.to_owned())
            },
            body: Arc::from(body),
        })
    })();
    human.map_or(ClassifiedText::Unknown, ClassifiedText::Human)
}
