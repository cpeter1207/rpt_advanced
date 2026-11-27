//! Bounded status handoffs from the audio and serialized controller owners.
use super::model::{Owner, RecordKey};
use super::query::Topic;
use crate::management::Id;
use crossbeam_queue::ArrayQueue;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

/// The source can lose observations independently of its logical key state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RadioSignal {
    Source {
        raw: bool,
        carrier: bool,
        vad_mode: u32,
        voice: bool,
    },
    Receive {
        qualified: bool,
        duration_ms: Option<u64>,
    },
    ReceiveRelease(rpt_advanced_core::controller::LocalReceiveRelease),
    PhysicalTransmit {
        rendered: Option<bool>,
        serviced: Option<bool>,
        observed: Option<bool>,
    },
    ReceiveUnknown,
    Transmit {
        requested: Option<bool>,
        ctcss_requested: Option<bool>,
    },
    TransmitPolicy(Option<rpt_advanced_core::controller::TransmitPolicyStatus>),
    Fault,
}

#[derive(Clone, Copy)]
pub(crate) struct RadioEdge {
    pub(crate) at_ms: u64,
    pub(crate) signal: RadioSignal,
}

/// Audio-safe multi-producer queue, allocated before opening the radio.
pub(crate) struct RadioEdges {
    queue: ArrayQueue<RadioEdge>,
    lost: AtomicBool,
}

impl Default for RadioEdges {
    fn default() -> Self {
        Self {
            queue: ArrayQueue::new(64),
            lost: AtomicBool::new(false),
        }
    }
}

impl RadioEdges {
    pub(crate) fn push(&self, edge: RadioEdge) {
        if self.queue.push(edge).is_err() {
            self.mark_lost();
        }
    }

    pub(crate) fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }

    pub(crate) fn pop(&self) -> Option<RadioEdge> {
        self.queue.pop()
    }

    pub(crate) fn lost(&self) -> bool {
        self.lost.swap(false, Ordering::AcqRel)
    }
}

/// Public result from the same controller decision used by both status readers.
pub(super) fn transmit_policy_document(
    policy: Option<rpt_advanced_core::controller::TransmitPolicyStatus>,
) -> Value {
    json!({
        "quality":if policy.is_some() {"valid"} else {"unavailable"},
        "inhibited":policy.map(|policy|policy.inhibited),
        "timed_out":policy.map(|policy|policy.timed_out),
        "traffic_holding":policy.map(|policy|policy.traffic_holding)
    })
}

pub(crate) struct Edge {
    pub(crate) port: Id,
    /// Station authority that created the record. Replacements retire older records.
    pub(crate) generation: u64,
    pub(crate) at_ms: u64,
    pub(crate) payload: Payload,
}

/// The public identity and actual outcome of an edge. No operation inputs are stored.
pub(crate) enum Payload {
    Radio(RadioSignal),
    Peer {
        id: String,
        connection: u64,
        kind: &'static str,
        value: Option<Value>,
    },
    Phone {
        id: String,
        call_generation: u64,
        kind: &'static str,
        value: Option<Value>,
    },
    PhoneProgress {
        id: String,
        generation: u64,
        kind: &'static str,
        value: Option<Value>,
    },
    Telemetry {
        id: String,
        playback_generation: u64,
        kind: &'static str,
        value: Option<Value>,
        terminal: Option<Value>,
    },
}

