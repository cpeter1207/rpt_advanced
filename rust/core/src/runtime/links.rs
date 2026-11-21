//! Serialized link-command orchestration and final gates around external dialing.

use super::{
    GenerationWork,
    dtmf::{DigitEvent, DigitOperation, DigitOutcome, DtmfCommands},
    link_schedule::{LinkReservation, LinkScheduler, LinkTransition, ScheduleWarning, WarningGate},
};
use crate::{
    access::AccessPolicy,
    command::{DtmfCommandMap, LinkAction},
    link::{
        AdmissionError, GroupMemberSelection, GroupSelection, LinkManager, Mode, PeerIdentity,
        PeerReply, Protocol, ProtocolId, RetryAttempt, TopologyManager,
    },
    schedule::CivilTime,
};

/// Owned dial request. Carry this across external lookup/media/dial and return it for publication.
pub struct ConnectAttempt {
    pending_source: Option<DigitOperation>,
    remote: String,
    mode: Mode,
    permanent: bool,
    group: Option<Box<GroupMemberSelection>>,
    work: GenerationWork,
    scheduled: Option<LinkReservation>,
}

/// Retry work retains a host-generation permit until external dial cleanup completes.
pub struct RetryWork {
    pending_source: Option<DigitOperation>,
    attempt: RetryAttempt,
    work: GenerationWork,
}
impl RetryWork {
    /// Destination for external directory lookup and dialing.
    pub fn remote(&self) -> &str {
        self.attempt.name()
    }
    /// Retained original audio mode.
    pub fn mode(&self) -> Mode {
        self.attempt.mode()
    }
    /// Revalidate before each external operation.
    pub fn current(&self) -> bool {
        self.work.is_current() && pending_source_current(self.pending_source.as_ref(), None)
    }
    /// Revalidate an initial source-owned retry before external dialing.
    pub fn current_at(&self, now_ms: u64) -> bool {
        self.work.is_current() && pending_source_current(self.pending_source.as_ref(), Some(now_ms))
    }
}
impl ConnectAttempt {
    /// Exact destination requested by current policy.
    pub fn remote(&self) -> &str {
        &self.remote
    }
    /// Requested audio forwarding/transmit mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }
    /// Whether a failed initial dial retains automatic recovery intent.
    pub fn permanent(&self) -> bool {
        self.permanent
    }
    /// Recheck this after each external blocking preparation step.
    pub fn current(&self) -> bool {
        self.work.is_current() && pending_source_current(self.pending_source.as_ref(), None)
    }
    /// Recheck source cancellation and the inclusive elapsed bound before external work.
    pub fn current_at(&self, now_ms: u64) -> bool {
        self.work.is_current() && pending_source_current(self.pending_source.as_ref(), Some(now_ms))
    }
}

fn pending_source_current(source: Option<&DigitOperation>, now_ms: Option<u64>) -> bool {
    source.is_none_or(|source| {
        source
            .action_permit
            .as_ref()
            .is_none_or(|p| p.current(now_ms))
            && source.reply.as_ref().is_none_or(|reply| !reply.ended())
            && source.macro_permit.as_ref().is_none_or(|permit| {
                now_ms.map_or_else(|| permit.active(), |now| permit.current_at(now))
            })
    })
}

/// Effective per-ingress receive state, sampled without altering its raw qualifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveStatus {
    /// Operator mute on this exact source.
    pub muted: bool,
    /// Healthy qualified reception after applying this source's mute.
    pub active: bool,
}
/// Adapter-facing consequence of one serialized existing link command.
pub enum LinkEffect {
    /// The actual command owner can retain and poll this individual macro's result.
    CommandQueued(super::CommandResponse),
    /// Prepared reply for one authenticated caller, without controller TX or peer fanout.
    PrivatePhoneMedia(PeerReply, Box<dyn crate::audio::PcmStreamReader>),
    /// Complete source-owned command mode selection using the host's actual directory.
    VerifyCommandTarget(DigitOperation),
    /// Apply through the existing station site controller and its physical submission owner.
    SiteCommand(crate::site::SiteCommand, super::dtmf::ActionPermit),
    /// Dial outside the serialized owner, then return the token before attaching resources.
    Connect(ConnectAttempt),
    /// Unpublished identities whose external channels/readers must be stopped and drained.
    Detach(Vec<String>),
    /// Advisory keyed-source messages, queued on the destination channel owner.
    Text(Vec<(String, String)>),
    /// Addressed command envelopes; delivery failures are not advisory.
    ForwardDigits(Vec<(String, String)>),
    /// A locally addressed digit interpreted on station control.
    Digit(DigitOutcome),
    /// Remote-command mode was selected without transmitting a digit.
    SelectedRemote,
    /// Prepare existing status/topology/time telemetry outside audio.
    Telemetry(LinkAction, Option<PeerReply>),
    /// Play a configured read-only macro, retaining the exact command source.
    Message {
        /// Canonical decimal selector, not a link destination.
        selector: String,
        /// Originating endpoint; absence selects the local transmitter.
        reply: Option<PeerReply>,
    },
    /// Query one site reading without changing remembered link destinations.
    SiteStatus {
        /// Canonical sensor/input selector.
        selector: String,
        /// Exact source connection, or the local receiver.
        reply: Option<PeerReply>,
    },
    /// Change the live node's parrot state; apply buffer lifecycle work on control.
    ParrotEnabled(bool),
    /// Already-authorized account telemetry selection; runtime applies the policy.
    EchoTelemetry(crate::config::EchoLinkTelemetry),
    /// Submit typed rig work to its physical owner, retaining the exact source.
    RemoteBase(DigitOperation),
    /// Cancel only the currently admitted message attempt on the audio owner.
    CancelMessage,
    /// No physical operation is needed (for example resumed retry intent).
    None,
}

/// Long-lived node link/control policy. Retain this owner, its peers and retry intent on reload.
pub struct NodeLinkControl {
    local: String,
    manager: LinkManager,
    policy: AccessPolicy,
    commands: DtmfCommands,
    peer_commands: Vec<(String, PeerReply, DtmfCommands)>,
    ingress: Vec<(PeerIdentity, PeerReply)>,
    pending_source_retries: Vec<(String, DigitOperation)>,
    digit_sequence: i32,
    schedule: Option<LinkScheduler>,
    groups: Vec<ConfiguredGroup>,
    topology: TopologyManager,
    admitting: bool,
}
struct ConfiguredGroup {
    label: String,
    name: String,
    members: Vec<(usize, String)>,
    selection: GroupSelection,
}

/// Control-plane view of one immutable priority group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PriorityGroupStatus {
    pub label: String,
    pub name: String,
    pub members: Vec<String>,
    pub selected: Option<String>,
    pub unavailable: bool,
}

