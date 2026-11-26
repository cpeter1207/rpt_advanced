//! Adapter-neutral product runtime: prepare all resources, then publish one control revision.

use super::{
    DeviceHandoff, GenerationSettings, GenerationWork, LifecycleState, LifecycleStatus, NodeHost,
    OwnedReceiveOwner, OwnedTransmitOwner, RuntimeGeneration,
    dtmf::{DigitEvent, DigitOperation, DigitOutcome, DtmfDispatcher, DtmfWorker},
    link_schedule::{LinkScheduler, RouteSpec},
    links::{LinkEffect, NodeLinkControl},
    prepare::{self, NativeMediaPreparer, RuntimeError},
    render,
    scheduler::{CalendarObservation, EventScheduler, EventTerminals},
};
use crate::{
    access::AccessPolicy,
    command::{Command, LinkAction},
    config::{ResolvedIdentifierSettings, ResolvedNodeSettings, ResolvedSiteSettings},
    controller::{ActivitySnapshot, ControllerControl, NodeController, ParrotLevels},
    media::{PreparedAudio, StationMediaSession},
    messages::{Message, MessageArgument, MessageCatalog, MessageForms},
    schedule::CivilTime,
    time::{TimeAnnouncement, TimeFormat},
};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

#[path = "command_dispatch.rs"]
mod command_dispatch;
#[path = "command_program.rs"]
mod command_program;
pub use command_dispatch::CommandRenderers;
use command_program::CommandPrograms;
pub use command_program::{CommandCompletion, CommandReceipt, CommandResponse};

#[path = "reload.rs"]
mod reload;
use crate::config::resources::generation::RuntimeConfiguration;
use reload::ReloadTransaction;

#[path = "aggregate_modular.rs"]
mod modular;
pub use modular::PreparedConfigurationTransition;

fn queue_status_media(
    telemetry: &mut ControllerControl,
    station: &mut Option<Box<dyn StationMediaSession>>,
    settings: &ResolvedIdentifierSettings,
    morse: &str,
    speech: &str,
    reply: Option<crate::link::PeerReply>,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<(), RuntimeError> {
    if reply
        .as_ref()
        .is_some_and(|reply| reply.phone_source().is_some())
    {
        // Phone calls have no RF peer slot; only their explicit private outbox can deliver.
        return Err(RuntimeError::Rejected);
    }
    let station = station.as_mut().ok_or(RuntimeError::Preparation)?;
    let mut prepared =
        prepare::streamed_status(station.as_mut(), settings, morse, speech)?.reply_to(reply);
    if let Some(cancellation) = cancellation {
        prepared = prepared.cancel_when(cancellation);
    }
    telemetry.reclaim().for_each(drop);
    if telemetry.queue_prepared_status(prepared) {
        Ok(())
    } else {
        Err(RuntimeError::Rejected)
    }
}

/// One externally captured tick; core never reads host clocks or time-zone services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeClock {
    /// Monotonic milliseconds.
    pub now_ms: u64,
    /// Original ticker wall-clock seconds, preserved through queue delay.
    pub wall_seconds: i64,
    /// Fraction of that captured wall-clock second, from zero through 999 milliseconds.
    pub wall_subsecond_ms: u16,
    /// Validated local calendar and second; unavailable clocks cannot select replacement windows.
    pub civil: Option<(CivilTime, u8)>,
}
/// Fixed callback bounds and candidate-owned adapter state prepared before publication.
pub struct PreparedAdapter<A: Send, C: Send = ()> {
    /// Canonical shared-radio command family, absent when remote control is disabled.
    pub remote_commands: Option<crate::remote_base_command::RemoteCommandConfiguration>,
    /// Unique TX link/ring/encoder state; never clone live consumers into a candidate.
    pub state: A,
    /// Prepared, not-yet-running generation-owned dispatcher/control state. Start it only through
    /// `RuntimeNode::adapter_control_mut` after publication, then explicitly stop/drain and detach.
    pub control: C,
    /// Maximum native input frame length.
    pub receive_maximum: usize,
    /// Maximum native output frame length.
    pub transmit_maximum: usize,
}
/// One TX generation's concrete controller and unique prepared adapter resources.
pub struct RuntimeTransmit<A: Send> {
    /// Core controller, exclusively owned by the fixed TX callback.
    pub controller: NodeController,
    /// Link and transport state supplied by the adapter's candidate preparation.
    pub adapter: A,
    /// Immutable resolved configuration for this coherent generation.
    pub settings: ResolvedNodeSettings,
}
/// Fixed registrations transferred once to external callback lifecycle control.
pub type RuntimeAudioOwners<A> = (
    OwnedReceiveOwner<DtmfWorker, RuntimeTransmit<A>>,
    OwnedTransmitOwner<DtmfWorker, RuntimeTransmit<A>>,
);

struct ControlState<C: Send> {
    generation: u64,
    adapter: C,
    adapter_exposed: bool,
    telemetry: ControllerControl,
    media_session: Option<Box<dyn crate::media::StationMediaSession>>,
    digits: DtmfDispatcher,
    activity: ActivitySnapshot,
    started_ms: u64,
    prior_activity_ms: Option<u64>,
    catalog: Arc<MessageCatalog>,
    messages: super::composition::MessageRecipes,
}
struct GroupAnnouncement {
    label: String,
    name: String,
    members: Vec<String>,
    outage_notice: Option<Arc<AtomicBool>>,
}

fn same_group_configuration(
    previous: Option<&GroupAnnouncement>,
    group: &crate::runtime::links::PriorityGroupStatus,
) -> bool {
    previous
        .is_some_and(|previous| previous.name == group.name && previous.members == group.members)
}

fn announce_unavailable_group(
    group_unavailable: bool,
    same_configuration: bool,
    previously_available: bool,
) -> bool {
    group_unavailable && (!same_configuration || previously_available)
}

impl<C: Send> ControlState<C> {
    fn activity_ms(&self) -> Option<u64> {
        self.activity
            .last_sample()
            .map(|sample| self.started_ms.saturating_add(sample / 48))
            .or(self.prior_activity_ms)
    }
}
impl<C: Send> Drop for ControlState<C> {
    fn drop(&mut self) {
        // Sessions join their station producer here, after generation quiescence on control.
        drop(self.media_session.take());
    }
}
/// Stable per-node control identity, retained across successful generation swaps.
pub struct RuntimeNode<A: Send, C: Send = ()> {
    name: String,
    programs: CommandPrograms,
    command_messages: Option<command_dispatch::CommandMessages>,
    site: crate::site::SiteController,
    settings: ResolvedNodeSettings,
    bounds: GenerationSettings,
    host: NodeHost<DtmfWorker, RuntimeTransmit<A>>,
    device: Box<dyn DeviceHandoff>,
    device_open: bool,
    handoff_pending: bool,
    failed_station_replacement: bool,
    pending_peer_cleanup: bool,
    links: NodeLinkControl,
    control: ControlState<C>,
    retired_control: Option<ControlState<C>>,
    status: ResolvedIdentifierSettings,
    time_format: u64,
    group_announcements: Vec<GroupAnnouncement>,
    echo_telemetry: crate::link::EchoTelemetryPolicy,
    echo_announcement: crate::config::EchoLinkAnnouncement,
}
impl<A: Send, C: Send> RuntimeNode<A, C> {
    /// Reconcile event group holds before reserving any route operation. Verify this node's
    /// exact registered port key and link generation at the existing route owner.
    pub fn apply_event_groups(&mut self, events: &EventScheduler) -> bool {
        self.links.apply_event_groups(&self.name, events)
    }
    fn message_values(
        &self,
        clock: RuntimeClock,
        catalog: &MessageCatalog,
    ) -> Result<super::MessageValueSnapshot, RuntimeError> {
        let (civil, _) = clock.civil.ok_or(RuntimeError::Clock)?;
        let site = self.site.snapshot(clock.now_ms);
        let (mut links, identity) = render::direct_status(self.links.manager(), catalog)?;
        links.tts = render::telemetry_speech(&links.tts, &[&identity]);
        Ok::<_, RuntimeError>(super::MessageValueSnapshot {
            civil,
            time_format: TimeFormat::try_from(self.time_format)
                .map_err(|_| RuntimeError::Preparation)?,
            node: self
                .links
                .command_profile()
                .map_or(self.name.as_str(), |profile| profile.public_key.as_str())
                .to_owned(),
            callsign: self.settings.callsign.clone(),
            port: self
                .command_messages
                .as_ref()
                .map_or_else(Default::default, |messages| messages.port.clone()),
            event: None,
            builtin: Default::default(),
            links,
            sensors: site
                .sensors
                .into_iter()
                .map(|(name, sensor)| {
                    (
                        name,
                        crate::site::Observation {
                            value: sensor.current.value.map(MessageArgument::Number),
                            observed_at_ms: sensor.current.observed_at_ms,
                            quality: sensor.current.quality,
                        },
                    )
                })
                .collect(),
            inputs: site
                .inputs
                .into_iter()
                .map(|(name, input)| {
                    (
                        name,
                        crate::site::Observation {
                            value: input
                                .value
                                .map(|value| MessageArgument::Number(f64::from(value))),
                            observed_at_ms: input.observed_at_ms,
                            quality: input.quality,
                        },
                    )
                })
                .collect(),
            time_remaining: None,
        })
    }
    fn command_message_event(
        &self,
        reply: &crate::link::PeerReply,
    ) -> Option<super::MessageEventIdentity> {
        use crate::link::ProtocolId;
        let peer = self.links.command_peer(reply);
        let protocol = match peer.map(crate::link::PeerIdentity::protocol) {
            Some(ProtocolId::AllStarLink) => "allstarlink",
            Some(ProtocolId::EchoLink) => "echolink",
            _ if reply.phone_authenticated() => "phone",
            _ => return None,
        };
        Some(super::MessageEventIdentity {
            kind: "command".into(),
            peer_protocol: Some(protocol.into()),
            peer_number: peer
                .and_then(crate::link::PeerIdentity::node)
                .map(str::to_owned),
            peer_callsign: reply.peer_callsign().map(str::to_owned),
        })
    }

