//! Original human-text routing, independent of local speech admission.

use rpt_advanced_core::link::{HumanMessage, TextAudience};
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
/// Source-verified ASL flooding, with no synthesis, protocol I/O or new wire IDs.
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
        mut send: impl FnMut(&TextDestination, &CStr) -> bool,
    ) -> TextDisposition {
        let mut result = TextDisposition::default();
        if self.locals.contains(&message.source) {
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
        // Only the ASL adapter currently implements human-text serialization.
        // Unknown future protocols must supply their own wire contract first.
        let wire = CString::new(format!(
            "M {} {destination} {}",
            message.source, message.body
        ));
        for peer in peers.iter().filter(|peer| {
            peer.can_forward_text
                && peer.node != message.origin.peer
                && peer.node != message.source
                && !peer.node.starts_with('0')
                && !(peer.protocol == message.origin.protocol
                    && peer.connection_generation == message.origin.connection_generation)
        }) {
            if peer.protocol == 1 && wire.as_ref().is_ok_and(|wire| send(peer, wire)) {
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
