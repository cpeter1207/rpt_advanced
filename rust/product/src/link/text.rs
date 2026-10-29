//! Original human-text routing, independent of local speech admission.

use rpt_advanced_core::link::{HumanMessage, ProtocolId, TextAudience};
use std::ffi::{CStr, CString};

/// Authenticated current egress, prepared on serialized control rather than audio.
pub struct TextDestination {
    /// Unique current connection, never just a reused node number.
    pub connection_generation: u64,
    /// Destination protocol adapter identity; ASL is1.
    pub protocol: u32,
    /// Direct connected peer name.
    pub node: String,
    /// Whether this connection supports human-text forwarding.
    pub can_forward_text: bool,
}
/// Independent local eligibility and bounded per-destination send outcomes.
#[derive(Default)]
pub struct TextDisposition {
    /// Broadcast or addressed to a configured local station.
    pub locally_eligible: bool,
    /// Original text accepted by a destination's bounded control queue.
    pub destinations_admitted: usize,
    /// Eligible destinations whose queue or serializer rejected only this message.
    pub destinations_rejected: usize,
}
/// Protocol-correct human text, borrowed for one bounded egress admission.
pub enum TextPayload<'a> {
    /// ASL's original sender/destination/body envelope.
    AllStarLink(&'a CStr),
    /// EchoLink's broadcast sender/body, never an ASL control envelope.
    EchoLink {
        /// Literal original represented sender, not the local account callsign.
        sender: &'a str,
        /// Literal body, never a command or template.
        body: &'a str,
    },
}
/// Source-verified flooding with per-protocol representation and no synthesis/I/O.
pub struct TextRouter {
    locals: Vec<String>,
}
impl TextRouter {
    /// Retain all configured local identities for ASL's local-consume rule.
    pub fn new(locals: Vec<String>) -> Self {
        Self { locals }
    }
    /// Forward once per eligible egress, regardless of local speech admission.
    /// The caller binds each destination to its current authenticated connection.
    pub fn route(
        &self,
        message: &HumanMessage,
        peers: &[TextDestination],
        mut send: impl FnMut(&TextDestination, TextPayload<'_>) -> bool,
    ) -> TextDisposition {
        let mut result = TextDisposition::default();
        let represented = message
            .represented_source
            .as_ref()
            .map(|id| id.operator_alias());
        if self.locals.contains(&message.source)
            || represented
                .as_ref()
                .is_some_and(|id| self.locals.contains(id))
        {
            return result;
        }
        let destination = match &message.audience {
            TextAudience::Broadcast => "0",
            TextAudience::Node(node) => node,
        };
        let local = self.locals.iter().any(|node| node == destination);
        result.locally_eligible = local || matches!(message.audience, TextAudience::Broadcast);
        if local {
            return result;
        }
        let asl_source = if message.origin.protocol == ProtocolId::EchoLink as u32 {
            represented.as_deref()
        } else {
            Some(message.source.as_str())
        };
        let wire = asl_source.and_then(|source| {
            CString::new(format!("M {source} {destination} {}", message.body)).ok()
        });
        for peer in peers.iter().filter(|peer| {
            peer.can_forward_text
                && peer.node != message.origin.peer
                && peer.node != message.source
                && Some(peer.node.as_str()) != represented.as_deref()
                && !peer.node.starts_with('0')
                && !(peer.protocol == message.origin.protocol
                    && peer.connection_generation == message.origin.connection_generation)
        }) {
            let admitted = match peer.protocol {
                1 => wire
                    .as_ref()
                    .is_some_and(|wire| send(peer, TextPayload::AllStarLink(wire))),
                2 if matches!(message.audience, TextAudience::Broadcast)
                    && !message.source.contains('\0')
                    && !message.body.contains('\0') =>
                {
                    send(
                        peer,
                        TextPayload::EchoLink {
                            sender: &message.source,
                            body: &message.body,
                        },
                    )
                }
                _ => false,
            };
            if admitted {
                result.destinations_admitted += 1;
            } else {
                result.destinations_rejected += 1;
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
