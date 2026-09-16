//! Adapter-neutral product runtime: prepare all resources, then publish one control revision.

use super::{
    DeviceHandoff, GenerationSettings, GenerationWork, LifecycleState, LifecycleStatus, NodeHost,
    OwnedReceiveOwner, OwnedTransmitOwner, RuntimeGeneration,
    dtmf::{DigitEvent, DigitOperation, DtmfDispatcher, DtmfWorker},
    link_schedule::{LinkScheduler, ReplacementSpec, RouteSpec},
    links::{LinkEffect, NodeLinkControl},
    prepare::{self, NativeMediaPreparer, RuntimeError},
    render,
    scheduler::{DispatchContent, EventScheduler, EventSpec, ScheduledDispatch, SchedulerError},
};
use crate::{
    access::AccessPolicy,
    command::{Command, LinkAction},
    config::{
        ConfigDocument, ConfigWarning, NodeId, ResolvedEventSettings, ResolvedIdentifierSettings,
        ResolvedMacroSettings, ResolvedNodeSettings, ResolvedPermanentLinkSettings,
        ResolvedScheduleSettings, ResolvedTemplateSettings, ResolvedTimeSettings, Schema,
    },
    controller::{ActivitySnapshot, ControllerControl, NodeController},
    schedule::{CivilTime, ScheduledWindow},
    template::MessageTemplate,
    time::{TimeAnnouncement, TimeFormat},
};

/// One externally captured tick; core never reads host clocks or time-zone services.
#[derive(Clone, Copy)]
pub struct RuntimeClock {
    /// Monotonic milliseconds.
    pub now_ms: u64,
    /// Original ticker wall-clock seconds, preserved through queue delay.
    pub wall_seconds: i64,
    /// Validated local calendar and second; unavailable clocks cannot select replacement windows.
    pub civil: Option<(CivilTime, u8)>,
}
/// Fixed callback bounds and candidate-owned adapter state prepared before publication.
pub struct PreparedAdapter<A: Send, C: Send = ()> {
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
    digits: DtmfDispatcher,
    activity: ActivitySnapshot,
    started_ms: u64,
    prior_activity_ms: u64,
}
impl<C: Send> ControlState<C> {
    fn activity_ms(&self) -> u64 {
        self.activity
            .last_sample()
            .map(|sample| self.started_ms.saturating_add(sample / 48))
            .unwrap_or(self.prior_activity_ms)
    }
}
/// Stable per-node control identity, retained across successful generation swaps.
pub struct RuntimeNode<A: Send, C: Send = ()> {
    name: String,
    settings: ResolvedNodeSettings,
    bounds: GenerationSettings,
    host: NodeHost<DtmfWorker, RuntimeTransmit<A>>,
    device: Box<dyn DeviceHandoff>,
    device_open: bool,
    pending_peer_cleanup: bool,
    links: NodeLinkControl,
    control: ControlState<C>,
    retired_control: Option<ControlState<C>>,
    status: ResolvedIdentifierSettings,
    time_format: u64,
}
impl<A: Send, C: Send> RuntimeNode<A, C> {
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
    /// Serialized link policy, shared by CLI, incoming admission, DTMF, and ticker work.
    pub fn links(&mut self) -> &mut NodeLinkControl {
        &mut self.links
    }
    /// Reserve current-generation ownership before submitting external work.
    pub fn work(&mut self) -> Option<GenerationWork> {
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
        self.links.digit(super::dtmf::DigitEvent::Lost);
    }
    /// Drain the bounded radio digit snapshot on control and return typed policy operations.
    pub fn drain_digits(&mut self) -> Vec<DigitOperation> {
        let generation = self.control.digits.generation();
        let mut output = Vec::new();
        let count = self.control.digits.queued();
        for _ in 0..count.saturating_add(1) {
            let Some(event) = self.control.digits.next(generation) else {
                break;
            };
            if let Some(operation) = self.links.digit(event) {
                output.push(operation);
            }
        }
        output
    }
}

