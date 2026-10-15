//! Generation-owned peer audio composition; no channel operations or locks.
use super::{AdmissionError, Mode, PeerSignals, ReceiveState};
use crate::{
    audio::{AudioSpan, LinkAudioProducer},
    controller::{NodeController, ParrotOutput, PeerReplySample},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
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
    names: Arc<[String]>,
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
            .map(String::as_str)
    }
}

/// Unique adapter-owned consumer of one released inbound ring.
pub trait PeerInput: Send {
    /// Decoded source sample rate.
    fn source_rate(&self) -> u32;
    /// Shared PCM-before-epoch, activity and terminal snapshots.
    fn signals(&self) -> &PeerSignals;
    /// Queued source-rate samples, excluding converter and PLC lookahead.
    fn available(&self) -> u64;
    /// Render native F32, returning real samples (not concealment), or None on failure.
    fn render(&mut self, output: &mut [f32]) -> Option<usize>;
}

/// Prepared slot whose endpoints move into exactly one generation's transmit owner.
pub struct AudioPeer<P: PeerInput> {
    direct: String,
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
        if !super::decimal_identity(direct) || maximum == 0 {
            return Err(AdmissionError::Invalid);
        }
        let receive = ReceiveState::new(input.source_rate())?;
        Ok(Self {
            direct: direct.into(),
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
    peers: Vec<AudioPeer<P>>,
    local: Vec<f32>,
    mix: Vec<f32>,
    parrot_mix: Vec<f32>,
    parrot_output: Vec<f32>,
    reply_output: Vec<PeerReplySample>,
    status: LinkAudioStatus,
    routing: Vec<Destination>,
    groups: Vec<GroupSelection>,
    selected_groups: Vec<usize>,
    publish: Producer<ProgramBlock>,
    recycled: Consumer<ProgramBlock>,
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
    eligible: bool,
    target: usize,
    span: AudioSpan,
    activity_edge: u64,
}

#[derive(Debug)]
struct ProgramBlock {
    // One peer-routable composite plane followed by each destination's contribution.
    audio: Vec<f32>,
    frames: usize,
    routing: Vec<Destination>,
    replies: Vec<PeerReplySample>,
}

/// Sole peer-audio loopback consumer and producer of all per-peer transmit queues.
/// It performs no codec/channel I/O and runs outside both radio workers.
pub struct LinkDispatcher {
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
            if destination.eligible {
                let offset = (index + 1) * self.scratch.len();
                for (frame, sample) in self.scratch[..block.frames].iter_mut().enumerate() {
                    *sample = if destination.enabled {
                        block.audio[frame] - block.audio[offset + frame]
                    } else {
                        0.0
                    };
                    if block.replies[frame].target == destination.target {
                        *sample += block.replies[frame].sample;
                    }
                }
                if self.outbound[index].write(destination.span, &self.scratch[..block.frames])
                    < block.frames
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
            names: peers.iter().map(|peer| peer.direct.clone()).collect(),
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
                })
                .expect("new loopback pool");
        }
        let dispatcher = LinkDispatcher {
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
                routing: vec![Destination::default(); peers.len()],
                peers,
                local: vec![0.0; maximum],
                mix: vec![0.0; maximum],
                parrot_mix: vec![0.0; maximum],
                parrot_output: vec![0.0; maximum],
                reply_output: vec![PeerReplySample::default(); maximum],
                status,
                groups,
                selected_groups,
                publish,
                recycled,
            },
            dispatcher,
        ))
    }
    /// Clone the read-only status handle before audio ownership is published.
    pub fn status(&self) -> LinkAudioStatus {
        self.status.clone()
    }
    /// Count receive-qualified peers on the owning audio worker.
    pub fn active_count(&self) -> usize {
        self.peers.iter().filter(|peer| peer.active).count()
    }
    /// Render RF and publish one prepared pre-access-tone peer-audio block for the dispatcher.
    pub fn process(
        &mut self,
        controller: &mut NodeController,
        receiving: bool,
        audio: &mut [f32],
        source: SourcePosition,
    ) -> Result<bool, AdmissionError> {
        let count = audio.len();
        if count > self.local.len() {
            return Err(AdmissionError::Invalid);
        }
        self.local[..count].copy_from_slice(audio);
        self.mix[..count].fill(0.0);
        for peer in &mut self.peers {
            let (pcm_epoch, available, ended) = {
                let signal = peer.input.signals();
                (signal.pcm_epoch(), peer.input.available(), signal.ended())
            };
            let audio_active = peer
                .receive
                .should_render(pcm_epoch, available, ended, count);
            let audio_active = audio_active
                && peer
                    .receive
                    .rendered(peer.input.render(&mut peer.audio[..count]), count);
            if !audio_active {
                peer.audio[..count].fill(0.0);
            }
            let active = audio_active || peer.input.signals().radio_keyed();
            peer.active = active;
            peer.input.signals().set_active(active);
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
                if !peer.reported_active {
                    controller.link_keyed(&peer.direct);
                    peer.duration = 0;
                    self.status.last_keyed.store(index + 1, Ordering::Release);
                }
                peer.duration = peer.duration.saturating_add(count as u64);
            } else if peer.reported_active {
                let mut storage = [0; 64];
                let signal = peer.input.signals();
                let source = signal
                    .selected_source(signal.activity_edge(), &mut storage)
                    .unwrap_or(&peer.direct);
                controller.link_unkeyed(
                    &peer.direct,
                    source,
                    peer.kerchunk_samples != 0 && peer.duration <= peer.kerchunk_samples,
                );
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
        controller.observe_parrot(receiving || any_peer_active, &self.parrot_mix[..count]);
        let mut parrot_active = false;
        let keyed = controller.process_audio_with_parrot(
            receiving,
            linked_active,
            receiving || any_peer_active,
            &self.mix[..count],
            audio,
            ParrotOutput {
                samples: &mut self.parrot_output[..count],
                active: &mut parrot_active,
                replies: Some(&mut self.reply_output[..count]),
            },
        );
        if count == 0 || self.peers.is_empty() {
            return Ok(keyed);
        }
        let forwarding_sources = self
            .peers
            .iter()
            .filter(|peer| {
                peer.active
                    && peer.mode.forwards()
                    && peer.group_index.is_none_or(|group| {
                        peer.group
                            .as_ref()
                            .is_some_and(|(_, slot)| self.selected_groups[group] == slot.get())
                    })
            })
            .count();
        // Track activity even when both blocks are in flight. Dropped callbacks
        // remain source-time gaps and cannot hide a new destination burst.
        for (route, peer) in self.routing.iter_mut().zip(&self.peers) {
            route.activity_edge = peer.input.signals().activity_edge();
            let selected = peer.group_index.is_none_or(|group| {
                peer.group
                    .as_ref()
                    .is_some_and(|(_, slot)| self.selected_groups[group] == slot.get())
            });
            let own_source = usize::from(peer.active && peer.mode.forwards() && selected);
            let ended = peer.input.signals().ended();
            route.target = if ended {
                0
            } else {
                peer.input.signals().reply_identity()
            };
            route.enabled = !ended
                && (parrot_active
                    || ((peer.mode.transmits() || peer.group.is_some())
                        && selected
                        && (receiving || forwarding_sources > own_source)));
            let eligible = route.target != 0
                && (route.enabled
                    || self.reply_output[..count]
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
        block.routing.copy_from_slice(&self.routing);
        block.replies[..count].copy_from_slice(&self.reply_output[..count]);
        block.audio[..count].fill(0.0);
        if receiving {
            for (output, local) in block.audio[..count].iter_mut().zip(&self.local) {
                *output += local;
            }
        }
        for peer in &self.peers {
            if peer.active
                && peer.mode.forwards()
                && peer.group_index.is_none_or(|group| {
                    peer.group
                        .as_ref()
                        .is_some_and(|(_, slot)| self.selected_groups[group] == slot.get())
                })
            {
                for (output, input) in block.audio[..count].iter_mut().zip(&peer.audio) {
                    *output += input;
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
            let selected = peer.group_index.is_none_or(|group| {
                peer.group
                    .as_ref()
                    .is_some_and(|(_, slot)| self.selected_groups[group] == slot.get())
            });
            let offset = (destination + 1) * self.local.len();
            let own = &mut block.audio[offset..offset + count];
            // Ordinary mix-minus removes this peer's own forwarding contribution.
            if peer.active && peer.mode.forwards() && selected {
                own.copy_from_slice(&peer.audio[..count]);
            } else {
                own.fill(0.0);
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
