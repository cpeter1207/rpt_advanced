//! Capture public status from the confirmed Host. Native registration queries run off its executor.
use super::*;
use crate::{
    abi,
    management::{
        live::{Call, Dial, Disconnect, Domain, Peer, Port, Receive, Registration},
        status::edges::{Edge, Payload},
    },
};
use rpt_advanced_core::config::resources::candidate::ResourceId as Id;
use rpt_advanced_core::link::LinkStatus;
use serde_json::json;
use std::{collections::BTreeMap, mem::size_of};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Account {
    Asl {
        generation: u64,
        wire: u64,
        udp_port: u32,
    },
    Echo {
        label: u64,
    },
    Phone {
        generation: u64,
    },
}

pub(crate) type Selection = BTreeMap<(Id, Id), Account>;

/// Identity-only receipt while one connection awaits its core ingress acknowledgement.
#[derive(Clone)]
pub(crate) struct PendingAslDial {
    pub(crate) station: Id,
    pub(crate) account: abi::rptadv_asl_identity_v1,
    pub(crate) generation: u64,
    pub(crate) admission: crate::lifecycle::MediaAdmission,
}

/// Small receipts for one current station owner, independent of transport-owned status.
#[derive(Default)]
pub(super) struct Handoff {
    pub(super) generation: u64,
    disconnect: Option<Disconnect>,
    dial: Option<Dial>,
}

impl Account {
    pub(crate) fn observe(self, host: HostServices, port: Id, account: Id) -> Option<Registration> {
        let mut fresh = None;
        let mut available = None;
        let (state, registered, generation) = match self {
            Self::Asl {
                generation,
                wire,
                udp_port,
            } => {
                let id = abi::rptadv_asl_identity_v1 {
                    station_uuid: *port.uuid().as_bytes(),
                    account_uuid: *account.uuid().as_bytes(),
                    wire,
                    generation,
                    udp_port,
                    reserved: 0,
                };
                let mut code = 0;
                let observe = host.asl_provider().ok()?.observe?;
                if unsafe { observe(host.context(), &id, &mut code) } != 0 {
                    return None;
                }
                let name = *[
                    "disabled",
                    "dormant",
                    "pending",
                    "registered",
                    "retry",
                    "withdrawn",
                ]
                .get(code as usize)?;
                (name, code == 3, generation)
            }
            Self::Phone { generation } => {
                let mut result = abi::rptadv_phone_account_status_v1 {
                    struct_size: size_of::<abi::rptadv_phone_account_status_v1>() as u32,
                    abi_version: 1,
                    generation: 0,
                    state: 0,
                    reserved: 0,
                };
                let observe = host.phone_provider().ok()?.observe?;
                if unsafe {
                    observe(
                        host.context(),
                        port.uuid().as_bytes().as_ptr(),
                        account.uuid().as_bytes().as_ptr(),
                        generation,
                        &mut result,
                    )
                } != 0
                    || result.struct_size < size_of::<abi::rptadv_phone_account_status_v1>() as u32
                    || result.abi_version != 1
                    || result.reserved != 0
                    || result.generation != if result.state == 0 { 0 } else { generation }
                {
                    return None;
                }
                let name = *["inactive", "starting", "registered", "retiring", "failed"]
                    .get(result.state as usize)?;
                (name, result.state == 2, result.generation)
            }
            Self::Echo { label } => {
                let mut result = abi::rptadv_managed_echo_status_v1 {
                    struct_size: size_of::<abi::rptadv_managed_echo_status_v1>() as u32,
                    abi_version: 1,
                    generation: 0,
                    state: 0,
                    registered: 0,
                    fresh: 0,
                    available: 0,
                };
                let observe = host.echo_provider().ok()?.observe?;
                if unsafe {
                    observe(
                        host.context(),
                        port.uuid().as_bytes().as_ptr(),
                        account.uuid().as_bytes().as_ptr(),
                        label,
                        &mut result,
                    )
                } != 0
                    || result.abi_version != 1
                    || result.struct_size < size_of::<abi::rptadv_managed_echo_status_v1>() as u32
                    || result.generation == 0
                    || result.registered > 1
                    || result.available > 1
                    || result.fresh > 1
                {
                    return None;
                }
                let name = match result.state {
                    1 => "registering",
                    2 => "online",
                    3 => "retrying",
                    _ => return None,
                };
                fresh = Some(result.fresh == 1);
                available = Some(result.available == 1);
                (name, result.registered == 1, result.generation)
            }
        };
        Some(Registration {
            state,
            registered,
            generation,
            fresh,
            available,
        })
    }
}

