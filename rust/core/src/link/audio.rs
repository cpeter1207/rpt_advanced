//! Generation-owned peer audio composition; no channel operations or locks.
use super::{AdmissionError, Mode, PeerSignals, ReceivePolicy, ReceiveState};
use crate::{
    audio::{AudioSpan, LinkAudioProducer},
    controller::{NodeController, ParrotOutput, PeerReplySample},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

/// Lock-free control/audio handoff for one immutable priority group.
#[derive(Clone, Debug, Default)]
pub struct GroupSelection {
    desired: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    revision: Arc<AtomicU64>,
}
impl GroupSelection {
    /// Create a group with no selected member.
    pub fn new() -> Self {
        Self::default()
    }
    /// Publish a one-based member slot, or clear selection when no member is reachable.
    pub fn publish_desired(&self, slot: Option<std::num::NonZeroUsize>) {
        self.desired.store(
            slot.map_or(0, std::num::NonZeroUsize::get),
            Ordering::Release,
        );
    }
    /// Current callback-active member slot, or none before initial activation.
    pub fn active(&self) -> Option<std::num::NonZeroUsize> {
        std::num::NonZeroUsize::new(self.active.load(Ordering::Acquire))
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub(crate) fn callback_slot(&self, inputs_idle: bool) -> usize {
        if inputs_idle {
            let desired = self.desired.load(Ordering::Acquire);
            let active = self.active.swap(desired, Ordering::AcqRel);
            if active != desired {
                self.revision.fetch_add(1, Ordering::Release);
            }
            desired
        } else {
            self.active.load(Ordering::Acquire)
        }
    }
}

/// A configured group identity, callback slot, and shared atomic selection.
#[derive(Clone, Debug)]
pub struct GroupMemberSelection {
    label: String,
    slot: std::num::NonZeroUsize,
    selection: GroupSelection,
}
impl GroupMemberSelection {
    /// Create one member view for a group's one-based priority slot.
    pub fn new(label: &str, slot: std::num::NonZeroUsize, selection: GroupSelection) -> Self {
        Self {
            label: label.into(),
            slot,
            selection,
        }
    }
    /// Permanent-section identity shared by all members.
    pub fn label(&self) -> &str {
        &self.label
    }
    /// One-based priority slot in the group.
    pub fn slot(&self) -> std::num::NonZeroUsize {
        self.slot
    }
    /// Shared atomic callback selection for this group.
    pub fn selection(&self) -> &GroupSelection {
        &self.selection
    }
}

/// Control-readable last-keyed direct identity for one prepared peer generation.
#[derive(Clone)]
pub struct LinkAudioStatus {
    names: Arc<[Option<String>]>,
    last_keyed: Arc<AtomicUsize>,
    dropped: Arc<AtomicU64>,
}
impl LinkAudioStatus {
    /// Newest program blocks rejected while the dispatcher held the fixed block pool.
    pub fn dropped_blocks(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    /// Coherent immutable-name/index snapshot, retained after the direct peer unkeys.
    pub fn last_keyed(&self) -> Option<&str> {
        self.last_keyed
            .load(Ordering::Acquire)
            .checked_sub(1)
            .and_then(|index| self.names.get(index))
            .and_then(Option::as_deref)
    }
}

/// Original producer coordinate, retaining negative startup and full u64 indices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceCoordinate {
    /// Original producer generation; zero is a valid generation.
    pub generation: u64,
    /// Whole source frames in the coordinate magnitude.
    pub frame: u64,
    /// Magnitude fraction, strictly below one billion.
    pub fraction_billionths: u32,
    /// Sign of the coordinate; exact zero is never negative.
    pub negative: bool,
}

/// Provenance of one actual emitted sample, independent of PCM amplitude.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SampleProvenance {
    /// None for unmapped startup, untagged input or unlocated shortfall.
    pub position: Option<SourceCoordinate>,
    /// Existing real-PCM classification, including PLC recovery blends.
    pub real: bool,
    /// A PLC request for this slot, not fresh provenance of its historical waveform.
    pub plc: bool,
    /// The mapped slot belongs to a producer-published missing interval.
    pub gap: bool,
    /// Intentional source or current DTMF muting applies to this output.
    pub muted: bool,
    /// Startup FIR history before the first admitted source sample.
    pub zero_extension: bool,
}

/// One emitted PCM sample with explicitly optional source provenance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedPeerSample {
    /// Actual rendered PCM, including silence and concealment.
    pub sample: f32,
    /// None when an intervening processor cannot prove its output provenance.
    pub provenance: Option<SampleProvenance>,
}

