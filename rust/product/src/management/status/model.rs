//! Public observation records copied from the live owner view.
use super::query::{Selection, Topic};
use crate::management::{Id, Observed, Snapshot};
use rpt_advanced_core::config::resources::candidate::{ResourceKind as Kind, ResourceScope};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// Observation coordinates use the service clock for ordering, with UTC for display.
#[derive(Clone, Debug)]
pub(super) struct Stamp {
    pub(super) utc: Option<String>,
    pub(super) monotonic_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Owner {
    Site,
    Port(Id),
}

impl Owner {
    fn document(self) -> Value {
        match self {
            Self::Site => json!({"site":true}),
            Self::Port(id) => json!({"port_id":id.to_string()}),
        }
    }

    pub(super) fn selected(self, selection: &Selection) -> bool {
        match self {
            Self::Site => selection.site,
            Self::Port(id) => selection.all_ports || selection.ports.contains(&id),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Entity {
    owner: Owner,
    class: &'static str,
    id: String,
    generation: u64,
}

/// A keyed record can be moved across snapshot pages without splitting its payload.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct RecordKey {
    pub(super) owner: Owner,
    pub(super) topic: Option<Topic>,
    pub(super) path: Vec<String>,
    class: &'static str,
    id: String,
    generation: Option<u64>,
    media_epoch: Option<u64>,
    pub(super) dependency: Option<Entity>,
}

impl RecordKey {
    pub(super) fn new(
        owner: Owner,
        topic: Option<Topic>,
        path: &[&str],
        class: &'static str,
        id: String,
        generation: Option<u64>,
    ) -> Self {
        Self {
            owner,
            topic,
            path: path.iter().map(|part| (*part).to_owned()).collect(),
            class,
            id,
            generation,
            media_epoch: None,
            dependency: None,
        }
    }

    pub(super) fn depends_on(mut self, class: &'static str, id: &str, generation: u64) -> Self {
        self.dependency = Some(Entity {
            owner: self.owner,
            class,
            id: id.to_owned(),
            generation,
        });
        self
    }

    pub(super) fn with_media_epoch(mut self, epoch: Option<u64>) -> Self {
        self.media_epoch = epoch;
        self
    }

    pub(super) fn entity(&self) -> Option<Entity> {
        matches!(self.class, "link" | "call").then_some(Entity {
            owner: self.owner,
            class: self.class,
            id: self.id.clone(),
            generation: self.generation?,
        })
    }

    pub(super) fn polled_fields(&self) -> &'static [&'static str] {
        if self.class == "link" {
            &["media", "permanent", "group_selected", "mode", "muted"]
        } else {
            &[]
        }
    }

    pub(super) fn live_entity(&self, value: &Value) -> Option<Entity> {
        match self.class {
            "link" if value["state"] == "connected" => self.entity(),
            "call" if value["attached"] == true => self.entity(),
            _ => None,
        }
    }

    pub(super) fn selected(&self, selection: &Selection) -> bool {
        self.owner.selected(selection)
            && self
                .topic
                .is_none_or(|topic| selection.topics.contains(&topic))
    }

    fn subject(&self) -> Subject<'_> {
        Subject {
            class: self.class,
            id: &self.id,
            generation: self.generation.map(|g| g.to_string()),
        }
    }

    fn change(&self, previous: Option<&Value>, next: Option<&Value>) -> &'static str {
        match (self.class, previous, next) {
            ("port", None, Some(_)) => "port.added",
            ("port", Some(_), None) => "port.removed",
            // Authoritative lifecycle/key kinds are emitted only by the owning producer.
            ("radio", _, _) => "radio.changed",
            ("link", _, _) => "link.changed",
            ("call", _, _) => "phone_patch.call_changed",
            ("registration", _, _) => "phone_patch.registration_changed",
            ("equipment", _, _) => "equipment.changed",
            ("graph", _, None) => "audio.graph_retired",
            ("graph", _, Some(_)) => "audio.graph_changed",
            ("resources", _, _) => "resources.sampled",
            ("site", _, _) => "site.changed",
            _ => "diagnostics.changed",
        }
    }

    pub(super) fn event(
        &self,
        cursor: u64,
        stamp: &Stamp,
        previous: Option<&Value>,
        next: Option<&Value>,
    ) -> Value {
        self.edge(cursor, stamp, self.change(previous, next), next, None)
    }

    pub(super) fn edge(
        &self,
        cursor: u64,
        stamp: &Stamp,
        kind: &'static str,
        next: Option<&Value>,
        terminal: Option<&Value>,
    ) -> Value {
        StateEdge {
            state_cursor: cursor.to_string(),
            observed_at: stamp.utc.as_deref(),
            monotonic_ms: stamp.monotonic_ms.to_string(),
            scope: self.owner.document(),
            subject: self.subject(),
            kind,
            operation: if next.is_some() { "upsert" } else { "remove" },
            data: next
                .or(terminal)
                .cloned()
                .unwrap_or_else(|| json!({"id":self.id,"reason":"retired"})),
        }
        .document()
    }