impl Host {
    /// Publish callback-origin edges with the current station's command authority.
    pub(super) fn collect_radio_status(
        &self,
        local: &str,
        generation: Option<u64>,
        status: &crate::worker::RadioStatus,
    ) {
        use crate::management::status::edges::{Edge, Payload};
        if status.edges.lost() {
            self.status_edges.mark_lost();
        }
        let owner = local.parse::<Id>().ok().zip(generation);
        while let Some(edge) = status.edges.pop() {
            if let Some((port, generation)) = owner {
                self.status_edges.push(Edge {
                    port,
                    generation,
                    at_ms: edge.at_ms,
                    payload: Payload::Radio(edge.signal),
                });
            }
        }
    }
    pub(crate) fn status_selection(&self) -> Selection {
        let mut selected = Selection::new();
        for a in self.asl.observations() {
            let id = a.identity();
            selected.insert(
                (a.station, a.account),
                Account::Asl {
                    generation: id.generation,
                    wire: id.wire,
                    udp_port: id.udp_port,
                },
            );
        }
        for (port, account, label) in self.echo.observations() {
            selected.insert((port, account), Account::Echo { label });
        }
        for (port, account, generation) in self.phone_accounts.observations() {
            selected.insert((port, account), Account::Phone { generation });
        }
        selected
    }

    pub(super) fn record_disconnect(&mut self, local: &str, receipt: Disconnect) {
        if let Ok(port) = local.parse()
            && let Some(generation) = self.handoff_owner(local)
        {
            self.handoff_receipt(port, generation).disconnect = Some(receipt);
        }
    }

    pub(crate) fn handoff_owner(&mut self, local: &str) -> Option<u64> {
        self.runtime
            .node(local)?
            .command_work()
            .map(|work| work.id())
    }

    pub(crate) fn record_asl_dial(
        &mut self,
        dial: PendingAslDial,
        local: &str,
        remote: &str,
        attached: bool,
    ) {
        if dial.admission.current()
            && self.handoff_owner(local) == Some(dial.generation)
            && self
                .asl_account(dial.station)
                .is_ok_and(|current| current.matches(&dial.account))
        {
            self.handoff_receipt(dial.station, dial.generation).dial = Some(Dial {
                node: remote.to_owned(),
                attached,
            });
        }
    }

    fn handoff_receipt(&mut self, id: Id, generation: u64) -> &mut Handoff {
        let receipt = self.handoff_status.entry(id).or_default();
        if receipt.generation != generation {
            *receipt = Handoff {
                generation,
                ..Handoff::default()
            };
        }
        receipt
    }