struct Event {
    spec: EventSpec,
    template: Option<MessageTemplate>,
    operation: Option<Command>,
}
/// Fully copied prepared event text and its private scheduler reservation.
pub struct RuntimeDispatch {
    token: ScheduledDispatch,
    morse: Option<String>,
    speech: Option<String>,
}
impl RuntimeDispatch {
    /// Owning local node.
    pub fn local(&self) -> &str {
        self.token.local()
    }
    /// Morse-safe optional message.
    pub fn morse(&self) -> Option<&str> {
        self.morse.as_deref()
    }
    /// Expanded speech including punctuation-safe node/callsign pronunciation.
    pub fn speech(&self) -> Option<&str> {
        self.speech.as_deref()
    }
    /// Optional existing linking command, executed only after successful message admission.
    pub fn operation(&self) -> Option<&Command> {
        self.token.content.operation.as_ref()
    }
}

struct Candidate<A: Send, C: Send> {
    name: String,
    settings: ResolvedNodeSettings,
    bounds: GenerationSettings,
    generation: RuntimeGeneration<DtmfWorker, RuntimeTransmit<A>>,
    control: ControlState<C>,
    status: ResolvedIdentifierSettings,
    time_format: u64,
    links: LinkScheduler,
    device: Option<Box<dyn DeviceHandoff>>,
}

