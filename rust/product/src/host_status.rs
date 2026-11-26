//! Capture public status from the confirmed Host. Native registration queries run off its executor.
use super::*;
use crate::{
    abi,
    management::live::{Dial, Disconnect, Domain, Peer, Port, Registration},
};
use rpt_advanced_core::config::resources::candidate::ResourceId as Id;
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
            port.peers = node
                .links()
                .ingress_receive_status()
                .map(|(id, rx)| Peer {
                    kind: match id.protocol() {
                        ProtocolId::AllStarLink => "allstarlink",
                        ProtocolId::EchoLink => "echolink",
                        ProtocolId::Phone => "phone",
                    },
                    node: id.node().map(str::to_owned),
                    call_id: id.call_id().map(|id| id.to_string()),
                    receive: rx.into(),
                })
                .collect();
            port.peers.sort_unstable_by(|a, b| {
                (a.kind, &a.node, &a.call_id).cmp(&(b.kind, &b.node, &b.call_id))
            });
            if let Some(lease) = site::live_station(&self.leases, &local, true) {
                if let Some(worker) = &lease.worker {
                    port.meters = worker.meters();
                }
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
        for port in domain.ports.values_mut() {
            port.calls
                .sort_unstable_by(|a, b| a.call_id.cmp(&b.call_id));
        }
        self.live_site_status(&mut domain, now_ms);
        Ok(domain)
    }
}