fn configured_groups(
    schedule: Option<&LinkScheduler>,
    previous: &[ConfiguredGroup],
) -> Vec<ConfiguredGroup> {
    let mut groups = Vec::<ConfiguredGroup>::new();
    if let Some(schedule) = schedule {
        for route in schedule.route_specs() {
            let Some(label) = route.group_label else {
                continue;
            };
            let index = groups
                .iter()
                .position(|group| group.label == label)
                .unwrap_or_else(|| {
                    groups.push(ConfiguredGroup {
                        label: label.clone(),
                        name: route.group_name.clone().unwrap_or_else(|| label.clone()),
                        members: Vec::new(),
                        selection: GroupSelection::new(),
                    });
                    groups.len() - 1
                });
            groups[index]
                .members
                .push((route.group_priority.unwrap_or(usize::MAX), route.remote));
        }
    }
    for group in &mut groups {
        group.members.sort_by_key(|(priority, _)| *priority);
        if let Some(old) = previous
            .iter()
            .find(|old| old.label == group.label && old.members == group.members)
        {
            group.selection = old.selection.clone();
        }
    }
    groups
}
impl NodeLinkControl {
    /// Compose validated node policy before registering any external callbacks.
    pub fn new(
        local: &str,
        policy: AccessPolicy,
        commands: DtmfCommandMap,
        schedule: Option<LinkScheduler>,
    ) -> Result<Self, AdmissionError> {
        let groups = configured_groups(schedule.as_ref(), &[]);
        Ok(Self {
            local: local.into(),
            manager: LinkManager::new(local)?,
            policy,
            commands: DtmfCommands::new(commands),
            peer_commands: Vec::new(),
            ingress: Vec::new(),
            pending_source_retries: Vec::new(),
            digit_sequence: 0,
            schedule,
            groups,
            topology: TopologyManager::default(),
            admitting: true,
        })
    }
    /// Adopt candidate policy while preserving the long-lived hub and invalidating old dials.
    /// Withdraw `LinkScheduler::removed_routes` before passing the candidate schedule here.
    pub fn reconfigure(
        &mut self,
        policy: AccessPolicy,
        commands: DtmfCommandMap,
        schedule: Option<LinkScheduler>,
    ) {
        for (remote, _) in self.pending_source_retries.drain(..) {
            self.manager.cancel_retry(&remote);
        }
        self.groups = configured_groups(schedule.as_ref(), &self.groups);
        self.manager.invalidate_generation();
        self.policy = policy;
        self.commands = DtmfCommands::new(commands);
        self.peer_commands.clear();
        self.schedule = schedule;
    }
    pub(super) fn install_profile(
        &mut self,
        profile: std::sync::Arc<super::dtmf::CommandProfile>,
        generation: u64,
    ) {
        self.manager
            .configure_wire_node(profile.network_node.as_deref());
        self.commands.install_profile(profile, generation);
        self.peer_commands.clear();
    }
    pub(super) fn radio_commands_enabled(&self) -> bool {
        self.commands.radio_enabled()
    }
    pub(super) fn command_profile(&self) -> Option<&std::sync::Arc<super::dtmf::CommandProfile>> {
        self.commands.profile()
    }
    pub(super) fn command_source(
        &mut self,
        source: &crate::command_language::CommandSource,
    ) -> Option<&mut DtmfCommands> {
        use crate::command_language::CommandSource;
        match source {
            CommandSource::Receiver => Some(&mut self.commands),
            CommandSource::Connection { origin, reply } if !reply.ended() => self
                .peer_commands
                .iter_mut()
                .find(|(name, token, _)| name == origin && token == reply)
                .map(|(_, _, commands)| commands),
            _ => None,
        }
    }
    /// Observe only currently attached incarnations, including private callers on this port.
    pub fn ingress_receive_status(&self) -> impl Iterator<Item = (&PeerIdentity, ReceiveStatus)> {
        self.ingress
            .iter()
            .filter(|(_, token)| !token.ended())
            .map(|(id, token)| {
                let muted = token.rx_muted();
                (
                    id,
                    ReceiveStatus {
                        muted,
                        active: !muted && token.active(),
                    },
                )
            })
    }
    /// Register the confirmed transport endpoint after attachment or an acknowledged ring redirect.
    /// Numeric peers must already be admitted. Only the phone attachment owner registers callers.
    pub fn attach_ingress(
        &mut self,
        identity: PeerIdentity,
        reply: PeerReply,
    ) -> Result<(), AdmissionError> {
        if !self.admitting || reply.ended() {
            return Err(AdmissionError::Stale);
        }
        if let Some(call) = identity.call_id() {
            if !reply.phone_authenticated()
                || reply.phone_source().is_none_or(|p| p.call_id() != call)
            {
                return Err(AdmissionError::Denied);
            }
        } else if reply.phone_source().is_some()
            || !identity.operator_alias().is_some_and(|alias| {
                self.manager
                    .snapshot()
                    .iter()
                    .any(|p| p.name == alias && !p.ended && !p.retrying)
            })
        {
            return Err(AdmissionError::Denied);
        }
        self.ingress.retain(|(_, r)| !r.ended());
        if let Some((_, r)) = self.ingress.iter().find(|(id, _)| id == &identity) {
            return if *r == reply {
                Ok(())
            } else {
                Err(AdmissionError::Stale)
            };
        }
        self.ingress.push((identity, reply));
        Ok(())
    }
    pub(super) fn toggle_ingress(
        &mut self,
        target: &crate::command_language::MuteTarget,
        source: &crate::command_language::CommandSource,
    ) -> Result<(), AdmissionError> {
        use crate::command_language::{CommandSource, MuteTarget as T};
        self.ingress.retain(|(_, r)| !r.ended());
        let select = |s: &PeerReply| {
            s.set_rx_muted(!s.rx_muted());
            Ok(())
        };
        match target {
            T::Port => Err(AdmissionError::Invalid),
            T::Peer(input) => {
                let id =
                    PeerIdentity::from_operator(input).map_err(|_| AdmissionError::Identity)?;
                select(
                    &self
                        .ingress
                        .iter()
                        .find(|(i, _)| i == &id)
                        .ok_or(AdmissionError::Denied)?
                        .1,
                )
            }
            T::Peers(protocol) => {
                let selected = || {
                    self.ingress.iter().filter(|(i, _)| {
                        i.protocol() != ProtocolId::Phone
                            && protocol.is_none_or(|p| p == i.protocol())
                    })
                };
                let muted = !selected().all(|(_, r)| r.rx_muted());
                for (_, r) in selected() {
                    r.set_rx_muted(muted);
                }
                Ok(())
            }
            T::PhonePatch(call) => {
                if let CommandSource::Connection { reply, .. } = source
                    && let Some(actual) = reply.phone_source()
                {
                    if !reply.phone_authenticated()
                        || call
                            .as_ref()
                            .is_some_and(|p| p.as_str() != actual.call_id().to_string())
                    {
                        return Err(AdmissionError::Denied);
                    }
                    return select(reply);
                }
                let mut callers = self.ingress.iter().filter(|(i, r)| {
                    r.phone_authenticated()
                        && i.call_id().is_some_and(|id| {
                            call.as_ref().is_none_or(|p| p.as_str() == id.to_string())
                        })
                });
                let (_, reply) = callers.next().ok_or(AdmissionError::Denied)?;
                if callers.next().is_some() {
                    Err(AdmissionError::Denied)
                } else {
                    select(reply)
                }
            }
        }
    }
    /// Configure per-node Argon2id credentials; missing either digest disables admin DTMF.
    pub fn configure_admin(&mut self, unlock_hash: &str, lock_hash: &str, timeout_ms: u64) {
        self.commands
            .configure_admin(unlock_hash, lock_hash, timeout_ms);
        self.peer_commands.clear();
    }
    pub(super) fn bind_admin(
        &mut self,
        codes: Option<std::sync::Arc<super::dtmf::AdminCodes>>,
        timeout_ms: u64,
    ) {
        self.commands.bind_admin(codes, timeout_ms);
        self.peer_commands.clear();
    }
    /// Bind the prepared port radio independently of the legacy digit family.
    pub fn configure_radio_owner(
        &mut self,
        owner: Option<&crate::remote_base_command::RemoteCommandConfiguration>,
    ) -> Result<(), crate::command::CommandMapError> {
        self.commands.configure_radio_owner(owner)?;
        for (_, _, commands) in &mut self.peer_commands {
            commands.configure_radio_owner(owner)?;
        }
        Ok(())
    }
    /// Install the shared radio's resolved DTMF family on every source collector.
    pub fn configure_remote(
        &mut self,
        prefix: &str,
        access: crate::remote_base_command::RemoteDtmfAccess,
    ) -> Result<(), crate::command::CommandMapError> {
        self.commands.configure_remote(prefix, access)?;
        for (_, _, commands) in &mut self.peer_commands {
            commands.configure_remote(prefix, access)?;
        }
        Ok(())
    }
    /// Install the generation's prepared catalog; per-source runners share immutable definitions.
    pub fn configure_macros(
        &mut self,
        macros: Vec<crate::dtmf_macro::CompiledDtmfMacro>,
        limits: crate::dtmf_macro::DtmfMacroLimits,
    ) -> Result<(), crate::command::CommandMapError> {
        self.commands.configure_macros(macros, limits)?;
        self.peer_commands.clear();
        Ok(())
    }