    pub(crate) fn live_domain(
        &mut self,
        owners: &BTreeMap<Id, u64>,
        now_ms: u64,
    ) -> Result<Domain, RuntimeError> {
        let mut domain = Domain::default();
        for (id, generation) in owners {
            let local = id.to_string();
            let node = self.runtime.node(&local).ok_or(RuntimeError::MissingNode)?;
            if node
                .command_work()
                .is_none_or(|work| work.id() != *generation)
            {
                return Err(RuntimeError::Rejected);
            }
            let mut port = Port {
                receiver: Some(node.radio_receive_status().into()),
                calendar: node.calendar_observation(),
                audio_generation: node.status(now_ms).active,
                ..Port::default()
            };
            if let Some(receipt) = self
                .handoff_status
                .get(id)
                .filter(|r| r.generation == *generation)
            {
                port.last_disconnect = receipt.disconnect.clone();
                port.last_dial = receipt.dial.clone();
            }
            for (event, issue, terminal) in node.observed_events() {
                use rpt_advanced_core::schedule::recurrence::Error;
                let label = issue.map(|e| match e {
                    Error::Provider => "provider",
                    Error::Text => "text",
                    Error::Zone => "zone",
                    Error::Parse => "parse",
                    Error::Range => "range",
                    Error::Limit => "limit",
                });
                let mut fields = serde_json::json!(terminal);
                fields["issue"] = serde_json::json!(label);
                domain.resources.insert(
                    (
                        rpt_advanced_core::config::resources::candidate::ResourceScope::Port(*id),
                        event,
                    ),
                    fields,
                );
            }
            let links = node.links();
            let logical = links.manager().snapshot();
            port.peers = links
                .ingress_receive_status()
                .map(|(identity, rx)| {
                    let alias = identity.operator_alias();
                    let owner = self.peers.iter().find(|peer| {
                        peer.local == local
                            && !peer.superseded
                            && alias.as_ref().is_some_and(|alias| alias == &peer.remote)
                    });
                    let status = logical
                        .iter()
                        .find(|status| alias.as_ref().is_some_and(|alias| alias == &status.name));
                    peer_value(identity, rx.into(), status, owner)
                })
                .collect();
            // Retry intent has no active ingress transport and does not masquerade as one.
            port.link_retries = logical
                .iter()
                .filter(|status| status.retrying)
                .filter_map(|status| {
                    let identity = PeerIdentity::from_operator(&status.name).ok()?;
                    Some(peer_value(
                        &identity,
                        Receive {
                            muted: false,
                            active: false,
                        },
                        Some(status),
                        None,
                    ))
                })
                .collect();
            for peer in self
                .peers
                .iter()
                .filter(|peer| peer.local == local && !peer.superseded)
            {
                let connection = peer.control.connection_generation();
                if let Some(status) = port
                    .peers
                    .iter()
                    .find(|status| status.connection == Some(connection))
                    && let Some(node) = &status.node
                {
                    port.link_meters.insert(
                        (format!("{}:{node}", status.kind), connection),
                        peer.control.status_meters(),
                    );
                }
            }
            port.peers.sort_unstable_by(|a, b| {
                (a.kind, &a.node, &a.call_id).cmp(&(b.kind, &b.node, &b.call_id))
            });
            if let Some(mut lease) = site::live_station(&self.leases, &local, true) {
                if let Some(worker) = lease.worker.as_mut() {
                    port.meters = worker.meters();
                    port.status_meters = worker.status_meters();
                    port.physical_receive = worker.physical_receive(now_ms);
                }
                port.receive_sample_ms = lease.status.receive_sample_time();
                port.physical_transmit = lease.status.physical_transmit(now_ms);
                port.transmit_key_requested = lease.status.recent_transmit_request(now_ms);
            }
            domain.ports.insert(*id, port);
        }
        for (local, call) in self.phone_calls.observations() {
            if let Ok(id) = local.parse::<Id>()
                && let Some(port) = domain.ports.get_mut(&id)
            {
                port.calls.push(call);
            }
        }
        for (local, id, generation, meters) in self.phone_calls.attached_meters() {
            let id = id.to_string();
            if let Ok(port) = local.parse::<Id>()
                && let Some(port) = domain.ports.get_mut(&port)
                && port.calls.iter().any(|call| {
                    call.call_id == id && call.generation == generation && call.attached
                })
            {
                port.call_meters.insert((id, generation), meters);
            }
        }
        for attempt in &self.phone_dials {
            let (port, station, id, value) = attempt.status_observation();
            if owners.get(&port).copied() == station
                && let Some(port) = domain.ports.get_mut(&port)
            {
                port.phone_progress.insert(id.to_string(), (id, value));
            }
        }
        for port in domain.ports.values_mut() {
            port.calls
                .sort_unstable_by(|a, b| a.call_id.cmp(&b.call_id));
        }
        self.live_site_status(&mut domain, now_ms);
        Ok(domain)
    }
}

