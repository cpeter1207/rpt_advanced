//! Concrete public-Asterisk devices and generation-owned link endpoints.
use crate::{
    link::{
        dispatcher::{MediaWorker, PreparedDispatch},
        input::AudioInput,
        ring::{InboundPolicy, InboundRing},
        session::{Command as PeerCommand, Event, PeerControl, PeerIoWorker, PeerSession},
        text::{TextDestination, TextPayload, TextRouter},
    },
    media::{
        NativeMediaPreparer,
        text::{TextExecutor, TextReader},
    },
    phone::{PhoneCalls, PrivatePhoneReply},
    services::{HostServices, PeerIo, ProcessedPeer},
    status_post::{PostConfig, StatusPostService},
    worker::{Audio, AudioOwners, LocalRingUpdate, PrepareFailure, RadioWorker},
};
use rpt_advanced_core::{
    audio::LinkAudioQueue,
    config::{
        ConfigDocument, LinkLookupMethod, NodeId, ResolvedNodeSettings, ResolvedSpeechSettings,
    },
    link::{
        AudioPeer, GroupMemberSelection, HumanMessage, LinkAudio, LinkAudioStatus, LocalTextInput,
        Mode, PeerIdentity, PeerReply, PhoneCallId, PhoneSource, ProtocolId, TextAudience,
        TextOrigin,
    },
    media::SpeechSource,
    runtime::{
        DeviceHandoff, GenerationSettings, PreparedAdapter, Runtime, RuntimeClock, RuntimeError,
        dtmf::DigitOutcome, links::LinkEffect,
    },
};
use std::{
    ffi::CString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[path = "host_phone_auth.rs"]
mod phone_auth;
#[path = "host_phone_worker.rs"]
mod phone_worker;
#[path = "host_remote.rs"]
mod remote;

/// Maximum supported hardware voice frame, prepared before worker entry.
pub const MAXIMUM_FRAMES: usize = 4096;

#[path = "host_site.rs"]
mod site;

struct Lease {
    quiesced: bool,
    quiescing: bool,
    owners: Option<AudioOwners>,
    worker: Option<RadioWorker>,
    epoch: Instant,
    status: crate::worker::RadioStatus,
}
impl Lease {
    fn quiesce(&mut self) -> bool {
        self.quiescing = true;
        if self
            .worker
            .as_mut()
            .is_some_and(|worker| worker.poll_stop() != Ok(true))
        {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            self.quiesced = true;
            if let Some(owners) = worker.stop() {
                self.owners = Some(owners);
            }
        }
        self.quiescing = false;
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
    name: String,
    lease: Arc<Mutex<Lease>>,
    services: HostServices,
}
impl DeviceHandoff for Device {
    fn activation_ready(&mut self) -> Result<bool, RuntimeError> {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .as_mut()
            .ok_or(RuntimeError::Device)?
            .activation_ready()
            .map_err(|cause| {
                self.services
                    .scheduler_notice(&format!("radio {} activation failed: {cause}", self.name));
                RuntimeError::Device
            })
    }
    fn update(&mut self, _: &GenerationSettings) -> Result<bool, RuntimeError> {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .as_mut()
            .ok_or(RuntimeError::Device)?
            .configuration_update(0)
            .map_err(|_| RuntimeError::Device)
    }

    fn finish_update(&mut self, commit: bool) -> Result<(), RuntimeError> {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .as_mut()
            .ok_or(RuntimeError::Restore)?
            .configuration_update(if commit { 1 } else { 2 })
            .map_err(|_| RuntimeError::Restore)?
            .then_some(())
            .ok_or(RuntimeError::Restore)
    }

    fn quiesce(&mut self) -> bool {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .quiesce()
    }
    fn close(&mut self) {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if lease.quiesce() {
            lease.quiesced = false;
        }
    }
    fn open(&mut self, settings: &GenerationSettings) -> bool {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if lease.worker.is_some() || lease.quiesced {
            return false;
        }
        let radio = self.services.radio(
            &settings.node,
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
            settings.status_snapshot_interval_ms,
        ) {
            Ok(worker) => {
                lease.worker = Some(worker);
                if let Some(owners) = lease.owners.take() {
                    return lease.attach(owners);
                }
                true
            }
            Err(failure) => {
                if let PrepareFailure::Activation(_, worker) = failure {
                    lease.worker = Some(*worker);
                    lease.quiesce();
                }
                false
            }
        }
    }
}

/// Dispatcher lifetime and endpoint-redirection acknowledgments for one audio generation.
pub struct GenerationControl {
    text_origins: Vec<(String, TextOrigin)>,
    dispatcher: PreparedDispatch,
    status: LinkAudioStatus,
    redirects: Vec<(String, PeerCommand)>,
    awaiting: Vec<(String, crate::link::ring::InboundObserver)>,
    local_update: Option<LocalRingUpdate>,
    awaiting_local: bool,
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
    group: Option<GroupMemberSelection>,
    // Successful publication permits later ordinary lifecycle notices, even when
    // its initial solicited phone notice used a private sink.
    announced: bool,
    control: PeerControl,
}

/// Product runtime; every method is invoked by the selected serialized control executor.
pub struct Host {
    reload_text: Option<TextExecutor>,
    reload_failure: Option<RuntimeError>,
    reload_basis: u64,
    activated_revision: u64,
    remote_pending: Vec<remote::Pending>,
    text: TextExecutor,
    text_notice_after_ms: u64,
    /// Core configuration, controller, schedule, and link-policy aggregate.
    pub runtime: Runtime<Audio, GenerationControl>,
    /// Selected descriptor-backed native media preparation owner.
    pub media: NativeMediaPreparer,
    services: HostServices,
    leases: Vec<(String, Arc<Mutex<Lease>>)>,
    media_workers: Vec<(String, MediaWorker)>,
    peer_io: PeerIoWorker,
    peers: Vec<PeerOwner>,
    phone_calls: PhoneCalls,
    operations: Vec<(String, DigitOutcome)>,
    status_post: StatusPostService,
    site_acquisition: crate::site::acquisition::Acquisition,
    site_radio: crate::site::radio::RadioSite,
    site_transition: Option<site::Transition>,
    site_retired: Vec<crate::site::worker::DeviceWorker>,
    site_delivery: site::Delivery,
    epoch: Instant,
}

fn prepared(
    local: &str,
    settings: &ResolvedNodeSettings,
    peers: &[PeerOwner],
    worker: &mut MediaWorker,
    text: &mut TextExecutor,
    services: HostServices,
) -> Result<PreparedAdapter<Audio, GenerationControl>, RuntimeError> {
    let remote_commands = crate::configuration::radio::ResolvedRadio::resolve(
        &settings.radio.processing_configuration,
        local,
        None,
        std::ptr::null_mut(),
    )
    .and_then(|configuration| configuration.remote_commands())
    .map_err(|_| RuntimeError::Preparation)?;
    if remote_commands.is_some() && !services.remote_control_available() {
        return Err(RuntimeError::Preparation);
    }
    static NEXT_TEXT_STATION: AtomicU64 = AtomicU64::new(1);
    let text_generation = NEXT_TEXT_STATION.fetch_add(1, Ordering::Relaxed);
    let mut text_readers = Vec::new();
    let mut text_origins = Vec::new();
    let mut audio = Vec::new();
    let mut redirects = Vec::new();
    let mut awaiting = Vec::new();
    let mut wakes = Vec::new();
    for peer in peers.iter().filter(|peer| peer.local == local) {
        let snapshot = peer
            .control
            .snapshot()
            .map_err(|_| RuntimeError::Preparation)?;
        if snapshot.ended {
            continue;
        }
        let mut origin = peer.control.text_origin();
        origin.station_generation = text_generation;
        text_readers.push(
            text.register(local, origin.clone())
                .map_err(|_| RuntimeError::Preparation)?,
        );
        text_origins.push((peer.remote.clone(), origin));
        let (inbound, input) =
            InboundRing::open(snapshot.input_rate, peer.control.inbound_policy())
                .map_err(|_| RuntimeError::Preparation)?;
        let processor = services
            .prepare_peer_processing(
                &settings.radio.processing_configuration,
                local,
                MAXIMUM_FRAMES,
            )
            .map_err(|_| RuntimeError::Preparation)?;
        let input = AudioInput::Peer(ProcessedPeer::new(input, processor));
        let (output, outbound) = LinkAudioQueue::new(48000 / 5, 64)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        let prepared_peer = AudioPeer::new(
            rpt_advanced_core::link::PeerIdentity::from_operator(&peer.remote)
                .map_err(|_| RuntimeError::Preparation)?,
            peer.mode,
            input,
            output,
            MAXIMUM_FRAMES,
            u32::try_from(settings.kerchunk_max_ms).map_err(|_| RuntimeError::Preparation)?,
        )
        .map_err(|_| RuntimeError::Preparation)?;
        audio.push(match &peer.group {
            Some(group) => prepared_peer.with_group_member(group.clone()),
            None => prepared_peer,
        });
        awaiting.push((peer.remote.clone(), inbound.observer()));
        wakes.push(peer.control.wake());
        redirects.push((
            peer.remote.clone(),
            PeerCommand::Redirect {
                inbound,
                outbound,
                dtmf_muting: settings.dtmf_muting,
            },
        ));
    }
    let retirement = worker.retirement_domain();
    let (mut audio, dispatcher) =
        LinkAudio::new_in_domain(audio, MAXIMUM_FRAMES, retirement.clone())
            .map_err(|_| RuntimeError::Preparation)?;
    audio = audio.with_text(Box::new(TextMix(text_readers)));
    let regeneration = if settings.dtmf_regeneration.enabled {
        let (writer, reader) = InboundRing::open(48000, InboundPolicy::Media)
            .map_err(|_| RuntimeError::Preparation)?;
        audio = audio.with_regeneration(AudioInput::Peer(ProcessedPeer::new(reader, None)));
        Some((settings.dtmf_regeneration, writer, MAXIMUM_FRAMES))
    } else {
        None
    };
    let status = audio.status();
    let dispatcher = worker
        .prepare(dispatcher, wakes, regeneration)
        .map_err(|_| RuntimeError::Busy)?;
    Ok(PreparedAdapter {
        phone_retirement: Some(retirement),
        remote_commands,
        state: audio,
        control: GenerationControl {
            text_origins,
            dispatcher,
            status,
            redirects,
            awaiting,
            local_update: None,
            awaiting_local: false,
        },
        receive_maximum: MAXIMUM_FRAMES,
        transmit_maximum: MAXIMUM_FRAMES,
    })
}

struct TextMix(Vec<TextReader>);
impl LocalTextInput for TextMix {
    fn render(&mut self, pcm: &mut [f32]) -> bool {
        let mut active = false;
        for reader in &mut self.0 {
            active |= reader.render(pcm);
        }
        active
    }
}
fn bind_text_origins(
    local: &str,
    control: &mut GenerationControl,
    peers: &[PeerOwner],
    text: &TextExecutor,
) {
    for (remote, origin) in control.text_origins.drain(..) {
        if let Some(peer) = peers.iter().find(|peer| {
            peer.local == local
                && peer.remote == remote
                && peer.control.text_origin().connection_generation == origin.connection_generation
        }) {
            text.cancel(&peer.control.text_origin());
            peer.control.bind_text_generation(origin.station_generation);
        }
    }
}

fn media_worker<'a>(
    workers: &'a mut Vec<(String, MediaWorker)>,
    local: &str,
) -> Result<&'a mut MediaWorker, RuntimeError> {
    let index = match workers.iter().position(|(name, _)| name == local) {
        Some(index) => index,
        None => {
            workers
                .try_reserve(1)
                .map_err(|_| RuntimeError::Preparation)?;
            workers.push((
                local.to_owned(),
                MediaWorker::start().map_err(|_| RuntimeError::Preparation)?,
            ));
            workers.len() - 1
        }
    };
    Ok(&mut workers[index].1)
}

