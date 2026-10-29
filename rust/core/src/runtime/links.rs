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
    remote: String,
    mode: Mode,
    permanent: bool,
    group: Option<Box<GroupMemberSelection>>,
    work: GenerationWork,
    scheduled: Option<LinkReservation>,
}

/// Retry work retains a host-generation permit until external dial cleanup completes.
pub struct RetryWork {
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
        self.work.is_current()
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
        self.work.is_current()
    }
}

/// Adapter-facing consequence of one serialized existing link command.
pub enum LinkEffect {
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
        self.groups = configured_groups(schedule.as_ref(), &self.groups);
        self.manager.invalidate_generation();
        self.policy = policy;
        self.commands = DtmfCommands::new(commands);
        self.peer_commands.clear();
        self.schedule = schedule;
    }
    /// Configure per-node Argon2id credentials; missing either digest disables admin DTMF.
    pub fn configure_admin(&mut self, unlock_hash: &str, lock_hash: &str, timeout_ms: u64) {
        self.commands
            .configure_admin(unlock_hash, lock_hash, timeout_ms);
        self.peer_commands.clear();
    }
    /// Renew the DTMF administration window only after the parrot toggle is accepted.
    pub(crate) fn confirm_parrot_command(&mut self, now_ms: u64) {
        self.commands.confirm_parrot_action(now_ms);
    }
    /// Renew only the collector that authorized a successfully applied peer command.
    pub(crate) fn confirm_peer_parrot_command(
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
            commands.confirm_parrot_action(now_ms);
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
                if !self.manager.remote_digit(remote, *digit, &self.policy) {
                    return Err(AdmissionError::Denied);
                }
                return Ok(if destination == &self.local {
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
    /// Interpret a peer digit only while its direct connection and current policy allow it.
    pub fn peer_digit(
        &mut self,
        remote: &str,
        digit: char,
        now_ms: u64,
        reply: PeerReply,
    ) -> Option<DigitOutcome> {
        if reply.ended() || !self.manager.remote_digit(remote, digit, &self.policy) {
            return None;
        }
        self.source_digit(remote, digit, now_ms, reply)
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
            if digit != '*' {
                return Some(DigitOutcome::Regenerate(digit));
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
        self.peer_commands[index]
            .2
            .feed(DigitEvent::Digit { digit, now_ms })
            .map(|outcome| Self::source_outcome(outcome, origin, reply))
    }
    /// Attach source identity only to outcomes that carry a command.
    fn source_outcome(mut outcome: DigitOutcome, origin: &str, reply: PeerReply) -> DigitOutcome {
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
    /// Expire peer commands independently, preserving their exact reply connection.
    pub fn expire_peer_digits(&mut self, now_ms: u64) -> Vec<DigitOutcome> {
        self.peer_commands.retain(|(_, source, _)| !source.ended());
        let outcomes = self
            .peer_commands
            .iter_mut()
            .filter_map(|(origin, reply, commands)| {
                commands
                    .expire(now_ms)
                    .map(|outcome| Self::source_outcome(outcome, origin, reply.clone()))
            })
            .collect();
        self.retain_peer_commands(now_ms);
        outcomes
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
        if !self.admitting || !work.is_current() {
            return Err(AdmissionError::Stale);
        }
        if operation.reply.as_ref().is_some_and(PeerReply::ended) {
            return Err(AdmissionError::Stale);
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
            operation.command.node
        };
        let effect = match operation.command.action {
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
                if remote == self.local {
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
                if remote == self.local
                    || !self.policy.allows(&remote, true)
                    || (operation.digit.is_none() && !directory_verified)
                {
                    return Err(AdmissionError::Denied);
                }
                let message = Protocol::RemoteDigit {
                    destination: remote.clone(),
                    source: self.local.clone(),
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
                    commands.consume_parrot_authorization(operation.admin_authorized, now_ms)
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
            || !attempt.work.is_current()
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
        result
    }
    /// Reserve one due hub-recovery dial outside the serialized control owner.
    pub fn take_retry(&mut self, now_ms: u64, work: GenerationWork) -> Option<RetryWork> {
        if !self.admitting || !work.is_current() {
            return None;
        }
        self.manager
            .take_retry(now_ms)
            .map(|attempt| RetryWork { attempt, work })
    }
    /// Publish only a still-current retry; the adapter installs its prepared resources on true.
    pub fn finish_retry(
        &mut self,
        retry: RetryWork,
        answered: bool,
        now_ms: u64,
    ) -> Result<bool, AdmissionError> {
        let attempt = retry.attempt;
        let result = if !self.admitting || !retry.work.is_current() {
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
        self.manager.close()
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