/// Latest public call state on the exact station generation, used to publish real transitions.
pub(super) type PhoneObservations = BTreeMap<(Id, String), (u64, Call)>;

fn protocol_label(protocol: ProtocolId) -> &'static str {
    match protocol {
        ProtocolId::AllStarLink => "allstarlink",
        ProtocolId::EchoLink => "echolink",
        ProtocolId::Phone => "phone",
        ProtocolId::WebRtc => "webrtc",
    }
}

fn mode_label(mode: Mode) -> &'static str {
    if mode.transmits() {
        "transceive"
    } else if mode.forwards() {
        "monitor"
    } else {
        "local-monitor"
    }
}

fn peer_value(
    identity: &PeerIdentity,
    receive: Receive,
    logical: Option<&LinkStatus>,
    owner: Option<&PeerOwner>,
) -> Peer {
    let connection = owner.map(|peer| peer.control.connection_generation());
    let media = owner.and_then(|peer| {
        let snapshot = peer.control.snapshot().ok()?;
        let jitter = snapshot.peer.map(|provider| {
            json!({
                "queued_packets": provider.jitter_queued_packets,
                "delay_samples": provider.jitter_delay_samples,
                "buffered_samples": provider.jitter_buffered_span_samples.to_string(),
                "late_drops": provider.jitter_late_drops.to_string(),
                "duplicate_drops": provider.jitter_duplicate_drops.to_string(),
                "full_drops": provider.jitter_full_drops.to_string(),
                "missing_samples": provider.jitter_missing_samples.to_string(),
                "codec_plc_samples": (provider.echo_valid != 0)
                    .then(|| provider.echo_codec_plc_samples.to_string())
            })
        });
        Some(json!({
            "input_rate_hz": snapshot.input_rate,
            "ring": {
                "available_samples": snapshot.ring.available_samples.to_string(),
                "target_samples": snapshot.ring.target_samples.to_string(),
                "missing_samples": snapshot.ring.missing_samples.to_string(),
                "discarded_samples": snapshot.ring.discarded_samples.to_string(),
                "ratio_correction_ppm": snapshot.ring.ratio_correction_ppm,
            },
            "egress": {
                "accepted_packets": snapshot.egress.accepted.to_string(),
                "blocked_attempts": snapshot.egress.blocked.to_string(),
                "expired_native_samples": snapshot.egress.expired_native_samples.to_string(),
                "rejected_native_samples": snapshot.egress.rejected_native_samples.to_string()
            },
            "jitter": jitter
        }))
    });
    Peer {
        kind: protocol_label(identity.protocol()),
        node: identity.node().map(str::to_owned),
        call_id: identity.call_id().map(|id| id.to_string()),
        connection,
        connection_generation: connection.map(|id| id.to_string()),
        direction: owner
            .map(|peer| {
                if peer.control.status_outgoing {
                    "outgoing"
                } else {
                    "incoming"
                }
            })
            .or_else(|| {
                logical
                    .is_some_and(|status| status.retrying)
                    .then_some("outgoing")
            }),
        mode: logical
            .map(|status| mode_label(status.mode))
            .or_else(|| owner.map(|peer| mode_label(peer.mode))),
        permanent: logical.map(|status| status.permanent),
        group_selected: owner.and_then(|peer| {
            peer.group
                .as_ref()
                .map(|group| group.selection().active() == Some(group.slot()))
        }),
        state: match logical {
            Some(status) if status.paused => "paused",
            Some(status) if status.retrying => "retrying",
            Some(status) if status.ended => "disconnected",
            _ => "connected",
        },
        quality: "valid",
        transmit_keyed: owner.and_then(|peer| peer.control.receive_status_key()),
        media,
        receive,
    }
}