impl Edge {
    pub(super) fn record(self) -> (RecordKey, &'static str, Option<Value>, Option<Value>) {
        let owner = Owner::Port(self.port);
        let station = Some(self.generation);
        match self.payload {
            Payload::Radio(signal) => {
                let (id, kind, value) = match signal {
                    RadioSignal::Source {
                        raw,
                        carrier,
                        vad_mode,
                        voice,
                    } => (
                        "source",
                        "radio.rx.source_changed",
                        json!({
                            "quality":"valid", "raw_keyed":raw, "qualified_carrier":carrier,
                            "physical_cos":null, "ctcss":null, "dcs":null,
                            "vad":{"mode":match vad_mode { 0 => "disabled", 1 => "continuous", _ => "kerchunk" }, "speech":voice}
                        }),
                    ),
                    RadioSignal::Receive {
                        qualified,
                        duration_ms,
                    } => (
                        "admission",
                        if qualified {
                            "radio.rx.keyed"
                        } else if duration_ms.is_some() {
                            "radio.rx.input_unkeyed"
                        } else {
                            "radio.rx.observed"
                        },
                        json!({"quality":"valid","qualified":qualified,"duration_ms":duration_ms.map(|ms|ms.to_string())}),
                    ),
                    RadioSignal::ReceiveRelease(release) => (
                        "last_release",
                        "radio.rx.unkeyed",
                        json!({
                            "quality":"valid","qualified":false,
                            "duration_ms":(release.duration_samples / 48).to_string(),
                            "duration_samples":release.duration_samples.to_string(),
                            "suppression_reason":release.kerchunk.then_some("kerchunk")
                        }),
                    ),
                    RadioSignal::ReceiveUnknown => (
                        "admission",
                        "radio.rx.unavailable",
                        json!({"quality":"unavailable","qualified":null,"duration_ms":null,"reason":"receive_path_unavailable"}),
                    ),
                    RadioSignal::Transmit {
                        requested,
                        ctcss_requested,
                    } => (
                        "transmit",
                        "transmit.ptt_changed",
                        json!({
                            "quality": if requested.is_some() { "valid" } else { "unavailable" },
                            "transmit_key_requested":requested, "physical_ptt":null,
                            "ctcss_requested":ctcss_requested, "ctcss_encoded":null, "dcs_encoded":null
                        }),
                    ),
                    RadioSignal::TransmitPolicy(policy) => (
                        "transmit_policy",
                        "transmit.protection_changed",
                        transmit_policy_document(policy),
                    ),
                    RadioSignal::PhysicalTransmit {
                        rendered,
                        serviced,
                        observed,
                    } => (
                        "physical_transmit",
                        "transmit.physical_changed",
                        json!({"quality":if observed.is_some() {"valid"} else {"unavailable"},
                            "physical_ptt":observed,"rendered_ptt":rendered,"serviced_ptt":serviced,
                            "reason":observed.is_none().then_some("readback_unavailable")}),
                    ),
                    RadioSignal::Fault => (
                        "source",
                        "radio.rx.source_lost",
                        json!({"quality":"fault","physical_cos":null,
                            "raw_keyed":null,"qualified_carrier":null,"ctcss":null,"dcs":null,"vad":null}),
                    ),
                };
                (
                    RecordKey::new(
                        owner,
                        Some(Topic::Radio),
                        &["radio_signals", id],
                        "radio",
                        id.to_owned(),
                        station,
                    ),
                    kind,
                    Some(value),
                    None,
                )
            }
            Payload::Peer {
                id,
                connection,
                kind,
                value,
            } => (
                RecordKey::new(
                    owner,
                    Some(Topic::Links),
                    &["links", &id],
                    "link",
                    id.clone(),
                    Some(connection),
                ),
                kind,
                value,
                None,
            ),
            Payload::Phone {
                id,
                call_generation,
                kind,
                value,
            } => (
                RecordKey::new(
                    owner,
                    Some(Topic::PhonePatch),
                    &["phone_patch", "calls", &id],
                    "call",
                    id.clone(),
                    Some(call_generation),
                ),
                kind,
                value,
                None,
            ),
            Payload::PhoneProgress {
                id,
                generation,
                kind,
                value,
            } => (
                RecordKey::new(
                    owner,
                    Some(Topic::PhonePatch),
                    &["phone_patch", "progress", &id],
                    "phone_progress",
                    id.clone(),
                    Some(generation),
                ),
                kind,
                value,
                None,
            ),
            Payload::Telemetry {
                id,
                playback_generation,
                kind,
                value,
                terminal,
            } => (
                RecordKey::new(
                    owner,
                    Some(Topic::Telemetry),
                    &["telemetry", &id],
                    "playback",
                    id.clone(),
                    Some(playback_generation),
                ),
                kind,
                value,
                terminal,
            ),
        }
    }
}

/// One bounded control-to-observer queue for real transitions.
pub(crate) struct EdgeQueue {
    queue: ArrayQueue<Edge>,
    lost: AtomicBool,
}

impl Default for EdgeQueue {
    fn default() -> Self {
        Self {
            queue: ArrayQueue::new(1024),
            lost: AtomicBool::new(false),
        }
    }
}

impl EdgeQueue {
    pub(crate) fn push(&self, edge: Edge) {
        if self.queue.push(edge).is_err() {
            self.mark_lost();
        }
    }

    pub(crate) fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }

    pub(crate) fn drain(&self) -> (bool, Vec<Edge>) {
        let mut values = Vec::new();
        for _ in 0..self.queue.capacity() {
            let Some(value) = self.queue.pop() else {
                break;
            };
            values.push(value);
        }
        (self.lost.swap(false, Ordering::AcqRel), values)
    }
}