impl Host {
    /// Resolve the live generation on control, never exposing a handle to the caller.
    pub(crate) fn radio_control(
        &mut self,
        local: &str,
        mut request: crate::abi::rptadv_radio_control_v1,
    ) -> Result<crate::radio_control::Reply, i32> {
        if self.runtime.node(local).is_none() {
            return Err(-4);
        }
        let (_, lease) = self
            .leases
            .iter()
            .rev()
            .find(|(name, _)| name == local)
            .ok_or(-7)?;
        let mut lease = lease.lock().unwrap_or_else(|error| error.into_inner());
        if lease.quiesced {
            return Err(-7);
        }
        let text = lease.worker.as_mut().ok_or(-7)?.maintenance(&mut request)?;
        Ok(crate::radio_control::Reply { request, text })
    }

    fn prune_leases(&mut self) {
        prune_leases(&mut self.leases);
        self.media_workers
            .retain(|(local, _)| self.runtime.node(local).is_some());
    }
    /// Prepare and activate inactive callback contexts before publishing fixed registrations.
    pub fn start(
        document: ConfigDocument,
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        clock: RuntimeClock,
    ) -> Result<Self, RuntimeError> {
        let phone_calls = PhoneCalls::new(128, 16).map_err(|_| RuntimeError::Preparation)?;
        let mut text = TextExecutor::new(document.nodes().len().max(1), media.clone())
            .map_err(|_| RuntimeError::Preparation)?;
        let peer_io = PeerIoWorker::start(epoch).map_err(|_| RuntimeError::Device)?;
        let mut leases = Vec::new();
        let mut media_workers = Vec::new();
        let runtime = Runtime::start(
            document,
            &media,
            |name, settings| {
                prepared(
                    name,
                    settings,
                    &[],
                    media_worker(&mut media_workers, name)?,
                    &mut text,
                    services,
                )
            },
            |name, _| {
                let lease = Arc::new(Mutex::new(Lease {
                    quiesced: false,
                    quiescing: false,
                    owners: None,
                    worker: None,
                    epoch,
                    status: crate::worker::RadioStatus::with_epoch(epoch),
                }));
                leases.push((name.to_owned(), Arc::clone(&lease)));
                Ok(Box::new(Device {
                    name: name.to_owned(),
                    lease,
                    services,
                }))
            },
            clock,
        )?;
        let mut host = Self {
            reload_text: None,
            reload_failure: None,
            reload_basis: 0,
            activated_revision: 0,
            remote_pending: Vec::new(),
            text,
            text_notice_after_ms: 0,
            runtime,
            media,
            services,
            leases,
            media_workers,
            peer_io,
            peers: Vec::new(),
            phone_calls,
            operations: Vec::new(),
            status_post: StatusPostService::default(),
            site_acquisition: Default::default(),
            site_delivery: Default::default(),
            site_radio: Default::default(),
            site_transition: None,
            site_retired: Vec::new(),
            epoch,
        };
        host.advance_reload(clock)?;
        Ok(host)
    }
    fn activate_dispatchers(&mut self, now_ms: u64) {
        for (local, status) in self.runtime.status(now_ms) {
            if let Some(generation) = status.active {
                let clock = self
                    .leases
                    .iter()
                    .rev()
                    .find(|(name, _)| name == &local)
                    .map(|(_, lease)| {
                        lease
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .status
                            .source_clock
                            .clone()
                    })
                    .expect("active generation retains its device lease");
                // This borrow marks the external owner exposed: core reclamation
                // must wait for its explicit quiescence acknowledgment.
                let control = self
                    .runtime
                    .adapter_control_mut(&local, generation)
                    .expect("active generation retains its adapter control");
                control.dispatcher.bind_clock(clock);
                control.dispatcher.activate();
                bind_text_origins(&local, control, &self.peers, &self.text);
            }
        }
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
    pub fn take_operations(&mut self) -> Vec<(String, DigitOutcome)> {
        std::mem::take(&mut self.operations)
    }
    /// Submit eligible addressed digits only to this node's active local signaling lane.
    pub fn regenerate(&mut self, local: &str, digit: char) -> Result<(), RuntimeError> {
        let generation = self
            .runtime
            .status(0)
            .into_iter()
            .find(|(name, _)| name == local)
            .and_then(|(_, status)| status.active)
            .ok_or(RuntimeError::Rejected)?;
        let control = self
            .runtime
            .adapter_control_mut(local, generation)
            .ok_or(RuntimeError::Rejected)?;
        let _ = control.dispatcher.queue_digit(digit);
        Ok(())
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
        if self.runtime.reload_pending() {
            return Err(RuntimeError::Busy);
        }
        self.reload_failure = None;
        self.reload_basis = self.runtime.revision();
        let mut text = TextExecutor::prepare(document.nodes().len().max(1), self.media.clone())
            .map_err(|_| RuntimeError::Preparation)?;
        // Keep the live pair until commit. Only one candidate pair may overlap it;
        // a failed spawn drops that candidate without stopping live text or calls.
        text.start().map_err(|_| RuntimeError::Preparation)?;
        let local_delays: Vec<_> = self
            .leases
            .iter()
            .filter_map(|(name, lease)| {
                let lease = lease.lock().unwrap_or_else(|error| error.into_inner());
                lease.worker.as_ref().map(|worker| {
                    (
                        name.clone(),
                        worker.squelch_delay_ms(),
                        worker.minimum_delay_ms(),
                    )
                })
            })
            .collect();
        let peers = &self.peers;
        let media_workers = &mut self.media_workers;
        let leases = &mut self.leases;
        let epoch = self.epoch;
        let services = self.services;
        let result = self.runtime.prepare_reload(
            document,
            &self.media,
            |name, settings| {
                let mut candidate = prepared(
                    name,
                    settings,
                    peers,
                    media_worker(media_workers, name)?,
                    &mut text,
                    services,
                )?;
                if let Some((_, delay, minimum)) = local_delays
                    .iter()
                    .rev()
                    .find(|(local, _, _)| local == name)
                {
                    let target = settings.squelch_delay_ms.max(*minimum);
                    if *delay != target {
                        candidate.control.local_update = Some(
                            LocalRingUpdate::prepare(target)
                                .map_err(|_| RuntimeError::Preparation)?,
                        );
                        candidate.control.awaiting_local = true;
                    }
                }
                Ok(candidate)
            },
            |name, _| {
                let lease = Arc::new(Mutex::new(Lease {
                    quiesced: false,
                    quiescing: false,
                    owners: None,
                    worker: None,
                    epoch,
                    status: crate::worker::RadioStatus::with_epoch(epoch),
                }));
                leases.push((name.to_owned(), Arc::clone(&lease)));
                Ok(Box::new(Device {
                    name: name.to_owned(),
                    lease,
                    services,
                }))
            },
            clock,
        );
        if let Err(error) = result {
            self.prune_leases();
            return Err(error);
        }
        self.reload_text = Some(text);
        self.advance_reload(clock)?;
        self.pump(clock)
    }
    fn advance_reload(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        let phone_calls = &mut self.phone_calls;
        let acquisition = &mut self.site_acquisition;
        let radio = &mut self.site_radio;
        let transition = &mut self.site_transition;
        let retired = &mut self.site_retired;
        let ready = self
            .runtime
            .poll_reload_with_commit(clock, |runtime| {
                Self::retire_phones(runtime, phone_calls);
                Self::commit_sites(acquisition, radio, transition, retired, phone_calls);
            })
            .inspect_err(|_| self.reload_text = None)?;
        if !ready {
            return Ok(());
        }
        self.prune_leases();
        if self.activated_revision == self.runtime.revision() {
            self.reload_text = None;
            return Ok(());
        }
        if let Some(text) = self.reload_text.take() {
            self.remote_pending.clear();
            self.text.stop();
            self.text = text;
        }
        self.activated_revision = self.runtime.revision();
        self.activate_dispatchers(clock.now_ms);
        self.configure_status_posts(clock.now_ms);
        self.attach_workers();
        let effects = self.runtime.take_effects();
        for (local, effect) in effects {
            self.immediate(&local, effect, clock)?;
        }
        Ok(())
    }
    /// Observe a retained transaction; failures stay visible even when the regular
    /// control pump completed rollback before the host queried it.
    pub fn poll_reload(&mut self, clock: RuntimeClock) -> Result<bool, RuntimeError> {
        self.advance_reload(clock)
            .inspect_err(|error| self.reload_failure = Some(error.clone()))?;
        if let Some(error) = &self.reload_failure {
            return Err(error.clone());
        }
        Ok(!self.runtime.reload_pending())
    }
    /// Report publication independently of preparation or operational failure.
    pub(crate) fn reload_status(&self, error: Option<&RuntimeError>) -> i32 {
        let error = error.or(self.reload_failure.as_ref());
        if matches!(error, Some(RuntimeError::Restore)) {
            -3
        } else if self.runtime.revision() != self.reload_basis {
            if error.is_some() { -2 } else { 0 }
        } else if error.is_some() {
            -1
        } else if self.runtime.reload_pending() {
            1
        } else {
            0
        }
    }
    /// Route reserved Echo aliases before ASL lookup; an address alone cannot admit an Echo QSO.
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
        let identity = PeerIdentity::from_operator(remote).map_err(|_| RuntimeError::Rejected)?;
        if identity.protocol() == ProtocolId::EchoLink {
            return if source.is_none() {
                Ok(format!(
                    "EchoLink/{}",
                    identity.node().ok_or(RuntimeError::Rejected)?
                ))
            } else {
                Err(RuntimeError::Rejected)
            };
        }
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
    /// Admit an inspected peer through common policy. Echo's fresh provider QSO
    /// supplies authentication; only IAX uses an ASL directory/source lookup.
    pub fn incoming_peer(
        &mut self,
        io: PeerIo,
        source: &str,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let local = io.local().to_owned();
        if io.identity().protocol() == ProtocolId::Phone {
            self.runtime
                .settings(&local)
                .ok_or(RuntimeError::MissingNode)?;
            let source = self
                .phone_calls
                .admit(io, clock.now_ms)
                .map_err(|_| RuntimeError::Rejected)?;
            let reply = self
                .phone_calls
                .reply(&source)
                .ok_or(RuntimeError::Rejected)?;
            if let Err(error) = self.prepare_login_prompt(
                &source,
                &reply,
                rpt_advanced_core::messages::PhoneAuthenticationPrompt::Login,
            ) {
                self.retire_phone(&source);
                return Err(error);
            }
            let started = self.start_private_phone(&source, &reply);
            if let Err(error) = started {
                self.retire_phone(&source);
                return Err(error);
            }
            return Ok(());
        }
        let remote = io
            .identity()
            .operator_alias()
            .ok_or(RuntimeError::Rejected)?;
        if io.identity().protocol() == ProtocolId::AllStarLink {
            self.lookup(&local, &remote, Some(source))?;
        }
        self.runtime.incoming(&local, &remote, true)?;
        let result = self.attach_peer(&local, &remote, Mode::TRANSCEIVE, io, clock);
        if result.is_err() {
            self.reject_peer(&local, &remote, clock.now_ms);
        }
        result
    }
    /// Bind an authorized peer to its exact radio before media starts, then publish its peer set.
    pub fn attach_peer(
        &mut self,
        local: &str,
        remote: &str,
        mode: Mode,
        io: PeerIo,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        self.attach_peer_notifying(local, remote, mode, io, clock, true)
    }
    /// A caller-requested node link uses ordinary media without public command announcements.
    pub(crate) fn attach_peer_private(
        &mut self,
        local: &str,
        remote: &str,
        mode: Mode,
        io: PeerIo,
        clock: RuntimeClock,
        reply: &PeerReply,
    ) -> Result<(), RuntimeError> {
        self.current_private_phone(local, reply)?;
        self.attach_peer_notifying(local, remote, mode, io, clock, false)
    }
    fn attach_peer_notifying(
        &mut self,
        local: &str,
        remote: &str,
        mode: Mode,
        mut io: PeerIo,
        clock: RuntimeClock,
        announce: bool,
    ) -> Result<(), RuntimeError> {
        // Telephone media needs explicit authenticated *3 policy and per-node rings.
        // Pending private calls must never enter this numeric-node/ASL path.
        if io.identity().protocol() == ProtocolId::Phone {
            return Err(RuntimeError::Rejected);
        }
        self.peers
            .try_reserve(1)
            .map_err(|_| RuntimeError::Preparation)?;
        let local_name = local.to_owned();
        let remote_name = remote.to_owned();
        let group = self
            .runtime
            .node(local)
            .ok_or(RuntimeError::MissingNode)?
            .links()
            .group_member(remote);
        let source_clock = {
            let (_, lease) = self
                .leases
                .iter()
                .rev()
                .find(|(name, _)| name == local)
                .ok_or(RuntimeError::MissingNode)?;
            let lease = lease.lock().unwrap_or_else(|e| e.into_inner());
            lease
                .worker
                .as_ref()
                .ok_or(RuntimeError::Device)?
                .bind_peer(&mut io)
                .map_err(|_| RuntimeError::Preparation)?;
            Arc::clone(&lease.status.source_clock)
        };
        let (_, outbound) = LinkAudioQueue::new(48000 / 5, 64)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        let (mut session, control) = PeerSession::prepare(
            io,
            outbound,
            local,
            remote,
            self.runtime
                .settings(local)
                .ok_or(RuntimeError::MissingNode)?
                .dtmf_muting,
        )
        .map_err(|_| RuntimeError::Preparation)?;
        session.bind_clock(source_clock);
        if let Err(session) = self.peer_io.attach(session) {
            session.stop();
            return Err(RuntimeError::Rejected);
        }
        self.peers.push(PeerOwner {
            local: local_name,
            remote: remote_name,
            mode,
            group,
            announced: false,
            control,
        });
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
            if announce {
                let _ = if peer.control.text_origin().protocol == ProtocolId::EchoLink as u32 {
                    self.runtime
                        .queue_echo_link_event(local, remote, peer.control.callsign(), true)
                } else {
                    self.runtime.queue_link_event(local, remote, true)
                };
            }
        }
        Ok(())
    }
    fn refresh(&mut self, local: &str, clock: RuntimeClock) -> Result<(), RuntimeError> {
        for _ in 0..100 {
            // A preceding publication may still await dispatcher/redirect quiescence.
            // This control owner must collect it before another replacement can succeed.
            self.pump(clock)?;
            let settings = self
                .runtime
                .settings(local)
                .ok_or(RuntimeError::MissingNode)?;
            let candidate = prepared(
                local,
                settings,
                &self.peers,
                media_worker(&mut self.media_workers, local)?,
                &mut self.text,
                self.services,
            );
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(RuntimeError::Busy) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if replacement_completed(self.runtime.replace_adapter(local, candidate, clock.now_ms))?
            {
                self.activate_dispatchers(clock.now_ms);
                return self.pump(clock);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(RuntimeError::Busy)
    }
    fn detach(&mut self, local: &str, remote: &str, now_ms: u64) {
        self.detach_notifying(local, remote, now_ms, true);
    }
    fn detach_notifying(&mut self, local: &str, remote: &str, now_ms: u64, announce: bool) {
        let announced = if let Some(index) = self
            .peers
            .iter()
            .position(|peer| peer.local == local && peer.remote == remote)
        {
            let peer = self.peers.remove(index);
            self.text.cancel(&peer.control.text_origin());
            let announced = (announce && peer.announced).then(|| {
                (
                    peer.control.text_origin().protocol,
                    peer.control.callsign().to_owned(),
                )
            });
            peer.control.stop();
            self.peer_io.reap();
            announced
        } else {
            None
        };
        if let Some(node) = self.runtime.node(local) {
            node.links().ended(remote);
        }
        self.runtime.peer_detached(local, remote, now_ms);
        if let Some((protocol, callsign)) = announced {
            let _ = if protocol == ProtocolId::EchoLink as u32 {
                self.runtime
                    .queue_echo_link_event(local, remote, &callsign, false)
            } else {
                self.runtime.queue_link_event(local, remote, false)
            };
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
        match self.services.echo_account_status(local) {
            Ok(Some(account)) => {
                let _ = writeln!(result, "  {account}");
            }
            Ok(None) => {}
            Err(_) => {
                let _ = writeln!(result, "  echo-account status-unavailable");
            }
        }
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
        let text = self.text.observation();
        let _ = writeln!(
            result,
            "  incoming-text admitted={} rejected={:?} failed={} cancelled-origins={} queued={} active={} pending={} forwarded={} forwarding-rejected={}",
            text.admitted,
            text.rejected,
            text.failures,
            text.cancelled_origins,
            text.queued,
            text.active,
            text.pending,
            text.forwarded,
            text.forwarding_rejected
        );
        for origin in self.text.origins().into_iter().filter(|origin| {
            self.peers
                .iter()
                .any(|peer| peer.local == local && peer.control.text_origin() == origin.origin)
        }) {
            let _ = writeln!(
                result,
                "  incoming-text peer={} station-generation={} connection={} pending={} occupancy={} source-rate={}",
                origin.origin.peer,
                origin.origin.station_generation,
                origin.origin.connection_generation,
                origin.pending,
                origin.occupancy_samples,
                origin.source_rate_hz
            );
        }
        let generation = self
            .runtime
            .status(0)
            .into_iter()
            .find(|(name, _)| name == local)
            .and_then(|(_, status)| status.active);
        if let Some(control) =
            generation.and_then(|generation| self.runtime.adapter_control_mut(local, generation))
        {
            let (batches, maximum_ns) = control.dispatcher.timing();
            let _ = writeln!(
                result,
                "  fanout batches={batches} maximum-batch-ns={maximum_ns} dropped-blocks={} regeneration-dropped={}",
                control.status.dropped_blocks(),
                control.dispatcher.regeneration_dropped()
            );
        }
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
                .as_ref()
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
            if let Some(peer_observation) = snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.peer.as_ref())
            {
                let _ = writeln!(
                    result,
                    "    jitter delay={}ms buffered-span={}ms queued={} duplicate-drops={} late-drops={} full-drops={} missing-samples={} insertion-samples={} skipped-samples={} limit-resets={} overload-drops={} overload-span-samples={} ingress-queued={}/{} ingress-overflow-drops={}",
                    ms(peer_observation.jitter_delay_samples.into()),
                    ms(peer_observation.jitter_buffered_span_samples),
                    peer_observation.jitter_queued_packets,
                    peer_observation.jitter_duplicate_drops,
                    peer_observation.jitter_late_drops,
                    peer_observation.jitter_full_drops,
                    peer_observation.jitter_missing_samples,
                    peer_observation.jitter_insertion_samples,
                    peer_observation.jitter_skipped_samples,
                    peer_observation.jitter_limit_resets,
                    peer_observation.jitter_overload_drops,
                    peer_observation.jitter_overload_span_samples,
                    peer_observation.ingress_queued_packets,
                    peer_observation.ingress_capacity_packets,
                    peer_observation.ingress_overflow_drops,
                );
                let _ = writeln!(
                    result,
                    "    egress datagrams={} socket-blocked={} reliable-retries={} reliable-pending={} reply-pending={}",
                    peer_observation.egress_datagrams,
                    peer_observation.egress_would_block,
                    peer_observation.egress_reliable_retries,
                    peer_observation.egress_reliable_pending,
                    peer_observation.egress_reply_pending
                );
                if let Some(description) =
                    crate::link::session::describe_peer_observation(peer_observation)
                {
                    let _ = writeln!(result, "    {description}");
                }
            }
            if let Some(snapshot) = snapshot {
                let egress = snapshot.egress;
                let _ = writeln!(
                    result,
                    "    media accepted={} blocked={} expired-native-samples={} rejected-native-samples={} maximum-queue-age={}ms",
                    egress.accepted,
                    egress.blocked,
                    egress.expired_native_samples,
                    egress.rejected_native_samples,
                    egress.maximum_queue_age_ns / 1_000_000
                );
            }
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
        self.write_site_status(local, &mut result)?;
        Ok(result)
    }
    /// Execute consequences which never require a blocking outbound dial.
    pub fn immediate(
        &mut self,
        local: &str,
        effect: LinkEffect,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        if let LinkEffect::Telemetry(action, Some(reply)) = &effect {
            if let Some(source) = reply.phone_source() {
                if self.phone_calls.reply(source).as_ref() != Some(reply) {
                    return Err(RuntimeError::Rejected);
                }
                let last = self
                    .runtime
                    .node(local)
                    .and_then(|node| node.status(clock.now_ms).active)
                    .and_then(|generation| self.runtime.adapter_control_mut(local, generation))
                    .and_then(|control| control.status.last_keyed().map(str::to_owned));
                let stream = self.runtime.prepare_phone_status(
                    local,
                    *action,
                    last.as_deref(),
                    clock,
                    reply,
                )?;
                return self
                    .phone_calls
                    .queue_reply(source, reply, stream)
                    .map_err(|_| RuntimeError::Rejected);
            }
        }
        let private_reply = match &effect {
            LinkEffect::Message { reply, .. } | LinkEffect::SiteStatus { reply, .. } => {
                reply.as_ref()
            }
            LinkEffect::RemoteBase(operation) => operation.reply.as_ref(),
            _ => None,
        };
        if private_reply.is_some_and(|reply| reply.phone_source().is_some()) {
            // Additional private report families need their own prepared stream route.
            let reply = private_reply.expect("matched private reply");
            let source = reply.phone_source().expect("matched phone source");
            self.current_private_phone(local, reply)?;
            self.prepare_login_prompt(
                source,
                reply,
                rpt_advanced_core::messages::PhoneAuthenticationPrompt::Unavailable,
            )?;
            return Err(RuntimeError::Rejected);
        }
        match effect {
            LinkEffect::Connect(_) => Err(RuntimeError::Rejected),
            LinkEffect::PrivateDetach { remotes, reply } => {
                self.current_private_phone(local, &reply)?;
                for remote in remotes {
                    self.detach_notifying(local, &remote, clock.now_ms, false);
                }
                self.private_phone_result(&reply, true)
            }
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
            LinkEffect::ForwardDigits(messages) => {
                for (remote, text) in messages {
                    self.text(local, &remote, &text, false)?;
                }
                Ok(())
            }
            LinkEffect::Digit(outcome) => {
                if let DigitOutcome::RemoteCandidate(operation) = outcome {
                    // Selection changes collection of the very next queued digit.
                    let verified = self.lookup(local, &operation.command.node, None).is_ok();
                    let _ = self.runtime.command(local, operation, clock, verified);
                } else {
                    self.operations.push((local.to_owned(), outcome));
                }
                Ok(())
            }
            LinkEffect::Telemetry(action, reply) => {
                let last = self
                    .runtime
                    .node(local)
                    .and_then(|node| node.status(clock.now_ms).active)
                    .and_then(|generation| self.runtime.adapter_control_mut(local, generation))
                    .and_then(|control| control.status.last_keyed().map(str::to_owned));
                self.runtime
                    .queue_status(local, action, last.as_deref(), clock, reply)
            }
            LinkEffect::Message { selector, reply } => {
                self.runtime.queue_message(local, &selector, reply)
            }
            LinkEffect::CancelMessage => self.runtime.cancel_message(local),
            LinkEffect::SiteStatus { selector, reply } => {
                self.runtime.queue_site_report(local, &selector, reply)
            }
            LinkEffect::RemoteBase(operation) => self.submit_remote(local, operation, clock),
            LinkEffect::SelectedRemote
            | LinkEffect::ParrotEnabled(_)
            | LinkEffect::EchoTelemetry(_)
            | LinkEffect::None => Ok(()),
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
    fn report_text_failures(&mut self, now_ms: u64) {
        for _ in 0..64 {
            let Some(failure) = self.text.take_failure() else {
                break;
            };
            if now_ms < self.text_notice_after_ms {
                continue;
            }
            self.text_notice_after_ms = now_ms.saturating_add(60000);
            let message = format!(
                "rpt_advanced incoming text: peer={} generation={} job={} error={:?}",
                failure.origin.peer, failure.origin.station_generation, failure.job, failure.error
            );
            #[cfg(test)]
            crate::fixture::error_log(3, &message);
            {
                let message = CString::new(message).expect("validated peer identity in text error");
                unsafe extern "C" {
                    fn syslog(priority: std::ffi::c_int, format: *const std::ffi::c_char, ...);
                }
                // Constant format, validated peer identity and value-free error kind only.
                unsafe {
                    syslog(3, c"%s".as_ptr(), message.as_ptr());
                }
            }
        }
    }
    fn human_text(&mut self, local: &str, message: HumanMessage) {
        if !self.peers.iter().any(|peer| {
            peer.local == local
                && !peer.control.ended()
                && peer.control.text_origin() == message.origin
        }) {
            return;
        }
        let mut locals = Vec::new();
        for local in self.runtime.node_names() {
            locals.push(local.to_owned());
            let node = NodeId::new(local).expect("admitted station");
            let account = rpt_advanced_core::config::EchoLinkSettings::resolve(
                self.runtime.document(),
                &node,
            )
            .expect("validated account settings")
            .value;
            if account.enabled {
                locals.push(rpt_advanced_core::link::EchoAlias::format(
                    account.node_number,
                ));
                locals.push(account.callsign);
            }
        }
        let router = TextRouter::new(locals);
        let destinations = self
            .peers
            .iter()
            .filter(|peer| peer.local == local)
            .map(|peer| TextDestination {
                connection_generation: peer.control.text_origin().connection_generation,
                protocol: peer.control.text_origin().protocol,
                node: peer.remote.clone(),
                can_forward_text: peer.control.can_forward_text() && !peer.control.ended(),
            })
            .collect::<Vec<_>>();
        let disposition = router.route(&message, &destinations, |destination, text| {
            self.peers
                .iter_mut()
                .find(|peer| {
                    peer.local == local
                        && peer.remote == destination.node
                        && peer.control.text_origin().connection_generation
                            == destination.connection_generation
                })
                .expect("current text route snapshot")
                .control
                .send(match text {
                    TextPayload::AllStarLink(text) => PeerCommand::Text {
                        text: text.to_owned(),
                        advisory: false,
                    },
                    TextPayload::EchoLink { sender, body } => PeerCommand::HumanText {
                        sender: sender.to_owned(),
                        body: Arc::from(body),
                    },
                })
                .is_ok()
        });
        self.text.record_forwarding(
            disposition.destinations_admitted,
            disposition.destinations_rejected,
        );
        // Each node is an independent station, not an implicit local patch.
        // ASL consumes other configured local addresses without rendering here.
        if disposition.locally_eligible
            && !matches!(&message.audience, TextAudience::Node(node) if node != local)
        {
            // The local identity and immutable document were validated at station admission.
            let node = NodeId::new(local).expect("admitted local station");
            let settings = ResolvedSpeechSettings::resolve(self.runtime.document(), &node)
                .expect("validated station speech settings")
                .value;
            let spoken = self
                .runtime
                .node(local)
                .expect("admitted local station")
                .incoming_text_speech(&message.source, &message.body);
            let _ = self.text.admit(
                message,
                SpeechSource {
                    text: spoken,
                    model: settings.voice.into(),
                    speed_percent: settings.speed_percent as u32,
                    level_db: settings.level_db as i32,
                },
            );
        }
    }
    /// Drain control snapshots and complete old-generation redirect handshakes.
    pub fn pump(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        for reply in self.phone_calls.expire_authentication(clock.now_ms) {
            Self::retire_phone_source(&mut self.runtime, &reply);
        }
        self.pump_private_phones(clock);
        if let Err(error) = self.advance_reload(clock) {
            self.reload_failure = Some(error);
            self.reload_text = None;
            self.prune_leases();
        }
        self.pump_remote(clock);
        self.report_text_failures(clock.now_ms);
        self.advance_sites(clock);
        self.runtime.prepare_messages(clock);
        for (local, lease) in &self.leases {
            let mut lease = lease.lock().unwrap_or_else(|e| e.into_inner());
            if lease.quiescing {
                lease.quiesce();
                continue;
            }
            if let Some(worker) = &mut lease.worker {
                // A failed device cannot stop control/peer service on other nodes.
                // The physical owner closes admission and retains its first cause.
                let _ = worker.service_remote();
                worker.report_faults(local, clock.now_ms);
                worker.collect_snapshots();
            }
        }
        let mut events = Vec::new();
        let mut ended = Vec::new();
        for peer in &mut self.peers {
            if peer.control.text_origin().protocol == ProtocolId::EchoLink as u32 {
                peer.control
                    .set_telemetry_allowed(self.runtime.echo_telemetry_allows(
                        &peer.local,
                        &peer.control.reply_target(),
                        clock.now_ms,
                    ));
            }
            while let Some(event) = peer.control.event() {
                events.push((
                    peer.local.clone(),
                    peer.remote.clone(),
                    event,
                    peer.control.reply_target(),
                ));
            }
            if peer.control.ended() {
                ended.push((peer.local.clone(), peer.remote.clone()));
            }
        }
        for (local, remote) in ended {
            self.detach(&local, &remote, clock.now_ms);
        }
        for (local, remote, event, reply) in events {
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
                Event::HumanText(message) => {
                    self.human_text(&local, message);
                }
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
                        reply,
                        receiving,
                        transitioned,
                        clock.now_ms,
                    ) {
                        self.immediate(&local, effect, clock)?;
                    }
                }
                Event::Digit(digit) => {
                    if let Some(operation) =
                        node.links().peer_digit(&remote, digit, clock.now_ms, reply)
                    {
                        self.immediate(&local, LinkEffect::Digit(operation), clock)?;
                    }
                }
                Event::RemoteUnkey(now_ms) => {
                    for outcome in node.links().peer_remote_unkey(now_ms, reply) {
                        self.immediate(&local, LinkEffect::Digit(outcome), clock)?;
                    }
                }
            }
        }
        for (local, status) in self.runtime.status(clock.now_ms) {
            let events = self
                .runtime
                .node(&local)
                .map(|node| node.drain_digits())
                .unwrap_or_default();
            for event in events {
                if let Some(outcome) = self.runtime.digit(&local, event) {
                    self.immediate(&local, LinkEffect::Digit(outcome), clock)?;
                }
            }
            if let Some(node) = self.runtime.node(&local) {
                let keyed = self
                    .leases
                    .iter()
                    .rev()
                    .find(|(name, _)| name == &local)
                    .is_some_and(|(_, lease)| {
                        lease
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .status
                            .transmit_keyed()
                    });
                self.status_post.observe(
                    &local,
                    clock.now_ms,
                    clock.wall_seconds,
                    keyed,
                    node.links().manager().snapshot(),
                );
                self.operations.extend(
                    node.links()
                        .expire_peer_digits_with_collection(clock.now_ms, |reply| {
                            self.phone_calls.collection_expired(reply);
                        })
                        .into_iter()
                        .map(|operation| (local.clone(), operation)),
                );
                self.operations.extend(
                    node.links()
                        .advance_macros(clock.now_ms)
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
            let configured_delay = self
                .runtime
                .settings(&local)
                .map_or(0, |settings| settings.squelch_delay_ms);
            // No publication/reclamation occurs between this snapshot and its borrow.
            let control = self
                .runtime
                .adapter_control_mut(&local, generation)
                .expect("snapshot generation retains its adapter control");
            if status.active == Some(generation) {
                control.dispatcher.activate();
                bind_text_origins(&local, control, &self.peers, &self.text);
            }
            {
                // Runtime retains the device for every active or retiring generation.
                let (_, lease) = self
                    .leases
                    .iter()
                    .rev()
                    .find(|(name, _)| name == &local)
                    .expect("runtime node retains its device lease");
                let mut lease = lease.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(worker) = &mut lease.worker {
                    let target = configured_delay.max(worker.minimum_delay_ms());
                    if !control.awaiting_local && target != worker.squelch_delay_ms() {
                        control.local_update = Some(
                            LocalRingUpdate::prepare(target)
                                .map_err(|_| RuntimeError::Preparation)?,
                        );
                        control.awaiting_local = true;
                    }
                    if control.awaiting_local {
                        if let Some(update) = control.local_update.take() {
                            control.local_update =
                                worker.update_local_ring(generation, update).err();
                        }
                        control.awaiting_local = !worker.local_ring_applied(generation);
                    }
                }
            }
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
            let redirects_done = control.awaiting.is_empty() && !control.awaiting_local;
            if let Some(old) = status.retiring {
                let retired = self
                    .runtime
                    .adapter_control_mut(&local, old)
                    .expect("retiring generation retains its adapter control");
                retired.dispatcher.request_stop();
                if redirects_done && retired.dispatcher.is_quiescent() {
                    self.runtime.detached(&local, old);
                }
            }
        }
        let _ = self.runtime.queue_priority_group_events();
        self.runtime.reclaim();
        self.prune_leases();
        Ok(())
    }
    fn configure_status_posts(&mut self, now_ms: u64) {
        let config = self
            .runtime
            .status(now_ms)
            .into_iter()
            .filter_map(|(node, _)| {
                self.runtime.settings(&node).map(|settings| PostConfig {
                    node,
                    url: settings.statpost_url.clone(),
                    interval_seconds: settings.statpost_time,
                })
            })
            .collect();
        self.status_post.configure(config);
    }
    /// Gate audio, stop media distribution, quiesce peers, then acknowledge exact generations.
    pub fn stop(&mut self, now_ms: u64) -> bool {
        Self::retire_phones(&mut self.runtime, &mut self.phone_calls);
        self.remote_pending.clear();
        if self.site_shutdown(now_ms) != Ok(true) {
            return false;
        }
        self.status_post.request_stop();
        self.text.stop();
        let statuses = self.runtime.status(now_ms);
        for peer in &self.peers {
            peer.control.stop();
        }
        self.runtime.stop(now_ms);
        for (_, worker) in &mut self.media_workers {
            worker.stop();
        }
        for peer in self.peers.drain(..) {
            self.runtime
                .peer_detached(&peer.local, &peer.remote, now_ms);
        }
        self.peer_io.stop();
        for (local, status) in statuses {
            for generation in [status.active, status.retiring].into_iter().flatten() {
                if let Some(control) = self.runtime.adapter_control_mut(&local, generation) {
                    debug_assert!(control.dispatcher.is_quiescent());
                    control.redirects.clear();
                    control.awaiting.clear();
                }
                self.runtime.detached(&local, generation);
            }
        }
        self.runtime.stop(now_ms)
    }

    pub(crate) fn take_status_posts(&mut self) -> StatusPostService {
        std::mem::take(&mut self.status_post)
    }

    /// Actual private source of a currently owned call; never a public topology entry.
    pub fn phone_source(&self, call: PhoneCallId) -> Option<&PhoneSource> {
        self.phone_calls.source(call)
    }
    /// Retain the exact private return path before dispatching source-tagged work.
    pub fn phone_reply(&self, source: &PhoneSource) -> Option<PeerReply> {
        self.phone_calls.reply(source)
    }
    /// Queue a prepared prompt directly to one private call without entering radio media.
    pub fn queue_phone_reply(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
        stream: Box<dyn rpt_advanced_core::audio::PcmStreamReader>,
    ) -> Result<(), RuntimeError> {
        self.phone_calls
            .queue_reply(source, reply, stream)
            .map_err(|_| RuntimeError::Rejected)
    }
    /// Transfer private PCM to its future serial phone egress owner.
    pub fn take_phone_reply(
        &mut self,
        source: &PhoneSource,
        reply: &PeerReply,
    ) -> Result<Option<PrivatePhoneReply>, RuntimeError> {
        self.phone_calls
            .take_reply(source, reply)
            .map_err(|_| RuntimeError::Rejected)
    }
    /// Revoke the call and every hosted node's source collector/grant before reclamation.
    pub fn retire_phone(&mut self, source: &PhoneSource) -> bool {
        let Some(reply) = self.phone_calls.retire(source) else {
            return false;
        };
        Self::retire_phone_source(&mut self.runtime, &reply);
        true
    }
    fn retire_phone_source(runtime: &mut Runtime<Audio, GenerationControl>, reply: &PeerReply) {
        for (local, _) in runtime.status(0) {
            if let Some(node) = runtime.node(&local) {
                node.links().retire_source(reply);
            }
        }
    }
    fn retire_phones(runtime: &mut Runtime<Audio, GenerationControl>, calls: &mut PhoneCalls) {
        for reply in calls.retire_all() {
            Self::retire_phone_source(runtime, &reply);
        }
    }
}
fn prune_leases(leases: &mut Vec<(String, Arc<Mutex<Lease>>)>) {
    leases.retain(|(_, lease)| {
        Arc::strong_count(lease) > 1 || !lease.lock().unwrap_or_else(|e| e.into_inner()).quiesce()
    });
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
pub(crate) mod tests;
