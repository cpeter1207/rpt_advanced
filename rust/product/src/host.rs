//! Concrete public-Asterisk devices and generation-owned link endpoints.
use crate::{
    link::{
        dispatcher::{MediaWorker, PreparedDispatch},
        ring::InboundRing,
        session::{Command as PeerCommand, Event, PeerControl, PeerIoWorker, PeerSession},
        text::{TextDestination, TextPayload, TextRouter},
    },
    media::{
        NativeMediaPreparer,
        text::{TextExecutor, TextReader},
    },
    phone::{PhoneCalls, PrivatePhoneReply},
    services::{HostServices, PeerIo, ProcessedPeer},
    status_post::StatusPostService,
    worker::{Audio, AudioOwners, LocalRingUpdate, PrepareFailure, RadioWorker},
};
use rpt_advanced_core::{
    audio::LinkAudioQueue,
    config::ResolvedNodeSettings,
    link::{
        AudioPeer, GroupMemberSelection, HumanMessage, LinkAudio, LinkAudioStatus, LocalTextInput,
        Mode, PeerIdentity, PeerReply, PhoneCallId, PhoneSource, ProtocolId, TextAudience,
        TextOrigin,
    },
    media::SpeechSource,
    messages::OperationResult,
    runtime::{
        DeviceHandoff, GenerationSettings, GenerationWork, PreparedAdapter, Runtime, RuntimeClock,
        RuntimeError, dtmf::DigitOutcome, links::LinkEffect,
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

#[cfg(target_os = "linux")]
#[path = "host_asl.rs"]
pub(crate) mod asl;
#[cfg(target_os = "linux")]
#[path = "host_echo.rs"]
mod echo;
#[cfg(target_os = "linux")]
#[path = "host_modular.rs"]
mod modular;
#[cfg(target_os = "linux")]
#[path = "host_native.rs"]
mod native;
#[cfg(target_os = "linux")]
#[path = "host_status.rs"]
pub(crate) mod status;
#[cfg(target_os = "linux")]
pub(crate) use native::RadioTicket;
pub(crate) use site::SiteBasis;

#[path = "host_phone_auth.rs"]
mod phone_auth;
#[cfg(target_os = "linux")]
#[path = "host_phone_dial.rs"]
mod phone_dial;
#[cfg(target_os = "linux")]
#[path = "host_phone_progress.rs"]
mod phone_progress;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use phone_progress::fixture_cut_progress;
#[path = "host_remote.rs"]
mod remote;
#[cfg(target_os = "linux")]
#[path = "host_selection.rs"]
pub(crate) mod selection;

/// Maximum supported hardware voice frame, prepared before worker entry.
pub const MAXIMUM_FRAMES: usize = 4096;

#[path = "host_site.rs"]
mod site;

#[path = "host_maintenance.rs"]
mod maintenance;

struct Lease {
    maintenance: Option<Arc<crate::management::maintenance::Grant>>,
    #[cfg(target_os = "linux")]
    binding: native::LeaseBindings,
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
        if let Some(grant) = &self.maintenance {
            grant.revoke();
        }
        if self.clear_manual().is_err() {
            return false;
        }
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
        self.open_plan(settings, false)
    }

    fn restore(&mut self, settings: &GenerationSettings) -> bool {
        self.open_plan(settings, true)
    }

    #[cfg(target_os = "linux")]
    fn poll_update(
        &mut self,
        settings: &GenerationSettings,
    ) -> Result<rpt_advanced_core::runtime::DeviceUpdate, RuntimeError> {
        use rpt_advanced_core::runtime::DeviceUpdate;
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(binding) = lease.binding.next.clone() {
            let worker = lease.worker.as_mut().ok_or(RuntimeError::Device)?;
            return Ok(
                match worker
                    .stage_station_binding(&binding)
                    .map_err(|_| RuntimeError::Device)?
                {
                    crate::services::StationStage::Staged => DeviceUpdate::Staged,
                    crate::services::StationStage::Handoff => DeviceUpdate::Handoff,
                    crate::services::StationStage::Pending => DeviceUpdate::Pending,
                },
            );
        }
        if lease.binding.active.is_some() {
            return Err(RuntimeError::Rejected);
        }
        drop(lease);
        self.update(settings).map(|staged| {
            if staged {
                DeviceUpdate::Staged
            } else {
                DeviceUpdate::Handoff
            }
        })
    }

    #[cfg(target_os = "linux")]
    fn poll_finish_update(&mut self, commit: bool) -> Result<bool, RuntimeError> {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if lease.binding.next.is_some() {
            let worker = lease.worker.as_mut().ok_or(RuntimeError::Restore)?;
            return worker
                .finish_station_binding(commit)
                .map(|r| r == crate::services::StationFinish::Complete)
                .map_err(|_| RuntimeError::Restore);
        }
        if lease.binding.active.is_some() {
            return Err(RuntimeError::Restore);
        }
        drop(lease);
        self.finish_update(commit).map(|()| true)
    }
}

impl Device {
    fn open_plan(&mut self, settings: &GenerationSettings, previous: bool) -> bool {
        let mut lease = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        if lease.worker.is_some() || lease.quiesced {
            return false;
        }
        let maximum = settings.receive_maximum.max(settings.transmit_maximum);
        #[cfg(target_os = "linux")]
        let radio = match (previous, &lease.binding.active, &lease.binding.next) {
            (true, Some(plan), _) | (false, _, Some(plan)) => plan.open_radio(maximum),
            (_, None, None) => return false,
            _ => return false,
        };
        #[cfg(not(target_os = "linux"))]
        let radio = {
            let _ = previous;
            Err::<crate::services::Radio, _>(crate::Error::Reservation)
        };
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
    #[cfg(target_os = "linux")]
    progress: Vec<phone_progress::Redirect>,
    text_origins: Vec<(String, TextOrigin)>,
    dispatcher: PreparedDispatch,
    status: LinkAudioStatus,
    redirects: Vec<(u64, PeerCommand)>,
    phones: Vec<crate::link::private_radio::Redirect>,
    awaiting: Vec<(u64, crate::link::ring::InboundObserver)>,
    local_update: Option<LocalRingUpdate>,
    awaiting_local: bool,
}
impl GenerationControl {
    fn acknowledge(
        &mut self,
        connection: u64,
        observer: &crate::link::ring::InboundObserver,
    ) -> bool {
        let before = self.awaiting.len();
        self.awaiting
            .retain(|(name, expected)| *name != connection || !expected.same_generation(observer));
        self.awaiting.len() != before
    }
}
struct PeerOwner {
    superseded: bool,
    local: String,
    remote: String,
    mode: Mode,
    group: Option<GroupMemberSelection>,
    announced: bool,
    control: PeerControl,
}

/// Own one dial's outcomes until its exact peer ingress is acknowledged or torn down.
#[derive(Default)]
pub(crate) struct PendingLinkDial {
    #[cfg(target_os = "linux")]
    pub(crate) asl: Option<status::PendingAslDial>,
    pub(crate) phone: Option<PhoneLinkReply>,
}

pub(crate) struct PhoneLinkReply {
    pub(crate) reply: PeerReply,
    pub(crate) generation: Arc<GenerationWork>,
    pub(crate) report_failure: bool,
}

/// Product runtime; every method is invoked by the selected serialized control executor.
pub struct Host {
    reload_text: Option<TextExecutor>,
    reload_failure: Option<RuntimeError>,
    activated_revision: u64,
    #[cfg(target_os = "linux")]
    radio_preparation: native::RadioPreparation,
    #[cfg(target_os = "linux")]
    target_selections: Vec<selection::Pending>,
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
    phone_accounts: crate::phone::accounts::Participant,
    #[cfg(target_os = "linux")]
    phone_dials: std::collections::VecDeque<phone_dial::Pending>,
    #[cfg(target_os = "linux")]
    phone_failures: std::collections::VecDeque<phone_dial::Failure>,
    #[cfg(target_os = "linux")]
    asl: asl::Participant,
    #[cfg(target_os = "linux")]
    echo: echo::Participant,
    operations: Vec<(String, DigitOutcome)>,
    status_post: StatusPostService,
    #[cfg(target_os = "linux")]
    handoff_status: std::collections::BTreeMap<
        rpt_advanced_core::config::resources::candidate::ResourceId,
        status::Handoff,
    >,
    pending_link_dials: std::collections::BTreeMap<u64, PendingLinkDial>,
    site_acquisition: crate::site::acquisition::Acquisition,
    site_radio: crate::site::radio::RadioSite,
    site_transition: Option<site::Transition>,
    site_retired: Vec<crate::site::worker::DeviceWorker>,
    site_delivery: site::Delivery,
    epoch: Instant,
}

#[derive(Clone, Copy)]
enum StationAudio<'a> {
    #[cfg(target_os = "linux")]
    Modular(&'a native::RadioPlan),
}

fn prepared(
    local: &str,
    settings: &ResolvedNodeSettings,
    connections: (&[PeerOwner], Option<&PhoneCalls>),
    worker: &mut MediaWorker,
    text: &mut TextExecutor,
    services: HostServices,
    source: StationAudio<'_>,
) -> Result<PreparedAdapter<Audio, GenerationControl>, RuntimeError> {
    let remote_commands = match source {
        #[cfg(target_os = "linux")]
        StationAudio::Modular(plan) => plan
            .station
            .remote_commands()
            .map_err(|_| RuntimeError::Preparation)?,
    };
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
    let (peers, phone_calls) = connections;
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
        let (inbound, input) = InboundRing::redirected(
            snapshot.input_rate,
            peer.control.inbound_policy(),
            &peer.control.reply_target(),
        )
        .map_err(|_| RuntimeError::Preparation)?;
        let input = match source {
            #[cfg(target_os = "linux")]
            StationAudio::Modular(plan) if plan.process_incoming => {
                let graph = plan
                    .incoming
                    .prepare_peer(snapshot.input_rate, MAXIMUM_FRAMES as u32)
                    .map_err(|_| RuntimeError::Preparation)?;
                ProcessedPeer::modular(input, graph, MAXIMUM_FRAMES)
            }
            #[cfg(target_os = "linux")]
            StationAudio::Modular(_) => ProcessedPeer::new(input),
        };
        let (output, outbound) = LinkAudioQueue::new(48000 / 5, 64)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        let prepared_peer = AudioPeer::new(
            &peer.remote,
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
        awaiting.push((peer.control.connection_generation(), inbound.observer()));
        wakes.push(peer.control.wake());
        redirects.push((
            peer.control.connection_generation(),
            PeerCommand::Redirect {
                inbound,
                outbound,
                dtmf_muting: settings.dtmf_muting,
            },
        ));
    }
    let mut phones = Vec::new();
    #[cfg(target_os = "linux")]
    for (caller, activity, control) in phone_calls
        .into_iter()
        .flat_map(|calls| calls.audio_routes(local))
    {
        let StationAudio::Modular(plan) = source;
        let Some(policy) = plan
            .phone
            .as_ref()
            .filter(|p| p.account.to_string() == caller.account().as_str())
        else {
            continue;
        };
        let settings = &policy.settings;
        use crate::link::{
            private_radio::{Radio, Redirect},
            ring::InboundPolicy,
        };
        use rpt_advanced_core::config::resources::profiles::PhonePlayoutDestination;
        let reply = activity.reply_target();
        let (inbound, input) = InboundRing::redirected(control.rate, InboundPolicy::Phone, &reply)
            .map_err(|_| RuntimeError::Preparation)?;
        let bound = u64::from(settings.timing.maximum_latency_ms)
            .checked_mul(48)
            .and_then(|samples| usize::try_from(samples).ok())
            .ok_or(RuntimeError::Preparation)?;
        let (output, outbound) = LinkAudioQueue::new(bound, 64)
            .map_err(|_| RuntimeError::Preparation)?
            .into_endpoints();
        let mode =
            Mode::phone(settings.playout_destination == PhonePlayoutDestination::LocalAndLinks);
        audio.push(
            AudioPeer::for_phone(
                caller,
                &reply,
                mode,
                ProcessedPeer::new(input),
                output,
                MAXIMUM_FRAMES,
            )
            .map_err(|_| RuntimeError::Preparation)?
            .with_phone_policy(
                settings.isolate_peers,
                policy.hold_isolation,
                Arc::clone(&policy.confirmed),
            ),
        );
        let radio = Radio::new(
            inbound,
            outbound,
            Arc::clone(activity),
            settings.vad,
            settings.timing,
            Arc::clone(&policy.confirmed),
        )
        .map_err(|_| RuntimeError::Preparation)?;
        wakes.push(control.wake.clone());
        phones.push(Redirect::new(reply, control, radio));
    }
    let (mut audio, dispatcher) =
        LinkAudio::new(audio, MAXIMUM_FRAMES).map_err(|_| RuntimeError::Preparation)?;
    #[cfg(target_os = "linux")]
    let mut dispatcher = dispatcher;
    #[cfg(target_os = "linux")]
    {
        let StationAudio::Modular(plan) = source;
        audio = audio.with_media_isolation(&mut dispatcher, Arc::clone(&plan.media_isolation));
    }
    #[cfg(target_os = "linux")]
    let (progress_readers, progress) = match source {
        StationAudio::Modular(plan) => plan.progress.prepare()?,
    };
    audio = audio.with_text(Box::new(TextMix {
        text: text_readers,
        #[cfg(target_os = "linux")]
        progress: progress_readers,
    }));
    let regeneration = if settings.dtmf_regeneration.enabled {
        let (writer, reader) =
            InboundRing::regeneration().map_err(|_| RuntimeError::Preparation)?;
        audio = audio.with_regeneration(ProcessedPeer::new(reader));
        Some((settings.dtmf_regeneration, writer, MAXIMUM_FRAMES))
    } else {
        None
    };
    let status = audio.status();
    let dispatcher = worker
        .prepare(dispatcher, wakes, regeneration)
        .map_err(|_| RuntimeError::Busy)?;
    Ok(PreparedAdapter {
        remote_commands,
        state: audio,
        control: GenerationControl {
            #[cfg(target_os = "linux")]
            progress,
            text_origins,
            dispatcher,
            status,
            redirects,
            phones,
            awaiting,
            local_update: None,
            awaiting_local: false,
        },
        receive_maximum: MAXIMUM_FRAMES,
        transmit_maximum: MAXIMUM_FRAMES,
    })
}

struct TextMix {
    text: Vec<TextReader>,
    #[cfg(target_os = "linux")]
    progress: Vec<phone_progress::Reader>,
}
impl LocalTextInput for TextMix {
    fn render(&mut self, pcm: &mut [f32]) -> bool {
        let mut active = false;
        #[cfg(target_os = "linux")]
        for reader in &mut self.progress {
            active |= reader.render(pcm);
        }
        for reader in &mut self.text {
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
    fn prune_leases(&mut self) {
        prune_leases(&mut self.leases);
        self.media_workers
            .retain(|(local, _)| self.runtime.node(local).is_some());
    }
    /// Start control, private-call and media owners with no selected station generation.
    #[cfg(target_os = "linux")]
    pub(crate) fn management_only(
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
    ) -> Result<Self, RuntimeError> {
        Self::with_capacity(media, services, epoch, 1)
    }

    fn with_capacity(
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        ports: usize,
    ) -> Result<Self, RuntimeError> {
        let phone_calls = PhoneCalls::new(128, 16).map_err(|_| RuntimeError::Preparation)?;
        let text =
            TextExecutor::new(ports, media.clone()).map_err(|_| RuntimeError::Preparation)?;
        let peer_io = PeerIoWorker::start(epoch).map_err(|_| RuntimeError::Device)?;
        Ok(Self {
            reload_text: None,
            reload_failure: None,
            activated_revision: 0,
            #[cfg(target_os = "linux")]
            radio_preparation: Default::default(),
            #[cfg(target_os = "linux")]
            target_selections: Vec::new(),
            remote_pending: Vec::new(),
            text,
            text_notice_after_ms: 0,
            runtime: Runtime::idle(),
            media,
            services,
            leases: Vec::new(),
            media_workers: Vec::new(),
            peer_io,
            peers: Vec::new(),
            phone_calls,
            phone_accounts: Default::default(),
            #[cfg(target_os = "linux")]
            phone_dials: std::collections::VecDeque::new(),
            #[cfg(target_os = "linux")]
            phone_failures: std::collections::VecDeque::new(),
            #[cfg(target_os = "linux")]
            asl: Default::default(),
            #[cfg(target_os = "linux")]
            echo: Default::default(),
            operations: Vec::new(),
            status_post: StatusPostService::default(),
            #[cfg(target_os = "linux")]
            handoff_status: Default::default(),
            pending_link_dials: Default::default(),
            site_acquisition: Default::default(),
            site_delivery: Default::default(),
            site_radio: Default::default(),
            site_transition: None,
            site_retired: Vec::new(),
            epoch,
        })
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
                #[cfg(target_os = "linux")]
                for redirect in control.progress.drain(..) {
                    redirect.activate();
                }
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
        self.regenerate_digit(local, digit.into())
    }
    pub(crate) fn regenerate_digit(
        &mut self,
        local: &str,
        digit: crate::link::regeneration::RegenerationDigit,
    ) -> Result<(), RuntimeError> {
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
        #[cfg(target_os = "linux")]
        self.cancel_target_selections();
        self.runtime.discard_digits();
    }
    fn advance_reload(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        #[cfg(target_os = "linux")]
        if self.radio_preparation.adopting.is_some()
            && let Some(error) = &self.reload_failure
        {
            return Err(error.clone());
        }
        let ready = self.runtime.poll_reload(clock).inspect_err(|error| {
            #[cfg(target_os = "linux")]
            self.core_adoption_error(error);
            #[cfg(not(target_os = "linux"))]
            let _ = error;
        })?;
        if !ready {
            return Ok(());
        }
        self.prune_leases();
        if self.activated_revision == self.runtime.revision() {
            self.reload_text = None;
            return Ok(());
        }
        if let Some(mut text) = self.reload_text.take() {
            self.remote_pending.clear();
            self.text.stop();
            text.start().map_err(|_| RuntimeError::Preparation)?;
            self.text = text;
            self.commit_sites()?;
        }
        #[cfg(target_os = "linux")]
        self.publish_physical_plans();
        self.activated_revision = self.runtime.revision();
        self.activate_dispatchers(clock.now_ms);
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
    /// Name supplied to the actual native protocol owner for this outbound route.
    pub(crate) fn wire_local(&mut self, local: &str, remote: &str) -> Result<String, RuntimeError> {
        if PeerIdentity::from_operator(remote)
            .map_err(|_| RuntimeError::Rejected)?
            .protocol()
            == ProtocolId::AllStarLink
        {
            self.runtime
                .node(local)
                .and_then(|node| node.network_account())
                .map(str::to_owned)
                .ok_or(RuntimeError::Unavailable)
        } else {
            Ok(local.to_owned())
        }
    }
    /// Route reserved Echo aliases before ASL lookup; an address alone cannot admit an Echo QSO.
    pub fn lookup(
        &self,
        local: &str,
        remote: &str,
        source: Option<&str>,
    ) -> Result<String, RuntimeError> {
        self.runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        let identity = PeerIdentity::from_operator(remote).map_err(|_| RuntimeError::Rejected)?;
        if identity.protocol() == ProtocolId::EchoLink {
            #[cfg(target_os = "linux")]
            if self.runtime.modular_configuration().is_some() && !self.echo.accepts(local) {
                return Err(RuntimeError::Rejected);
            }
            return if source.is_none() {
                Ok(format!(
                    "EchoLink/{}",
                    identity.node().ok_or(RuntimeError::Rejected)?
                ))
            } else {
                Err(RuntimeError::Rejected)
            };
        }
        Err(RuntimeError::Rejected)
    }
    /// Admit an inspected peer through common policy. Echo's fresh provider QSO
    /// supplies authentication; only IAX uses an ASL directory/source lookup.
    pub fn incoming_peer(
        &mut self,
        io: PeerIo,
        _source: &str,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let local = if io.identity().protocol() == ProtocolId::AllStarLink {
            self.runtime
                .network_owner(io.local())
                .ok_or(RuntimeError::MissingNode)?
        } else {
            io.local()
        }
        .to_owned();
        #[cfg(target_os = "linux")]
        if io.identity().protocol() == ProtocolId::EchoLink
            && self.runtime.modular_configuration().is_some()
            && !self.echo.accepts(&local)
        {
            return Err(RuntimeError::Rejected);
        }
        if io.identity().protocol() == ProtocolId::Phone {
            if !io.source_media() || io.async_owner().is_none() {
                return Err(RuntimeError::Rejected);
            }
            self.authorize_phone_owner(
                &local,
                io.phone_account().ok_or(RuntimeError::Rejected)?,
                io.generations().0,
            )?;
            let source = self
                .phone_calls
                .admit(io, clock.now_ms)
                .map_err(|_| RuntimeError::Rejected)?;
            let reply = self
                .phone_calls
                .reply(&source)
                .ok_or(RuntimeError::Rejected)?;
            if self
                .phone_calls
                .start_transport(&source, &reply, &mut self.peer_io)
                .is_err()
            {
                self.retire_phone(&source);
                return Err(RuntimeError::Rejected);
            }
            if let Err(error) = self.prepare_login_prompt(
                &source,
                &reply,
                rpt_advanced_core::messages::PhoneAuthenticationPrompt::Login,
            ) {
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
            // ASL channels use their account-bound source/admission proof.
            return Err(RuntimeError::Rejected);
        }
        self.accept_verified_peer(&local, &remote, io, clock)
    }

    /// Finish ordinary peer admission on control after source and metadata validation.
    pub(crate) fn accept_verified_peer(
        &mut self,
        local: &str,
        remote: &str,
        io: PeerIo,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        self.runtime.incoming(local, remote, true)?;
        let result = self.attach_peer(local, remote, Mode::TRANSCEIVE, io, clock);
        if result.is_err() {
            self.reject_peer(local, remote, clock.now_ms);
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
        self.attach_peer_observed(
            local,
            remote,
            mode,
            io,
            clock,
            &mut PendingLinkDial::default(),
        )
    }

    /// Bind an outgoing peer; transfer its outcomes only once the session owns the attempt.
    pub(crate) fn attach_peer_observed(
        &mut self,
        local: &str,
        remote: &str,
        mode: Mode,
        mut io: PeerIo,
        clock: RuntimeClock,
        dial: &mut PendingLinkDial,
    ) -> Result<(), RuntimeError> {
        if let Some(phone) = &dial.phone {
            self.current_phone_reply(local, &phone.reply)?;
        }
        let private = dial.phone.is_some();
        let protocol = io.identity().protocol();
        #[cfg(target_os = "linux")]
        if protocol == ProtocolId::EchoLink
            && self.runtime.modular_configuration().is_some()
            && (io.local() != local || !self.echo.accepts(local))
        {
            return Err(RuntimeError::Rejected);
        }
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
        let wire_local = self.wire_local(local, remote)?;
        let (mut session, control) = PeerSession::prepare(
            io,
            outbound,
            &wire_local,
            remote,
            self.runtime
                .settings(local)
                .ok_or(RuntimeError::MissingNode)?
                .dtmf_muting,
        )
        .map_err(|_| RuntimeError::Preparation)?;
        session.bind_clock(source_clock);
        #[cfg(target_os = "linux")]
        if self.runtime.modular_configuration().is_some() {
            session.bind_media_isolation(Arc::clone(
                &self.radio_preparation.active_plan(local)?.media_isolation,
            ));
        }
        if let Err(session) = self.peer_io.attach(session) {
            session.stop();
            return Err(RuntimeError::Rejected);
        }
        let connection = control.connection_generation();
        for old in self
            .peers
            .iter_mut()
            .filter(|p| p.local == local && p.remote == remote && !p.superseded)
        {
            old.superseded = true;
            self.runtime
                .node(local)
                .expect("running port")
                .links()
                .retire_source(&old.control.reply_target());
        }
        self.peers.push(PeerOwner {
            superseded: false,
            local: local_name,
            remote: remote_name,
            mode,
            group,
            announced: false,
            control,
        });
        // The first refresh can already dispatch the acknowledged redirect.
        let has_status = {
            #[cfg(target_os = "linux")]
            {
                dial.asl.is_some()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };
        if private || has_status {
            self.pending_link_dials
                .insert(connection, std::mem::take(dial));
        }
        if let Err(error) = self.refresh(local, clock) {
            self.detach_connection(local, remote, Some(connection), clock.now_ms);
            return Err(error);
        }
        if let Some(peer) = self
            .peers
            .iter_mut()
            .find(|peer| peer.control.connection_generation() == connection)
        {
            peer.announced = true;
            if private {
                self.runtime
                    .cancel_link_disconnect_notice(local, remote, protocol);
            } else {
                let _ = if protocol == ProtocolId::EchoLink {
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
            self.pump(clock)?;
            if self.try_refresh(local, clock)? {
                return self.pump(clock);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(RuntimeError::Busy)
    }

    /// Attempt one route publication after the existing control pump serviced quiescence.
    fn try_refresh(&mut self, local: &str, clock: RuntimeClock) -> Result<bool, RuntimeError> {
        let settings = self
            .runtime
            .settings(local)
            .ok_or(RuntimeError::MissingNode)?;
        #[cfg(target_os = "linux")]
        let source = StationAudio::Modular(self.radio_preparation.active_plan(local)?);
        #[cfg(not(target_os = "linux"))]
        return Err(RuntimeError::Preparation);
        let candidate = prepared(
            local,
            settings,
            (&self.peers, Some(&self.phone_calls)),
            media_worker(&mut self.media_workers, local)?,
            &mut self.text,
            self.services,
            source,
        );
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(RuntimeError::Busy) => return Ok(false),
            Err(error) => return Err(error),
        };
        let published =
            replacement_completed(self.runtime.replace_adapter(local, candidate, clock.now_ms))?;
        if published {
            self.activate_dispatchers(clock.now_ms);
        }
        Ok(published)
    }
    fn detach(&mut self, local: &str, remote: &str, now_ms: u64) {
        self.detach_connection(local, remote, None, now_ms);
    }
    fn detach_connection(
        &mut self,
        local: &str,
        remote: &str,
        connection: Option<u64>,
        now_ms: u64,
    ) -> bool {
        self.detach_peer(local, remote, connection, now_ms, true)
    }

    fn detach_peer(
        &mut self,
        local: &str,
        remote: &str,
        connection: Option<u64>,
        now_ms: u64,
        notify: bool,
    ) -> bool {
        let match_at = self.peers.iter().position(|peer| {
            peer.local == local
                && peer.remote == remote
                && connection.map_or(!peer.superseded, |id| {
                    id == peer.control.connection_generation()
                })
        });
        let announced = if let Some(index) = match_at {
            let peer = self.peers.remove(index);
            self.finish_link_dial(peer.control.connection_generation(), local, remote, false);
            self.text.cancel(&peer.control.text_origin());
            let announced = (notify && peer.announced).then(|| {
                (
                    peer.control.text_origin().protocol,
                    peer.control.callsign().to_owned(),
                )
            });
            peer.control.stop();
            self.peer_io.reap();
            if peer.superseded {
                return true;
            }
            announced
        } else if connection.is_some() {
            return false;
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
        match_at.is_some()
    }
    fn finish_link_dial(&mut self, connection: u64, local: &str, remote: &str, attached: bool) {
        if let Some(dial) = self.pending_link_dials.remove(&connection) {
            self.settle_link_dial(dial, local, remote, attached);
        }
    }

    /// Deliver the exact outcomes of an acknowledged link or an unpublished attempt.
    pub(crate) fn settle_link_dial(
        &mut self,
        dial: PendingLinkDial,
        local: &str,
        _remote: &str,
        attached: bool,
    ) {
        #[cfg(target_os = "linux")]
        if let Some(asl) = dial.asl {
            self.record_asl_dial(asl, local, _remote, attached);
        }
        if let Some(phone) = dial.phone
            && phone.generation.is_current()
            && (attached || phone.report_failure)
        {
            let result = if attached {
                OperationResult::Completed
            } else {
                OperationResult::Unavailable
            };
            let _ = self.phone_operation_result(local, &phone.reply, result);
        }
    }

    pub(crate) fn reject_peer(&mut self, local: &str, remote: &str, now_ms: u64) {
        self.detach(local, remote, now_ms);
    }
    pub(crate) fn status_text(&mut self, local: &str) -> Result<String, RuntimeError> {
        use std::fmt::Write;
        if self.runtime.shared_site_name() == Some(local) {
            let mut status = String::new();
            self.write_site_status(local, &mut status)?;
            return Ok(status);
        }
        let node = self.runtime.node(local).ok_or(RuntimeError::MissingNode)?;
        let radio_receive = node.radio_receive_status();
        let ingress: Vec<_> = node
            .links()
            .ingress_receive_status()
            .map(|(id, state)| (id.clone(), state))
            .collect();
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
        let _ = writeln!(
            result,
            "  radio.rx.muted={} active={}",
            radio_receive.muted, radio_receive.active
        );
        for (id, rx) in ingress {
            let kind = match id.protocol() {
                ProtocolId::AllStarLink => "allstarlink",
                ProtocolId::EchoLink => "echolink",
                ProtocolId::Phone => "phone",
            };
            let _ = writeln!(
                result,
                "  {} {} rx.muted={} active={}",
                kind,
                id.wire_identity(),
                rx.muted,
                rx.active
            );
        }
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
    /// Apply a command against the station's current registered ASL authority.
    pub(crate) fn command(
        &mut self,
        local: &str,
        operation: rpt_advanced_core::runtime::dtmf::DigitOperation,
        clock: RuntimeClock,
    ) -> Result<LinkEffect, RuntimeError> {
        self.command_with_receipt(local, operation, clock, None)
    }
    pub(crate) fn command_with_receipt(
        &mut self,
        local: &str,
        operation: rpt_advanced_core::runtime::dtmf::DigitOperation,
        clock: RuntimeClock,
        receipt: Option<rpt_advanced_core::runtime::CommandReceipt>,
    ) -> Result<LinkEffect, RuntimeError> {
        let remote = &operation.command.node;
        let verified = operation.command.action == rpt_advanced_core::command::LinkAction::Command
            && operation.digit.is_none()
            && match PeerIdentity::from_operator(remote).map(|id| id.protocol()) {
                Ok(ProtocolId::EchoLink) => self.lookup(local, remote, None).is_ok(),
                #[cfg(target_os = "linux")]
                Ok(ProtocolId::AllStarLink) => {
                    let Some(account) = local
                        .parse()
                        .ok()
                        .and_then(|id| self.asl_account(id).ok())
                        .filter(|account| account.permits(remote))
                    else {
                        return self.runtime.command(local, operation, clock, false);
                    };
                    if !account.directory.registered.contains_key(remote) {
                        return self.begin_target_selection(
                            local,
                            operation,
                            account,
                            clock.now_ms,
                            receipt,
                        );
                    }
                    account
                        .directory
                        .resolve(self.services, remote, None)
                        .is_ok_and(|proof| {
                            proof
                                .destination(&account.directory, remote, None)
                                .is_some()
                        })
                }
                _ => false,
            };
        let result = self.runtime.command(local, operation, clock, verified);
        if let Some(receipt) = receipt {
            receipt.settle(result.is_ok());
        }
        result
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
            return Err(RuntimeError::Rejected);
        }
        match effect {
            LinkEffect::PrivatePhoneMedia(reply, stream) => {
                if !reply.phone_authenticated() {
                    return Err(RuntimeError::Rejected);
                }
                let caller = reply.phone_source().ok_or(RuntimeError::Rejected)?;
                self.queue_phone_reply(caller, &reply, stream)
            }
            LinkEffect::VerifyCommandTarget(operation) => {
                let effect = self.command(local, operation, clock)?;
                self.immediate(local, effect, clock)
            }
            LinkEffect::PrivatePhoneControl(reply, action, permit) => {
                if !permit.current(Some(clock.now_ms)) {
                    return Err(RuntimeError::Rejected);
                }
                self.private_action(local, reply, action, clock)
            }
            LinkEffect::SiteCommand(command, permit) => {
                if !permit.current(Some(clock.now_ms)) {
                    return Err(RuntimeError::Rejected);
                }
                self.authorized_site_command(local, command, clock, Some(&permit))
                    .map_err(|_| RuntimeError::Rejected)
            }
            LinkEffect::Connect(_) | LinkEffect::PhoneDial(..) | LinkEffect::PhoneHangup(_) => {
                Err(RuntimeError::Rejected)
            }
            LinkEffect::Detach(remotes) => {
                self.detach_command_links(local, remotes, clock.now_ms, true);
                Ok(())
            }
            LinkEffect::PrivateDetach { remotes, reply } => {
                // Runtime already committed the removal. Only the reply is revocable.
                self.detach_command_links(local, remotes, clock.now_ms, false);
                self.phone_operation_result(local, &reply, OperationResult::Completed)
            }
            LinkEffect::PrivatePhoneResult(reply, result) => {
                self.phone_operation_result(local, &reply, result)
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
                if let DigitOutcome::Mapped(input) = outcome {
                    let phone = input.phone_reply().cloned();
                    let done =
                        self.runtime
                            .dispatch_mapping(local, input, clock)
                            .and_then(|effect| {
                                if matches!(
                                    effect,
                                    LinkEffect::Connect(_)
                                        | LinkEffect::PhoneDial(..)
                                        | LinkEffect::PhoneHangup(_)
                                ) {
                                    self.defer_link(local, effect);
                                    return Ok(false);
                                }
                                let generic = !matches!(
                                    effect,
                                    LinkEffect::Telemetry(..)
                                        | LinkEffect::PrivatePhoneMedia(..)
                                        | LinkEffect::PrivatePhoneResult(..)
                                        | LinkEffect::PrivatePhoneControl(..)
                                        | LinkEffect::PrivateDetach { .. }
                                        | LinkEffect::CommandQueued(_)
                                );
                                self.immediate(local, effect, clock).map(|()| generic)
                            });
                    if let Some(reply) = phone {
                        let result = match done {
                            Ok(false) => return Ok(()),
                            Ok(true) => OperationResult::Completed,
                            Err(
                                RuntimeError::Rejected
                                | RuntimeError::Unavailable
                                | RuntimeError::Link(_),
                            ) => OperationResult::Unavailable,
                            Err(_) => OperationResult::Failed,
                        };
                        let _ = self.phone_operation_result(local, &reply, result);
                    }
                } else if let DigitOutcome::RemoteCandidate(operation) = outcome {
                    // Selection changes collection of the very next queued digit.
                    let _ = self.command(local, operation, clock);
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
            | LinkEffect::CommandQueued(_)
            | LinkEffect::None => Ok(()),
        }
    }
    fn detach_command_links(
        &mut self,
        local: &str,
        remotes: Vec<String>,
        now_ms: u64,
        notify: bool,
    ) {
        #[cfg(target_os = "linux")]
        let mut receipt = crate::management::live::Disconnect {
            selected_count: remotes.len(),
            removed_count: 0,
            peer_nodes: Vec::new(),
        };
        for remote in remotes {
            let removed = self.detach_peer(local, &remote, None, now_ms, notify);
            #[cfg(target_os = "linux")]
            {
                receipt.removed_count += usize::from(removed);
                if receipt.peer_nodes.len() < crate::management::live::DISCONNECT_PEERS_LIMIT
                    && let Some(node) = PeerIdentity::from_operator(&remote)
                        .ok()
                        .and_then(|id| id.operator_alias())
                {
                    receipt.peer_nodes.push(node);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = removed;
        }
        #[cfg(target_os = "linux")]
        self.record_disconnect(local, receipt);
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
            .find(|peer| peer.local == local && peer.remote == remote && !peer.superseded)
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
        let mut addressed_here = matches!(message.audience, TextAudience::Broadcast);
        for station in self.runtime.node_names() {
            let first = locals.len();
            locals.push(station.to_owned());
            if let Some(profile) = self.runtime.modular_configuration().and_then(|domain| {
                station
                    .parse()
                    .ok()
                    .and_then(|port| domain.protocols.port(port))
            }) {
                if let Some(number) = profile
                    .allstarlink
                    .as_ref()
                    .and_then(|p| p.settings.node_number)
                {
                    locals.push(number.to_string());
                }
                if let Some(account) = profile.echolink.as_ref().filter(|p| p.settings.enabled) {
                    locals.push(rpt_advanced_core::link::EchoAlias::format(
                        account.settings.node_number,
                    ));
                    locals.push(account.settings.callsign.clone());
                }
            }
            if station == local
                && let TextAudience::Node(target) = &message.audience
            {
                addressed_here = locals[first..].contains(target);
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
        if disposition.locally_eligible && addressed_here {
            let Some(settings) =
                self.runtime
                    .modular_configuration()
                    .and_then(|domain| {
                        local.parse().ok().and_then(|port| {
                            domain.scopes.get(
                    &rpt_advanced_core::config::resources::candidate::ResourceScope::Port(port))
                        })
                    })
                    .map(|scope| scope.telemetry.speech.clone())
            else {
                return;
            };
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
        // An ended call may be the last peer. Join stopped registrations on
        // this control turn without waiting for an unrelated peer admission.
        self.expire_maintenance();
        self.peer_io.reap();
        self.reap_abandoned_accounts()?;
        #[cfg(target_os = "linux")]
        self.recheck_phone_dials();
        self.pump_private_input(clock);
        for reply in self.phone_calls.expire_authentication(clock.now_ms) {
            self.retire_phone_source(&reply);
        }
        if let Err(error) = self.advance_reload(clock) {
            self.reload_failure = Some(error);
            self.reload_text = None;
            self.prune_leases();
        }
        self.poll_phone_attachments(clock);
        #[cfg(target_os = "linux")]
        self.poll_target_selections(clock)?;
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
            while let Some(event) = peer.control.received_event() {
                events.push((
                    peer.local.clone(),
                    peer.remote.clone(),
                    peer.control.connection_generation(),
                    peer.superseded,
                    event,
                    peer.control.reply_target(),
                ));
            }
            if peer.control.ended() {
                ended.push((
                    peer.local.clone(),
                    peer.remote.clone(),
                    peer.control.connection_generation(),
                ));
            }
        }
        for (local, remote, connection) in ended {
            self.detach_connection(&local, &remote, Some(connection), clock.now_ms);
        }
        for (local, remote, connection, superseded, event, reply) in events {
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
            let (event, reply, received_at, regeneration) = match event {
                Event::ReceivedDigit(d, r) => (Event::Digit(d), r.reply, r.at, r.regeneration),
                Event::ReceivedText(t, r) => (Event::Text(t), r.reply, r.at, r.regeneration),
                Event::ReceivedUnkey(r) => (Event::RemoteUnkey(r.at), r.reply, r.at, None),
                // Only the session owner can attest a command symbol's time and source.
                Event::Digit(_) | Event::Text(_) | Event::RemoteUnkey(_) => continue,
                other => (other, reply, clock.now_ms, None),
            };
            if !superseded && matches!(event, Event::Digit(_) | Event::Text(_)) {
                let ready = self
                    .runtime
                    .node(&local)
                    .map(|n| n.links().expire_connection_digits(&reply, received_at))
                    .unwrap_or_default();
                for outcome in ready {
                    self.immediate(&local, LinkEffect::Digit(outcome), clock)?;
                }
            }
            let Some(node) = self.runtime.node(&local) else {
                continue;
            };
            match event {
                Event::HumanText(message) => {
                    self.human_text(&local, message);
                }
                Event::Redirected(observer) => {
                    let expected = node
                        .status(clock.now_ms)
                        .active
                        .and_then(|generation| node.adapter_control_mut(generation))
                        .is_some_and(|control| control.acknowledge(connection, &observer));
                    let target = observer.signals().reply_target();
                    if superseded {
                        target.retire();
                        self.finish_link_dial(connection, &local, &remote, false);
                    } else if expected {
                        if target.ended() {
                            self.finish_link_dial(connection, &local, &remote, false);
                            continue;
                        }
                        let id = PeerIdentity::from_operator(&remote)
                            .map_err(|_| RuntimeError::Rejected)?;
                        let attached = node.links().attach_ingress(id, target);
                        self.finish_link_dial(connection, &local, &remote, attached.is_ok());
                        attached.map_err(RuntimeError::Link)?;
                    }
                }
                _ if superseded => continue,
                Event::ReceivedDigit(..) | Event::ReceivedText(..) | Event::ReceivedUnkey(..) => {
                    unreachable!()
                }
                Event::Text(text) => {
                    if let Ok(effect) = node.links().peer_text_received(
                        &remote,
                        &text,
                        (reply, regeneration.clone()),
                        receiving,
                        transitioned,
                        received_at,
                    ) {
                        self.immediate(&local, received_effect(effect, regeneration), clock)?;
                    }
                }
                Event::Digit(digit) => {
                    if let Some(operation) = node.links().peer_digit_received(
                        &remote,
                        digit,
                        received_at,
                        reply,
                        regeneration.clone(),
                    ) {
                        self.immediate(
                            &local,
                            received_effect(LinkEffect::Digit(operation), regeneration),
                            clock,
                        )?;
                    }
                }
                Event::LegacyDigitBoundary => {
                    if let Some(operation) =
                        node.links()
                            .legacy_peer_boundary(&remote, clock.now_ms, reply)
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
                use rpt_advanced_core::runtime::dtmf::DigitEvent;
                if let DigitEvent::Digit { now_ms, .. } | DigitEvent::Unkey { now_ms } = event
                    && let Some(ready) = self
                        .runtime
                        .node(&local)
                        .and_then(|node| node.links().expire_receiver_digits(now_ms))
                {
                    self.immediate(&local, LinkEffect::Digit(ready), clock)?;
                }
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
                            .transmit_observation(clock.now_ms)
                            == Some(true)
                    });
                self.status_post.observe(
                    &local,
                    clock.now_ms,
                    clock.wall_seconds,
                    keyed,
                    node.links().manager().snapshot(),
                );
                let mut ready = node.links().expire_peer_digits(clock.now_ms);
                ready.extend(node.links().advance_macros(clock.now_ms));
                let messages = node.links().due_topology(clock.now_ms);
                for outcome in ready {
                    self.immediate(&local, LinkEffect::Digit(outcome), clock)?;
                }
                for (remote, text) in messages {
                    let _ = self.text(&local, &remote, &text, false);
                }
            }
            let Some(generation) = status.active.or(status.retiring) else {
                continue;
            };
            let mut confirmed_phone = Vec::new();
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
                if status.active == Some(generation) {
                    control.phones.retain_mut(|route| {
                        if !route.submit(&lease.status.source_clock) {
                            return true;
                        }
                        if route.grant().phone_media_allowed() {
                            confirmed_phone.push(route.grant().clone());
                        }
                        false
                    });
                }
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
            for (connection, command) in control.redirects.drain(..) {
                if let Some(peer) = self.peers.iter_mut().find(|peer| {
                    peer.local == local && peer.control.connection_generation() == connection
                }) {
                    if let Err(command) = peer.control.send(command) {
                        pending.push((connection, *command));
                    }
                } else {
                    control.awaiting.retain(|(name, _)| *name != connection);
                }
            }
            control.redirects = pending;
            control.awaiting.retain(|(connection, _)| {
                self.peers.iter().any(|peer| {
                    peer.local == local && peer.control.connection_generation() == *connection
                })
            });
            let redirects_done =
                control.awaiting.is_empty() && control.phones.is_empty() && !control.awaiting_local;
            for reply in confirmed_phone {
                let call = reply
                    .phone_source()
                    .ok_or(RuntimeError::Rejected)?
                    .call_id();
                let id = PeerIdentity::parse(ProtocolId::Phone, &call.to_string())
                    .map_err(|_| RuntimeError::Rejected)?;
                self.runtime
                    .node(&local)
                    .ok_or(RuntimeError::MissingNode)?
                    .links()
                    .attach_ingress(id, reply.clone())
                    .map_err(RuntimeError::Link)?;
                self.phone_calls.confirm_radio_attachment(&reply);
            }
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
    /// Gate audio, stop media distribution, quiesce peers, then acknowledge exact generations.
    pub fn stop(&mut self, now_ms: u64) -> bool {
        #[cfg(target_os = "linux")]
        self.cancel_target_selections();
        let mut cleared = true;
        for (_, lease) in &self.leases {
            let mut lease = lease.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(grant) = &lease.maintenance {
                grant.revoke();
            }
            cleared &= lease.clear_manual().is_ok();
        }
        if !cleared {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            self.phone_dials.clear();
            self.phone_failures.clear();
        }
        self.retire_phones();
        if self.phone_accounts.stop(self.services) != Ok(true) {
            return false;
        }

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
        self.pending_link_dials.clear();
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
        let radio_stopped = self.runtime.stop(now_ms);
        #[cfg(target_os = "linux")]
        if self.asl.stop(self.services) != Ok(true) {
            return false;
        }
        #[cfg(target_os = "linux")]
        if !self.echo.stop(self.services) {
            return false;
        }
        radio_stopped
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
        self.retire_phone_source(&reply);
        true
    }
    fn retire_phone_source(&mut self, reply: &PeerReply) {
        for (local, _) in self.runtime.status(0) {
            if let Some(node) = self.runtime.node(&local) {
                node.links().retire_source(reply);
            }
        }
    }
    fn retire_phones(&mut self) {
        for reply in self.phone_calls.retire_all() {
            self.retire_phone_source(&reply);
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

#[cfg(all(test, target_os = "linux"))]
#[path = "host_fixture_tests.rs"]
mod fixture;

#[cfg(test)]
#[path = "host_tests.rs"]
pub(crate) mod tests;

fn received_effect(
    effect: LinkEffect,
    origin: Option<rpt_advanced_core::link::RegenerationOrigin>,
) -> LinkEffect {
    match effect {
        LinkEffect::Digit(DigitOutcome::ReceivedDigit(d, _)) => origin
            .filter(|o| o.current())
            .map_or(LinkEffect::None, |o| {
                LinkEffect::Digit(DigitOutcome::ReceivedDigit(d, o))
            }),
        other => other,
    }
}