    /// Advance each admitted source once, after all new live input has been consumed.
    pub fn advance_macros(&mut self, now_ms: u64) -> Vec<DigitOutcome> {
        self.retain_peer_commands(now_ms);
        let mut outcomes = self
            .commands
            .advance_macro(now_ms)
            .map(DigitOutcome::Operation)
            .into_iter()
            .collect::<Vec<_>>();
        outcomes.extend(self.commands.take_macro_stopped());
        outcomes.extend(
            self.peer_commands
                .iter_mut()
                .filter_map(|(origin, reply, commands)| {
                    commands.advance_macro(now_ms).map(|operation| {
                        Self::source_outcome(
                            DigitOutcome::Operation(operation),
                            origin,
                            reply.clone(),
                        )
                    })
                }),
        );
        outcomes.extend(
            self.peer_commands
                .iter_mut()
                .filter_map(|(origin, reply, commands)| {
                    commands
                        .take_macro_stopped()
                        .map(|outcome| Self::source_outcome(outcome, origin, reply.clone()))
                }),
        );
        outcomes
    }

    fn macro_operation_current(&mut self, operation: &DigitOperation, now_ms: u64) -> bool {
        if operation
            .action_permit
            .as_ref()
            .is_some_and(|p| !p.current(Some(now_ms)))
        {
            return false;
        }
        if operation.macro_permit.is_none() {
            return true;
        }
        let commands = if let Some(reply) = &operation.reply {
            self.peer_commands
                .iter_mut()
                .find(|(name, source, _)| {
                    Some(name.as_str()) == operation.origin.as_deref() && source == reply
                })
                .map(|(_, _, commands)| commands)
        } else {
            Some(&mut self.commands)
        };
        commands.is_some_and(|commands| commands.macro_operation_current(operation, now_ms))
    }