/// Unique adapter-owned consumer of one released inbound ring.
pub trait PeerInput: Send {
    /// Decoded source sample rate.
    fn source_rate(&self) -> u32;
    /// Immutable priming policy prepared with this unique source consumer.
    fn receive_policy(&self) -> ReceivePolicy {
        ReceivePolicy::Peer
    }
    /// Whether this input can qualify source-timed output. Individual samples
    /// can still have unmapped positions; opaque post-DSP timing remains absent.
    fn source_timing_available(&self) -> bool {
        false
    }
    /// Render one actual sample through the optional timed primitive. None
    /// reports failure or absent capability; the default consumes nothing.
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        None
    }
    /// Shared PCM-before-epoch, activity and terminal snapshots.
    fn signals(&self) -> &PeerSignals;
    /// Queued source-rate samples, excluding converter and PLC lookahead.
    fn available(&self) -> u64;
    /// An explicit finite source boundary: true retains a bounded tail, false
    /// means it finished or was cancelled. None preserves normal peer priming
    /// and shortfall grace; this is not terminal transport EOF.
    fn finite_playout(&self) -> Option<bool> {
        None
    }
    /// Supply the exact native output-clock span before rendering this peer.
    fn begin_observation_span(&mut self, _source: SourcePosition, _frames: usize) {}
    /// Render native F32, returning real samples (not concealment), or None on failure.
    fn render(&mut self, output: &mut [f32]) -> Option<usize>;
    /// Whether processed output belongs to the current operational receive epoch.
    fn receive_audio_current(&self) -> bool {
        true
    }
    /// Real samples processed by the native converter, including suppressed receive audio.
    fn native_real_count(&self, audible: usize) -> usize {
        audible
    }
    /// Advance configured silence processing independently of input admission.
    fn silence(&mut self, _scratch: &mut [f32]) {}
    /// One real media sample; unlike PCM zero, absence cannot demand PTT.
    fn render_sample(&mut self) -> Option<f32> {
        let mut sample = [0.0];
        self.render(&mut sample)
            .filter(|made| *made != 0)
            .map(|_| sample[0])
    }
}

enum AudioOrigin {
    Direct(String),
    Phone {
        call: super::PeerReply,
        isolate: bool,
        staged: Option<(Arc<AtomicBool>, bool)>,
    },
}
impl AudioOrigin {
    fn direct(&self) -> Option<&str> {
        match self {
            Self::Direct(name) => Some(name),
            Self::Phone { .. } => None,
        }
    }
    fn isolates(&self) -> bool {
        match self {
            Self::Direct(_) => false,
            Self::Phone {
                call,
                isolate,
                staged,
            } => {
                call.phone_media_allowed()
                    && staged
                        .as_ref()
                        .filter(|(permit, _)| !permit.load(Ordering::Acquire))
                        .map_or(*isolate, |(_, held)| *held)
            }
        }
    }
    fn current(&self) -> bool {
        match self {
            Self::Direct(_) => true,
            Self::Phone { call, staged, .. } => {
                call.phone_media_allowed()
                    && staged
                        .as_ref()
                        .is_none_or(|(permit, _)| permit.load(Ordering::Acquire))
            }
        }
    }
}

/// Prepared slot whose endpoints move into exactly one generation's transmit owner.
pub struct AudioPeer<P: PeerInput> {
    origin: AudioOrigin,
    mode: Mode,
    input: P,
    outbound: Option<LinkAudioProducer>,
    receive: ReceiveState,
    audio: Vec<f32>,
    active: bool,
    duration: u64,
    kerchunk_samples: u64,
    group: Option<(GroupSelection, std::num::NonZeroUsize)>,
    group_index: Option<usize>,
    reported_active: bool,
}
impl<P: PeerInput> AudioPeer<P> {
    /// Allocate scratch before publication; no endpoint is cloneable.
    pub fn new(
        direct: &str,
        mode: Mode,
        input: P,
        outbound: LinkAudioProducer,
        maximum: usize,
        kerchunk_ms: u32,
    ) -> Result<Self, AdmissionError> {
        if !super::decimal_identity(direct) {
            return Err(AdmissionError::Invalid);
        }
        Self::prepare(
            AudioOrigin::Direct(direct.into()),
            mode,
            input,
            outbound,
            maximum,
            kerchunk_ms,
        )
    }