    /// Accepted calendar samples from this station's installed command scheduler.
    pub fn calendar_observation(&self) -> Option<CalendarObservation> {
        self.programs.scheduler.as_ref()?.calendar_observation()
    }

    /// Current calendar observations from this station's installed command scheduler.
    pub fn observed_events(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::config::resources::candidate::ResourceId,
            Option<crate::schedule::recurrence::Error>,
            EventTerminals,
        ),
    > + '_ {
        self.programs
            .scheduler
            .iter()
            .flat_map(EventScheduler::observed_events)
    }

    /// Capture the local receiver separately from link and telemetry activity.
    pub fn radio_receive_status(&self) -> super::links::ReceiveStatus {
        let telemetry = &self.control.telemetry;
        let muted = telemetry.rx_mute().load(Ordering::Acquire) & 1 != 0;
        super::links::ReceiveStatus {
            muted,
            active: !muted && telemetry.qualified_receive(),
        }
    }
    /// Continuous receive activity for calendar politeness in the same runtime monotonic epoch.
    pub fn event_activity(&self) -> super::scheduler::ReceiveActivity {
        super::scheduler::ReceiveActivity {
            active: self.control.activity.is_active(),
            last_active_ms: Some(
                self.control
                    .activity_ms()
                    .unwrap_or(self.control.started_ms),
            ),
        }
    }
    /// Station-owned site policy; callers deliver observations/effects off audio.
    pub fn site(&mut self) -> &mut crate::site::SiteController {
        &mut self.site
    }
    /// Apply already-authorized site policy on control. The RF gate must be
    /// published before returning, independently of hardware or media admission.
    pub fn site_command(
        &mut self,
        command: crate::site::SiteCommand,
        now_ms: u64,
    ) -> Result<Vec<crate::site::SiteEffect>, crate::site::SiteError> {
        let result = self.site.command(command, now_ms);
        self.publish_rf_inhibit();
        result
    }
    /// Advance station protection and publish its RF gate independently of media admission.
    pub fn advance_site(
        &mut self,
        now_ms: u64,
        observations: &crate::site::SiteObservations,
        ptt: &std::collections::BTreeMap<crate::config::resources::PortKey, bool>,
    ) -> Vec<crate::site::SiteEffect> {
        let effects = self.site.advance(now_ms, observations, ptt);
        self.publish_rf_inhibit();
        effects
    }
    fn publish_rf_inhibit(&self) {
        self.control
            .telemetry
            .set_site_inhibit(self.failed_station_replacement || self.site.transmit_inhibited());
    }

    /// Borrow a current/retiring adapter dispatcher only on control. Once exposed, its generation
    /// requires an explicit `detached` acknowledgment after dispatcher stop/drain before reclaim.
    pub fn adapter_control_mut(&mut self, generation: u64) -> Option<&mut C> {
        let control = if self.control.generation == generation {
            Some(&mut self.control)
        } else {
            self.retired_control
                .as_mut()
                .filter(|control| control.generation == generation)
        }?;
        control.adapter_exposed = true;
        Some(&mut control.adapter)
    }
    /// Register fixed owners before starting external callbacks; a second registration rejects.
    pub fn register_audio(&mut self) -> Option<RuntimeAudioOwners<A>> {
        self.host.register_audio()
    }
    /// Immutable active resolved node settings for directory/adapter control operations.
    pub fn settings(&self) -> &ResolvedNodeSettings {
        &self.settings
    }

    /// Local speech rendition with a localized sender label and literal human body.
    pub fn incoming_text_speech(&self, source: &str, body: &str) -> String {
        self.control
            .catalog
            .format_incoming_text(source, body)
            .expect("validated catalog and bounded human text")
            .tts
    }
    /// Serialized link policy, shared by CLI, incoming admission, DTMF, and ticker work.
    pub fn links(&mut self) -> &mut NodeLinkControl {
        &mut self.links
    }
    /// Reserve current-generation ownership before submitting external work.
    pub fn work(&mut self) -> Option<GenerationWork> {
        if self.handoff_pending {
            return None;
        }
        self.host.control().work()
    }
    /// Copy lifecycle status without entering an audio owner.
    pub fn status(&mut self, now_ms: u64) -> LifecycleStatus {
        self.host.control().status(now_ms)
    }
    /// Confirm old producer/callback detachment after the adapter's redirect/stop/drain handshake.
    pub fn detached(&mut self, generation: u64) -> bool {
        self.host.control().mark_detached(generation).is_ok()
    }
    /// Reclaim completed telemetry and safely detached generation state only on control.
    pub fn reclaim(&mut self) -> bool {
        self.control.telemetry.reclaim().for_each(drop);
        let released = self.host.reclaim();
        if self.host.control().status(0).retiring.is_none() {
            self.retired_control = None;
        }
        released
    }
    /// Discard queued radio digits and invalidate the partial command after admission loss.
    pub fn discard_digits(&mut self) {
        self.control.digits.discard();
        self.links.lost_digits();
    }
    /// Drain a bounded raw-event snapshot. The host applies each policy result
    /// before interpreting the next event, so remote selection cannot lag digits.
    pub fn drain_digits(&mut self) -> Vec<DigitEvent> {
        let generation = self.control.digits.generation();
        let mut output = Vec::new();
        let count = self.control.digits.queued();
        for _ in 0..count.saturating_add(1) {
            let Some(event) = self.control.digits.next(generation) else {
                break;
            };
            output.push(event);
        }
        output
    }
}

struct Candidate<A: Send, C: Send> {
    scope: Option<crate::config::resources::candidate::ResourceId>,
    commands: Option<command_dispatch::PreparedCommands>,
    admin: Option<Arc<super::dtmf::AdminCodes>>,
    dtmf_macros: Vec<crate::dtmf_macro::CompiledDtmfMacro>,
    dtmf_macro_limits: crate::dtmf_macro::DtmfMacroLimits,
    remote_commands: Option<crate::remote_base_command::RemoteCommandConfiguration>,
    name: String,
    settings: ResolvedNodeSettings,
    site: ResolvedSiteSettings,
    bounds: GenerationSettings,
    generation: RuntimeGeneration<DtmfWorker, RuntimeTransmit<A>>,
    control: ControlState<C>,
    status: ResolvedIdentifierSettings,
    time_format: u64,
    station_changed: bool,
    echo_telemetry: crate::link::EchoTelemetryPolicy,
    echo_announcement: crate::config::EchoLinkAnnouncement,
    links: LinkScheduler,
    device: Option<Box<dyn DeviceHandoff>>,
}

/// Disconnect copies share cancellation until every listener reclaims its media.
struct DisconnectNotice {
    first: String,
    second: String,
    protocol: crate::link::ProtocolId,
    cancellation: Weak<AtomicBool>,
}

