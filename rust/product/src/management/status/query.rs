//! Canonical scope and topic selection shared by status GET and WebSocket.
use crate::management::{Fault, Id};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Topic {
    Radio,
    Links,
    PhonePatch,
    Telemetry,
    Equipment,
    Resources,
    Diagnostics,
    Meters,
}

impl Topic {
    pub(super) const STATE: [Self; 7] = [
        Self::Radio,
        Self::Links,
        Self::PhonePatch,
        Self::Telemetry,
        Self::Equipment,
        Self::Resources,
        Self::Diagnostics,
    ];
    pub(super) const ALL: [Self; 8] = [
        Self::Radio,
        Self::Links,
        Self::PhonePatch,
        Self::Telemetry,
        Self::Equipment,
        Self::Resources,
        Self::Diagnostics,
        Self::Meters,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Radio => "radio",
            Self::Links => "links",
            Self::PhonePatch => "phone_patch",
            Self::Telemetry => "telemetry",
            Self::Equipment => "equipment",
            Self::Resources => "resources",
            Self::Diagnostics => "diagnostics",
            Self::Meters => "meters",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Selection {
    pub(super) site: bool,
    pub(super) all_ports: bool,
    pub(super) ports: BTreeSet<Id>,
    pub(super) topics: BTreeSet<Topic>,
}

impl Selection {
    pub(super) fn document(&self) -> serde_json::Value {
        let mut document = serde_json::json!({
            "site": self.site,
            "ports": self.ports.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "topics": self.topics,
        });
        if self.all_ports {
            document["all_ports"] = true.into();
        }
        document
    }

    pub(super) fn query(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if self.site {
            query.append_pair("site", "true");
        }
        if self.all_ports {
            query.append_pair("all_ports", "true");
        }
        for port in &self.ports {
            query.append_pair("port", &port.to_string());
        }
        if self.topics.iter().copied().ne(Topic::STATE) {
            let topics = self
                .topics
                .iter()
                .map(|topic| topic.name())
                .collect::<Vec<_>>();
            query.append_pair("topics", &topics.join(","));
        }
        query.finish()
    }
}

#[derive(Debug)]
pub(super) struct Query {
    pub(super) selection: Selection,
    pub(super) page: Option<(Id, usize)>,
    pub(super) meter_hz: Option<u8>,
}

fn id(text: &str) -> Result<Id, Fault> {
    text.parse::<Id>()
        .ok()
        .filter(|id| id.to_string() == text)
        .ok_or(Fault::Malformed)
}

impl Query {
    pub(super) fn parse(query: Option<&str>, stream: bool) -> Result<Self, Fault> {
        let mut site = false;
        let mut all_ports = false;
        let mut ports = BTreeSet::new();
        let mut topics = None;
        let mut meter_hz = None;
        let mut cursor = None;
        let mut page = None;
        for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
            match name.as_ref() {
                "site" if !site && value == "true" => site = true,
                "all_ports" if !all_ports && value == "true" => all_ports = true,
                "port" => {
                    ports.insert(id(&value)?);
                }
                "topics" if topics.is_none() => {
                    topics = Some(
                        value
                            .split(',')
                            .map(|part| {
                                Topic::ALL
                                    .iter()
                                    .copied()
                                    .find(|topic| topic.name() == part)
                                    .ok_or(Fault::Malformed)
                            })
                            .collect::<Result<BTreeSet<_>, _>>()?,
                    );
                }
                "meter_hz" if stream && meter_hz.is_none() => {
                    meter_hz = Some(match value.as_ref() {
                        "1" => 1,
                        "2" => 2,
                        "5" => 5,
                        "10" => 10,
                        "20" => 20,
                        _ => return Err(Fault::Malformed),
                    });
                }
                "cursor" if !stream && cursor.is_none() => cursor = Some(id(&value)?),
                "page" if !stream && page.is_none() => {
                    page = Some(
                        value
                            .parse::<usize>()
                            .ok()
                            .filter(|index| *index > 0 && index.to_string() == value)
                            .ok_or(Fault::Malformed)?,
                    );
                }
                _ => return Err(Fault::Malformed),
            }
        }
        if ports.len() > 128 || all_ports && !ports.is_empty() || cursor.is_some() != page.is_some()
        {
            return Err(Fault::Malformed);
        }
        let topics = topics.unwrap_or_else(|| BTreeSet::from(Topic::STATE));
        let meter_hz = match (stream && topics.contains(&Topic::Meters), meter_hz) {
            (true, rate) => Some(rate.unwrap_or(20)),
            (false, _) => None,
        };
        Ok(Self {
            selection: Selection {
                site: site || ports.is_empty(),
                all_ports,
                ports,
                topics,
            },
            page: cursor.zip(page),
            meter_hz,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PORT: &str = "361c602c-7664-4600-990d-6bd6cae4b32c";

    #[test]
    fn status_selection_is_shared_and_canonical() {
        let defaults = Query::parse(None, false).unwrap();
        assert!(defaults.selection.site);
        assert!(!defaults.selection.topics.contains(&Topic::Meters));
        let query = format!("port={PORT}&port={PORT}&topics=meters,radio");
        let rest = Query::parse(Some(&query), false).unwrap();
        let stream = Query::parse(Some(&format!("{query}&meter_hz=5")), true).unwrap();
        assert_eq!(rest.selection, stream.selection);
        assert_eq!(stream.meter_hz, Some(5));
        assert!(!rest.selection.site);
        assert_eq!(rest.selection.ports.len(), 1);
        assert_eq!(
            Query::parse(Some(&rest.selection.query()), false)
                .unwrap()
                .selection,
            rest.selection
        );
        assert_eq!(
            Query::parse(Some("topics=meters"), true).unwrap().meter_hz,
            Some(20)
        );
    }

    #[test]
    fn unsupported_or_ambiguous_status_queries_are_rejected() {
        for query in [
            "site=false",
            "all_ports=false",
            "all_ports=true&all_ports=true",
            "site=true&site=true",
            "port=x",
            "topics=",
            "topics=cpu",
            "topics=meters&meter_hz=3",
            "unknown=true",
            "page=1",
            "cursor=x",
            "topics=radio&topics=meters",
        ] {
            assert!(Query::parse(Some(query), false).is_err(), "{query}");
        }
        assert!(Query::parse(Some(&format!("port={PORT}&all_ports=true")), false).is_err());
        assert!(Query::parse(Some("topics=meters&meter_hz=03"), true).is_err());
        assert_eq!(
            Query::parse(Some("meter_hz=10"), true).unwrap().meter_hz,
            None
        );
    }
}