    /// Prepare caller audio only for its exact authenticated source. A private
    /// call has no direct network-node identity or link-status announcements.
    pub fn for_phone(
        source: &super::PhoneSource,
        reply: &super::PeerReply,
        mode: Mode,
        input: P,
        outbound: LinkAudioProducer,
        maximum: usize,
    ) -> Result<Self, AdmissionError> {
        if !reply.phone_media_allowed()
            || reply.phone_source() != Some(source)
            || input.signals().reply_target().phone_source() != Some(source)
        {
            return Err(AdmissionError::Invalid);
        }
        input.signals().set_telemetry_allowed(false);
        Self::prepare(
            AudioOrigin::Phone {
                call: reply.clone(),
                isolate: false,
                staged: None,
            },
            mode,
            input,
            outbound,
            maximum,
            0,
        )
    }

    /// Keep source admission and routing tied to the candidate's native confirmation.
    /// During handoff a retained caller holds any existing or newly required isolation.
    pub fn with_phone_policy(
        mut self,
        isolate: bool,
        held_isolation: bool,
        confirmed: Arc<AtomicBool>,
    ) -> Self {
        if let AudioOrigin::Phone {
            isolate: target,
            staged,
            ..
        } = &mut self.origin
        {
            *target = isolate;
            *staged = Some((confirmed, held_isolation));
        }
        self
    }

    fn prepare(
        origin: AudioOrigin,
        mode: Mode,
        input: P,
        outbound: LinkAudioProducer,
        maximum: usize,
        kerchunk_ms: u32,
    ) -> Result<Self, AdmissionError> {
        if maximum == 0 {
            return Err(AdmissionError::Invalid);
        }
        let receive = ReceiveState::with_policy(input.source_rate(), input.receive_policy())?;
        Ok(Self {
            origin,
            mode,
            input,
            outbound: Some(outbound),
            receive,
            audio: vec![0.0; maximum],
            active: false,
            duration: 0,
            kerchunk_samples: u64::from(kerchunk_ms) * 48,
            group: None,
            group_index: None,
            reported_active: false,
        })
    }
    /// Mark this peer as a one-based member of a configured priority group.
    pub fn with_group(mut self, selection: GroupSelection, slot: std::num::NonZeroUsize) -> Self {
        self.group = Some((selection, slot));
        self
    }
    /// Mark this peer as a configured priority-group member.
    pub fn with_group_member(mut self, member: GroupMemberSelection) -> Self {
        self.group = Some((member.selection, member.slot));
        self
    }
}

/// Fixed prepared peer set retained until its generation is detached and reclaimed.
pub struct LinkAudio<P: PeerInput> {
    media_isolation: Option<Arc<super::MediaIsolation>>,
    peer_source_active: bool,
    text: Option<Box<dyn LocalTextInput>>,
    text_samples: Vec<f32>,
    peers: Vec<AudioPeer<P>>,
    local: Vec<f32>,
    mix: Vec<f32>,
    parrot_mix: Vec<f32>,
    parrot_output: Vec<f32>,
    reply_output: Vec<PeerReplySample>,
    regeneration: Option<P>,
    regeneration_samples: Vec<Option<f32>>,
    status: LinkAudioStatus,
    routing: Vec<Destination>,
    groups: Vec<GroupSelection>,
    selected_groups: Vec<usize>,
    publish: Producer<ProgramBlock>,
    recycled: Consumer<ProgramBlock>,
}

/// Prepared local incoming speech, never a peer source or telemetry command.
pub trait LocalTextInput: Send {
    /// Add available PCM without blocking/allocating; pending synthesis is inactive.
    fn render(&mut self, pcm: &mut [f32]) -> bool;
}
/// Local-only media and its playable-output demand under normal RF safety gates.
#[derive(Clone, Copy, Default)]
pub struct LocalMedia<'a> {
    /// Prepared normalized48-kHz speech; excluded from fanout/capture.
    pub samples: &'a [f32],
    /// True for playable media, not merely queued text.
    pub active: bool,
}