/// Multi-node runtime owned exclusively by the serialized control executor.
/// Failed preparation retains the document, nodes, schedule, device leases and current revision.
pub struct Runtime<A: Send = (), C: Send = ()> {
    revision: u64,
    generation: u64,
    modular: Option<Arc<RuntimeConfiguration>>,
    shared_site: Option<modular::SharedSite>,
    configuration_gate: Arc<AtomicBool>,
    nodes: Vec<RuntimeNode<A, C>>,
    retired: Vec<RuntimeNode<A, C>>,
    effects: Vec<(String, LinkEffect)>,
    detaching: Vec<(String, String)>,
    disconnect_notices: Vec<DisconnectNotice>,
    stopping: bool,
    pending_reload: Option<ReloadTransaction<A, C>>,
}
impl<A: Send, C: Send> Runtime<A, C> {
    /// Start with no selected configuration, active generation or radio.
    /// The first successful adoption uses the ordinary runtime publication path.
    pub fn idle() -> Self {
        Self {
            revision: 0,
            generation: 0,
            modular: None,
            shared_site: None,
            configuration_gate: Arc::new(AtomicBool::new(false)),
            nodes: Vec::new(),
            retired: Vec::new(),
            effects: Vec::new(),
            detaching: Vec::new(),
            disconnect_notices: Vec::new(),
            stopping: false,
            pending_reload: None,
        }
    }

