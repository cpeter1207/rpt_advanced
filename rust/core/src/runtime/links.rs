//! Serialized link-command orchestration and final gates around external dialing.

use super::{
    GenerationWork,
    dtmf::{DigitEvent, DigitOperation, DtmfCommands},
    link_schedule::{LinkReservation, LinkScheduler, LinkTransition},
};
use crate::{
    access::AccessPolicy,
    command::{DtmfCommandMap, LinkAction},
    link::{AdmissionError, LinkManager, Mode, Protocol, RetryAttempt, TopologyManager},
    schedule::CivilTime,
};

/// Owned dial request. Carry this across external lookup/media/dial and return it for publication.
pub struct ConnectAttempt {
    remote: String,
    mode: Mode,
    permanent: bool,
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
    /// Send one copied remote digit outside native audio.
    RemoteDigit {
        /// Direct selected peer.
        remote: String,
        /// Completed DTMF symbol.
        digit: char,
    },
    /// Remote-command mode was selected without transmitting a digit.
    SelectedRemote,
    /// Prepare existing status/topology/time telemetry outside audio.
    Telemetry(LinkAction),
    /// No physical operation is needed (for example resumed retry intent).
    None,
}

/// Long-lived node link/control policy. Retain this owner, its peers and retry intent on reload.
pub struct NodeLinkControl {
    local: String,
    manager: LinkManager,
    policy: AccessPolicy,
    commands: DtmfCommands,
    schedule: Option<LinkScheduler>,
    topology: TopologyManager,
    admitting: bool,
}
impl NodeLinkControl {
    /// Compose validated node policy before registering any external callbacks.
    pub fn new(
        local: &str,
        policy: AccessPolicy,
        commands: DtmfCommandMap,
        schedule: Option<LinkScheduler>,
    ) -> Result<Self, AdmissionError> {
        Ok(Self {
            local: local.into(),
            manager: LinkManager::new(local)?,
            policy,
            commands: DtmfCommands::new(commands),
            schedule,
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
        self.manager.invalidate_generation();
        self.policy = policy;
        self.commands = DtmfCommands::new(commands);
        self.schedule = schedule;
    }
    /// Immutable current hub view for status/topology publication.
    pub fn manager(&self) -> &LinkManager {
        &self.manager
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
        local_receiving: bool,
        transitioned_ms: u64,
        now_ms: u64,
    ) -> Result<LinkEffect, AdmissionError> {
        if !self.admitting {
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
            Protocol::NewKey | Protocol::IaxKey => false,
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
        self.manager
            .authorize_incoming(remote, verified, &self.policy)
    }
    /// Recheck and publish a directory-verified incoming peer.
    pub fn accept(&mut self, remote: &str, verified: bool) -> Result<(), AdmissionError> {
        self.authorize(remote, verified)?;
        self.manager.admit_incoming(remote, verified, &self.policy)
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
    pub fn digit(&mut self, event: DigitEvent) -> Option<DigitOperation> {
        self.commands.feed(event)
    }
    /// Interpret a peer digit only while its direct connection and current policy allow it.
    pub fn peer_digit(&mut self, remote: &str, digit: char, now_ms: u64) -> Option<DigitOperation> {
        if !self.manager.remote_digit(remote, digit, &self.policy) {
            return None;
        }
        self.commands.feed(DigitEvent::Digit { digit, now_ms })
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
        let remote = operation.command.node;
        let effect = match operation.command.action {
            LinkAction::Monitor
            | LinkAction::LocalMonitor
            | LinkAction::Transceive
            | LinkAction::PermanentMonitor
            | LinkAction::PermanentLocalMonitor
            | LinkAction::PermanentTransceive => {
                if !self.policy.allows(&remote, true) {
                    return Err(AdmissionError::Denied);
                }
                if remote == self.local || self.manager.reaches(&remote, true) {
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
                LinkEffect::Connect(ConnectAttempt {
                    remote,
                    mode,
                    permanent,
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
            LinkAction::Command => {
                if !self.manager.remote_digit(&remote, '*', &self.policy)
                    || (operation.digit.is_none() && !directory_verified)
                {
                    return Err(AdmissionError::Denied);
                }
                if let Some(digit) = operation.digit {
                    LinkEffect::RemoteDigit { remote, digit }
                } else {
                    self.commands.select_remote(&remote);
                    LinkEffect::SelectedRemote
                }
            }
            action @ (LinkAction::Status
            | LinkAction::LastKeyed
            | LinkAction::FullStatus
            | LinkAction::Time) => LinkEffect::Telemetry(action),
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
            match self
                .manager
                .attach(&attempt.remote, attempt.mode, attempt.permanent)
            {
                Ok(()) => Ok(true),
                Err(AdmissionError::Loop) if attempt.permanent => {
                    self.manager
                        .retain_topology_blocked(&attempt.remote, attempt.mode);
                    topology_blocked = true;
                    Err(AdmissionError::Loop)
                }
                Err(error) => Err(error),
            }
        } else if attempt.permanent {
            self.manager
                .retain_retry(&attempt.remote, attempt.mode, now_ms)
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
            work,
            scheduled: Some(reservation),
        }))
    }
    /// Suppress a failed reader immediately; its identity remains reserved until quiescence.
    pub fn ended(&mut self, remote: &str) {
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
