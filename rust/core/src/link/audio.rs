//! Generation-owned peer audio composition; no channel operations or locks.
use super::{AdmissionError, Mode, PeerSignals, ReceiveState};
use crate::{audio::LinkAudioProducer, controller::NodeController};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

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
    /// Currently available source-rate samples.
    fn available(&self) -> u64;
    /// Render native F32, concealing exactly once inside the released ring.
    fn render(&mut self, output: &mut [f32]) -> bool;
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
        })
    }
}

/// Fixed prepared peer set retained until its generation is detached and reclaimed.
pub struct LinkAudio<P: PeerInput> {
    peers: Vec<AudioPeer<P>>,
    local: Vec<f32>,
    mix: Vec<f32>,
    program: Vec<f32>,
    status: LinkAudioStatus,
    destinations: Vec<usize>,
    publish: Producer<ProgramBlock>,
    recycled: Consumer<ProgramBlock>,
}

#[derive(Debug)]
struct ProgramBlock {
    // One composite plane followed by each transmitting destination's own contribution.
    audio: Vec<f32>,
    frames: usize,
    enabled: Vec<bool>,
}

/// Sole program-loopback consumer and producer of all per-peer transmit queues.
/// It performs no codec/channel I/O and runs outside both radio workers.
pub struct LinkDispatcher {
    incoming: Consumer<ProgramBlock>,
    recycle: Producer<ProgramBlock>,
    outbound: Vec<LinkAudioProducer>,
    scratch: Vec<f32>,
}
impl LinkDispatcher {
    /// Dispatch at most `maximum_blocks`, then return every block to its radio-side pool.
    /// Only this owner writes peer queues; no block allocation or wait occurs here.
    pub fn dispatch(&mut self, maximum_blocks: usize) -> usize {
        let mut count = 0;
        for _ in 0..maximum_blocks {
            let Ok(block) = self.incoming.pop() else {
                break;
            };
            for (index, outbound) in self.outbound.iter_mut().enumerate() {
                if !block.enabled[index] {
                    continue;
                }
                let offset = (index + 1) * self.scratch.len();
                for (frame, sample) in self.scratch[..block.frames].iter_mut().enumerate() {
                    *sample = block.audio[frame] - block.audio[offset + frame];
                }
                outbound.write(&self.scratch[..block.frames]);
            }
            // The two queues hold exactly two blocks in total. Owning this block
            // proves the recycle queue has a free slot, even if radio has stopped.
            let _ = self.recycle.push(block);
            count += 1;
        }
        count
    }
}
impl<P: PeerInput> LinkAudio<P> {
    /// Prepare radio and dispatcher owners plus exactly two recycled program blocks.
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
        let mut destinations = Vec::new();
        let mut outbound = Vec::new();
        for (index, peer) in peers.iter_mut().enumerate() {
            // Control-only construction consumes each freshly prepared peer exactly once.
            let producer = peer.outbound.take().expect("prepared outbound owner");
            if peer.mode.transmits() {
                destinations.push(index);
                outbound.push(producer);
            }
        }
        let samples = maximum
            .checked_mul(destinations.len() + 1)
            .ok_or(AdmissionError::Invalid)?;
        let (publish, incoming) = RingBuffer::new(2);
        let (mut recycle, recycled) = RingBuffer::new(2);
        // Control-only preparation fills the new two-slot pool before either owner runs.
        for _ in 0..2 {
            recycle
                .push(ProgramBlock {
                    audio: vec![0.0; samples],
                    frames: 0,
                    enabled: vec![false; destinations.len()],
                })
                .expect("new loopback pool");
        }
        let dispatcher = LinkDispatcher {
            incoming,
            recycle,
            outbound,
            scratch: vec![0.0; maximum],
        };
        Ok((
            Self {
                peers,
                local: vec![0.0; maximum],
                mix: vec![0.0; maximum],
                program: vec![0.0; maximum],
                status,
                destinations,
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
    /// Render RF and publish one prepared pre-access-tone program block for the dispatcher.
    pub fn process(
        &mut self,
        controller: &mut NodeController,
        receiving: bool,
        audio: &mut [f32],
    ) -> Result<bool, AdmissionError> {
        let count = audio.len();
        if count > self.local.len() {
            return Err(AdmissionError::Invalid);
        }
        self.local[..count].copy_from_slice(audio);
        self.mix[..count].fill(0.0);
        for (index, peer) in self.peers.iter_mut().enumerate() {
            let signal = peer.input.signals();
            let active = peer.receive.should_render(
                signal.pcm_epoch(),
                peer.input.available(),
                signal.ended(),
                count,
            );
            let active = active && peer.input.render(&mut peer.audio[..count]);
            if active {
                if !peer.active {
                    controller.link_keyed(&peer.direct);
                    peer.duration = 0;
                    self.status.last_keyed.store(index + 1, Ordering::Release);
                }
                peer.duration = peer.duration.saturating_add(count as u64);
                for (mixed, input) in self.mix[..count].iter_mut().zip(&peer.audio) {
                    *mixed += input;
                }
            } else if peer.active {
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
            peer.active = active;
            peer.input.signals().set_active(active);
        }
        let keyed = controller
            .process_audio_with_program(
                receiving,
                self.active_count() != 0,
                &self.mix[..count],
                audio,
                &mut self.program[..count],
            )
            .unwrap_or(false);
        if count == 0 || self.destinations.is_empty() {
            return Ok(keyed);
        }
        let Ok(mut block) = self.recycled.pop() else {
            self.status.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(keyed);
        };
        block.frames = count;
        block.audio[..count].copy_from_slice(&self.program[..count]);
        if receiving {
            for (output, local) in block.audio[..count].iter_mut().zip(&self.local) {
                *output += local;
            }
        }
        for peer in &self.peers {
            if peer.active && peer.mode.forwards() {
                for (output, input) in block.audio[..count].iter_mut().zip(&peer.audio) {
                    *output += input;
                }
            }
        }
        for (destination, &peer_index) in self.destinations.iter().enumerate() {
            let peer = &self.peers[peer_index];
            block.enabled[destination] = !peer.input.signals().ended();
            let offset = (destination + 1) * self.local.len();
            let own = &mut block.audio[offset..offset + count];
            // Every transmitting mode forwards; preparation excludes monitor destinations.
            if peer.active {
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
