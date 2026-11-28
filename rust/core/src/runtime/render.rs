//! Existing bounded scheduler/status text and speech pronunciation, independent of adapters.

use super::prepare::RuntimeError;
use crate::{
    link::LinkManager,
    messages::{LinkDisplayMode, Message, MessageCatalog, MessageForms},
};

pub(super) fn direct_status(
    manager: &LinkManager,
    catalog: &MessageCatalog,
) -> Result<(MessageForms, String), RuntimeError> {
    let peers: Vec<_> = manager
        .snapshot()
        .into_iter()
        .filter(|peer| !peer.ended && !peer.retrying)
        .collect();
    let Some(peer) = peers.first() else {
        return catalog
            .format(&Message::NoLinks)
            .map(|forms| (forms, String::new()))
            .map_err(|_| RuntimeError::Preparation);
    };
    let mode = if peer.mode.transmits() {
        LinkDisplayMode::Transceive
    } else if peer.mode.forwards() {
        LinkDisplayMode::Monitor
    } else {
        LinkDisplayMode::Local
    };
    catalog
        .format(&Message::LinkStatus {
            count: peers.len(),
            peer: &peer.name,
            mode,
        })
        .map(|forms| (forms, peer.name.clone()))
        .map_err(|_| RuntimeError::Preparation)
}

pub(crate) fn morse(source: &str) -> Option<String> {
    const ALLOWED: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/.,?-=+@()'!\":;_$& \t\r\n";
    let text: String = source
        .bytes()
        .map(|byte| {
            if ALLOWED.contains(&byte) {
                char::from(byte)
            } else {
                ' '
            }
        })
        .collect();
    text.bytes()
        .any(|byte| !byte.is_ascii_whitespace())
        .then_some(text)
}

/// Spell raw identities once, preserving prepared comma-separated forms and ordinary numbers.
pub fn telemetry_speech(source: &str, nodes: &[&str]) -> String {
    let mut output = String::new();
    for (index, word) in source.split(' ').enumerate() {
        if index > 0 {
            output.push(' ');
        }
        let node = nodes.contains(&word) && !word.is_empty();
        let body = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let prepared = word.contains(',')
            && body
                .strip_prefix("node,")
                .unwrap_or(body)
                .split(',')
                .all(|part| part.len() == 1);
        let callsign = word.bytes().any(|b| b.is_ascii_alphabetic())
            && word.bytes().any(|b| b.is_ascii_digit())
            && !prepared;
        if node {
            output.push_str("node,");
        }
        if node || callsign {
            output.push_str(
                &word
                    .chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        } else {
            output.push_str(word);
        }
    }
    output
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