fn peer_id(identity: &PeerIdentity) -> Option<String> {
    Some(format!(
        "{}:{}",
        protocol_label(identity.protocol()),
        identity.node()?
    ))
}

impl Host {
    /// Record network publication only when the peer's actual ingress is acknowledged.
    pub(super) fn peer_status_connected(&mut self, local: &str, connection: u64, at_ms: u64) {
        let Some(peer) = self
            .peers
            .iter_mut()
            .find(|peer| peer.control.connection_generation() == connection)
        else {
            return;
        };
        if std::mem::replace(&mut peer.control.status_connected, true) {
            return;
        }
        // Earlier transport observations have no published ingress yet.
        peer.control.discard_receive_status_edges();
        let (Some(identity), Some(port), Some(node)) = (
            PeerIdentity::from_operator(&peer.remote).ok(),
            local.parse().ok(),
            self.runtime.node(local),
        ) else {
            return;
        };
        let Some(work) = node.command_work() else {
            return;
        };
        let Some(id) = peer_id(&identity) else { return };
        let logical = node
            .links()
            .manager()
            .snapshot()
            .into_iter()
            .find(|s| s.name == peer.remote);
        let reply = peer.control.reply_target();
        let muted = reply.rx_muted();
        let status = peer_value(
            &identity,
            Receive {
                muted,
                active: !muted && reply.active(),
            },
            logical.as_ref(),
            Some(peer),
        );
        self.status_edges.push(Edge {
            port,
            generation: work.id(),
            at_ms,
            payload: Payload::Peer {
                id,
                connection,
                kind: "link.connected",
                value: Some(json!(status)),
            },
        });
    }

    /// Preserve every accepted provider key transition before processing a disconnect.
    pub(super) fn collect_peer_status_edges(&mut self) {
        for peer in self
            .peers
            .iter()
            .filter(|peer| peer.control.status_connected && !peer.superseded)
        {
            if peer.control.receive_status_lost() {
                self.status_edges.mark_lost();
            }
            let Some(first) = peer.control.take_receive_status_edge() else {
                continue;
            };
            let (Some(identity), Some(port), Some(node)) = (
                PeerIdentity::from_operator(&peer.remote).ok(),
                peer.local.parse().ok(),
                self.runtime.node(&peer.local),
            ) else {
                continue;
            };
            let Some(work) = node.command_work() else {
                continue;
            };
            let Some(id) = peer_id(&identity) else {
                continue;
            };
            let logical = node
                .links()
                .manager()
                .snapshot()
                .into_iter()
                .find(|s| s.name == peer.remote);
            let connection = peer.control.connection_generation();
            let muted = peer.control.reply_target().rx_muted();
            let mut status = peer_value(
                &identity,
                Receive {
                    muted,
                    active: false,
                },
                logical.as_ref(),
                Some(peer),
            );
            for (at_ms, keyed) in std::iter::once(first)
                .chain(std::iter::from_fn(|| peer.control.take_receive_status_edge()).take(63))
            {
                status.transmit_keyed = Some(keyed);
                status.receive.active = !muted && keyed;
                self.status_edges.push(Edge {
                    port,
                    generation: work.id(),
                    at_ms,
                    payload: Payload::Peer {
                        id: id.clone(),
                        connection,
                        kind: if keyed { "link.keyed" } else { "link.unkeyed" },
                        value: Some(json!(status)),
                    },
                });
            }
        }
    }