    fn prune_pending_source_retries(&mut self, now_ms: u64) {
        if self.pending_source_retries.is_empty() {
            return;
        }
        let pending = self
            .manager
            .snapshot()
            .into_iter()
            .filter(|peer| peer.retrying)
            .map(|peer| peer.name)
            .collect::<std::collections::BTreeSet<_>>();
        for (remote, source) in std::mem::take(&mut self.pending_source_retries) {
            if !pending.contains(&remote) {
                continue;
            }
            if pending_source_current(Some(&source), Some(now_ms))
                && self.macro_operation_current(&source, now_ms)
            {
                self.pending_source_retries.push((remote, source));
            } else {
                self.manager.cancel_retry(&remote);
            }
        }
    }
    /// Recheck the exact collector without extending its administrative grant.
    pub(crate) fn remote_authorized(&mut self, operation: &DigitOperation, now_ms: u64) -> bool {
        if !self.admitting
            || operation.reply.as_ref().is_some_and(PeerReply::ended)
            || !self.macro_operation_current(operation, now_ms)
        {
            return false;
        }
        let Some(request) = operation.remote else {
            return false;
        };
        let commands = if let Some(reply) = &operation.reply {
            self.peer_commands
                .iter_mut()
                .find(|(name, source, _)| {
                    Some(name.as_str()) == operation.origin.as_deref() && source == reply
                })
                .map(|(_, _, commands)| commands)
        } else {
            Some(&mut self.commands)
        };
        commands.is_some_and(|commands| {
            commands.remote_authorized(request.administrative(), operation.admin_authorized, now_ms)
        })
    }
    /// Renew the local source grant only after successful administrative work.
    pub(crate) fn confirm_administrative_command(&mut self, now_ms: u64) {
        self.commands.confirm_administrative_action(now_ms);
    }
    /// Renew only the collector that authorized a successfully applied peer command.
    pub(crate) fn confirm_peer_administrative_command(
        &mut self,
        origin: Option<&str>,
        reply: &PeerReply,
        now_ms: u64,
    ) {
        if let Some((_, _, commands)) = self
            .peer_commands
            .iter_mut()
            .find(|(name, source, _)| Some(name.as_str()) == origin && source == reply)
        {
            commands.confirm_administrative_action(now_ms);
        }
    }
    /// Immutable current hub view for status/topology publication.
    pub fn manager(&self) -> &LinkManager {
        &self.manager
    }
    /// Whether an exact peer belongs to a configured permanent group.
    pub fn is_group_member(&self, remote: &str) -> bool {
        self.schedule
            .as_ref()
            .is_some_and(|schedule| schedule.is_group_member(remote))
    }
    /// Resolve the peer's currently desired group, not an inactive same-peer schedule.
    pub fn group_member(&self, remote: &str) -> Option<GroupMemberSelection> {
        let route = self
            .schedule
            .as_ref()?
            .desired_routes()
            .find(|route| route.remote == remote && route.group_label.is_some())?;
        let label = route.group_label.as_deref()?;
        let group = self.groups.iter().find(|group| group.label == label)?;
        let slot = std::num::NonZeroUsize::new(route.group_priority?.saturating_add(1))?;
        Some(GroupMemberSelection::new(
            label,
            slot,
            group.selection.clone(),
        ))
    }
    /// Snapshot desired groups; a latched audio slot is selected only while its peer is live.
    pub(crate) fn priority_groups(&self) -> Vec<PriorityGroupStatus> {
        let Some(schedule) = self.schedule.as_ref() else {
            return Vec::new();
        };
        let peers = self.manager.snapshot();
        let reachable = |remote: &str| {
            peers
                .iter()
                .any(|peer| peer.name == remote && !peer.ended && !peer.retrying)
        };
        self.groups
            .iter()
            .filter(|group| {
                schedule
                    .desired_routes()
                    .any(|route| route.group_label.as_deref() == Some(group.label.as_str()))
            })
            .map(|group| {
                let selected = group.selection.active().and_then(|slot| {
                    group
                        .members
                        .iter()
                        .find(|(priority, peer)| {
                            priority.saturating_add(1) == slot.get() && reachable(peer)
                        })
                        .map(|(_, peer)| peer.clone())
                });
                let any_reachable = group.members.iter().any(|(_, remote)| reachable(remote));
                let all_attempted = group
                    .members
                    .iter()
                    .all(|(_, remote)| peers.iter().any(|peer| peer.name == *remote));
                PriorityGroupStatus {
                    label: group.label.clone(),
                    name: group.name.clone(),
                    members: group
                        .members
                        .iter()
                        .map(|(_, remote)| remote.clone())
                        .collect(),
                    selected,
                    unavailable: all_attempted && !any_reachable,
                }
            })
            .collect()
    }
    /// Prepare changed or thirty-second topology messages; these are not key advice.
    pub fn due_topology(&mut self, now_ms: u64) -> Vec<(String, String)> {
        self.topology.publish(&self.manager, now_ms)
    }
    /// Process copied peer text on control, never from its channel/audio callback.
    /// A returned detach unpublishes immediately; join the reader before `reclaimed`.
    pub fn peer_text(
        &mut self,
        remote: &str,
        bytes: &[u8],
        reply: PeerReply,
        local_receiving: bool,
        transitioned_ms: u64,
        now_ms: u64,
    ) -> Result<LinkEffect, AdmissionError> {
        if !self.admitting || reply.ended() {
            return Err(AdmissionError::Stale);
        }
        if !self
            .manager
            .snapshot()
            .iter()
            .any(|peer| peer.name == remote && !peer.ended && !peer.retrying)
        {
            return Err(AdmissionError::Invalid);
        }
        let message = Protocol::parse(bytes).ok_or(AdmissionError::Invalid)?;
        let detach = match &message {
            Protocol::RemoteDigit {
                destination,
                source,
                digit,
                ..
            } => {
                if !self.admits_digit_source(remote, &reply)
                    || !self.manager.remote_digit(remote, *digit, &self.policy)
                {
                    return Err(AdmissionError::Denied);
                }
                return Ok(if self.manager.wire_node() == Some(destination.as_str()) {
                    self.source_digit(source, *digit, now_ms, reply)
                        .map_or(LinkEffect::None, LinkEffect::Digit)
                } else {
                    LinkEffect::ForwardDigits(self.manager.relay_digit(remote, &message))
                });
            }
            Protocol::Topology(_) => {
                let looped = self.manager.update_topology(remote, bytes)?;
                if looped {
                    self.manager.reject_loop(remote);
                }
                looped
            }
            Protocol::Disconnect => true,
            Protocol::Query { .. } | Protocol::Key { .. } => {
                return Ok(LinkEffect::Text(self.manager.relay_key(
                    remote,
                    &message,
                    local_receiving,
                    transitioned_ms,
                    now_ms,
                )));
            }
            Protocol::NewKey | Protocol::NewKey1 | Protocol::IaxKey => false,
        };
        if detach {
            self.ended(remote);
            Ok(LinkEffect::Detach(vec![remote.into()]))
        } else {
            Ok(LinkEffect::None)
        }
    }
    /// Admit a directory-verified inbound peer through current deny-first and loop checks.
    pub fn authorize(&self, remote: &str, verified: bool) -> Result<(), AdmissionError> {
        if !self.admitting {
            return Err(AdmissionError::Stale);
        }
        let remote =
            PeerIdentity::canonical_operator(remote).map_err(|_| AdmissionError::Identity)?;
        self.manager.authorize_incoming_member(
            &remote,
            verified,
            &self.policy,
            self.group_member(&remote).as_ref(),
        )
    }
    /// Recheck and publish a directory-verified incoming peer.
    pub fn accept(&mut self, remote: &str, verified: bool) -> Result<(), AdmissionError> {
        let remote =
            PeerIdentity::canonical_operator(remote).map_err(|_| AdmissionError::Identity)?;
        self.authorize(&remote, verified)?;
        self.manager.admit_incoming_member(
            &remote,
            verified,
            &self.policy,
            self.group_member(&remote),
        )
    }
    pub(super) fn apply_event_groups(
        &mut self,
        local: &str,
        events: &super::scheduler::EventScheduler,
    ) -> bool {
        let Some(links) = self.schedule.as_mut() else {
            return false;
        };
        if let Some(profile) = self.commands.profile() {
            profile.route_generation.is_some_and(|generation| {
                events.apply_owned_group_holds(
                    profile.public_key.as_str(),
                    local,
                    Some(profile.owner),
                    generation,
                    links,
                )
            })
        } else {
            events.apply_group_holds(local, links)
        }
    }
    pub(super) fn schedule(&self) -> Option<&LinkScheduler> {
        self.schedule.as_ref()
    }
    pub(super) fn withdraw_removed(&mut self, candidate: &LinkScheduler) -> Vec<String> {
        let removed = self
            .schedule
            .as_ref()
            .map(|old| old.removed_routes(candidate))
            .unwrap_or_default();
        removed
            .into_iter()
            .map(|route| {
                self.manager.disconnect_permanent(&route.remote);
                self.commands.disconnect(&route.remote);
                route.remote
            })
            .collect()
    }
    /// Interpret radio events on the same serialized owner as reload and incoming admission.
    pub fn digit(&mut self, event: DigitEvent) -> Option<DigitOutcome> {
        self.commands.feed(event)
    }
    /// Submission loss revokes every source's queued expansion and partial command.
    pub fn lost_digits(&mut self) {
        self.commands.feed(DigitEvent::Lost);
        for (_, _, commands) in &mut self.peer_commands {
            commands.feed(DigitEvent::Lost);
        }
    }
    /// Adapt the legacy session timer only for legacy command profiles.
    pub fn legacy_peer_boundary(
        &mut self,
        remote: &str,
        now_ms: u64,
        reply: PeerReply,
    ) -> Option<DigitOutcome> {
        if self.command_profile().is_some() {
            return None;
        }
        self.peer_digit(remote, '#', now_ms, reply)
    }
    fn admits_digit_source(&self, remote: &str, reply: &PeerReply) -> bool {
        !reply.ended()
            && (self.command_profile().is_none()
                || self.ingress.iter().any(|(id, token)| {
                    id.operator_alias().as_deref() == Some(remote) && token == reply
                }))
    }
    /// Interpret a peer digit only while its direct connection and current policy allow it.
    pub fn peer_digit(
        &mut self,
        remote: &str,
        digit: char,
        now_ms: u64,
        reply: PeerReply,
    ) -> Option<DigitOutcome> {
        if !self.admits_digit_source(remote, &reply)
            || !self.manager.remote_digit(remote, digit, &self.policy)
        {
            return None;
        }
        self.source_digit(remote, digit, now_ms, reply)
    }
    /// Collect only an admitted private call's selected-node commands. No link
    /// membership or RF attachment is created, and idle phone digits never regenerate.
    /// The caller checks phone/destination selection permissions before this entry.
    pub fn phone_digit(
        &mut self,
        source: &crate::link::PhoneSource,
        digit: char,
        now_ms: u64,
        reply: PeerReply,
    ) -> Option<DigitOutcome> {
        if !self.admitting
            || !reply.phone_authenticated()
            || reply.phone_source() != Some(source)
            || !crate::link::valid_digit(digit)
        {
            return None;
        }
        self.source_digit(&source.call_id().to_string(), digit, now_ms, reply)
            .filter(|outcome| {
                !matches!(
                    outcome,
                    DigitOutcome::Regenerate(_) | DigitOutcome::ReceivedDigit(..)
                )
            })
    }
    /// Retire an actual private ingress before reclaiming its collectors and grants.
    /// Already queued work retains this ended token and fails its dispatch fence.
    pub fn retire_source(&mut self, reply: &PeerReply) {
        reply.retire();
        self.peer_commands.retain(|(_, source, _)| source != reply);
    }
    fn source_digit(
        &mut self,
        origin: &str,
        digit: char,
        now_ms: u64,
        reply: PeerReply,
    ) -> Option<DigitOutcome> {
        self.retain_peer_commands(now_ms);
        let index = if let Some(index) = self
            .peer_commands
            .iter()
            .position(|(name, source, _)| name == origin && source == &reply)
        {
            index
        } else {
            // Idle link digits need no collector. Bound claimed origins per ingress
            // without evicting another source's command, selection or admin state.
            if self.commands.profile().is_none() && digit != '*' {
                return reply
                    .regeneration_origin()
                    .map(|origin| DigitOutcome::ReceivedDigit(digit, origin));
            }
            if self
                .peer_commands
                .iter()
                .filter(|(_, source, _)| source == &reply)
                .count()
                >= 128
            {
                return None;
            }
            self.peer_commands
                .push((origin.into(), reply.clone(), self.commands.for_source()));
            self.peer_commands.len() - 1
        };
        let commands = &mut self.peer_commands[index].2;
        let outcome = commands.feed(DigitEvent::Digit { digit, now_ms });
        match outcome {
            Some(DigitOutcome::Regenerate(symbol)) => reply
                .regeneration_origin()
                .map(|token| DigitOutcome::ReceivedDigit(symbol, token)),
            other => other.map(|o| Self::source_outcome(o, origin, reply)),
        }
    }
    /// Attach source identity only to outcomes that carry a command.
    fn source_outcome(mut outcome: DigitOutcome, origin: &str, reply: PeerReply) -> DigitOutcome {
        if let DigitOutcome::Mapped(input) = &mut outcome {
            input.connection = Some((origin.to_owned(), reply.clone()));
        }
        if let DigitOutcome::MacroStopped { reply: target } = &mut outcome {
            *target = Some(reply.clone());
        }
        if let DigitOutcome::Operation(operation) | DigitOutcome::RemoteCandidate(operation) =
            &mut outcome
        {
            operation.reply = Some(reply);
            operation.origin = Some(origin.into());
        }
        outcome
    }
    /// Reclaim ended connections and expired idle collectors at each control boundary.
    fn retain_peer_commands(&mut self, now_ms: u64) {
        self.peer_commands
            .retain(|(_, source, commands)| !source.ended() && commands.retains_state(now_ms));
    }
    /// Complete overdue local modular input before collecting the next RF event.
    pub fn expire_receiver_digits(&mut self, at: u64) -> Option<DigitOutcome> {
        self.commands.profile()?;
        self.commands.expire(at)
    }
    /// Complete overdue candidates on this exact transport before feeding its next received symbol.
    pub fn expire_connection_digits(&mut self, source: &PeerReply, at: u64) -> Vec<DigitOutcome> {
        if source.ended() {
            return Vec::new();
        }
        self.peer_commands
            .iter_mut()
            .filter(|(_, r, _)| r == source)
            .filter_map(|(name, reply, commands)| {
                commands
                    .expire(at)
                    .map(|o| Self::source_outcome(o, name, reply.clone()))
            })
            .collect()
    }
    /// Expire peer commands independently, preserving their exact reply connection.
    pub fn expire_peer_digits(&mut self, now_ms: u64) -> Vec<DigitOutcome> {
        self.peer_commands.retain(|(_, source, _)| !source.ended());
        let mut outcomes: Vec<_> = self
            .peer_commands
            .iter_mut()
            .filter_map(|(origin, reply, commands)| {
                commands
                    .expire(now_ms)
                    .map(|outcome| Self::source_outcome(outcome, origin, reply.clone()))
            })
            .collect();
        if self.commands.profile().is_some() {
            outcomes.extend(self.commands.expire(now_ms));
        }
        self.retain_peer_commands(now_ms);
        outcomes
    }
    /// Complete only remote-base families on an actual ingress unkey. Addressed
    /// `*4` forwarding and ordinary peer-command termination keep their own rules.
    pub fn peer_remote_unkey(&mut self, now_ms: u64, reply: PeerReply) -> Vec<DigitOutcome> {
        if reply.ended() {
            return Vec::new();
        }
        self.peer_commands
            .iter_mut()
            .filter(|(_, source, commands)| source == &reply && commands.remote_collecting())
            .filter_map(|(origin, reply, commands)| {
                commands
                    .feed(DigitEvent::Unkey { now_ms })
                    .map(|outcome| Self::source_outcome(outcome, origin, reply.clone()))
            })
            .collect()
    }
    /// Apply policy, returning external work without doing a dial, media operation, or write.
    /// `directory_verified` is needed only when selecting a remote-command peer.
    pub fn command(
        &mut self,
        operation: DigitOperation,
        work: GenerationWork,
        now_ms: u64,
        directory_verified: bool,
    ) -> Result<LinkEffect, AdmissionError> {
        self.prune_pending_source_retries(now_ms);
        if !self.admitting || !work.is_current() {
            return Err(AdmissionError::Stale);
        }
        if operation.reply.as_ref().is_some_and(PeerReply::ended) {
            return Err(AdmissionError::Stale);
        }
        if !self.macro_operation_current(&operation, now_ms) {
            return Err(AdmissionError::Stale);
        }
        if let Some((reply, source)) = operation
            .reply
            .as_ref()
            .and_then(|reply| reply.phone_source().map(|source| (reply, source)))
        {
            if !reply.phone_authenticated()
                || operation.origin.as_deref() != Some(source.call_id().to_string().as_str())
                || operation.command.action == LinkAction::Command
            {
                // Legacy D envelopes provide neither private replies nor per-call retirement.
                return Err(AdmissionError::Denied);
            }
        }
        let remote = if matches!(
            operation.command.action,
            LinkAction::Monitor
                | LinkAction::LocalMonitor
                | LinkAction::Transceive
                | LinkAction::PermanentMonitor
                | LinkAction::PermanentLocalMonitor
                | LinkAction::PermanentTransceive
                | LinkAction::Disconnect
                | LinkAction::DisconnectPermanent
                | LinkAction::Command
        ) {
            PeerIdentity::canonical_operator(&operation.command.node)
                .map_err(|_| AdmissionError::Identity)?
        } else {
            operation.command.node.clone()
        };
        let effect = match operation.command.action {
            LinkAction::RemoteBase => {
                operation.remote.ok_or(AdmissionError::Invalid)?;
                if !self.remote_authorized(&operation, now_ms) {
                    return Err(AdmissionError::Denied);
                }
                LinkEffect::RemoteBase(operation)
            }
            LinkAction::CancelMessage => LinkEffect::CancelMessage,
            action @ (LinkAction::Message | LinkAction::SiteStatus) => {
                if remote.is_empty() || !crate::config::message_selector_valid(&remote) {
                    return Err(AdmissionError::Invalid);
                }
                if action == LinkAction::SiteStatus {
                    LinkEffect::SiteStatus {
                        selector: remote,
                        reply: operation.reply,
                    }
                } else {
                    LinkEffect::Message {
                        selector: remote,
                        reply: operation.reply,
                    }
                }
            }
            LinkAction::Monitor
            | LinkAction::LocalMonitor
            | LinkAction::Transceive
            | LinkAction::PermanentMonitor
            | LinkAction::PermanentLocalMonitor
            | LinkAction::PermanentTransceive => {
                if !self.policy.allows(&remote, true) {
                    return Err(AdmissionError::Denied);
                }
                if self.manager.is_wire_self(&remote) {
                    return Err(AdmissionError::Loop);
                }
                let mode = match operation.command.action {
                    LinkAction::Monitor | LinkAction::PermanentMonitor => Mode::MONITOR,
                    LinkAction::LocalMonitor | LinkAction::PermanentLocalMonitor => {
                        Mode::LOCAL_MONITOR
                    }
                    _ => Mode::TRANSCEIVE,
                };
                let permanent = matches!(
                    operation.command.action,
                    LinkAction::PermanentMonitor
                        | LinkAction::PermanentLocalMonitor
                        | LinkAction::PermanentTransceive
                );
                if permanent && operation.action_permit.is_some() {
                    if self
                        .schedule
                        .as_ref()
                        .is_some_and(|s| s.owns_target(&remote))
                    {
                        return Err(AdmissionError::Loop);
                    }
                    if self.manager.active_permanent(&remote, mode) {
                        return Ok(LinkEffect::None);
                    }
                }
                if permanent
                    && !self.manager.reaches(&remote, false)
                    && self.manager.resume_permanent_retry(&remote, mode, now_ms)
                {
                    return Ok(LinkEffect::None);
                }
                if self.manager.reaches(&remote, true) {
                    return Err(AdmissionError::Loop);
                }
                LinkEffect::Connect(ConnectAttempt {
                    pending_source: (operation.action_permit.is_some()
                        || operation.macro_permit.is_some()
                        || operation
                            .reply
                            .as_ref()
                            .is_some_and(|reply| reply.phone_source().is_some()))
                    .then(|| operation.clone()),
                    remote,
                    mode,
                    permanent,
                    group: None,
                    work,
                    scheduled: None,
                })
            }
            LinkAction::Disconnect | LinkAction::DisconnectPermanent => {
                let removed = if operation.command.action == LinkAction::DisconnectPermanent {
                    self.manager.disconnect_permanent(&remote)
                } else {
                    self.manager.disconnect(&remote)
                };
                if removed {
                    self.commands.disconnect(&remote);
                }
                LinkEffect::Detach(if removed { vec![remote] } else { Vec::new() })
            }
            LinkAction::DisconnectAll => {
                if let Some(schedule) = &mut self.schedule {
                    schedule.pause(&self.local);
                }
                let removed = self.manager.disconnect_all();
                for remote in &removed {
                    self.commands.disconnect(remote);
                }
                LinkEffect::Detach(removed)
            }
            LinkAction::DisconnectPermanentAll => {
                if let Some(schedule) = &mut self.schedule {
                    schedule.pause(&self.local);
                }
                let removed = self.manager.disconnect_permanent_all();
                for remote in &removed {
                    self.commands.disconnect(remote);
                }
                LinkEffect::Detach(removed)
            }
            LinkAction::DisconnectNonPermanentAll => {
                let removed = self.manager.disconnect_temporary();
                for remote in &removed {
                    self.commands.disconnect(remote);
                }
                LinkEffect::Detach(removed)
            }
            LinkAction::ReconnectAll => {
                let mut removed = Vec::new();
                if let Some(schedule) = &mut self.schedule {
                    schedule.resume(&self.local);
                    removed = schedule.withdraw_undesired();
                    for remote in &removed {
                        self.manager.disconnect_permanent(remote);
                        self.commands.disconnect(remote);
                    }
                }
                self.manager.resume_retries(now_ms);
                if removed.is_empty() {
                    LinkEffect::None
                } else {
                    LinkEffect::Detach(removed)
                }
            }
            LinkAction::ReconnectPermanentAll => {
                let mut removed = Vec::new();
                if let Some(schedule) = &mut self.schedule {
                    schedule.resume(&self.local);
                    removed = schedule.withdraw_undesired();
                    for remote in &removed {
                        self.manager.disconnect_permanent(remote);
                        self.commands.disconnect(remote);
                    }
                }
                self.manager.resume_permanent_retries(now_ms);
                if removed.is_empty() {
                    LinkEffect::None
                } else {
                    LinkEffect::Detach(removed)
                }
            }
            LinkAction::Command => {
                if PeerIdentity::from_operator(&remote)
                    .is_ok_and(|identity| identity.protocol() == ProtocolId::EchoLink)
                {
                    return Err(AdmissionError::UnsupportedRoute);
                }
                if self.manager.is_wire_self(&remote)
                    || !self.policy.allows(&remote, true)
                    || (operation.digit.is_none() && !directory_verified)
                {
                    return Err(AdmissionError::Denied);
                }
                let message = Protocol::RemoteDigit {
                    destination: remote.clone(),
                    source: self
                        .manager
                        .wire_node()
                        .ok_or(AdmissionError::Denied)?
                        .to_owned(),
                    sequence: self.digit_sequence.wrapping_add(1),
                    digit: operation.digit.unwrap_or('*'),
                };
                let deliveries = self.manager.relay_digit("", &message);
                if deliveries.is_empty() {
                    return Err(AdmissionError::Denied);
                }
                if let Some(digit) = operation.digit {
                    if !b"0123456789*#ABCD".contains(&(digit as u8)) || !digit.is_ascii() {
                        return Err(AdmissionError::Invalid);
                    }
                    self.digit_sequence = self.digit_sequence.wrapping_add(1);
                    LinkEffect::ForwardDigits(deliveries)
                } else {
                    if let Some(reply) = &operation.reply {
                        if let Some((_, _, commands)) =
                            self.peer_commands.iter_mut().find(|(name, source, _)| {
                                Some(name.as_str()) == operation.origin.as_deref()
                                    && source == reply
                            })
                        {
                            commands.select_remote(&remote);
                        }
                    } else {
                        self.commands.select_remote(&remote);
                    }
                    LinkEffect::SelectedRemote
                }
            }
            action @ (LinkAction::Status
            | LinkAction::LastKeyed
            | LinkAction::FullStatus
            | LinkAction::Time) => LinkEffect::Telemetry(action, operation.reply),
            action @ (LinkAction::ParrotEnable
            | LinkAction::ParrotDisable
            | LinkAction::EchoTelemetry) => {
                let commands = if let Some(reply) = &operation.reply {
                    self.peer_commands
                        .iter_mut()
                        .find(|(name, source, _)| {
                            Some(name.as_str()) == operation.origin.as_deref() && source == reply
                        })
                        .map(|(_, _, commands)| commands)
                } else {
                    Some(&mut self.commands)
                };
                if !commands.is_some_and(|commands| {
                    commands
                        .consume_administrative_authorization(operation.admin_authorized, now_ms)
                }) {
                    return Err(AdmissionError::Denied);
                }
                if action == LinkAction::EchoTelemetry {
                    use crate::config::EchoLinkTelemetry as Mode;
                    LinkEffect::EchoTelemetry(match remote.as_str() {
                        "0" => Mode::Off,
                        "1" => Mode::On,
                        "2" => Mode::Follow,
                        "3" => Mode::Demand,
                        _ => return Err(AdmissionError::Invalid),
                    })
                } else {
                    LinkEffect::ParrotEnabled(action == LinkAction::ParrotEnable)
                }
            }
            LinkAction::AdminUnlock | LinkAction::AdminLock | LinkAction::PadTest => {
                return Err(AdmissionError::Denied);
            }
        };
        Ok(effect)
    }
    /// Abandon an externally invalidated dial without adding retry intent or changing topology.
    /// Only its still-matching schedule reservation is released for a later current attempt.
    pub fn cancel_connect(&mut self, attempt: ConnectAttempt) {
        if let (Some(schedule), Some(reservation)) = (&mut self.schedule, &attempt.scheduled) {
            schedule.complete(reservation, false);
        }
    }

