//! Concrete public-Asterisk devices and generation-owned link endpoints.
use crate::{
    link::{
        ring::{InboundPolicy, InboundRing},
        session::{Command as PeerCommand, Event, PeerControl, PeerReader, PeerSession},
    },
    media::NativeMediaPreparer,
    services::{HostServices, PeerIo},
    worker::{Audio, AudioOwners, RadioWorker},
};
use rpt_advanced_core::{
    audio::LinkAudioQueue,
    config::{ConfigDocument, LinkLookupMethod, ResolvedNodeSettings},
    link::{AudioPeer, LinkAudio, LinkAudioStatus, LinkDispatcher, Mode},
    runtime::{
        DeviceHandoff, GenerationSettings, PreparedAdapter, Runtime, RuntimeClock, RuntimeError,
        dtmf::DigitOperation, links::LinkEffect,
    },
};
use std::{
    ffi::CString,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Maximum supported hardware voice frame, prepared before worker entry.
pub const MAXIMUM_FRAMES: usize = 4096;

struct Lease {
    quiesced: bool,
    owners: Option<AudioOwners>,
    worker: Option<RadioWorker>,
    epoch: Instant,
    status: crate::worker::RadioStatus,
}
impl Lease {
    fn quiesce(&mut self) -> bool {
        if let Some(worker) = self.worker.take() {
            self.quiesced = true;
            if let Some(owners) = worker.stop() {
                self.owners = Some(owners);
            }
        }
        true
    }
    fn attach(&mut self, owners: AudioOwners) -> bool {
        let Some(worker) = &mut self.worker else {
            self.owners = Some(owners);
            return false;
        };
        match worker.attach(owners) {
            Ok(()) => true,
            Err(owners) => {
                self.owners = Some(owners);
                false
            }
        }
    }
}
struct Device {
    lease: Arc<Mutex<Lease>>,
    services: HostServices,
}
impl DeviceHandoff for Device {
    fn quiesce(&mut self) -> bool {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .quiesce()
    }
    fn close(&mut self) {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        lease.quiesce();
        lease.quiesced = false;
    }
    fn open(&mut self, settings: &GenerationSettings) -> bool {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if lease.worker.is_some() || lease.quiesced {
            return false;
        }
        let radio = self.services.radio(
            &settings.device,
            settings.receive_maximum.max(settings.transmit_maximum),
        );
        let Ok(radio) = radio else {
            return false;
        };
        match RadioWorker::prepare(
            radio,
            lease.epoch,
            lease.status.clone(),
            settings.squelch_delay_ms,
        ) {
            Ok(worker) => {
                lease.worker = Some(worker);
                if let Some(owners) = lease.owners.take() {
                    return lease.attach(owners);
                }
                true
            }
            Err((_error, radio)) => {
                drop(radio);
                false
            }
        }
    }
}

/// Control-only dispatcher and endpoint-redirection acknowledgments for one audio generation.
pub struct GenerationControl {
    dispatcher: LinkDispatcher,
    status: LinkAudioStatus,
    redirects: Vec<(String, PeerCommand)>,
    awaiting: Vec<(String, crate::link::ring::InboundObserver)>,
}
impl GenerationControl {
    fn acknowledge(&mut self, remote: &str, observer: &crate::link::ring::InboundObserver) {
        self.awaiting
            .retain(|(name, expected)| name != remote || !expected.same_generation(observer));
    }
}
struct PeerOwner {
    local: String,
    remote: String,
    mode: Mode,
    announced: bool,
    reader: PeerReader,
    control: PeerControl,
}

/// Product runtime; every method is invoked by the selected serialized control executor.
pub struct Host {
    /// Core configuration, controller, schedule, and link-policy aggregate.
    pub runtime: Runtime<Audio, GenerationControl>,
    /// Selected descriptor-backed native media preparation owner.
    pub media: NativeMediaPreparer,
    services: HostServices,
    leases: Vec<(String, Arc<Mutex<Lease>>)>,
    peers: Vec<PeerOwner>,
    operations: Vec<(String, DigitOperation)>,
    epoch: Instant,
}

fn prepared(
    local: &str,
    settings: &ResolvedNodeSettings,
    peers: &[PeerOwner],
) -> Result<PreparedAdapter<Audio, GenerationControl>, RuntimeError> {
    let mut audio = Vec::new();
    let mut redirects = Vec::new();
    let mut awaiting = Vec::new();
    for peer in peers.iter().filter(|peer| peer.local == local) {
        let snapshot = peer
            .control
            .snapshot()
            .map_err(|_| RuntimeError::Preparation)?;
        if snapshot.ended {
            continue;
        }
        let (inbound, input) = InboundRing::open(snapshot.input_rate, InboundPolicy::Peer)
            .map_err(|_| RuntimeError::Preparation)?;
        let (output, outbound) = LinkAudioQueue::new(48000 / 5)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        audio.push(
            AudioPeer::new(
                &peer.remote,
                peer.mode,
                input,
                output,
                MAXIMUM_FRAMES,
                u32::try_from(settings.kerchunk_max_ms).map_err(|_| RuntimeError::Preparation)?,
            )
            .map_err(|_| RuntimeError::Preparation)?,
        );
        awaiting.push((peer.remote.clone(), inbound.observer()));
        redirects.push((
            peer.remote.clone(),
            PeerCommand::Redirect { inbound, outbound },
        ));
    }
    let (audio, dispatcher) =
        LinkAudio::new(audio, MAXIMUM_FRAMES).map_err(|_| RuntimeError::Preparation)?;
    let status = audio.status();
    Ok(PreparedAdapter {
        state: audio,
        control: GenerationControl {
            dispatcher,
            status,
            redirects,
            awaiting,
        },
        receive_maximum: MAXIMUM_FRAMES,
        transmit_maximum: MAXIMUM_FRAMES,
    })
}

impl Host {
    fn prune_leases(&mut self) {
        prune_leases(&mut self.leases);
    }
    /// Prepare and activate inactive callback contexts before publishing fixed registrations.
    pub fn start(
        document: ConfigDocument,
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        clock: RuntimeClock,
    ) -> Result<Self, RuntimeError> {
        let mut leases = Vec::new();
        let runtime = Runtime::start(
            document,
            &media,
            |name, settings| prepared(name, settings, &[]),
            |name, _| {
                let lease = Arc::new(Mutex::new(Lease {
                    quiesced: false,
                    owners: None,
                    worker: None,
                    epoch,
                    status: Default::default(),
                }));
                leases.push((name.to_owned(), Arc::clone(&lease)));
                Ok(Box::new(Device { lease, services }))
            },
            clock,
        )?;
        let mut host = Self {
            runtime,
            media,
            services,
            leases,
            peers: Vec::new(),
            operations: Vec::new(),
            epoch,
        };
        host.attach_workers();
        Ok(host)
    }
    fn attach_workers(&mut self) {
        // New leases precede retained same-name leases in reverse order. Registering
        // takes the unique owners once, so an older lease cannot attach them again.
        // Inactive prepared contexts accept their fixed registrations without failure.
        for (local, lease) in self.leases.iter().rev() {
            if let Some(owners) = self
                .runtime
                .node(local)
                .and_then(|node| node.register_audio())
            {
                assert!(
                    lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .attach(owners)
                );
            }
        }
    }
    /// Take copied radio/peer operations after pump; dispatch their effects on this same owner.
    pub fn take_operations(&mut self) -> Vec<(String, DigitOperation)> {
        std::mem::take(&mut self.operations)
    }
    /// Admission/queue loss invalidates every partial collector before subsequent queued digits.
    pub fn lost_digits(&mut self) {
        self.runtime.discard_digits();
    }
    /// Resolve and atomically replace configuration; failed preparation retains live leases.
    pub fn reload(
        &mut self,
        document: ConfigDocument,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let peers = &self.peers;
        let leases = &mut self.leases;
        let epoch = self.epoch;
        let services = self.services;
        let result = self.runtime.reload(
            document,
            &self.media,
            |name, settings| prepared(name, settings, peers),
            |name, _| {
                let lease = Arc::new(Mutex::new(Lease {
                    quiesced: false,
                    owners: None,
                    worker: None,
                    epoch,
                    status: Default::default(),
                }));
                leases.push((name.to_owned(), Arc::clone(&lease)));
                Ok(Box::new(Device { lease, services }))
            },
            clock,
        );
        self.prune_leases();
        result?;
        self.attach_workers();
        let effects = self.runtime.take_effects();
        for (local, effect) in effects {
            self.immediate(&local, effect, clock)?;
        }
        self.pump(clock)
    }
    /// Resolve a destination and optionally authenticate the numeric incoming source address.
    pub fn lookup(
        &self,
        local: &str,
        remote: &str,
        source: Option<&str>,
    ) -> Result<String, RuntimeError> {
        let settings = self
            .runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        let method = match settings.link_lookup_method {
            LinkLookupMethod::Both => 0,
            LinkLookupMethod::Dns => 1,
            LinkLookupMethod::File => 2,
        };
        self.services
            .lookup(
                method,
                &settings.link_static_directory_file,
                &settings.link_directory_file,
                remote,
                source,
            )
            .map_err(|_| RuntimeError::Rejected)
    }
    /// Prepare/start one already-authorized answered peer, then publish a state-preserving peer set.
    pub fn attach_peer(
        &mut self,
        local: &str,
        remote: &str,
        mode: Mode,
        io: PeerIo,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let (_, outbound) = LinkAudioQueue::new(48000 / 5)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        let (session, control) = PeerSession::prepare(io, outbound, local, remote)
            .map_err(|_| RuntimeError::Preparation)?;
        let reader = session.start().map_err(|_| RuntimeError::Preparation)?;
        self.peers.push(PeerOwner {
            local: local.into(),
            remote: remote.into(),
            mode,
            announced: false,
            reader,
            control,
        });
        #[cfg(test)]
        crate::fixture::wait_for_peer_end(&self.peers.last().unwrap().reader);
        if let Err(error) = self.refresh(local, clock) {
            self.detach(local, remote, clock.now_ms);
            return Err(error);
        }
        if let Some(peer) = self
            .peers
            .iter_mut()
            .find(|peer| peer.local == local && peer.remote == remote)
        {
            peer.announced = true;
            let _ = self
                .runtime
                .queue_link_event(local, remote, true, &self.media);
        }
        Ok(())
    }
    fn refresh(&mut self, local: &str, clock: RuntimeClock) -> Result<(), RuntimeError> {
        for _ in 0..100 {
            let settings = self
                .runtime
                .settings(local)
                .ok_or(RuntimeError::MissingNode)?;
            let candidate = prepared(local, settings, &self.peers)?;
            if replacement_completed(self.runtime.replace_adapter(local, candidate, clock.now_ms))?
            {
                return self.pump(clock);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(RuntimeError::Busy)
    }
    fn detach(&mut self, local: &str, remote: &str, now_ms: u64) {
        let announced = if let Some(index) = self
            .peers
            .iter()
            .position(|peer| peer.local == local && peer.remote == remote)
        {
            let peer = self.peers.remove(index);
            let announced = peer.announced;
            peer.control.stop();
            peer.reader.join();
            announced
        } else {
            false
        };
        if let Some(node) = self.runtime.node(local) {
            node.links().ended(remote);
        }
        self.runtime.peer_detached(local, remote, now_ms);
        if announced {
            let _ = self
                .runtime
                .queue_link_event(local, remote, false, &self.media);
        }
    }
    pub(crate) fn reject_peer(&mut self, local: &str, remote: &str, now_ms: u64) {
        self.detach(local, remote, now_ms);
    }
    pub(crate) fn status_text(&mut self, local: &str) -> Result<String, RuntimeError> {
        use std::fmt::Write;
        let node = self.runtime.node(local).ok_or(RuntimeError::MissingNode)?;
        let peers = node.links().manager().snapshot();
        let topology = self
            .runtime
            .topology(local)
            .ok_or(RuntimeError::MissingNode)?;
        let mut result = if peers.is_empty() {
            format!("rpt_advanced: {local} has no active links\n")
        } else {
            format!(
                "rpt_advanced: {local} has {} {}\n",
                peers.len(),
                if peers.len() == 1 { "link" } else { "links" }
            )
        };
        let (_, lease) = self
            .leases
            .iter()
            .rev()
            .find(|(name, _)| name == local)
            .expect("runtime node retains its device lease");
        let lease = lease.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(worker) = &lease.worker {
            let _ = writeln!(result, "{}", worker.local_status_text());
        }
        drop(lease);
        for peer in peers {
            let mode = if peer.mode.transmits() {
                "transceive"
            } else if peer.mode.forwards() {
                "monitor"
            } else {
                "local-monitor"
            };
            let permanent = if peer.permanent { " (permanent)" } else { "" };
            let retry = if !peer.retrying {
                ""
            } else if peer.paused {
                " (paused)"
            } else {
                " (retrying)"
            };
            let snapshot = self
                .peers
                .iter()
                .find(|owner| owner.local == local && owner.remote == peer.name)
                .and_then(|owner| owner.control.snapshot().ok());
            let rate = snapshot
                .as_ref()
                .map_or(48000, |snapshot| snapshot.input_rate)
                .max(1) as u64;
            let observation = snapshot
                .map(|snapshot| snapshot.ring)
                .unwrap_or(unsafe { std::mem::zeroed() });
            let ms = |samples: u64| samples.saturating_mul(1000) / rate;
            let _ = writeln!(
                result,
                "  {}: {mode}{permanent}{retry} rx-missing={} current-underrun-samples={} 10s-underrun-samples={:.3} reserve={}ms occupancy={}/{}ms filtered={}ms target={}ms ratio={:+}ppm",
                peer.name,
                observation.missing_samples,
                observation.consecutive_shortfall_samples,
                observation.shortfall_average_milli as f64 / 1000.0,
                ms(observation.reserve_samples),
                ms(observation.available_samples),
                ms(observation.capacity_samples),
                ms(observation.filtered_occupancy_samples),
                ms(observation.target_samples),
                observation.ratio_correction_ppm
            );
        }
        let _ = writeln!(
            result,
            "  topology: {}",
            if topology.is_empty() {
                "none"
            } else {
                &topology
            }
        );
        Ok(result)
    }
    /// Execute consequences which never require a blocking outbound dial.
    pub fn immediate(
        &mut self,
        local: &str,
        effect: LinkEffect,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        match effect {
            LinkEffect::Connect(_) => Err(RuntimeError::Rejected),
            LinkEffect::Detach(remotes) => {
                for remote in remotes {
                    self.detach(local, &remote, clock.now_ms);
                }
                Ok(())
            }
            LinkEffect::Text(messages) => {
                for (remote, text) in messages {
                    self.text(local, &remote, &text, true)?;
                }
                Ok(())
            }
            LinkEffect::RemoteDigit { remote, digit } => {
                let peer = self
                    .peers
                    .iter_mut()
                    .find(|peer| peer.local == local && peer.remote == remote)
                    .ok_or(RuntimeError::Rejected)?;
                peer.control
                    .send(PeerCommand::Digit(digit))
                    .map_err(|_| RuntimeError::Rejected)
            }
            LinkEffect::Telemetry(action) => {
                let last = self
                    .runtime
                    .node(local)
                    .and_then(|node| node.status(clock.now_ms).active)
                    .and_then(|generation| self.runtime.adapter_control_mut(local, generation))
                    .and_then(|control| control.status.last_keyed().map(str::to_owned));
                self.runtime
                    .queue_status(local, action, last.as_deref(), clock, &self.media)
            }
            LinkEffect::SelectedRemote | LinkEffect::None => Ok(()),
        }
    }
    fn text(
        &mut self,
        local: &str,
        remote: &str,
        text: &str,
        advisory: bool,
    ) -> Result<(), RuntimeError> {
        let peer = self
            .peers
            .iter_mut()
            .find(|peer| peer.local == local && peer.remote == remote)
            .ok_or(RuntimeError::Rejected)?;
        peer.control
            .send(PeerCommand::Text {
                text: CString::new(text).map_err(|_| RuntimeError::Rejected)?,
                advisory,
            })
            .map_err(|_| RuntimeError::Rejected)
    }
    /// Drain bounded dispatcher/control snapshots and complete old-generation redirect handshakes.
    pub fn pump(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        for (local, lease) in &self.leases {
            if let Some(worker) = &mut lease.lock().unwrap_or_else(|e| e.into_inner()).worker {
                worker.report_faults(local, clock.now_ms);
            }
        }
        let mut events = Vec::new();
        let mut ended = Vec::new();
        for peer in &mut self.peers {
            while let Some(event) = peer.control.event() {
                events.push((peer.local.clone(), peer.remote.clone(), event));
            }
            // Old ring EOF is expected while a Redirected acknowledgment is in flight.
            // Only the exclusive reader's termination ends the direct channel identity.
            if peer.reader.ended() {
                ended.push((peer.local.clone(), peer.remote.clone()));
            }
        }
        for (local, remote) in ended {
            self.detach(&local, &remote, clock.now_ms);
        }
        for (local, remote, event) in events {
            let (receiving, transitioned) = self
                .leases
                .iter()
                .find(|(name, _)| name == &local)
                .map(|(_, lease)| {
                    lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .status
                        .snapshot()
                })
                .unwrap_or((false, 0));
            let Some(node) = self.runtime.node(&local) else {
                continue;
            };
            match event {
                Event::Redirected(observer) => {
                    if let Some(generation) = node.status(clock.now_ms).active {
                        // This serialized owner has not changed the just-read generation.
                        node.adapter_control_mut(generation)
                            .expect("active generation retains its adapter control")
                            .acknowledge(&remote, &observer);
                    }
                }
                Event::Text(text) => {
                    if let Ok(effect) = node.links().peer_text(
                        &remote,
                        &text,
                        receiving,
                        transitioned,
                        clock.now_ms,
                    ) {
                        self.immediate(&local, effect, clock)?;
                    }
                }
                Event::Digit(digit) => {
                    if let Some(operation) = node.links().peer_digit(&remote, digit, clock.now_ms) {
                        self.operations.push((local, operation));
                    }
                }
            }
        }
        for (local, status) in self.runtime.status(clock.now_ms) {
            if let Some(node) = self.runtime.node(&local) {
                self.operations.extend(
                    node.drain_digits()
                        .into_iter()
                        .map(|operation| (local.clone(), operation)),
                );
                let messages = node.links().due_topology(clock.now_ms);
                for (remote, text) in messages {
                    let _ = self.text(&local, &remote, &text, false);
                }
            }
            let Some(generation) = status.active.or(status.retiring) else {
                continue;
            };
            // No publication/reclamation occurs between this snapshot and its borrow.
            let control = self
                .runtime
                .adapter_control_mut(&local, generation)
                .expect("snapshot generation retains its adapter control");
            control.dispatcher.dispatch(2);
            let mut pending = Vec::new();
            for (remote, command) in control.redirects.drain(..) {
                if let Some(peer) = self
                    .peers
                    .iter_mut()
                    .find(|peer| peer.local == local && peer.remote == remote)
                {
                    if let Err(command) = peer.control.send(command) {
                        pending.push((remote, command));
                    }
                } else {
                    control.awaiting.retain(|(name, _)| name != &remote);
                }
            }
            control.redirects = pending;
            control.awaiting.retain(|(remote, _)| {
                self.peers
                    .iter()
                    .any(|peer| peer.local == local && &peer.remote == remote)
            });
            if control.awaiting.is_empty() {
                if let Some(old) = status.retiring {
                    self.runtime.detached(&local, old);
                }
            }
        }
        self.runtime.reclaim();
        self.prune_leases();
        Ok(())
    }
    /// Stop all producers/readers, drain dispatcher blocks, then acknowledge exact generations.
    pub fn stop(&mut self, now_ms: u64) -> bool {
        let statuses = self.runtime.status(now_ms);
        self.runtime.stop(now_ms);
        for peer in self.peers.drain(..) {
            peer.control.stop();
            peer.reader.join();
            self.runtime
                .peer_detached(&peer.local, &peer.remote, now_ms);
        }
        for (local, status) in statuses {
            for generation in [status.active, status.retiring].into_iter().flatten() {
                if let Some(control) = self.runtime.adapter_control_mut(&local, generation) {
                    control.dispatcher.dispatch(2);
                    control.redirects.clear();
                    control.awaiting.clear();
                }
                self.runtime.detached(&local, generation);
            }
        }
        self.runtime.stop(now_ms)
    }
}
fn prune_leases(leases: &mut Vec<(String, Arc<Mutex<Lease>>)>) {
    leases.retain(|(_, lease)| Arc::strong_count(lease) > 1);
}
// Only a protected/retiring generation is retryable; preparation failures keep their diagnosis.
fn replacement_completed(result: Result<u64, RuntimeError>) -> Result<bool, RuntimeError> {
    match result {
        Ok(_) => Ok(true),
        Err(RuntimeError::Busy) => Ok(false),
        Err(error) => Err(error),
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop(self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
