//! Immutable, public-only observations from one confirmed graph.
use rpt_advanced_core::{
    config::resources::candidate::{
        ResourceId as Id, ResourceKind as Kind, ResourceScope as Scope,
    },
    management::routes::Target,
    runtime::scheduler::CalendarObservation,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Default)]
pub(crate) struct Domain {
    pub(crate) site_resources: Option<Arc<super::resources::Resources>>,
    pub(crate) ports: BTreeMap<Id, Port>,
    pub(crate) registrations: BTreeMap<(Id, Id), Registration>,
    pub(crate) resources: BTreeMap<(Scope, Id), Value>,
}

#[derive(Default)]
pub(crate) struct Port {
    pub(crate) receiver: Option<Receive>,
    pub(crate) receive_sample_ms: Option<u64>,
    pub(crate) transmit_key_requested: Option<bool>,
    pub(crate) physical_transmit: Option<crate::radio_feedback::PhysicalTransmit>,
    pub(crate) physical_receive: Option<crate::radio_feedback::PhysicalReceive>,
    pub(crate) meters: [Option<Meter>; 2],
    pub(crate) audio_generation: Option<u64>,
    pub(crate) link_meters: BTreeMap<(String, u64), crate::link::status_audio::PeerMeasurements>,
    pub(crate) call_meters: BTreeMap<(String, u64), crate::link::status_audio::PeerMeasurements>,
    pub(crate) status_meters:
        [Option<crate::status_meter::Measurement>; crate::status_meter::LOCAL_TAP_COUNT],
    pub(crate) peers: Vec<Peer>,
    pub(crate) link_retries: Vec<Peer>,
    pub(crate) calls: Vec<Call>,
    pub(crate) phone_progress: BTreeMap<String, (u64, Value)>,
    pub(crate) calendar: Option<CalendarObservation>,
    pub(crate) last_disconnect: Option<Disconnect>,
    pub(crate) last_dial: Option<Dial>,
}

pub(crate) const DISCONNECT_PEERS_LIMIT: usize = 16;

/// Latest serialized selection and actual removal of public peer owners.
#[derive(Clone, Serialize)]
pub(crate) struct Disconnect {
    pub selected_count: usize,
    pub removed_count: usize,
    pub peer_nodes: Vec<String>,
}

/// Terminal result of the latest current account's outgoing attempt.
#[derive(Clone, Serialize)]
pub(crate) struct Dial {
    pub node: String,
    pub attached: bool,
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Receive {
    pub muted: bool,
    pub active: bool,
}
impl From<rpt_advanced_core::runtime::links::ReceiveStatus> for Receive {
    fn from(s: rpt_advanced_core::runtime::links::ReceiveStatus) -> Self {
        Self {
            muted: s.muted,
            active: s.active,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Meter {
    pub timestamp_ms: u64,
    pub samples: u64,
    pub peak: f32,
    pub rms: f32,
    pub clipped_samples: u64,
    pub dropped_snapshots: u64,
}

#[derive(Serialize)]
pub(crate) struct Peer {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip)]
    pub connection: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permanent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_selected: Option<bool>,
    pub state: &'static str,
    pub quality: &'static str,
    pub transmit_keyed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Value>,
    #[serde(flatten)]
    pub receive: Receive,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Call {
    pub call_id: String,
    pub account: String,
    pub generation: u64,
    pub direction: &'static str,
    pub phase: &'static str,
    pub authenticated: bool,
    pub attached: bool,
    pub muted: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Registration {
    pub state: &'static str,
    pub generation: u64,
    pub registered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
}

impl Domain {
    pub(super) fn page(
        &self,
        id: Id,
        target: Target,
        offset: usize,
        limit: usize,
    ) -> (Value, bool) {
        fn slice<T: Serialize>(rows: &[T], offset: usize, limit: usize) -> (Value, bool) {
            let start = offset.min(rows.len());
            let end = start.saturating_add(limit).min(rows.len());
            (json!(&rows[start..end]), end < rows.len())
        }
        match self.ports.get(&id) {
            Some(port) if target == Target::Peers => slice(&port.peers, offset, limit),
            Some(port) => slice(&port.calls, offset, limit),
            None => (json!([]), false),
        }
    }

    pub(super) fn fields(&self, scope: Scope, id: Option<Id>, target: Target) -> Value {
        let kind = match target {
            Target::Section(k) | Target::Member(k) => k,
            _ => Kind::Metadata,
        };
        let Scope::Port(port_id) = scope else {
            let mut fields = id
                .and_then(|id| self.resources.get(&(scope, id)))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if target == Target::Metadata
                && let Some(resources) = self.site_resources.as_deref()
            {
                fields["resources"] = json!(resources);
            }
            return fields;
        };
        if matches!(
            target,
            Target::Member(Kind::Allstarlink | Kind::Echolink | Kind::PhonePatch)
        ) {
            let registration = id.and_then(|id| self.registrations.get(&(port_id, id)));
            return json!({"registration": registration.map(|r| json!(r)).unwrap_or_else(|| json!({"state":"unavailable"}))});
        }
        let port = self.ports.get(&port_id);
        match kind {
            Kind::Metadata | Kind::Radio => json!({
                "receiver":port.and_then(|p|p.receiver),
                "transmit_key_requested":port.and_then(|p|p.transmit_key_requested),
                "calendar":port.and_then(|p|p.calendar),
                "last_disconnect":port.and_then(|p|p.last_disconnect.as_ref()),
                "last_dial":port.and_then(|p|p.last_dial.as_ref())
            }),
            Kind::RxAudio | Kind::TxAudio => {
                let direction = usize::from(kind == Kind::TxAudio);
                json!({"meter":port.and_then(|p|p.meters[direction].as_ref())})
            }
            Kind::RxSignaling => json!({"receiver":port.and_then(|p|p.receiver)}),
            Kind::TxSignaling => {
                json!({"transmit_key_requested":port.and_then(|p|p.transmit_key_requested)})
            }
            _ => id
                .and_then(|id| self.resources.get(&(scope, id)))
                .cloned()
                .unwrap_or_else(|| json!({})),
        }
    }
}

#[cfg(test)]
impl Domain {
    pub(crate) fn fixture_status_fields(
        &self,
        scope: Scope,
        id: Option<Id>,
        target: Target,
    ) -> Value {
        self.fields(scope, id, target)
    }
}