    pub(super) fn is_meter(&self) -> bool {
        self.class == "meter"
    }
}

#[derive(Serialize)]
struct Subject<'a> {
    #[serde(rename = "type")]
    class: &'static str,
    id: &'a str,
    generation: Option<String>,
}

#[derive(Serialize)]
struct StateEdge<'a> {
    state_cursor: String,
    observed_at: Option<&'a str>,
    monotonic_ms: String,
    scope: Value,
    subject: Subject<'a>,
    kind: &'static str,
    operation: &'static str,
    data: Value,
}

impl StateEdge<'_> {
    fn document(self) -> Value {
        serde_json::to_value(self).expect("public state record")
    }
}

pub(super) type Records = BTreeMap<RecordKey, Arc<Value>>;

/// Immutable shared cut. Page rendering and network writes borrow the same records.
#[derive(Clone)]
pub(super) struct Capture {
    pub(super) stamp: Stamp,
    pub(super) active_revision: Option<String>,
    pub(super) cursor: u64,
    pub(super) records: Records,
    pub(super) visible_ports: Vec<Id>,
    source_health: BTreeMap<Id, [bool; 4]>,
}

impl Capture {
    pub(super) fn project(observed: &Observed, saved: &Snapshot, stamp: Stamp) -> Self {
        let site = saved.registry.site_id();
        let domain = &observed.domain;
        let mut records = Records::new();
        let mut put = |key, value| {
            records.insert(key, Arc::new(value));
        };
        let visible_ports = saved
            .registry
            .port_keys()
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        put(
            RecordKey::new(
                Owner::Site,
                None,
                &["identity"],
                "site",
                site.to_string(),
                None,
            ),
            json!({
                "id":site.to_string(),
                "availability":if observed.status_ready { "available" } else { "recovering" },
                "desired_revision":saved.revision,
                "active_revision":observed.active_revision,
                "tx_inhibited":!observed.status_ready,
                "activation":saved.activation,
                "storage":saved.storage,
            }),
        );
        if let Some(resources) = &domain.site_resources {
            put(
                RecordKey::new(
                    Owner::Site,
                    Some(Topic::Resources),
                    &["resources"],
                    "resources",
                    site.to_string(),
                    None,
                ),
                json!(resources.as_ref()),
            );
        }
        for (name, id) in saved.registry.port_keys() {
            let owner = Owner::Port(id);
            let live = domain.ports.get(&id);
            let generation = observed.owners.get(&id).copied();
            put(
                RecordKey::new(
                    owner,
                    None,
                    &["identity"],
                    "port",
                    id.to_string(),
                    generation,
                ),
                json!({
                    "id":id.to_string(), "key": name.as_str(), "generation":generation.map(|g| g.to_string()),
                    "state": if live.is_some() {"active"} else {"unavailable"}
                }),
            );
            let quality = if live.is_some() {
                "valid"
            } else {
                "unavailable"
            };
            put(
                RecordKey::new(
                    owner,
                    Some(Topic::Radio),
                    &["radio"],
                    "radio",
                    id.to_string(),
                    generation,
                ),
                json!({
                    "quality":quality,
                    "receiver":live.and_then(|port| port.receiver),
                    "receive_sample_monotonic_ms":live.and_then(|port|port.receive_sample_ms).map(|at|at.to_string()),
                    "transmit_key_requested":live.and_then(|port| port.transmit_key_requested),
                    "transmit_policy":live.and_then(|port|port.transmit_policy).map(|(at_ms, policy)| {
                        let mut value = super::edges::transmit_policy_document(Some(policy));
                        value["monotonic_ms"] = json!(at_ms.to_string());
                        value["observed_at"] = json!(super::stamp_at(observed.clock, at_ms).utc);
                        value
                    }),
                    "physical_ptt":live.and_then(|port|port.physical_transmit.as_ref()).and_then(|tx|tx.observed),
                    "feedback":live.and_then(|port|port.physical_transmit.as_ref()).map(|tx|json!({
                        "rendered_ptt":tx.rendered,"serviced_ptt":tx.serviced,"observed_ptt":tx.observed,
                        "monotonic_ms":tx.at_ms.to_string(),
                        "observed_at":super::stamp_at(observed.clock,tx.at_ms).utc,
                        "quality":if tx.observed.is_some() {"valid"} else {"unavailable"}
                    })),
                    "receiver_signaling":live.and_then(|port|port.physical_receive.as_ref()).map(|rx|json!({
                        "carrier_detected":rx.carrier_detected,"subaudible_detected":rx.subaudible_detected,
                        "qualified":rx.qualified,"ctcss_decode_index":rx.ctcss_decode_index,
                        "dcs_valid":rx.dcs_valid,"monotonic_ms":rx.at_ms.to_string(),
                        "observed_at":super::stamp_at(observed.clock,rx.at_ms).utc,"quality":"valid"
                    })),
                }),
            );
            let Some(port) = live else {
                continue;
            };
            for chain in crate::status_meter::LOCAL_CHAINS {
                let graph_generation = port.audio_generation;
                let stages = chain.stages.iter().enumerate().map(|(position, stage)| {
                    let available = stage.tap.is_some_and(|tap| {
                        !matches!(stage.id, "incoming_text" | "regeneration")
                            || port.status_meters[tap].is_some_and(|meter| {
                                Some(meter.graph_generation) == graph_generation
                                    && meter.quality != crate::status_meter::Quality::Unavailable
                            })
                    });
                    json!({
                        "id":stage.id,"purpose":stage.purpose,"position":position,"inputs":stage.inputs,
                        "state":if available {"active"} else {"unavailable"},
                        "signals":if available {json!(["mono"])} else {json!([])},
                    })
                }).collect::<Vec<_>>();
                put(
                    RecordKey::new(
                        owner,
                        Some(Topic::Meters),
                        &["graphs", chain.id],
                        "graph",
                        chain.id.to_owned(),
                        graph_generation,
                    ),
                    json!({"chain_id":chain.id,"direction":chain.direction,
                        "graph_generation":graph_generation.map(|generation|generation.to_string()),
                        "sample_rate_hz":48000,"sample_type":"float32",
                        "channels":["mono"],"stages":stages,
                        "meter_profile":meter_profile()}),
                );
                for stage in chain.stages {
                    let Some(meter) = stage
                        .tap
                        .and_then(|tap| port.status_meters[tap])
                        .filter(|meter| Some(meter.graph_generation) == graph_generation)
                    else {
                        continue;
                    };
                    let subject = format!("{}:{}:mono", chain.id, stage.id);
                    put(
                        RecordKey::new(
                            owner,
                            Some(Topic::Meters),
                            &["meters", &subject],
                            "meter",
                            subject.clone(),
                            graph_generation,
                        ),
                        meter_observation(
                            id,
                            (chain.id, stage.id, 48000),
                            meter,
                            &stamp,
                            observed.clock,
                        ),
                    );
                }
            }
            put(
                RecordKey::new(
                    owner,
                    Some(Topic::Diagnostics),
                    &["diagnostics", "summary"],
                    "diagnostics",
                    id.to_string(),
                    generation,
                ),
                json!({
                    "calendar":port.calendar,
                    "last_dial":port.last_dial.as_ref(),
                    "last_disconnect":port.last_disconnect.as_ref(),
                }),
            );
            for peer in port.peers.iter().chain(&port.link_retries) {
                let Some(alias) = peer
                    .node
                    .as_deref()
                    .or(peer.call_id.as_deref())
                    .or(peer.media_binding.as_deref())
                else {
                    continue;
                };
                let peer_id = format!("{}:{alias}", peer.kind);
                let mut status = json!(peer);
                status["quality"] = json!("valid");
                put(
                    RecordKey::new(
                        owner,
                        Some(Topic::Links),
                        &["links", &peer_id],
                        "link",
                        peer_id.clone(),
                        peer.connection,
                    ),
                    status,
                );
            }
            for (family, meters) in [("link", &port.link_meters), ("call", &port.call_meters)] {
                for ((entity, generation), measurement) in meters {
                    super::peer::project(
                        id,
                        (family, entity, *generation),
                        measurement,
                        super::peer::EgressGraph::Shared(port.audio_generation),
                        &stamp,
                        observed.clock,
                        &mut put,
                    );
                }
            }
            for webrtc in port.webrtc.values() {
                if port.audio_generation == Some(webrtc.graph_generation) {
                    super::webrtc::project(id, webrtc, &stamp, observed.clock, &mut put);
                }
            }
            for (attempt, (generation, state)) in &port.phone_progress {
                put(
                    RecordKey::new(
                        owner,
                        Some(Topic::PhonePatch),
                        &["phone_patch", "progress", attempt],
                        "phone_progress",
                        attempt.clone(),
                        Some(*generation),
                    ),
                    state.clone(),
                );
            }
            for call in &port.calls {
                put(
                    RecordKey::new(
                        owner,
                        Some(Topic::PhonePatch),
                        &["phone_patch", "calls", &call.call_id],
                        "call",
                        call.call_id.clone(),
                        Some(call.generation),
                    ),
                    json!(call),
                );
            }
        }
        for (&(port, account), registration) in &domain.registrations {
            let account_id = account.to_string();
            let (topic, family) = match saved.registry.by_id(account).map(|binding| binding.kind) {
                Some(Kind::PhonePatch) => (Topic::PhonePatch, "phone_patch"),
                Some(Kind::Allstarlink | Kind::Echolink) => (Topic::Links, "link_registrations"),
                _ => continue,
            };
            put(
                RecordKey::new(
                    Owner::Port(port),
                    Some(topic),
                    &[family, "registrations", &account_id],
                    "registration",
                    account_id.clone(),
                    Some(registration.generation),
                ),
                json!(registration),
            );
        }
        for (&(scope, id), value) in &domain.resources {
            let owner = match scope {
                ResourceScope::Site(_) => Owner::Site,
                ResourceScope::Port(port) => Owner::Port(port),
            };
            let event = saved
                .registry
                .by_id(id)
                .is_some_and(|binding| binding.kind == Kind::Event);
            let id = id.to_string();
            let (topic, path, class) = if event {
                (
                    Topic::Diagnostics,
                    vec!["diagnostics", "scheduled_events", &id],
                    "scheduler",
                )
            } else {
                (Topic::Equipment, vec!["equipment", &id], "equipment")
            };
            put(
                RecordKey::new(owner, Some(topic), &path, class, id.clone(), None),
                value.clone(),
            );
        }
        let source_health = domain
            .ports
            .iter()
            .map(|(&port, status)| {
                let rx = status
                    .receive_sample_ms
                    .is_some_and(|at| stamp.monotonic_ms.saturating_sub(at) <= 500);
                (
                    port,
                    [
                        rx,
                        status.transmit_key_requested.is_some(),
                        status.physical_transmit.is_some(),
                        status.transmit_policy.is_some(),
                    ],
                )
            })
            .collect();
        Self {
            stamp,
            active_revision: observed.active_revision.clone(),
            cursor: 0,
            records,
            visible_ports,
            source_health,
        }
    }