/// Multi-node runtime owned exclusively by the serialized control executor.
/// Failed preparation retains the document, nodes, schedule, device leases and current revision.
pub struct Runtime<A: Send = (), C: Send = ()> {
    revision: u64,
    generation: u64,
    document: ConfigDocument,
    warnings: Vec<ConfigWarning>,
    nodes: Vec<RuntimeNode<A, C>>,
    retired: Vec<RuntimeNode<A, C>>,
    events: Vec<Event>,
    scheduler: Option<EventScheduler>,
    effects: Vec<(String, LinkEffect)>,
    detaching: Vec<(String, String)>,
    stopping: bool,
}
impl<A: Send, C: Send> Runtime<A, C> {
    /// Prepare every enabled node and schedule before opening/publishing any candidate.
    pub fn start(
        document: ConfigDocument,
        media: &dyn NativeMediaPreparer,
        adapter: impl FnMut(&str, &ResolvedNodeSettings) -> Result<PreparedAdapter<A, C>, RuntimeError>,
        device: impl FnMut(&str, &ResolvedNodeSettings) -> Result<Box<dyn DeviceHandoff>, RuntimeError>,
        clock: RuntimeClock,
    ) -> Result<Self, RuntimeError> {
        let mut runtime = Self {
            revision: 0,
            generation: 0,
            document: ConfigDocument::default(),
            warnings: Vec::new(),
            nodes: Vec::new(),
            retired: Vec::new(),
            events: Vec::new(),
            scheduler: None,
            effects: Vec::new(),
            detaching: Vec::new(),
            stopping: false,
        };
        runtime.reload(document, media, adapter, device, clock)?;
        Ok(runtime)
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
        if self.stopping {
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
        let (receive, digits) = DtmfWorker::new(generation, node.settings.dtmf_muting);
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
            digits,
            activity,
            started_ms: now_ms,
            prior_activity_ms: 0,
        };
        std::mem::swap(&mut next.telemetry, &mut node.control.telemetry);
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
    /// Active immutable document.
    pub fn document(&self) -> &ConfigDocument {
        &self.document
    }
    /// Recoverable schema warnings for the published document, without logging from core.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }
    /// Enabled nodes in configuration order.
    pub fn node_names(&self) -> Vec<&str> {
        self.nodes.iter().map(|node| node.name.as_str()).collect()
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
        if let LinkEffect::Detach(peers) = effect {
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

    /// Fully prepare a candidate, switch changed devices with rollback, then publish all nodes.
    /// The adapter must redirect old generation endpoints and report detachment before another reload.
    pub fn reload(
        &mut self,
        document: ConfigDocument,
        media: &dyn NativeMediaPreparer,
        mut adapter: impl FnMut(
            &str,
            &ResolvedNodeSettings,
        ) -> Result<PreparedAdapter<A, C>, RuntimeError>,
        mut device: impl FnMut(
            &str,
            &ResolvedNodeSettings,
        ) -> Result<Box<dyn DeviceHandoff>, RuntimeError>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        self.reclaim();
        if self.stopping
            || !self.retired.is_empty()
            || self
                .nodes
                .iter_mut()
                .any(|node| node.status(clock.now_ms).retiring.is_some())
        {
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
        let warnings = Schema::validate(&document)?.warnings;
        let mut candidates = Vec::new();
        let mut events = Vec::new();
        for name in document.nodes() {
            let id = NodeId::new(name)?;
            let settings = ResolvedNodeSettings::resolve(&document, &id)?.value;
            if !settings.enabled {
                continue;
            }
            let existing = self.nodes.iter().find(|node| node.name == name);
            let links = prepare_links(
                &document,
                &id,
                revision,
                existing.and_then(|node| node.links.schedule()),
            )?;
            if links.requires_civil_time() && clock.civil.is_none_or(|(_, second)| second >= 60) {
                return Err(RuntimeError::Clock);
            }
            let (controller, telemetry, status) =
                prepare::controller(&document, &id, &settings, media)?;
            let activity = controller.activity();
            let (receive, digits) = DtmfWorker::new(generation_id, settings.dtmf_muting);
            let prepared = adapter(name, &settings)?;
            let bounds = GenerationSettings {
                node: name.into(),
                device: settings.channel.clone(),
                receive_maximum: prepared.receive_maximum,
                transmit_maximum: prepared.transmit_maximum,
            };
            let generation = RuntimeGeneration::prepare(
                generation_id,
                bounds.clone(),
                receive,
                RuntimeTransmit {
                    controller,
                    adapter: prepared.state,
                    settings: settings.clone(),
                },
            )
            .map_err(|_| RuntimeError::Preparation)?;
            let fresh_device = if existing.is_none() {
                Some(device(name, &settings)?)
            } else {
                None
            };
            events.extend(prepare_events(&document, &id)?);
            let control = ControlState {
                generation: generation_id,
                adapter: prepared.control,
                adapter_exposed: false,
                telemetry,
                digits,
                activity,
                started_ms: clock.now_ms,
                prior_activity_ms: existing.map_or(0, |node| node.control.activity_ms()),
            };
            candidates.push(Candidate {
                name: name.into(),
                settings,
                bounds,
                generation,
                control,
                status,
                time_format: ResolvedTimeSettings::resolve(&document, &id)?.value.format,
                links,
                device: fresh_device,
            });
        }
        let scheduler = EventScheduler::new(
            revision,
            events.iter().map(|event| event.spec.clone()).collect(),
            self.scheduler.as_ref(),
        )
        .map_err(|_| RuntimeError::Preparation)?;
        // Validate every remaining fallible policy constructor before touching physical leases.
        for candidate in &candidates {
            crate::link::LinkManager::new(&candidate.name).map_err(RuntimeError::Link)?;
            candidate
                .settings
                .command_map()
                .map_err(|_| RuntimeError::Preparation)?;
            AccessPolicy::new(
                &candidate.settings.link_allow_nodes,
                &candidate.settings.link_deny_nodes,
            )
            .map_err(|_| RuntimeError::Preparation)?;
        }
        let mut changed = Vec::new();
        let mut opened = Vec::new();
        for (index, candidate) in candidates.iter_mut().enumerate() {
            if let Some(old_index) = self
                .nodes
                .iter()
                .position(|node| node.name == candidate.name)
            {
                let old = &mut self.nodes[old_index];
                if old.bounds.device == candidate.bounds.device {
                    continue;
                }
                let status = old.status(clock.now_ms);
                if status.outstanding_work != 0 || !old.device.quiesce() {
                    self.rollback_devices(&changed, &mut candidates, &opened, clock.now_ms)?;
                    return Err(RuntimeError::Busy);
                }
                old.host.control().gate_callbacks();
                if old.status(clock.now_ms).protected_owners != 0 {
                    old.host.control().restore_callbacks();
                    self.rollback_devices(&changed, &mut candidates, &opened, clock.now_ms)?;
                    return Err(RuntimeError::Busy);
                }
                old.device.close();
                old.device_open = false;
                changed.push(old_index);
                if !old.device.open(&candidate.bounds) {
                    self.rollback_devices(&changed, &mut candidates, &opened, clock.now_ms)?;
                    return Err(RuntimeError::Device);
                }
                old.device_open = true;
            } else {
                let lease = candidate.device.as_mut().ok_or(RuntimeError::Preparation)?;
                if !lease.open(&candidate.bounds) {
                    self.rollback_devices(&changed, &mut candidates, &opened, clock.now_ms)?;
                    return Err(RuntimeError::Device);
                }
                opened.push(index);
            }
        }
        // No fallible preparation remains: external registration is still gated to these stable hosts.
        let mut next = Vec::with_capacity(candidates.len());
        for mut candidate in candidates {
            let policy = AccessPolicy::new(
                &candidate.settings.link_allow_nodes,
                &candidate.settings.link_deny_nodes,
            )
            .expect("validated policy");
            let commands = candidate
                .settings
                .command_map()
                .expect("validated commands");
            if let Some(index) = self
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
                let handoff = old.bounds.device != candidate.bounds.device;
                let removed = old.links.withdraw_removed(&candidate.links);
                if !removed.is_empty() {
                    self.effects
                        .push((old.name.clone(), LinkEffect::Detach(removed)));
                }
                old.host
                    .control()
                    .publish_prepared(candidate.generation, clock.now_ms)
                    .expect("prevalidated serialized publication");
                if handoff && !old.control.adapter_exposed {
                    old.host
                        .control()
                        .mark_detached(prior)
                        .expect("retired handoff");
                }
                candidate.control.prior_activity_ms = old.control.activity_ms();
                old.retired_control = Some(std::mem::replace(&mut old.control, candidate.control));
                old.links
                    .reconfigure(policy, commands, Some(candidate.links));
                old.settings = candidate.settings;
                old.bounds = candidate.bounds;
                old.status = candidate.status;
                old.time_format = candidate.time_format;
                next.push(old);
            } else {
                let links =
                    NodeLinkControl::new(&candidate.name, policy, commands, Some(candidate.links))
                        .expect("validated node identity");
                next.push(RuntimeNode {
                    name: candidate.name,
                    settings: candidate.settings,
                    bounds: candidate.bounds,
                    host: NodeHost::new(candidate.generation),
                    device: candidate.device.expect("prepared new lease"),
                    device_open: true,
                    pending_peer_cleanup: false,
                    links,
                    control: candidate.control,
                    retired_control: None,
                    status: candidate.status,
                    time_format: candidate.time_format,
                });
            }
        }
        for mut removed in self.nodes.drain(..) {
            stop_node(&mut removed, clock.now_ms, &mut self.effects);
            self.retired.push(removed);
        }
        self.nodes = next;
        self.events = events;
        self.scheduler = Some(scheduler);
        self.document = document;
        self.warnings = warnings;
        self.revision = revision;
        self.generation = generation_id;
        self.tick_links(clock);
        self.reclaim();
        Ok(())
    }

    fn rollback_devices(
        &mut self,
        changed: &[usize],
        candidates: &mut [Candidate<A, C>],
        opened: &[usize],
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        for &index in opened.iter().rev() {
            candidates[index].device.iter_mut().for_each(|device| {
                device.close();
            });
        }
        let mut restored = true;
        for &index in changed.iter().rev() {
            let node = &mut self.nodes[index];
            if node.device_open {
                node.device.close();
            }
            node.device_open = node.device.open(&node.bounds);
            if !node.device_open {
                node.host.control().stop(now_ms).ok();
                restored = false;
            } else {
                node.host.control().restore_callbacks();
            }
        }
        if restored {
            Ok(())
        } else {
            Err(RuntimeError::Restore)
        }
    }

    /// Capture current receive-only activity and reconcile window policy before reserving work.
    pub fn tick_links(&mut self, clock: RuntimeClock) {
        if let Some((civil, second)) = clock.civil.filter(|(_, second)| *second < 60) {
            for node in &mut self.nodes {
                let activity = node.control.activity_ms();
                node.links.tick(civil, second, clock.now_ms, |_| activity);
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
    /// Reserve the oldest due event, rendering all dynamic values from its captured trigger minute.
    pub fn next_event(
        &mut self,
        clock: RuntimeClock,
    ) -> Result<Option<RuntimeDispatch>, RuntimeError> {
        let (civil, _) = clock
            .civil
            .filter(|(_, second)| *second < 60)
            .ok_or(RuntimeError::Clock)?;
        let events = &self.events;
        let nodes = &self.nodes;
        let Some(scheduler) = &mut self.scheduler else {
            return Ok(None);
        };
        let token = scheduler
            .next(clock.wall_seconds, civil, |spec, civil| {
                let event = events
                    .iter()
                    .find(|event| event.spec == *spec)
                    .ok_or(SchedulerError::Render)?;
                let node = nodes
                    .iter()
                    .find(|node| node.name == spec.local)
                    .ok_or(SchedulerError::Render)?;
                let message = event
                    .template
                    .as_ref()
                    .map(|template| {
                        render::message(
                            template,
                            *civil,
                            node.time_format,
                            &node.name,
                            &node.settings.callsign,
                            node.links.manager(),
                        )
                    })
                    .transpose()
                    .map_err(|_| SchedulerError::Render)?;
                Ok(DispatchContent {
                    message: message.and_then(|text| render::morse(&text).map(|_| text)),
                    operation: event.operation.clone(),
                })
            })
            .map_err(|_| RuntimeError::Preparation)?;
        let Some(token) = token else {
            return Ok(None);
        };
        let node = self
            .nodes
            .iter()
            .find(|node| node.name == token.local())
            .ok_or(RuntimeError::MissingNode)?;
        let (_, peer) = render::direct_status(node.links.manager());
        let morse = token.content.message.as_deref().and_then(render::morse);
        let speech =
            token.content.message.as_deref().map(|text| {
                render::scheduled_speech(text, &node.name, &node.settings.callsign, &peer)
            });
        Ok(Some(RuntimeDispatch {
            token,
            morse,
            speech,
        }))
    }
    /// Queue a reserved event once. Stale reservations cannot synthesize or enqueue into a new node.
    pub fn queue_event(
        &mut self,
        dispatch: &RuntimeDispatch,
        media: &dyn NativeMediaPreparer,
    ) -> Result<(), RuntimeError> {
        let scheduler = self.scheduler.as_mut().ok_or(RuntimeError::Rejected)?;
        if !scheduler.current(&dispatch.token) {
            return Err(RuntimeError::Rejected);
        }
        if scheduler.is_message_queued(&dispatch.token) {
            return Ok(());
        }
        let Some(morse) = dispatch.morse() else {
            return Ok(());
        };
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.name == dispatch.local())
            .ok_or(RuntimeError::MissingNode)?;
        let audio = prepare::speech(media, &node.status, dispatch.speech().unwrap_or(""))?;
        node.control.telemetry.reclaim().for_each(drop);
        node.control
            .telemetry
            .queue_status(morse, audio)
            .map_err(|_| RuntimeError::Rejected)?;
        scheduler.message_queued(&dispatch.token);
        Ok(())
    }
    /// Settle a reservation only after its message and optional linking operation complete.
    pub fn complete_event(&mut self, dispatch: &RuntimeDispatch) -> bool {
        self.scheduler
            .as_mut()
            .is_some_and(|scheduler| scheduler.complete(&dispatch.token))
    }
    /// Queue existing time/direct/full/last-keyed telemetry, with speech failure falling back to Morse.
    pub fn queue_status(
        &mut self,
        local: &str,
        action: LinkAction,
        last_keyed: Option<&str>,
        clock: RuntimeClock,
        media: &dyn NativeMediaPreparer,
    ) -> Result<(), RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        let (text, spoken) = match action {
            LinkAction::Time => {
                let (civil, _) = clock.civil.ok_or(RuntimeError::Clock)?;
                let (_, _, _, _, hour, minute) = civil.components();
                let time = TimeAnnouncement::format(
                    hour.into(),
                    minute.into(),
                    TimeFormat::try_from(node.time_format)
                        .map_err(|_| RuntimeError::Preparation)?,
                )
                .map_err(|_| RuntimeError::Preparation)?;
                (time.morse().to_owned(), time.speech().to_owned())
            }
            LinkAction::LastKeyed => {
                let text = last_keyed.map_or_else(
                    || "NO LAST KEYED".into(),
                    |peer| format!("LAST KEYED {peer}"),
                );
                let spoken =
                    render::telemetry_speech(&text, &last_keyed.into_iter().collect::<Vec<_>>());
                (text, spoken)
            }
            _ => {
                let (text, identity) = render::direct_status(node.links.manager());
                let spoken = render::telemetry_speech(&text, &[&identity]);
                (text, spoken)
            }
        };
        let audio = prepare::speech(media, &node.status, &spoken)?;
        node.control.telemetry.reclaim().for_each(drop);
        node.control
            .telemetry
            .queue_status(&text, audio)
            .map_err(|_| RuntimeError::Rejected)
    }
    /// Process one copied local event on the same serialized owner as reload and incoming admission.
    pub fn digit(&mut self, local: &str, event: DigitEvent) -> Option<DigitOperation> {
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
        let work = node.work().ok_or(RuntimeError::Rejected)?;
        let effect = node
            .links
            .command(operation, work, clock.now_ms, directory_verified)
            .map_err(RuntimeError::Link)?;
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
    /// Execute a still-current event macro only after its optional message was admitted.
    pub fn event_command(
        &mut self,
        dispatch: &RuntimeDispatch,
        clock: RuntimeClock,
    ) -> Result<LinkEffect, RuntimeError> {
        let scheduler = self.scheduler.as_mut().ok_or(RuntimeError::Rejected)?;
        if !scheduler.current(&dispatch.token)
            || (dispatch.morse.is_some() && !scheduler.is_message_queued(&dispatch.token))
        {
            return Err(RuntimeError::Rejected);
        }
        let Some(command) = dispatch.operation().cloned() else {
            return Ok(LinkEffect::None);
        };
        self.command(
            dispatch.local(),
            DigitOperation {
                command,
                digit: None,
            },
            clock,
            false,
        )
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
        self.scheduler = None;
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
        if !node.pending_peer_cleanup
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
}

fn prepare_events(document: &ConfigDocument, node: &NodeId) -> Result<Vec<Event>, RuntimeError> {
    let mut events = Vec::new();
    for label in prepare::labels(document, "event", node.as_str()) {
        let event = ResolvedEventSettings::resolve(document, node, label)?.value;
        let text = if !event.message.is_empty() {
            Some(event.message)
        } else if !event.template.is_empty() {
            Some(
                ResolvedTemplateSettings::resolve(document, Some(node), &event.template)?
                    .value
                    .text,
            )
        } else {
            None
        };
        let template = text
            .map(|text| MessageTemplate::parse(&text).map_err(|_| RuntimeError::Preparation))
            .transpose()?;
        let operation = if event.macro_name.is_empty() {
            None
        } else {
            let settings =
                ResolvedMacroSettings::resolve(document, Some(node), &event.macro_name)?.value;
            Some(Command {
                action: match settings.action() {
                    crate::schedule::ScheduledAction::Connect => LinkAction::Transceive,
                    crate::schedule::ScheduledAction::Disconnect => LinkAction::Disconnect,
                    crate::schedule::ScheduledAction::DisconnectAll => LinkAction::DisconnectAll,
                    crate::schedule::ScheduledAction::ReconnectAll => LinkAction::ReconnectAll,
                },
                node: settings.target_node,
            })
        };
        events.push(Event {
            spec: EventSpec {
                local: node.as_str().into(),
                name: label.into(),
                trigger: event.at.parse().map_err(|_| RuntimeError::Preparation)?,
            },
            template,
            operation,
        });
    }
    Ok(events)
}
fn prepare_links(
    document: &ConfigDocument,
    node: &NodeId,
    generation: u64,
    previous: Option<&LinkScheduler>,
) -> Result<LinkScheduler, RuntimeError> {
    let mut routes = Vec::new();
    let mut permanent = Vec::new();
    let mut windows = Vec::new();
    for label in prepare::labels(document, "permanent", node.as_str()) {
        let settings = ResolvedPermanentLinkSettings::resolve(document, node, label)?.value;
        permanent.push((label.to_owned(), routes.len()));
        routes.push(RouteSpec {
            local: node.as_str().into(),
            remote: settings.remote_node,
            permanent: true,
        });
    }
    for label in prepare::labels(document, "schedule", node.as_str()) {
        let settings = ResolvedScheduleSettings::resolve(document, node, label)?.value;
        let replaced = permanent
            .iter()
            .find(|(label, _)| label == &settings.replace_permanent)
            .map(|(_, index)| *index)
            .ok_or(RuntimeError::Preparation)?;
        // Schema validation rejects duplicate remotes across permanent links and windows.
        let route = routes.len();
        routes.push(RouteSpec {
            local: node.as_str().into(),
            remote: settings.remote_node,
            permanent: false,
        });
        windows.push(ReplacementSpec {
            route,
            replaced,
            window: ScheduledWindow::parse(
                Some(&settings.days),
                Some(&settings.dates),
                &settings.start_time,
                &settings.end_time,
            )
            .map_err(|_| RuntimeError::Preparation)?,
            end_inactivity_ms: settings.end_inactivity_ms,
        });
    }
    LinkScheduler::new(generation, routes, windows, previous).map_err(|_| RuntimeError::Preparation)
}