    /// Revalidate a slow scheduled dial against a freshly captured local clock/activity.
    /// Only `Ok(true)` authorizes installing the caller-owned answered channel/media owner.
    /// A missing clock rejects window-controlled transitions; plain permanent routes work
    /// without a wall clock. Administrative dials do not acquire a calendar dependency.
    pub fn finish_connect_at(
        &mut self,
        attempt: ConnectAttempt,
        answered: bool,
        now_ms: u64,
        current_clock: Option<(CivilTime, u8)>,
        activity: impl FnMut(&str) -> Option<u64>,
    ) -> Result<bool, AdmissionError> {
        let needs_calendar = attempt.scheduled.is_some()
            && self
                .schedule
                .as_ref()
                .is_some_and(LinkScheduler::requires_civil_time);
        let calendar_valid =
            !needs_calendar || current_clock.is_some_and(|(_, second)| second < 60);
        if let Some((local, second)) = current_clock.filter(|_| needs_calendar && calendar_valid) {
            self.tick(local, second, now_ms, activity);
        }
        let scheduled_current = attempt.scheduled.as_ref().is_none_or(|reservation| {
            self.schedule
                .as_ref()
                .is_some_and(|schedule| schedule.current(reservation))
        });
        let mut topology_blocked = false;
        let result = if !self.admitting
            || !attempt.current_at(now_ms)
            || attempt
                .pending_source
                .as_ref()
                .is_some_and(|source| !self.macro_operation_current(source, now_ms))
            || !scheduled_current
            || !calendar_valid
        {
            Err(AdmissionError::Stale)
        } else if !self.policy.allows(&attempt.remote, true) {
            Err(AdmissionError::Denied)
        } else if answered {
            match self.manager.attach_member(
                &attempt.remote,
                attempt.mode,
                attempt.permanent,
                attempt.group.as_deref().cloned(),
            ) {
                Ok(()) => Ok(true),
                Err(AdmissionError::Loop) if attempt.permanent => {
                    self.manager.retain_topology_blocked_group(
                        &attempt.remote,
                        attempt.mode,
                        attempt.group.as_deref().map(GroupMemberSelection::label),
                        attempt.group.as_deref().cloned(),
                    );
                    topology_blocked = true;
                    Err(AdmissionError::Loop)
                }
                Err(error) => Err(error),
            }
        } else if attempt.permanent {
            self.manager
                .retain_retry_member(
                    &attempt.remote,
                    attempt.mode,
                    now_ms,
                    attempt.group.as_deref().cloned(),
                )
                .map(|()| false)
        } else {
            Ok(false)
        };
        if let (Some(schedule), Some(reservation)) = (&mut self.schedule, &attempt.scheduled) {
            schedule.complete(
                reservation,
                topology_blocked || (result.is_ok() && (answered || attempt.permanent)),
            );
        }
        if result == Ok(true) {
            self.pending_source_retries
                .retain(|(remote, _)| remote != &attempt.remote);
        } else if topology_blocked || (result == Ok(false) && attempt.permanent) {
            if let Some(source) = attempt.pending_source {
                self.pending_source_retries
                    .retain(|(remote, _)| remote != &attempt.remote);
                self.pending_source_retries.push((attempt.remote, source));
            }
        }
        result
    }
    /// Reserve one due hub-recovery dial outside the serialized control owner.
    pub fn take_retry(&mut self, now_ms: u64, work: GenerationWork) -> Option<RetryWork> {
        if !self.admitting || !work.is_current() {
            return None;
        }
        self.prune_pending_source_retries(now_ms);
        self.manager.take_retry(now_ms).map(|attempt| RetryWork {
            pending_source: self
                .pending_source_retries
                .iter()
                .find(|(remote, _)| remote == attempt.name())
                .map(|(_, source)| source.clone()),
            attempt,
            work,
        })
    }
    /// Publish only a still-current retry; the adapter installs its prepared resources on true.
    pub fn finish_retry(
        &mut self,
        retry: RetryWork,
        answered: bool,
        now_ms: u64,
    ) -> Result<bool, AdmissionError> {
        let owns_attempt = self.manager.owns_retry(&retry.attempt);
        let source_valid = retry.current_at(now_ms)
            && retry
                .pending_source
                .as_ref()
                .is_none_or(|source| self.macro_operation_current(source, now_ms));
        let attempt = retry.attempt;
        let remote = attempt.name().to_owned();
        let result = if !self.admitting || !owns_attempt || !source_valid {
            Err(AdmissionError::Stale)
        } else if !self.policy.allows(attempt.name(), true) {
            Err(AdmissionError::Denied)
        } else if answered {
            self.manager.publish_retry(&attempt).map(|()| true)
        } else {
            Ok(false)
        };
        self.manager
            .finish_retry(attempt, matches!(result, Ok(true)), now_ms);
        if owns_attempt && (result == Ok(true) || !source_valid) {
            if let Some(source) = &retry.pending_source {
                self.pending_source_retries
                    .retain(|(name, current)| name != &remote || current != source);
            }
            if !source_valid && retry.pending_source.is_some() {
                self.manager.cancel_retry(&remote);
            }
        }
        result
    }
    /// Refresh windows from captured time/activity before selecting or revalidating link work.
    pub fn tick(
        &mut self,
        local: CivilTime,
        second: u8,
        now_ms: u64,
        activity: impl FnMut(&str) -> Option<u64>,
    ) {
        if let Some(schedule) = &mut self.schedule {
            schedule.tick(local, second, now_ms, activity, |route| {
                self.manager.owns_permanent(&route.remote)
            });
        }
    }
    /// Refresh route policy and reserve one due warning only when its local queue has room.
    pub(super) fn tick_with_warnings(
        &mut self,
        local: CivilTime,
        second: u8,
        now_ms: u64,
        activity: impl FnMut(&str) -> Option<u64>,
        active: bool,
        warning_capacity: bool,
    ) -> Option<ScheduleWarning> {
        self.schedule.as_mut()?.tick_with_warnings(
            local,
            second,
            now_ms,
            activity,
            |route| self.manager.owns_permanent(&route.remote),
            WarningGate {
                source_active: |_: &str| active,
                capacity: warning_capacity,
            },
        )
    }
    /// Reserve a configured attach, or unpublish a withdrawal before allowing replacements.
    pub fn next_scheduled(&mut self, work: GenerationWork) -> Option<LinkEffect> {
        if !self.admitting || !work.is_current() {
            return None;
        }
        let schedule = self.schedule.as_mut()?;
        let reservation = schedule.next_operation()?;
        if reservation.action() == LinkTransition::Detach {
            let remote = reservation.remote().to_owned();
            self.manager.disconnect_permanent(&remote);
            self.commands.disconnect(&remote);
            schedule.complete(&reservation, true);
            return Some(LinkEffect::Detach(vec![remote]));
        }
        Some(LinkEffect::Connect(ConnectAttempt {
            pending_source: None,
            remote: reservation.remote().into(),
            mode: Mode::TRANSCEIVE,
            permanent: true,
            group: reservation
                .group_label()
                .and_then(|_| self.group_member(reservation.remote()))
                .map(Box::new),
            work,
            scheduled: Some(reservation),
        }))
    }
    /// Suppress a failed reader immediately; its identity remains reserved until quiescence.
    pub fn ended(&mut self, remote: &str) {
        self.peer_commands.retain(|(name, _, _)| name != remote);
        for (_, _, commands) in &mut self.peer_commands {
            commands.disconnect(remote);
        }
        self.commands.disconnect(remote);
        self.manager.end(remote);
    }
    /// Release logical peer ownership only after its external callbacks/channel are quiescent.
    pub fn reclaimed(&mut self, remote: &str, now_ms: u64) {
        self.manager.reclaim(remote, now_ms);
    }
    /// Gate old dials and return every external identity to stop/drain before host reclamation.
    pub fn close(&mut self) -> Vec<String> {
        self.admitting = false;
        self.ingress.clear();
        self.commands.cancel_macro();
        for (_, _, commands) in &mut self.peer_commands {
            commands.cancel_macro();
        }
        self.manager.close()
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
