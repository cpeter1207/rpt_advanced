//! Existing bounded scheduler/status text and speech pronunciation, independent of adapters.

use super::prepare::RuntimeError;
use crate::{
    link::LinkManager,
    messages::{Daypart, LinkDisplayMode, Message, MessageCatalog, MessageForms},
    schedule::CivilTime,
    template::{MessageTemplate, TemplateValues},
    time::{TimeAnnouncement, TimeFormat},
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

pub(super) fn message(
    template: &MessageTemplate,
    clock: CivilTime,
    format: u64,
    node: &str,
    callsign: &str,
    links: &LinkManager,
    catalog: &MessageCatalog,
) -> Result<String, RuntimeError> {
    let (year, month, day, weekday, hour, minute) = clock.components();
    let day_name = catalog
        .format(&Message::DayOfWeek { day: weekday })
        .map_err(|_| RuntimeError::Preparation)?;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = TimeAnnouncement::format(
        hour.into(),
        minute.into(),
        TimeFormat::try_from(format).map_err(|_| RuntimeError::Preparation)?,
        catalog,
    )
    .map_err(|_| RuntimeError::Preparation)?;
    let daypart = match hour {
        0..=11 => Daypart::Morning,
        12..=16 => Daypart::Afternoon,
        _ => Daypart::Evening,
    };
    let greeting = catalog
        .format(&Message::Greeting { daypart })
        .map_err(|_| RuntimeError::Preparation)?;
    let (status, _) = direct_status(links, catalog)?;
    template
        .render(&TemplateValues {
            day_of_week: &day_name.text,
            date: &date,
            time: time.morse(),
            greeting: &greeting.text,
            link_status: &status.text,
            node,
            callsign,
        })
        .map_err(|_| RuntimeError::Preparation)
}

pub(super) fn morse(source: &str) -> Option<String> {
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

/// Ordinary telemetry keeps numbers intact and spells known nodes and mixed letter/digit words.
pub fn telemetry_speech(source: &str, nodes: &[&str]) -> String {
    let mut output = String::new();
    for (index, word) in source.split(' ').enumerate() {
        if index > 0 {
            output.push(' ');
        }
        let node = nodes.contains(&word) && !word.is_empty();
        let callsign = word.bytes().any(|b| b.is_ascii_alphabetic())
            && word.bytes().any(|b| b.is_ascii_digit());
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

pub(super) fn scheduled_speech(source: &str, node: &str, callsign: &str, peer: &str) -> String {
    let mut output = String::new();
    let mut index = 0;
    let mut fragment = 0;
    while index < source.len() {
        let matched = [(node, true), (peer, true), (callsign, false)]
            .into_iter()
            .find(|(identity, _)| {
                !identity.is_empty()
                    && source[index..].starts_with(identity)
                    && (index == 0 || !source.as_bytes()[index - 1].is_ascii_alphanumeric())
                    && source
                        .as_bytes()
                        .get(index + identity.len())
                        .is_none_or(|b| !b.is_ascii_alphanumeric())
            });
        if let Some((identity, is_node)) = matched {
            output.push_str(&telemetry_speech(&source[fragment..index], &[]));
            if is_node {
                output.push_str("node,");
            }
            output.push_str(
                &identity
                    .chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            index += identity.len();
            fragment = index;
        } else {
            index += source[index..].chars().next().map_or(1, char::len_utf8);
        }
    }
    output.push_str(&telemetry_speech(&source[fragment..], &[]));
    output
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