    pub(super) fn source_stale(&self, key: &RecordKey) -> bool {
        let Owner::Port(port) = key.owner else {
            return false;
        };
        if key.path.first().is_none_or(|part| part != "radio_signals") {
            return false;
        }
        let index = match key.id.as_str() {
            "source" | "admission" => 0,
            "transmit" => 1,
            "physical_transmit" => 2,
            "transmit_policy" => 3,
            _ => return false,
        };
        self.source_health
            .get(&port)
            .is_some_and(|health| !health[index])
    }

    #[cfg(test)]
    pub(super) fn fixture(port: Id, stamp: Stamp) -> Self {
        Self {
            stamp,
            active_revision: Some("test-revision".into()),
            cursor: 0,
            records: BTreeMap::new(),
            visible_ports: vec![port],
            source_health: BTreeMap::new(),
        }
    }
}

pub(super) fn meter_profile() -> Value {
    json!({"peak_window_ms":100,"rms_window_ms":100,"loudness_window_ms":400,
        "peak_hold_window_ms":2000,"level_unit":"dBFS","loudness_unit":"LUFS","default_hz":20})
}

pub(super) fn meter_observation(
    port: Id,
    stage: (&str, &str, u32),
    meter: crate::status_meter::Measurement,
    capture: &Stamp,
    clock: Option<rpt_advanced_core::runtime::RuntimeClock>,
) -> Value {
    let (chain, stage, rate) = stage;
    let measured = super::stamp_at(clock, meter.monotonic_ms);
    let quality = if capture.monotonic_ms.saturating_sub(meter.monotonic_ms) > 500 {
        "stale"
    } else {
        meter.quality.as_str()
    };
    let position = (meter.valid_samples > 0).then(|| {
        json!({
            "source_generation":meter.source_generation.to_string(),
            "end_sample":meter.end_sample.to_string(),"rate_hz":rate
        })
    });
    let mut reading = json!({"port_id":port.to_string(),"chain_id":chain,"stage_id":stage,"signal":"mono",
        "graph_generation":meter.graph_generation.to_string(),"measurement_epoch":meter.measurement_epoch.to_string(),
        "monotonic_ms":meter.monotonic_ms.to_string(),"observed_at":measured.utc,
        "quality":quality,"freshness_ms":500,"signal_state":meter.signal_state.as_str(),
        "valid_samples":meter.valid_samples.to_string(),"clipped_samples":meter.clipped_samples.to_string(),
        "dropped_snapshots":meter.dropped_snapshots.to_string(),
                "peak_dbfs":meter.peak_dbfs,"rms_dbfs":meter.rms_dbfs,"lufs":meter.lufs,"peak_hold_dbfs":meter.peak_hold_dbfs});
    if let Some(position) = position {
        reading["audio_position"] = position;
    }
    reading
}