    /// Retire the exact previously acknowledged peer connection.
    pub(super) fn peer_status_disconnected(
        &mut self,
        local: &str,
        remote: &str,
        connection: u64,
        at_ms: u64,
    ) {
        if let (Ok(port), Ok(identity), Some(generation)) = (
            local.parse(),
            PeerIdentity::from_operator(remote),
            self.handoff_owner(local),
        ) && let Some(id) = peer_id(&identity)
        {
            self.status_edges.push(Edge {
                port,
                generation,
                at_ms,
                payload: Payload::Peer {
                    id,
                    connection,
                    kind: "link.disconnected",
                    value: None,
                },
            });
        }
    }

    /// Publish call phases, mute and each qualified receive transition of the live route.
    pub(super) fn collect_phone_status_edges(&mut self, at_ms: u64) {
        let mut current = PhoneObservations::new();
        let mut previous = std::mem::take(&mut self.phone_status);
        let mut generations = BTreeMap::new();
        let emit = |port, generation, at_ms, call: &Call, kind, present: bool| {
            self.status_edges.push(Edge {
                port,
                generation,
                at_ms,
                payload: Payload::Phone {
                    id: call.call_id.clone(),
                    call_generation: call.generation,
                    kind,
                    value: present.then(|| json!(call)),
                },
            });
        };
        for (local, mut call, control) in self.phone_calls.observations_with_control() {
            let Ok(port) = local.parse() else { continue };
            let generation = *generations.entry(port).or_insert_with(|| {
                self.runtime
                    .node(local)?
                    .command_work()
                    .map(|work| work.id())
            });
            let Some(generation) = generation else {
                continue;
            };
            let key = (port, call.call_id.clone());
            let old = previous.remove(&key);
            if let Some((old_generation, old_call)) = &old
                && old_call.generation != call.generation
            {
                emit(
                    port,
                    *old_generation,
                    at_ms,
                    old_call,
                    "phone_patch.call_disconnected",
                    false,
                );
            }
            let last = old
                .as_ref()
                .filter(|(owner, old)| *owner == generation && old.generation == call.generation)
                .map(|(_, old)| old);
            let control = control.filter(|_| call.attached);
            let sampled_active = call.active;
            if let Some(control) = control {
                if old.as_ref().is_some_and(|(owner, old)| {
                    *owner != generation && old.generation == call.generation
                }) {
                    // A retained call can change station owners. Do not relabel
                    // queued observations of the former owner as current.
                    control.discard_receive_status();
                } else {
                    call.active = Some(
                        call.muted != Some(true)
                            && last.and_then(|old| old.active).unwrap_or(false),
                    );
                }
            }
            let kind = match last {
                Some(last) if last.phase == call.phase => {
                    (last.muted != call.muted).then_some(if call.muted == Some(true) {
                        "phone_patch.call_muted"
                    } else {
                        "phone_patch.call_unmuted"
                    })
                }
                _ if call.attached => Some("phone_patch.call_attached"),
                _ if call.authenticated => Some("phone_patch.call_authenticated"),
                _ => Some("phone_patch.call_private"),
            };
            if let Some(kind) = kind {
                emit(port, generation, at_ms, &call, kind, true);
            }
            if let Some(control) = control {
                if control.receive_status_lost() {
                    self.status_edges.mark_lost();
                    control.discard_receive_status();
                    call.active = sampled_active;
                } else {
                    for (at_ms, keyed) in
                        std::iter::from_fn(|| control.next_receive_status()).take(64)
                    {
                        if call.active == Some(keyed) || (keyed && call.muted == Some(true)) {
                            continue;
                        }
                        call.active = Some(keyed);
                        let kind = if keyed {
                            "phone_patch.call_keyed"
                        } else {
                            "phone_patch.call_unkeyed"
                        };
                        emit(port, generation, at_ms, &call, kind, true);
                    }
                }
            }
            current.insert(key, (generation, call));
        }
        for ((port, _), (generation, call)) in previous {
            emit(
                port,
                generation,
                at_ms,
                &call,
                "phone_patch.call_disconnected",
                false,
            );
        }
        self.phone_status = current;
    }
}