    /// Share this aggregate's fail-closed transmit interlock with its service owner.
    /// Site policy retains an independent gate for each station.
    pub fn configuration_gate(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.configuration_gate)
    }

    /// Current successful configuration revision, unchanged by any failed candidate.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Publish fresh link endpoints without resetting controller timing, playback, RF policy,
    /// or the receive detector/collector. A protected callback rejects without mutation; retry
    /// on control after that bounded callback leaves. Old endpoints still require detachment.
    pub fn replace_adapter(
        &mut self,
        local: &str,
        prepared: PreparedAdapter<A, C>,
        now_ms: u64,
    ) -> Result<u64, RuntimeError> {
        self.reclaim();
        if self.stopping || self.reload_pending() {
            return Err(RuntimeError::Busy);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(RuntimeError::Preparation)?;
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        if node.status(now_ms).retiring.is_some()
            || prepared.receive_maximum != node.bounds.receive_maximum
            || prepared.transmit_maximum != node.bounds.transmit_maximum
        {
            return Err(RuntimeError::Busy);
        }
        // Empty valid placeholders are never published to callbacks. They retain the
        // previous generation's adapter until explicit external drain/detachment.
        let (controller, telemetry) = NodeController::new(
            Default::default(),
            Vec::new(),
            Vec::new(),
            Default::default(),
        )
        .map_err(|_| RuntimeError::Preparation)?;
        let activity = controller.activity();
        let (receive, digits) = DtmfWorker::new(generation, node.settings.dtmf_muting)
            .map_err(|_| RuntimeError::Preparation)?;
        let candidate = RuntimeGeneration::prepare(
            generation,
            node.bounds.clone(),
            receive,
            RuntimeTransmit {
                controller,
                adapter: prepared.state,
                settings: node.settings.clone(),
            },
        )
        .map_err(|_| RuntimeError::Preparation)?;
        node.host
            .control()
            .publish_transferring(candidate, now_ms, |old_rx, old_tx, new_rx, new_tx| {
                std::mem::swap(old_rx, new_rx);
                std::mem::swap(&mut old_tx.controller, &mut new_tx.controller);
            })
            .map_err(|_| RuntimeError::Busy)?;
        let mut next = ControlState {
            generation,
            adapter: prepared.control,
            adapter_exposed: false,
            telemetry,
            media_session: None,
            digits,
            activity,
            started_ms: now_ms,
            prior_activity_ms: None,
            catalog: Arc::clone(&node.control.catalog),
            messages: super::composition::MessageRecipes::default(),
        };
        std::mem::swap(&mut next.telemetry, &mut node.control.telemetry);
        std::mem::swap(&mut next.media_session, &mut node.control.media_session);
        std::mem::swap(&mut next.messages, &mut node.control.messages);
        std::mem::swap(&mut next.digits, &mut node.control.digits);
        std::mem::swap(&mut next.activity, &mut node.control.activity);
        std::mem::swap(&mut next.started_ms, &mut node.control.started_ms);
        std::mem::swap(
            &mut next.prior_activity_ms,
            &mut node.control.prior_activity_ms,
        );
        node.retired_control = Some(std::mem::replace(&mut node.control, next));

        self.generation = generation;
        Ok(generation)
    }
    /// Enabled nodes in configuration order.
    pub fn node_names(&self) -> Vec<&str> {
        self.nodes.iter().map(|node| node.name.as_str()).collect()
    }
    /// Resolve the single running port which owns an incoming ASL account.
    pub fn network_owner(&self, account: &str) -> Option<&str> {
        if account.is_empty() || !account.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let account = account.trim_start_matches('0');
        let mut owners = self.nodes.iter().filter(|node| {
            node.network_account()
                .is_some_and(|wire| wire.trim_start_matches('0') == account)
        });
        let owner = owners.next()?;
        owners.next().is_none().then_some(owner.name.as_str())
    }
    /// Exact current settings lookup, excluding removed/disabled nodes.
    pub fn settings(&self, name: &str) -> Option<&ResolvedNodeSettings> {
        self.nodes
            .iter()
            .find(|node| node.name == name)
            .map(|node| &node.settings)
    }
    /// Exact serialized node lookup.
    pub fn node(&mut self, name: &str) -> Option<&mut RuntimeNode<A, C>> {
        self.nodes.iter_mut().find(|node| node.name == name)
    }
    /// Borrow current/retiring dispatcher state even after its node was removed from configuration.
    /// Stop/drain its external users before acknowledging this generation with `detached`.
    pub fn adapter_control_mut(&mut self, local: &str, generation: u64) -> Option<&mut C> {
        self.nodes
            .iter_mut()
            .chain(&mut self.retired)
            .find(|node| node.name == local)?
            .adapter_control_mut(generation)
    }
    /// Drain physical detach consequences before performing replacement attach effects.
    pub fn take_effects(&mut self) -> Vec<(String, LinkEffect)> {
        let effects = std::mem::take(&mut self.effects);
        for (local, effect) in &effects {
            self.track_detach(local, effect);
        }
        effects
    }
    fn track_detach(&mut self, local: &str, effect: &LinkEffect) {
        if let LinkEffect::Detach(peers) | LinkEffect::PrivateDetach { remotes: peers, .. } = effect
        {
            for remote in peers {
                let key = (local.to_owned(), remote.clone());
                if !self.detaching.contains(&key) {
                    self.detaching.push(key);
                }
            }
        }
    }
    /// Confirm a peer reader/channel has stopped before permitting any replacement attachment.
    pub fn peer_detached(&mut self, local: &str, remote: &str, now_ms: u64) {
        self.detaching
            .retain(|(owner, peer)| owner != local || peer != remote);
        if let Some(node) = self
            .nodes
            .iter_mut()
            .chain(&mut self.retired)
            .find(|node| node.name == local)
        {
            node.links.reclaimed(remote, now_ms);
        }
    }
    /// Snapshot active and retained removed nodes, including stalled teardown.
    pub fn status(&mut self, now_ms: u64) -> Vec<(String, LifecycleStatus)> {
        self.nodes
            .iter_mut()
            .chain(&mut self.retired)
            .map(|node| (node.name.clone(), node.status(now_ms)))
            .collect()
    }

    fn next_configuration(&mut self, clock: RuntimeClock) -> Result<(u64, u64), RuntimeError> {
        self.reclaim();
        if self.stopping || self.pending_reload.is_some() || !self.retired.is_empty() {
            return Err(RuntimeError::Busy);
        }
        // A failed handoff can stop an incumbent permanently. Keep it reachable
        // through physical stop and detachment, then prepare a fresh owner.
        self.nodes.retain_mut(|node| {
            node.device_open || node.status(clock.now_ms).state != LifecycleState::Stopped
        });
        if self.nodes.iter_mut().any(|node| {
            let status = node.status(clock.now_ms);
            status.active.is_none() || status.retiring.is_some()
        }) {
            return Err(RuntimeError::Busy);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(RuntimeError::Preparation)?;
        let generation_id = self
            .generation
            .checked_add(1)
            .ok_or(RuntimeError::Preparation)?;
        Ok((revision, generation_id))
    }

    /// A prepared configuration still owns opening or rolling-back leases.
    pub fn reload_pending(&self) -> bool {
        self.pending_reload.is_some()
    }

    /// Advance every device without waiting for its physical worker. A failed
    /// candidate remains retained until rollback has safely completed.
    pub fn poll_reload(&mut self, clock: RuntimeClock) -> Result<bool, RuntimeError> {
        let Some(mut transaction) = self.pending_reload.take() else {
            return Ok(true);
        };
        match transaction.advance(self, clock) {
            Ok(false) => {
                self.pending_reload = Some(transaction);
                Ok(false)
            }
            Ok(true) => self.commit_reload(transaction, clock).map(|()| true),
            Err(error) => Err(error),
        }
    }

    fn commit_reload(
        &mut self,
        transaction: ReloadTransaction<A, C>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let ReloadTransaction {
            configuration,
            site,
            candidates,
            revision,
            generation_id,
            updated,
            ..
        } = transaction;
        // No fallible preparation remains: external registration is still gated to these stable hosts.
        let mut next = Vec::with_capacity(candidates.len());
        for mut candidate in candidates {
            let command_owner = candidate.commands.take().expect("prepared command owner");
            let policy = AccessPolicy::new(
                &candidate.settings.link_allow_nodes,
                &candidate.settings.link_deny_nodes,
            )
            .expect("validated policy");
            let commands =
                crate::command::DtmfCommandMap::new(Vec::new()).expect("empty command map");
            let previous_activity = if let Some(index) = self
                .nodes
                .iter()
                .position(|node| node.name == candidate.name)
            {
                let mut old = self.nodes.remove(index);
                let prior = old
                    .host
                    .control()
                    .status(clock.now_ms)
                    .active
                    .expect("active node");
                let handoff = old.bounds.device != candidate.bounds.device
                    || (candidate.station_changed && !updated.contains(&candidate.name));
                let removed = old.links.withdraw_removed(&candidate.links);
                if !removed.is_empty() {
                    self.effects
                        .push((old.name.clone(), LinkEffect::Detach(removed)));
                }
                // Seed retained protection before a callback can acquire the replacement.
                old.site.reconfigure(candidate.site);
                candidate.control.telemetry.rx_mute().store(
                    old.control.telemetry.rx_mute().load(Ordering::Acquire),
                    Ordering::Release,
                );
                candidate.control.telemetry.set_site_inhibit(
                    old.failed_station_replacement || old.site.transmit_inhibited(),
                );
                old.host
                    .control()
                    .publish_prepared(candidate.generation, clock.now_ms)
                    .expect("prevalidated serialized publication");
                old.handoff_pending = false;
                if handoff && !old.control.adapter_exposed {
                    old.host
                        .control()
                        .mark_detached(prior)
                        .expect("retired handoff");
                }
                let activity = old.event_activity();
                candidate.control.prior_activity_ms = old.control.activity_ms();
                old.retired_control = Some(std::mem::replace(&mut old.control, candidate.control));
                old.programs.generation = None;
                old.links
                    .reconfigure(policy, commands, Some(candidate.links));
                old.links
                    .bind_admin(candidate.admin, candidate.settings.dtmf_admin_timeout_ms);
                old.links
                    .configure_radio_owner(candidate.remote_commands.as_ref())
                    .expect("prevalidated remote command family");
                old.links
                    .configure_macros(candidate.dtmf_macros, candidate.dtmf_macro_limits)
                    .expect("prevalidated macro namespace");
                old.settings = candidate.settings;
                old.bounds = candidate.bounds;
                old.status = candidate.status;
                old.time_format = candidate.time_format;
                old.echo_telemetry = candidate.echo_telemetry;
                old.echo_announcement = candidate.echo_announcement;
                next.push(old);
                Some(activity)
            } else {
                let mut links =
                    NodeLinkControl::new(&candidate.name, policy, commands, Some(candidate.links))
                        .expect("validated node identity");
                links.bind_admin(candidate.admin, candidate.settings.dtmf_admin_timeout_ms);
                links
                    .configure_radio_owner(candidate.remote_commands.as_ref())
                    .expect("prevalidated remote command family");
                links
                    .configure_macros(candidate.dtmf_macros, candidate.dtmf_macro_limits)
                    .expect("prevalidated macro namespace");
                next.push(RuntimeNode {
                    programs: Default::default(),
                    command_messages: None,
                    name: candidate.name,
                    site: crate::site::SiteController::new(candidate.site),
                    settings: candidate.settings,
                    bounds: candidate.bounds,
                    host: NodeHost::new(candidate.generation),
                    device: candidate.device.expect("prepared new lease"),
                    device_open: true,
                    handoff_pending: false,
                    failed_station_replacement: false,
                    pending_peer_cleanup: false,
                    links,
                    control: candidate.control,
                    retired_control: None,
                    status: candidate.status,
                    time_format: candidate.time_format,
                    group_announcements: Vec::new(),
                    echo_telemetry: candidate.echo_telemetry,
                    echo_announcement: candidate.echo_announcement,
                });
                None
            };
            let node = next.last_mut().expect("published candidate");
            let activity = previous_activity.unwrap_or_else(|| node.event_activity());
            command_owner.install(node, clock, activity);
        }
        for mut removed in self.nodes.drain(..) {
            stop_node(&mut removed, clock.now_ms, &mut self.effects);
            self.retired.push(removed);
        }
        self.nodes = next;
        self.publish_site(site, generation_id);
        self.modular = Some(configuration);
        self.revision = revision;
        self.generation = generation_id;
        self.tick_links(clock);
        self.reclaim();
        Ok(())
    }

    /// Capture current receive-only activity and reconcile window policy before reserving work.
    pub fn tick_links(&mut self, clock: RuntimeClock) {
        for node in &mut self.nodes {
            node.control.telemetry.reclaim().for_each(drop);
            if node.control.telemetry.parrot_enabled() {
                while let Some(clip) = node.control.telemetry.take_parrot_capture() {
                    if node.control.activity.is_active() {
                        node.control.telemetry.recycle_parrot_capture(clip);
                        continue;
                    }
                    let queued = ParrotLevels::measure(clip.samples())
                        .and_then(|levels| {
                            node.control
                                .catalog
                                .format(&Message::ParrotLevels {
                                    peak_dbfs: levels.peak_dbfs,
                                    rms_dbfs: levels.rms_dbfs,
                                })
                                .ok()
                        })
                        .and_then(|forms| {
                            PreparedAudio::new(48_000, clip.samples().to_vec())
                                .ok()
                                .and_then(|recording| {
                                    prepare::streamed_parrot(
                                        node.control.media_session.as_deref_mut()?,
                                        &node.status,
                                        &forms.tts,
                                        recording,
                                    )
                                    .ok()
                                })
                        })
                        .is_some_and(|prepared| {
                            node.control
                                .telemetry
                                .queue_captured_parrot(&clip, prepared)
                        });
                    let _ = queued;
                    node.control.telemetry.recycle_parrot_capture(clip);
                }
            }
            let Some((civil, second)) = clock.civil.filter(|(_, second)| *second < 60) else {
                continue;
            };
            let activity = node.control.activity_ms();
            let warning = node.links.tick_with_warnings(
                civil,
                second,
                clock.now_ms,
                |_| activity,
                node.control.activity.is_active(),
                node.control.telemetry.can_queue_status(),
            );
            if let Some(warning) = warning.filter(|warning| clock.now_ms < warning.deadline_ms) {
                let seconds = warning.remaining_ms.div_ceil(1000);
                let target_node = warning.target_is_node.then_some(warning.target.as_str());
                let target = MessageForms {
                    text: warning.target.clone(),
                    tts: render::telemetry_speech(&warning.target, target_node.as_slice()),
                    morse: warning.target.clone(),
                };
                let forms = node.control.catalog.format_schedule_warning(
                    seconds,
                    civil,
                    &target,
                    warning.connecting,
                );
                let queued = forms
                    .map_err(|_| RuntimeError::Preparation)
                    .and_then(|forms| {
                        queue_status_media(
                            &mut node.control.telemetry,
                            &mut node.control.media_session,
                            &node.status,
                            &forms.morse,
                            &forms.tts,
                            None,
                            None,
                        )
                    });
                let _ = queued;
            }
        }
    }
    /// Reserve the next configured-route operation only after all earlier detach effects drained.
    pub fn next_link(&mut self) -> Option<(String, LinkEffect)> {
        if !self.effects.is_empty() || !self.detaching.is_empty() {
            return None;
        }
        let mut selected = None;
        for node in &mut self.nodes {
            let Some(work) = node.work() else {
                continue;
            };
            if let Some(effect) = node.links.next_scheduled(work) {
                selected = Some((node.name.clone(), effect));
                break;
            }
        }
        if let Some((local, effect)) = &selected {
            self.track_detach(local, effect);
        }
        selected
    }
    /// Prepare pending status, ID, and tail media using the current control snapshot.
    pub fn prepare_messages(&mut self, clock: RuntimeClock) {
        for node in &mut self.nodes {
            node.control.telemetry.reclaim().for_each(drop);
            let Some(first) = node.control.telemetry.take_message_request() else {
                continue;
            };
            let mut values = node.message_values(clock, &node.control.catalog);
            let mut first = Some(first);
            // At most one visit per configured recipe per pump. Fast failed retries
            // cannot monopolize the host's other control work.
            for _ in 0..node.control.messages.items.len() {
                let Some(current) = first
                    .take()
                    .or_else(|| node.control.telemetry.take_message_request())
                else {
                    break;
                };
                let prepared = (|| {
                    let values = values
                        .as_mut()
                        .map_err(|_| crate::controller::ControllerError)?;
                    values.event = node
                        .control
                        .telemetry
                        .message_event(current.attempt)
                        .cloned();
                    let recipe = node
                        .control
                        .messages
                        .items
                        .get(current.content_index)
                        .ok_or(crate::controller::ControllerError)?;
                    let program = recipe
                        .prepare(&node.control.catalog, values)
                        .map_err(|_| crate::controller::ControllerError)?;
                    let stream = node
                        .control
                        .media_session
                        .as_mut()
                        .ok_or(crate::controller::ControllerError)?
                        .register_program(program, true)
                        .map_err(|_| crate::controller::ControllerError)?;
                    crate::controller::PreparedMedia::new_stream(stream)
                })();
                // Both preparation failure and success settle the exact request.
                // A rejected stale envelope is destroyed here on control.
                let _ = node
                    .control
                    .telemetry
                    .complete_message(current.attempt, prepared);
            }
        }
    }

    /// Queue a compiled named local notification; false means retry admission, not an action retry.
    pub fn queue_named_message(&mut self, local: &str, name: &str) -> Result<bool, RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let index = *node
            .control
            .messages
            .names
            .get(name)
            .ok_or(RuntimeError::Preparation)?;
        Self::queue_local_notification(node, index)
    }

    /// Queue the alarm transition's compiled recipe for local playback.
    pub fn queue_alarm_message(
        &mut self,
        local: &str,
        alarm: &str,
        event: crate::site::AlarmEvent,
    ) -> Result<bool, RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        let index = *node
            .control
            .messages
            .alarm_messages
            .get(&(alarm.to_owned(), event.message_field()))
            .ok_or(RuntimeError::Preparation)?;
        Self::queue_local_notification(node, index)
    }

    fn queue_local_notification(
        node: &mut RuntimeNode<A, C>,
        index: usize,
    ) -> Result<bool, RuntimeError> {
        node.control.telemetry.reclaim().for_each(drop);
        if !node.control.telemetry.can_queue_status() {
            return Ok(false);
        }
        Self::queue_routed_message(
            node,
            index,
            None,
            Some(crate::controller::PlaybackDestination::Local),
        )?;
        Ok(true)
    }

    /// Admit a read-only site report, retaining the exact requesting connection.
    pub fn queue_site_report(
        &mut self,
        local: &str,
        selector: &str,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        if selector.is_empty() || !crate::config::message_selector_valid(selector) {
            return Err(RuntimeError::Rejected);
        }
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let index = node
            .control
            .messages
            .site_selectors
            .get(selector)
            .copied()
            .unwrap_or(node.control.messages.site_unavailable);
        Self::queue_message_index(node, index, reply)
    }

    /// Admit a read-only macro to the existing reply queue; resolve values after source unkey.
    /// A full queue rejects before creating an attempt, so a later retry uses fresh values.
    pub fn queue_message(
        &mut self,
        local: &str,
        selector: &str,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let index = *node
            .control
            .messages
            .selectors
            .get(selector)
            .ok_or(RuntimeError::Rejected)?;
        Self::queue_message_index(node, index, reply)
    }
    fn queue_message_index(
        node: &mut RuntimeNode<A, C>,
        index: usize,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        Self::queue_routed_message(node, index, reply, None)
    }
    fn queue_routed_message(
        node: &mut RuntimeNode<A, C>,
        index: usize,
        reply: Option<crate::link::PeerReply>,
        destination: Option<crate::controller::PlaybackDestination>,
    ) -> Result<(), RuntimeError> {
        node.control.telemetry.reclaim().for_each(drop);
        if reply.as_ref().is_some_and(crate::link::PeerReply::ended)
            || !node.control.telemetry.can_queue_status()
        {
            return Err(RuntimeError::Rejected);
        }
        let event = reply
            .as_ref()
            .and_then(|reply| node.command_message_event(reply));
        let mut prepared = node
            .control
            .telemetry
            .prepare_message(index, event)
            .map_err(|_| RuntimeError::Preparation)?;
        if let Some((mut policy, delivery)) = node.control.messages.items[index].playback() {
            if let Some(destination) = destination {
                policy.destination = destination;
            }
            let deadline = node
                .control
                .activity
                .current_sample()
                .saturating_add(delivery.maximum_wait_ms.saturating_mul(48));
            prepared = prepared
                .with_playback_policy(policy)
                .with_delivery(deadline, delivery)
                .wait_for_unkey(reply);
        } else if let Some(destination) = destination {
            prepared = prepared
                .with_playback_policy(crate::controller::PlaybackPolicy {
                    destination,
                    ..Default::default()
                })
                .wait_for_unkey(reply);
        } else {
            prepared = prepared.reply_to(reply);
        }
        // Only this serialized producer can consume the capacity admitted above.
        assert!(node.control.telemetry.queue_prepared_status(prepared));
        Ok(())
    }

    /// Cancel only the active message on the current generation's audio owner.
    pub fn cancel_message(&mut self, local: &str) -> Result<(), RuntimeError> {
        self.nodes
            .iter()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?
            .control
            .telemetry
            .cancel_message();
        Ok(())
    }

    /// Queue existing time/direct/full/last-keyed telemetry, with speech failure falling back to Morse.
    pub fn queue_status(
        &mut self,
        local: &str,
        action: LinkAction,
        last_keyed: Option<&str>,
        clock: RuntimeClock,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        let (text, spoken) = self.status_forms(local, action, last_keyed, clock)?;
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        queue_status_media(
            &mut node.control.telemetry,
            &mut node.control.media_session,
            &node.status,
            &text,
            &spoken,
            reply,
            None,
        )
    }
    /// Prepare the same status producer for an authenticated call's private sink.
    /// No controller telemetry queue, radio demand, or peer audio attachment is touched.
    pub fn prepare_phone_status(
        &mut self,
        local: &str,
        action: LinkAction,
        last_keyed: Option<&str>,
        clock: RuntimeClock,
        reply: &crate::link::PeerReply,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, RuntimeError> {
        if reply.phone_source().is_none() || !reply.phone_authenticated() {
            return Err(RuntimeError::Rejected);
        }
        let (text, spoken) = self.status_forms(local, action, last_keyed, clock)?;
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        prepare::status_stream(
            node.control
                .media_session
                .as_mut()
                .ok_or(RuntimeError::Preparation)?
                .as_mut(),
            &node.status,
            &text,
            &spoken,
        )
    }
    /// Queue a current local station dial failure under the originating command's actual permit.
    pub fn queue_phone_dial_failure(
        &mut self,
        local: &str,
        status: u32,
        current: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), RuntimeError> {
        use crate::{
            audio::{PcmRead, PcmStreamReader},
            controller::{PlaybackPolicy, PreparedMedia},
        };
        struct Spoken<F> {
            stream: Box<dyn PcmStreamReader>,
            current: F,
        }
        impl<F: Fn() -> bool + Send> PcmStreamReader for Spoken<F> {
            fn start(&mut self) {
                self.stream.start();
            }
            fn rewind(&mut self) {
                self.stream.rewind();
            }
            fn defer_before_output(&mut self) -> bool {
                self.stream.defer_before_output()
            }
            fn cancel(&mut self) {
                self.stream.cancel();
            }
            fn render(&mut self, output: &mut [f32]) -> PcmRead {
                if !(self.current)() {
                    self.stream.cancel();
                    return PcmRead::Finished;
                }
                let result = self.stream.render(output);
                if (self.current)() {
                    result
                } else {
                    output.fill(0.0);
                    self.stream.cancel();
                    PcmRead::Finished
                }
            }
        }
        if !current() {
            return Err(RuntimeError::Rejected);
        }
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        node.control.telemetry.reclaim().for_each(drop);
        if !node.control.telemetry.can_queue_status() {
            return Err(RuntimeError::Busy);
        }
        let forms = node
            .control
            .catalog
            .format(&Message::PhoneDialFailure { status })
            .map_err(|_| RuntimeError::Preparation)?;
        let mut source = prepare::status_source(&node.status, &forms.morse, &forms.tts);
        source.morse = None;
        let stream = node
            .control
            .media_session
            .as_mut()
            .ok_or(RuntimeError::Unavailable)?
            .register_once(source)
            .map_err(|_| RuntimeError::Preparation)?;
        let media = PreparedMedia::new_stream(Box::new(Spoken { stream, current }))
            .map_err(|_| RuntimeError::Preparation)?
            .with_playback_policy(PlaybackPolicy::default());
        node.control
            .telemetry
            .queue_prepared_status(media)
            .then_some(())
            .ok_or(RuntimeError::Busy)
    }

    /// Prepare credential-free authentication wording for a live private Phone token.
    /// The caller must retain its exact call owner and queue this stream only to
    /// that call. Login deliberately needs no prior mailbox authentication.
    pub fn prepare_private_phone_prompt(
        &mut self,
        local: &str,
        prompt: crate::messages::PhoneAuthenticationPrompt,
        reply: &crate::link::PeerReply,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, RuntimeError> {
        if reply.phone_source().is_none() || reply.ended() {
            return Err(RuntimeError::Rejected);
        }
        self.prepare_private_phone_message(local, Message::PhoneAuthentication { prompt })
    }

    fn prepare_private_phone_message(
        &mut self,
        local: &str,
        message: Message<'_>,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let forms = node
            .control
            .catalog
            .format(&message)
            .map_err(|_| RuntimeError::Preparation)?;
        prepare::status_stream(
            node.control
                .media_session
                .as_mut()
                .ok_or(RuntimeError::Preparation)?
                .as_mut(),
            &node.status,
            &forms.morse,
            &forms.tts,
        )
    }
    /// Prepare a fixed outcome for the authenticated private recipient only.
    pub fn prepare_phone_operation_result(
        &mut self,
        local: &str,
        result: crate::messages::OperationResult,
        reply: &crate::link::PeerReply,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, RuntimeError> {
        if reply.phone_source().is_none() || !reply.phone_authenticated() {
            return Err(RuntimeError::Rejected);
        }
        self.prepare_private_phone_message(local, Message::OperationResult { result })
    }

    fn status_forms(
        &self,
        local: &str,
        action: LinkAction,
        last_keyed: Option<&str>,
        clock: RuntimeClock,
    ) -> Result<(String, String), RuntimeError> {
        let node = self
            .nodes
            .iter()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let (text, spoken) = match action {
            LinkAction::Time => {
                let (civil, _) = clock.civil.ok_or(RuntimeError::Clock)?;
                let (_, _, _, _, hour, minute) = civil.components();
                let time = TimeAnnouncement::format(
                    hour.into(),
                    minute.into(),
                    TimeFormat::try_from(node.time_format)
                        .map_err(|_| RuntimeError::Preparation)?,
                    &node.control.catalog,
                )
                .map_err(|_| RuntimeError::Preparation)?;
                (time.morse().to_owned(), time.speech().to_owned())
            }
            LinkAction::LastKeyed => {
                let message = match last_keyed {
                    Some(peer) => Message::LastKeyed { peer },
                    None => Message::LastKeyedNone,
                };
                let forms = node
                    .control
                    .catalog
                    .format(&message)
                    .map_err(|_| RuntimeError::Preparation)?;
                let spoken = render::telemetry_speech(
                    &forms.tts,
                    &last_keyed.into_iter().collect::<Vec<_>>(),
                );
                (forms.morse, spoken)
            }
            _ => {
                let (forms, identity) =
                    render::direct_status(node.links.manager(), &node.control.catalog)?;
                let spoken = render::telemetry_speech(&forms.tts, &[&identity]);
                (forms.morse, spoken)
            }
        };
        Ok((text, spoken))
    }
    /// Queue one bounded pad report on the local transmitter after receiver unkey.
    pub fn queue_pad_test(
        &mut self,
        local: &str,
        report: &super::dtmf::PadTestResult,
    ) -> Result<(), RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let forms = node
            .control
            .catalog
            .format_pad_test(&report.digits, report.overflowed)
            .map_err(|_| RuntimeError::Preparation)?;
        queue_status_media(
            &mut node.control.telemetry,
            &mut node.control.media_session,
            &node.status,
            &forms.morse,
            &forms.tts,
            None,
            None,
        )
    }
    /// Queue a topology-loop explanation to the issuing RF or peer endpoint.
    pub fn queue_loop_rejected(
        &mut self,
        local: &str,
        peer: &str,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        self.queue_link_rejection(local, peer, reply, Message::LoopRejected { node: peer })
    }
    /// Explain a stopped macro only to the issuing RF or exact peer endpoint.
    pub fn queue_macro_stopped(
        &mut self,
        local: &str,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        self.queue_link_rejection(local, "", reply, Message::DtmfMacroStopped)
    }
    /// Explain an unsupported remote-command route to the issuing endpoint.
    pub fn queue_remote_command_unsupported(
        &mut self,
        local: &str,
        peer: &str,
        reply: Option<crate::link::PeerReply>,
    ) -> Result<(), RuntimeError> {
        self.queue_link_rejection(
            local,
            peer,
            reply,
            Message::RemoteCommandUnsupported { node: peer },
        )
    }
    fn queue_link_rejection(
        &mut self,
        local: &str,
        peer: &str,
        reply: Option<crate::link::PeerReply>,
        message: Message<'_>,
    ) -> Result<(), RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == local)
            .ok_or(RuntimeError::MissingNode)?;
        let forms = node
            .control
            .catalog
            .format(&message)
            .map_err(|_| RuntimeError::Preparation)?;
        let spoken = render::telemetry_speech(&forms.tts, &[peer]);
        queue_status_media(
            &mut node.control.telemetry,
            &mut node.control.media_session,
            &node.status,
            &forms.morse,
            &spoken,
            reply,
            None,
        )
    }
    /// Queue one direct-link lifecycle event from every configured node's perspective.
    ///
    /// Each node is attempted independently so one unavailable speech provider or full
    /// telemetry queue cannot suppress the announcement on another node.
    /// Reconnection cancels prior disconnect notices for the same unordered pair
    /// before attempting preparation or bounded admission of the new connect notice.
    pub fn queue_link_event(
        &mut self,
        first: &str,
        second: &str,
        connected: bool,
    ) -> Result<(), RuntimeError> {
        self.queue_link_event_with_echo(first, second, connected, None)
    }
    /// Use the owning QSO's validated callsign and full real ID, while retaining
    /// ordinary local/third-party wording, politeness and reconnect cancellation.
    pub fn queue_echo_link_event(
        &mut self,
        first: &str,
        second: &str,
        callsign: &str,
        connected: bool,
    ) -> Result<(), RuntimeError> {
        let node = crate::link::EchoAlias::parse(second)
            .map_err(|_| RuntimeError::Rejected)?
            .to_string();
        self.queue_link_event_with_echo(first, second, connected, Some((&node, callsign)))
    }
    /// Invalidate a superseded disconnect notice for a caller's privately confirmed connect.
    pub fn cancel_link_disconnect_notice(
        &mut self,
        first: &str,
        second: &str,
        protocol: crate::link::ProtocolId,
    ) {
        let (first, _, _) = self.link_notice_endpoint(first, protocol);
        let (second, _, _) = self.link_notice_endpoint(second, protocol);
        let (lower, upper) = if first <= second {
            (first.as_str(), second.as_str())
        } else {
            (second.as_str(), first.as_str())
        };
        self.prune_disconnect_notices(Some((lower, upper, protocol)));
    }

    fn prune_disconnect_notices(
        &mut self,
        connected: Option<(&str, &str, crate::link::ProtocolId)>,
    ) {
        self.disconnect_notices.retain(|notice| {
            let Some(cancellation) = notice.cancellation.upgrade() else {
                return false;
            };
            if connected
                == Some((
                    notice.first.as_str(),
                    notice.second.as_str(),
                    notice.protocol,
                ))
            {
                cancellation.store(true, Ordering::Release);
                false
            } else {
                true
            }
        });
    }

    // Pair cancellation and listener ownership use stable IDs. Only actual wire identities
    // participate in group membership or receive node-number spelling.
    fn link_notice_endpoint(
        &self,
        endpoint: &str,
        protocol: crate::link::ProtocolId,
    ) -> (String, String, bool) {
        let asl = protocol == crate::link::ProtocolId::AllStarLink;
        let slot = if asl {
            self.network_owner(endpoint).unwrap_or(endpoint)
        } else {
            endpoint
        };
        let Some(node) = self.nodes.iter().find(|node| node.name == slot) else {
            return (endpoint.to_owned(), endpoint.to_owned(), true);
        };
        if asl && let Some(number) = node.network_account() {
            return (slot.to_owned(), number.to_owned(), true);
        }
        let port = self
            .modular_configuration()
            .and_then(|config| config.protocols.port(slot.parse().ok()?));
        let display = port
            .map(|p| {
                if p.metadata.name.is_empty() {
                    p.key.as_str()
                } else {
                    &p.metadata.name
                }
            })
            .unwrap_or(endpoint);
        (slot.to_owned(), display.to_owned(), false)
    }
    fn queue_link_event_with_echo(
        &mut self,
        first: &str,
        second: &str,
        connected: bool,
        echo: Option<(&str, &str)>,
    ) -> Result<(), RuntimeError> {
        let protocol = if echo.is_some() {
            crate::link::ProtocolId::EchoLink
        } else {
            crate::link::ProtocolId::AllStarLink
        };
        let (first_id, first_label, first_wire) = self.link_notice_endpoint(first, protocol);
        let (second_id, second_label, second_wire) = self.link_notice_endpoint(second, protocol);
        let (lower, upper) = if first_id <= second_id {
            (first_id.as_str(), second_id.as_str())
        } else {
            (second_id.as_str(), first_id.as_str())
        };
        let (first, second) = (first_label.as_str(), second_label.as_str());
        self.prune_disconnect_notices(connected.then_some((lower, upper, protocol)));
        let cancellation = (!connected).then(|| Arc::new(AtomicBool::new(false)));
        if let Some(cancellation) = &cancellation {
            self.disconnect_notices.push(DisconnectNotice {
                first: lower.to_owned(),
                second: upper.to_owned(),
                protocol,
                cancellation: Arc::downgrade(cancellation),
            });
        }
        let mut failure = None;
        for node in &mut self.nodes {
            if (node.name == first_id
                && second_wire
                && node.links.suppresses_member_notice(second, connected))
                || (node.name == second_id
                    && first_wire
                    && node.links.suppresses_member_notice(first, connected))
            {
                continue;
            }
            let message = if node.name == first_id {
                if connected {
                    Message::PeerConnectedLocal { peer: second }
                } else {
                    Message::PeerDisconnectedLocal { peer: second }
                }
            } else if node.name == second_id {
                if connected {
                    Message::PeerConnectedLocal { peer: first }
                } else {
                    Message::PeerDisconnectedLocal { peer: first }
                }
            } else if connected {
                Message::PeerConnectedThirdParty { first, second }
            } else {
                Message::PeerDisconnectedThirdParty { first, second }
            };
            let forms = if let Some((number, callsign)) = echo {
                node.control.catalog.format_echo_peer(
                    node.echo_announcement,
                    number,
                    callsign,
                    &message,
                )
            } else {
                node.control.catalog.format(&message).map(Some)
            };
            let queued = forms
                .map_err(|_| RuntimeError::Preparation)
                .and_then(|forms| {
                    let Some(forms) = forms else { return Ok(()) };
                    let spoken = render::telemetry_speech(
                        &forms.tts,
                        &[
                            if first_wire { first } else { "" },
                            if second_wire { second } else { "" },
                        ],
                    );
                    queue_status_media(
                        &mut node.control.telemetry,
                        &mut node.control.media_session,
                        &node.status,
                        &forms.morse,
                        &spoken,
                        None,
                        cancellation.clone(),
                    )
                });
            if let Err(error) = queued {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Queue only a group's first active selection and transitions to complete unavailability.
    /// Member connects, recovery attempts, and later winner changes stay silent.
    pub fn queue_priority_group_events(&mut self) -> Result<(), RuntimeError> {
        let mut failure = None;
        for node in &mut self.nodes {
            let groups = node.links.priority_groups();
            node.group_announcements.retain_mut(|previous| {
                let current = groups.iter().find(|group| {
                    group.label == previous.label && same_group_configuration(Some(previous), group)
                });
                if current.is_none_or(|group| !group.unavailable)
                    && let Some(notice) = previous.outage_notice.take()
                {
                    notice.store(true, Ordering::Release);
                }
                current.is_some()
            });
            for group in groups {
                let previous = node
                    .group_announcements
                    .iter()
                    .find(|previous| previous.label == group.label);
                let same_configuration = previous.is_some();
                let previously_available =
                    previous.is_some_and(|previous| previous.outage_notice.is_none());
                let message = if group.selected.is_some() && !same_configuration {
                    Some(Message::PriorityGroupSelected {
                        group: &group.name,
                        peer: group.selected.as_deref().expect("selected group member"),
                    })
                } else if announce_unavailable_group(
                    group.unavailable,
                    same_configuration,
                    previously_available,
                ) {
                    Some(Message::PriorityGroupUnavailable { group: &group.name })
                } else {
                    None
                };
                let mut outage_notice =
                    previous.and_then(|previous| previous.outage_notice.clone());
                if let Some(message) = message {
                    outage_notice = group.unavailable.then(|| Arc::new(AtomicBool::new(false)));
                    let queued = node
                        .control
                        .catalog
                        .format(&message)
                        .map_err(|_| RuntimeError::Preparation)
                        .and_then(|forms| {
                            let spoken =
                                group.selected.as_deref().map_or(forms.tts.clone(), |peer| {
                                    render::telemetry_speech(&forms.tts, &[peer])
                                });
                            queue_status_media(
                                &mut node.control.telemetry,
                                &mut node.control.media_session,
                                &node.status,
                                &forms.morse,
                                &spoken,
                                None,
                                outage_notice.clone(),
                            )
                        });
                    if let Err(error) = queued {
                        failure = Some(error);
                        continue;
                    }
                } else if group.selected.is_none() && !group.unavailable {
                    continue;
                }
                let state = GroupAnnouncement {
                    label: group.label,
                    name: group.name,
                    members: group.members,
                    outage_notice,
                };
                if let Some(previous) = node
                    .group_announcements
                    .iter_mut()
                    .find(|previous| previous.label == state.label)
                {
                    *previous = state;
                } else {
                    node.group_announcements.push(state);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Process one copied local event on the same serialized owner as reload and incoming admission.
    pub fn remote_command_authorized(
        &mut self,
        local: &str,
        revision: u64,
        operation: &DigitOperation,
        now_ms: u64,
    ) -> bool {
        !self.stopping
            && self.revision == revision
            && self
                .node(local)
                .is_some_and(|node| node.links.remote_authorized(operation, now_ms))
    }
    /// Renew only the exact initiating source after an applied administrative request.
    pub fn complete_remote_command(
        &mut self,
        local: &str,
        operation: &DigitOperation,
        now_ms: u64,
    ) {
        if !operation.audible_source()
            || !operation
                .remote
                .is_some_and(|request| request.administrative())
        {
            return;
        }
        if let Some(node) = self.node(local) {
            if let Some(reply) = &operation.reply {
                node.links.confirm_peer_administrative_command(
                    operation.origin.as_deref(),
                    reply,
                    now_ms,
                );
            } else {
                node.links.confirm_administrative_command(now_ms);
            }
        }
    }
    /// Prepare one source-routed response through the existing streamed telemetry path.
    /// IDs and arguments are selected by the trusted presenter, never a radio packet.
    pub fn queue_remote_reply(
        &mut self,
        local: &str,
        parts: &[crate::remote_base_command::RemoteReplyPart],
        reply: Option<crate::link::PeerReply>,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        let mut speech = String::new();
        let mut morse = String::new();
        for part in parts {
            let forms = node
                .control
                .catalog
                .format_named(part.id, &part.arguments)
                .map_err(|_| RuntimeError::Preparation)?;
            if !speech.is_empty() {
                speech.push(' ');
                morse.push(' ');
            }
            speech.push_str(&forms.tts);
            morse.push_str(&forms.morse);
        }
        if let Some(reply) = &reply {
            node.echo_telemetry.requested(reply, now_ms);
        }
        queue_status_media(
            &mut node.control.telemetry,
            &mut node.control.media_session,
            &node.status,
            &morse,
            &speech,
            reply,
            None,
        )
    }
    /// Process one copied local event on the same serialized owner as reload and incoming admission.
    pub fn digit(&mut self, local: &str, event: DigitEvent) -> Option<DigitOutcome> {
        self.node(local)?.links.digit(event)
    }
    /// Discard queued digits and partial commands on every active node after admission loss.
    pub fn discard_digits(&mut self) {
        for node in &mut self.nodes {
            node.discard_digits();
        }
    }
    /// Read complete topology for the existing full-status operator diagnostic, not RF playback.
    pub fn topology(&self, local: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|node| node.name == local)
            .map(|node| node.links.manager().full_topology())
    }
    /// Apply a copied CLI/DTMF operation with current-generation ownership and clock revalidation.
    pub fn command(
        &mut self,
        local: &str,
        operation: DigitOperation,
        clock: RuntimeClock,
        directory_verified: bool,
    ) -> Result<LinkEffect, RuntimeError> {
        if operation.command.action == LinkAction::ReconnectAll {
            let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
            if node
                .links
                .schedule()
                .is_some_and(LinkScheduler::requires_civil_time)
                && clock.civil.is_none_or(|(_, second)| second >= 60)
            {
                return Err(RuntimeError::Clock);
            }
            self.tick_links(clock);
        }
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        let work = match &operation.action_permit {
            Some(permit) => permit.generation.reserve(),
            None => node.work().ok_or(RuntimeError::Rejected)?,
        };
        let reply = operation.reply.clone();
        let origin = operation.origin.clone();
        let mut effect = node
            .links
            .command(operation, work, clock.now_ms, directory_verified)
            .map_err(RuntimeError::Link)?;
        if let LinkEffect::EchoTelemetry(mode) = &effect {
            if !node.echo_telemetry.select(*mode, clock.now_ms) {
                return Err(RuntimeError::Rejected);
            }
            if let Some(reply) = &reply {
                node.links.confirm_peer_administrative_command(
                    origin.as_deref(),
                    reply,
                    clock.now_ms,
                );
            } else {
                node.links.confirm_administrative_command(clock.now_ms);
            }
            effect = LinkEffect::None;
        }
        if matches!(
            &effect,
            LinkEffect::Telemetry(..) | LinkEffect::Message { .. } | LinkEffect::SiteStatus { .. }
        ) {
            if let Some(reply) = &reply {
                node.echo_telemetry.requested(reply, clock.now_ms);
            }
        }
        if let LinkEffect::ParrotEnabled(enabled) = &effect {
            if !node.control.telemetry.set_parrot_enabled(*enabled) {
                return Err(RuntimeError::Rejected);
            }
            if let Some(reply) = &reply {
                node.links.confirm_peer_administrative_command(
                    origin.as_deref(),
                    reply,
                    clock.now_ms,
                );
            } else {
                node.links.confirm_administrative_command(clock.now_ms);
            }
        }
        self.track_detach(local, &effect);
        Ok(effect)
    }
    /// Admit a verified incoming peer on the same owner as reload and queued command work.
    pub fn incoming(
        &mut self,
        local: &str,
        remote: &str,
        verified: bool,
    ) -> Result<(), RuntimeError> {
        self.node(local)
            .ok_or(RuntimeError::MissingNode)?
            .links
            .accept(remote, verified)
            .map_err(RuntimeError::Link)
    }
    /// Trusted typed control selection; DTMF reaches this policy only after the
    /// source collector's administrative grant and mapping checks have passed.
    pub fn set_echo_telemetry(
        &mut self,
        local: &str,
        mode: crate::config::EchoLinkTelemetry,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        self.node(local)
            .ok_or(RuntimeError::MissingNode)?
            .echo_telemetry
            .select(mode, now_ms)
            .then_some(())
            .ok_or(RuntimeError::Rejected)
    }
    /// Open only an explicitly prepared command-response window for this QSO.
    pub fn echo_response_requested(
        &mut self,
        local: &str,
        reply: &crate::link::PeerReply,
        now_ms: u64,
    ) {
        if let Some(node) = self.node(local) {
            node.echo_telemetry.requested(reply, now_ms);
        }
    }
    /// Additional eligibility beneath existing source/audience and polite rules.
    /// Local telemetry is enabled in this controller; follow cannot broaden it.
    pub fn echo_telemetry_allows(
        &mut self,
        local: &str,
        reply: &crate::link::PeerReply,
        now_ms: u64,
    ) -> bool {
        self.node(local)
            .is_some_and(|node| node.echo_telemetry.allows(reply, now_ms, true))
    }
    /// Check current inbound policy and topology without publishing a peer.
    pub fn authorize_incoming(
        &mut self,
        local: &str,
        remote: &str,
        verified: bool,
    ) -> Result<(), RuntimeError> {
        self.node(local)
            .ok_or(RuntimeError::MissingNode)?
            .links
            .authorize(remote, verified)
            .map_err(RuntimeError::Link)
    }
    /// Revalidate a slow dial against the current node, generation, topology, access and clock.
    pub fn finish_connect(
        &mut self,
        local: &str,
        attempt: super::links::ConnectAttempt,
        answered: bool,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        let activity = node.control.activity_ms();
        node.links
            .finish_connect_at(attempt, answered, clock.now_ms, clock.civil, |_| activity)
            .map_err(RuntimeError::Link)
    }
    /// Reclaim only detached generations; removed nodes remain addressable for detachment acknowledgments.
    pub fn reclaim(&mut self) {
        for node in self.nodes.iter_mut().chain(&mut self.retired) {
            node.reclaim();
        }
        self.retired
            .retain_mut(|node| node.status(0).state != LifecycleState::Stopped);
    }
    /// Acknowledge external producer/channel cleanup for active or removed nodes.
    pub fn detached(&mut self, local: &str, generation: u64) -> bool {
        self.nodes
            .iter_mut()
            .chain(&mut self.retired)
            .find(|node| node.name == local)
            .is_some_and(|node| node.detached(generation))
    }
    /// Gate all new work, stop devices and emit peer teardown effects; return true only when reclaimed.
    pub fn stop(&mut self, now_ms: u64) -> bool {
        self.stopping = true;
        self.shared_site = None;
        if self.pending_reload.is_some() {
            for node in &mut self.nodes {
                node.host.control().stop(now_ms).ok();
            }
            // Rollback becomes cleanup-only while stopping; retain all indices
            // and callback owners until the replacement is physically safe.
            let _ = self.poll_reload(RuntimeClock {
                now_ms,
                wall_seconds: 0,
                wall_subsecond_ms: 0,
                civil: None,
            });
            if self.pending_reload.is_some() {
                return false;
            }
        }
        for mut node in self.nodes.drain(..) {
            stop_node(&mut node, now_ms, &mut self.effects);
            self.retired.push(node);
        }
        for node in &mut self.retired {
            if node.device_open {
                stop_node(node, now_ms, &mut self.effects);
            }
        }
        self.reclaim();
        self.retired.is_empty()
    }
}
impl<A: Send, C: Send> Drop for Runtime<A, C> {
    fn drop(&mut self) {
        self.stop(0);
        if let Some(transaction) = self.pending_reload.take() {
            // As with unreclaimed generations below, an uncooperative external
            // owner cannot authorize freeing contexts still used by callbacks.
            std::mem::forget(transaction);
            for node in self.nodes.drain(..) {
                std::mem::forget(node);
            }
        }
        for node in self.retired.drain(..) {
            std::mem::forget(node);
        }
    }
}

fn stop_node<A: Send, C: Send>(
    node: &mut RuntimeNode<A, C>,
    now_ms: u64,
    effects: &mut Vec<(String, LinkEffect)>,
) {
    node.programs = Default::default();
    let before = node.status(now_ms);
    node.host.control().stop(now_ms).ok();
    let peers = node.links.close();
    let has_peers = !peers.is_empty();
    node.pending_peer_cleanup |= has_peers;
    if has_peers {
        effects.push((node.name.clone(), LinkEffect::Detach(peers)));
    }
    if node.device_open && node.device.quiesce() {
        node.device.close();
        node.device_open = false;
    }
    if !node.device_open
        && !node.pending_peer_cleanup
        && !node.control.adapter_exposed
        && node
            .retired_control
            .as_ref()
            .is_none_or(|control| !control.adapter_exposed)
    {
        for id in [before.active, before.retiring].into_iter().flatten() {
            node.detached(id);
        }
    }
}

#[cfg(test)]
mod group_announcement_tests {
    use super::*;

    fn group(
        name: &str,
        members: &[&str],
        unavailable: bool,
    ) -> crate::runtime::links::PriorityGroupStatus {
        crate::runtime::links::PriorityGroupStatus {
            label: "network".into(),
            name: name.into(),
            members: members.iter().map(|member| (*member).into()).collect(),
            selected: None,
            unavailable,
        }
    }

    #[test]
    fn group_change_and_outage_rules_cover_first_changed_recovered_and_repeated_states() {
        let current = group("Blind Hams", &["2000", "3000"], true);
        assert!(!same_group_configuration(None, &current));
        let previous = GroupAnnouncement {
            label: "network".into(),
            name: "Blind Hams".into(),
            members: vec!["2000".into(), "3000".into()],
            outage_notice: None,
        };
        assert!(same_group_configuration(Some(&previous), &current));

        let renamed = group("Old Network", &["2000", "3000"], true);
        assert!(!same_group_configuration(Some(&previous), &renamed));
        let changed_members = group("Blind Hams", &["2000", "4000"], true);
        assert!(!same_group_configuration(Some(&previous), &changed_members));

        assert!(announce_unavailable_group(true, false, false));
        assert!(announce_unavailable_group(true, true, true));
        assert!(!announce_unavailable_group(true, true, false));
        assert!(!announce_unavailable_group(false, false, false));
    }
}