/// Position supplied by the existing native stream clock, not generation preparation time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourcePosition {
    /// Active immutable configuration generation.
    pub generation: u64,
    /// First native sample in this callback; advances through publication drops.
    pub sample: u64,
    /// Physical stream identity, retained across configuration-only reloads.
    pub stream: u64,
    /// Source time mapped off audio and captured once for this native block.
    pub born_ns: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Destination {
    enabled: bool,
    broadcast: bool,
    telemetry: bool,
    eligible: bool,
    target: usize,
    span: AudioSpan,
    activity_edge: u64,
    direct: bool,
}

#[derive(Debug)]
struct ProgramBlock {
    // One peer-routable composite plane followed by each destination's contribution.
    audio: Vec<f32>,
    frames: usize,
    routing: Vec<Destination>,
    replies: Vec<PeerReplySample>,
    isolation_epoch: u64,
}

/// Sole peer-audio loopback consumer and producer of all per-peer transmit queues.
/// It performs no codec/channel I/O and runs outside both radio workers.
pub struct LinkDispatcher {
    media_isolation: Option<Arc<super::MediaIsolation>>,
    incoming: Consumer<ProgramBlock>,
    recycle: Producer<ProgramBlock>,
    outbound: Vec<LinkAudioProducer>,
    scratch: Vec<f32>,
    current: Option<ProgramBlock>,
    next_destination: usize,
    activity_edges: Vec<u64>,
}

/// Work completed by one bounded fan-out pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchProgress {
    /// Destinations examined, including disabled or full peers.
    pub destinations: usize,
    /// Complete program blocks returned to the radio-side pool.
    pub completed_blocks: usize,
    /// A partial block or queued block still needs distribution.
    pub pending: bool,
}
impl LinkDispatcher {
    /// Dispatch at most `maximum_blocks`, then return every block to its radio-side pool.
    /// Only this owner writes peer queues; no block allocation or wait occurs here.
    pub fn dispatch(&mut self, maximum_blocks: usize) -> usize {
        self.dispatch_batch(maximum_blocks.saturating_mul(self.outbound.len()), |_| {})
            .completed_blocks
    }

    /// Visit at most `maximum_destinations`; retain partially dispatched blocks.
    /// Notify accepted PCM and activity edges, after their atomic publication.
    pub fn dispatch_batch(
        &mut self,
        maximum_destinations: usize,
        mut on_written: impl FnMut(usize),
    ) -> DispatchProgress {
        let mut progress = DispatchProgress::default();
        while progress.destinations < maximum_destinations {
            if self.current.is_none() {
                self.current = self.incoming.pop().ok();
                self.next_destination = 0;
            }
            let Some(block) = &self.current else {
                break;
            };
            let index = self.next_destination;
            let destination = block.routing[index];
            let mut notify = self.activity_edges[index] != destination.activity_edge;
            self.activity_edges[index] = destination.activity_edge;
            let epoch = if destination.direct {
                block.isolation_epoch
            } else {
                0
            };
            let current = !destination.direct
                || self
                    .media_isolation
                    .as_ref()
                    .is_none_or(|gate| gate.epoch() == epoch);
            if destination.eligible && current {
                let offset = (index + 1) * self.scratch.len();
                for (frame, sample) in self.scratch[..block.frames].iter_mut().enumerate() {
                    *sample = if destination.enabled {
                        block.audio[frame] - block.audio[offset + frame]
                    } else {
                        0.0
                    };
                    if destination.telemetry && destination.broadcast {
                        *sample += block.replies[frame].broadcast.unwrap_or(0.0);
                    }
                    if destination.telemetry && block.replies[frame].target == destination.target {
                        *sample += block.replies[frame].sample;
                    }
                }
                if self.outbound[index].write_at_isolation_epoch(
                    destination.span,
                    &self.scratch[..block.frames],
                    epoch,
                ) < block.frames
                {
                    notify = true;
                }
            }
            if notify {
                on_written(index);
            }
            self.next_destination += 1;
            progress.destinations += 1;
            // The two queues hold exactly two blocks in total. Owning this block
            // proves the recycle queue has a free slot, even if radio has stopped.
            if self.next_destination == self.outbound.len() {
                let _ = self
                    .recycle
                    .push(self.current.take().expect("finished block"));
                progress.completed_blocks += 1;
            }
        }
        progress.pending = self.current.is_some() || self.incoming.slots() != 0;
        progress
    }
}
impl<P: PeerInput> LinkAudio<P> {
    /// Prepare radio and dispatcher owners plus exactly two recycled peer-audio blocks.
    /// Retain the dispatcher with this generation until its producer/callbacks are detached.
    pub fn new(
        mut peers: Vec<AudioPeer<P>>,
        maximum: usize,
    ) -> Result<(Self, LinkDispatcher), AdmissionError> {
        if maximum == 0 || peers.iter().any(|peer| peer.audio.len() < maximum) {
            return Err(AdmissionError::Invalid);
        }
        let status = LinkAudioStatus {
            names: peers
                .iter()
                .map(|peer| peer.origin.direct().map(str::to_owned))
                .collect(),
            last_keyed: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let mut groups = Vec::<GroupSelection>::new();
        for peer in &mut peers {
            let Some((selection, _)) = &peer.group else {
                continue;
            };
            let index = groups
                .iter()
                .position(|group| Arc::ptr_eq(&group.active, &selection.active))
                .unwrap_or_else(|| {
                    groups.push(selection.clone());
                    groups.len() - 1
                });
            peer.group_index = Some(index);
        }
        let selected_groups = vec![0; groups.len()];
        let mut outbound = Vec::new();
        for peer in &mut peers {
            // Control-only construction consumes each freshly prepared peer exactly once.
            let producer = peer.outbound.take().expect("prepared outbound owner");
            outbound.push(producer);
        }
        let samples = maximum
            .checked_mul(peers.len() + 1)
            .ok_or(AdmissionError::Invalid)?;
        let (publish, incoming) = RingBuffer::new(2);
        let (mut recycle, recycled) = RingBuffer::new(2);
        // Control-only preparation fills the new two-slot pool before either owner runs.
        for _ in 0..2 {
            recycle
                .push(ProgramBlock {
                    audio: vec![0.0; samples],
                    frames: 0,
                    routing: vec![Destination::default(); peers.len()],
                    replies: vec![PeerReplySample::default(); maximum],
                    isolation_epoch: 0,
                })
                .expect("new loopback pool");
        }
        let dispatcher = LinkDispatcher {
            media_isolation: None,
            incoming,
            recycle,
            outbound,
            scratch: vec![0.0; maximum],
            current: None,
            next_destination: 0,
            activity_edges: vec![0; peers.len()],
        };
        Ok((
            Self {
                media_isolation: None,
                peer_source_active: false,
                text: None,
                text_samples: vec![0.0; maximum],
                routing: vec![Destination::default(); peers.len()],
                peers,
                local: vec![0.0; maximum],
                mix: vec![0.0; maximum],
                parrot_mix: vec![0.0; maximum],
                parrot_output: vec![0.0; maximum],
                reply_output: vec![PeerReplySample::default(); maximum],
                regeneration: None,
                regeneration_samples: vec![None; maximum],
                status,
                groups,
                selected_groups,
                publish,
                recycled,
            },
            dispatcher,
        ))
    }
    /// Attach a prepared finite local-only signaling consumer before publication.
    pub fn with_regeneration(mut self, input: P) -> Self {
        self.regeneration = Some(input);
        self
    }
    /// Compose local incoming speech before this generation reaches audio.
    pub fn with_text(mut self, input: Box<dyn LocalTextInput>) -> Self {
        self.text = Some(input);
        self
    }
    /// Recheck direct-peer programme at mixing and again at off-audio dispatch.
    pub fn with_media_isolation(
        mut self,
        dispatcher: &mut LinkDispatcher,
        gate: Arc<super::MediaIsolation>,
    ) -> Self {
        dispatcher.media_isolation = Some(Arc::clone(&gate));
        self.media_isolation = Some(gate);
        self
    }
    /// Clone the read-only status handle before audio ownership is published.
    pub fn status(&self) -> LinkAudioStatus {
        self.status.clone()
    }
    /// Count receive-qualified peers on the owning audio worker.
    pub fn active_count(&self) -> usize {
        self.peers.iter().filter(|peer| peer.active).count()
    }
    /// Current frame's peer/text activity before controller muting and RF admission.
    pub fn peer_source_active(&self) -> bool {
        self.peer_source_active
    }
    /// Render RF and publish one prepared pre-access-tone peer-audio block for the dispatcher.
    /// Terminal peers contribute neither PCM nor key state, even before reclamation.
    pub fn process(
        &mut self,
        controller: &mut NodeController,
        receiving: bool,
        audio: &mut [f32],
        source: SourcePosition,
    ) -> Result<bool, AdmissionError> {
        self.process_with_branches(controller, receiving, audio, source, None)
    }

    /// Render normally and copy the admitted station and transmit branches into matching
    /// callback-owned scratch spans when requested.
    pub fn process_with_branches(
        &mut self,
        controller: &mut NodeController,
        receiving: bool,
        audio: &mut [f32],
        source: SourcePosition,
        branches: Option<crate::controller::LocalTransmitBranches<'_>>,
    ) -> Result<bool, AdmissionError> {
        let count = audio.len();
        if count > self.local.len() {
            return Err(AdmissionError::Invalid);
        }
        let receiving = controller.admits_local_receive(receiving);
        let isolation_epoch = self.media_isolation.as_ref().map_or(0, |gate| gate.epoch());
        let outgoing_isolation = isolation_epoch & 1 != 0;
        self.local[..count].copy_from_slice(audio);
        let mut branches = branches.filter(|output| {
            output.station_input.len() == count
                && output.voice.len() == count
                && output.public_telemetry.len() == count
        });
        if let Some(output) = &mut branches {
            if receiving {
                output.station_input.copy_from_slice(&self.local[..count]);
            } else {
                output.station_input.fill(0.0);
            }
        }
        self.mix[..count].fill(0.0);
        for peer in &mut self.peers {
            peer.input.begin_observation_span(source, count);
            let mute_epoch = peer.input.signals().receive_epoch();
            let (pcm_epoch, available, ended) = {
                let signal = peer.input.signals();
                (
                    signal.pcm_epoch(),
                    peer.input.available(),
                    signal.ended() || !peer.origin.current(),
                )
            };
            let mut render_attempted = false;
            let audio_active = match peer.input.finite_playout() {
                Some(pending) => {
                    // An explicit finite boundary bypasses ordinary startup and
                    // PLC grace. Preserve the last real block even when rendering
                    // it also publishes the consumer's drained acknowledgement.
                    peer.receive.rendered(None, count);
                    !ended && pending && {
                        render_attempted = true;
                        peer.input
                            .render(&mut peer.audio[..count])
                            .is_some_and(|real| {
                                real != 0 || peer.input.finite_playout() == Some(true)
                            })
                    }
                }
                None => {
                    peer.receive
                        .should_render(pcm_epoch, available, ended, count)
                        && {
                            render_attempted = true;
                            let real = peer
                                .input
                                .render(&mut peer.audio[..count])
                                .map(|n| peer.input.native_real_count(n));
                            peer.receive.rendered(real, count)
                        }
                }
            };
            // Rendered PLC already advanced processing, even with no real PCM.
            if !render_attempted {
                peer.input.silence(&mut peer.audio[..count]);
            }
            let muted = (outgoing_isolation && peer.origin.direct().is_some())
                || mute_epoch & 1 != 0
                || peer.input.signals().receive_epoch() != mute_epoch;
            if !audio_active || muted {
                peer.audio[..count].fill(0.0);
            }
            let active = !ended && (audio_active || peer.input.signals().radio_keyed());
            peer.input.signals().set_active(active);
            let qualified = peer.input.signals().output_radio_keyed(mute_epoch);
            peer.active = peer.origin.current()
                && !ended
                && !muted
                && (qualified
                    || (peer.origin.direct().is_some()
                        && audio_active
                        && peer.input.receive_audio_current()));
        }
        let inputs_idle = !receiving && self.peers.iter().all(|peer| !peer.active);
        for (index, group) in self.groups.iter().enumerate() {
            self.selected_groups[index] = group.callback_slot(inputs_idle);
        }
        let selected_groups = &self.selected_groups;
        for (index, peer) in self.peers.iter_mut().enumerate() {
            let selected = peer.group_index.is_none_or(|group| {
                peer.group
                    .as_ref()
                    .is_some_and(|(_, slot)| selected_groups[group] == slot.get())
            });
            let active = peer.active && selected;
            if active {
                if !peer.reported_active
                    && let Some(direct) = peer.origin.direct()
                {
                    controller.link_keyed(direct);
                    peer.duration = 0;
                    self.status.last_keyed.store(index + 1, Ordering::Release);
                }
                peer.duration = peer.duration.saturating_add(count as u64);
            } else if peer.reported_active
                && let Some(direct) = peer.origin.direct()
            {
                let mut storage = [0; 64];
                let signal = peer.input.signals();
                let source = signal
                    .selected_source(signal.activity_edge(), &mut storage)
                    .unwrap_or(direct);
                controller.link_unkeyed(
                    direct,
                    source,
                    peer.kerchunk_samples != 0 && peer.duration <= peer.kerchunk_samples,
                );
            }
            if active != peer.reported_active
                && let AudioOrigin::Phone { call, .. } = &peer.origin
                && peer.origin.current()
                && !peer.input.signals().ended()
                && !peer.input.signals().rx_muted()
                && let Some(source) = call.phone_source()
            {
                controller.phone_voice_edge(source.ingress(), active);
            }
            peer.reported_active = active;
            if active {
                for (mixed, input) in self.mix[..count].iter_mut().zip(&peer.audio) {
                    *mixed += input;
                }
            }
        }
        let linked_active = self.peers.iter().any(|peer| {
            peer.active
                && peer.group_index.is_none_or(|group| {
                    peer.group
                        .as_ref()
                        .is_some_and(|(_, slot)| self.selected_groups[group] == slot.get())
                })
        });
        let any_peer_active = self.peers.iter().any(|peer| peer.active);
        self.parrot_mix[..count].fill(0.0);
        if receiving {
            self.parrot_mix[..count].copy_from_slice(&self.local[..count]);
        }
        for peer in &self.peers {
            if peer.active {
                for (mixed, input) in self.parrot_mix[..count]
                    .iter_mut()
                    .zip(&peer.audio[..count])
                {
                    *mixed += input;
                }
            }
        }
        for sample in &mut self.parrot_mix[..count] {
            *sample = sample.clamp(-1.0, 1.0);
        }
        controller.observe_parrot_at_epoch(
            receiving || any_peer_active,
            &self.parrot_mix[..count],
            isolation_epoch,
        );
        let mut parrot_active = false;
        for sample in &mut self.regeneration_samples[..count] {
            *sample = self
                .regeneration
                .as_mut()
                .and_then(PeerInput::render_sample);
        }
        self.text_samples[..count].fill(0.0);
        let text_active = self
            .text
            .as_mut()
            .is_some_and(|input| input.render(&mut self.text_samples[..count]));
        self.peer_source_active = any_peer_active || text_active;
        controller.set_nonlocal_source_activity(self.peer_source_active);
        let keyed = controller.process_audio_with_parrot(
            receiving,
            linked_active,
            receiving || any_peer_active,
            &self.mix[..count],
            audio,
            ParrotOutput {
                branches,
                samples: &mut self.parrot_output[..count],
                active: &mut parrot_active,
                replies: Some(&mut self.reply_output[..count]),
                regeneration: Some(&self.regeneration_samples[..count]),
                local_media: LocalMedia {
                    samples: &self.text_samples[..count],
                    active: text_active,
                },
            },
        );
        if count == 0 || self.peers.is_empty() {
            return Ok(keyed);
        }
        let isolate_peers = self.peers.iter().any(|p| p.origin.isolates());
        let is_selected = |peer: &AudioPeer<P>| {
            peer.group_index.is_none_or(|group| {
                peer.group
                    .as_ref()
                    .is_some_and(|(_, slot)| selected_groups[group] == slot.get())
            })
        };
        let mut forwarding_sources = 0;
        let mut forwarding_phone_sources = 0;
        let mut local_phone_sources = 0;
        // Reuse the consumed parrot tap for the direct sources removed from
        // public destinations during isolation.
        let direct_forwarding = &mut self.parrot_mix[..count];
        if isolate_peers {
            direct_forwarding.fill(0.0);
        }
        // The RF mix has been consumed. Reuse its scratch for callers whose
        // speech stays off ordinary public links but can reach other callers.
        let local_phone = &mut self.mix[..count];
        for peer in self.peers.iter().filter(|peer| peer.active) {
            let forwards = peer.mode.forwards();
            let direct = peer.origin.direct().is_some();
            if forwards && !direct {
                forwarding_phone_sources += 1;
            }
            if !is_selected(peer) {
                continue;
            }
            if forwards {
                forwarding_sources += 1;
                if isolate_peers && direct {
                    for (frame, sample) in direct_forwarding.iter_mut().enumerate() {
                        if !self.reply_output[frame].suppress_repeat {
                            *sample += peer.audio[frame];
                        }
                    }
                }
            } else if peer.mode.transmits() && !direct {
                if local_phone_sources == 0 {
                    local_phone.fill(0.0);
                }
                local_phone_sources += 1;
                for (frame, sample) in local_phone.iter_mut().enumerate() {
                    if !self.reply_output[frame].suppress_repeat {
                        *sample += peer.audio[frame];
                    }
                }
            }
        }
        // Track activity even when both blocks are in flight. Dropped callbacks
        // remain source-time gaps and cannot hide a new destination burst.
        for (route, peer) in self.routing.iter_mut().zip(&self.peers) {
            route.activity_edge = peer.input.signals().activity_edge();
            route.direct = peer.origin.direct().is_some();
            let selected = is_selected(peer);
            let own_source = usize::from(
                peer.active
                    && selected
                    && (peer.mode.forwards() || (!route.direct && peer.mode.transmits())),
            );
            let ended = peer.input.signals().ended() || !peer.origin.current();
            route.target = if ended {
                0
            } else {
                peer.input.signals().reply_identity()
            };
            route.enabled = !(ended || outgoing_isolation && route.direct)
                && (parrot_active
                    || ((peer.mode.transmits() || peer.group.is_some())
                        && selected
                        && (receiving
                            || if !route.direct {
                                forwarding_sources + local_phone_sources > own_source
                            } else if isolate_peers {
                                forwarding_phone_sources != 0
                            } else {
                                forwarding_sources > own_source
                            })));
            route.broadcast = !ended && peer.mode.transmits() && selected;
            route.telemetry = peer.input.signals().telemetry_allowed();
            let eligible = route.target != 0
                && (route.enabled
                    || (route.telemetry
                        && route.broadcast
                        && self.reply_output[..count]
                            .iter()
                            .any(|sample| sample.broadcast.is_some()))
                    || route.telemetry
                        && self.reply_output[..count]
                            .iter()
                            .any(|reply| reply.target == route.target));
            if eligible && !route.eligible {
                route.span.burst = route.span.burst.wrapping_add(1);
            }
            route.eligible = eligible;
            route.span.generation = source.generation;
            route.span.source_start = source.sample;
            route.span.stream = source.stream;
            route.span.born_ns = source.born_ns;
        }
        let Ok(mut block) = self.recycled.pop() else {
            self.status.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(keyed);
        };
        block.frames = count;
        block.isolation_epoch = isolation_epoch;
        block.routing.copy_from_slice(&self.routing);
        block.replies[..count].copy_from_slice(&self.reply_output[..count]);
        block.audio[..count].fill(0.0);
        if receiving {
            for (frame, output) in block.audio[..count].iter_mut().enumerate() {
                if !self.reply_output[frame].suppress_repeat {
                    *output += self.local[frame];
                }
            }
        }
        for peer in &self.peers {
            if peer.active && peer.mode.forwards() && is_selected(peer) {
                for (frame, output) in block.audio[..count].iter_mut().enumerate() {
                    if !self.reply_output[frame].suppress_repeat {
                        *output += peer.audio[frame];
                    }
                }
            }
        }
        for (output, parrot) in block.audio[..count]
            .iter_mut()
            .zip(&self.parrot_output[..count])
        {
            *output += parrot;
        }
        for (destination, peer) in self.peers.iter().enumerate() {
            let direct = block.routing[destination].direct;
            let offset = (destination + 1) * self.local.len();
            let own = &mut block.audio[offset..offset + count];
            if isolate_peers && direct {
                own.copy_from_slice(direct_forwarding);
                continue;
            }
            // A caller's own source is subtracted whether or not it is
            // allowed to forward to ordinary public links.
            if peer.active
                && is_selected(peer)
                && (peer.mode.forwards() || (!direct && peer.mode.transmits()))
            {
                for (frame, sample) in own.iter_mut().enumerate() {
                    *sample = if self.reply_output[frame].suppress_repeat {
                        0.0
                    } else {
                        peer.audio[frame]
                    };
                }
            } else {
                own.fill(0.0);
            }
            if !direct && local_phone_sources != 0 {
                // The dispatcher subtracts this slot from the public base mix.
                // A negative local aggregate adds those callers only here.
                for (sample, local) in own.iter_mut().zip(local_phone.iter()) {
                    *sample -= local;
                }
            }
        }
        // A popped recycle block guarantees space in the other equal-capacity queue.
        let _ = self.publish.push(block);
        Ok(keyed)
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
