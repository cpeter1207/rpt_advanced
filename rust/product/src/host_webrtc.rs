//! A WebRTC connection uses the existing generation-owned station ingress and egress.
use super::*;
use crate::link::{
    ring::InboundObserver,
    session::status::ReceiveStatus,
    source_clock::SourceClock,
    status_audio::{PeerMeasurements, StageReading},
};
use crate::management::live::WebRtcObservation;
use rpt_advanced_core::{
    config::resources::candidate::ResourceId, link::PeerSignals, runtime::links::NodeLinkControl,
};
use std::sync::{
    atomic::AtomicBool,
    mpsc::{self, Receiver, SyncSender, TrySendError},
};

/// Exact station endpoint. Drops revoke queued input and output for its old graph.
pub(crate) struct Endpoint {
    pub(crate) ingress: InboundProducer,
    pub(crate) egress: LinkAudioConsumer,
    pub(crate) clock: Arc<SourceClock>,
    pub(crate) isolation: Arc<rpt_advanced_core::link::MediaIsolation>,
    installed: Arc<AtomicBool>,
    admitted: Arc<AtomicBool>,
}
impl Endpoint {
    #[cfg(test)]
    pub(crate) fn for_playout_test(clock: Arc<SourceClock>, egress: LinkAudioConsumer) -> Self {
        let (ingress, _) = InboundRing::open(48_000, InboundPolicy::Peer).unwrap();
        Self {
            ingress,
            egress,
            clock,
            isolation: Arc::default(),
            installed: Arc::new(AtomicBool::new(true)),
            admitted: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(crate) fn admitted(&self) -> bool {
        !self.ingress.signals().ended() && self.admitted.load(Ordering::Acquire)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.ingress.signals().end();
    }
}

/// The exclusive media owner receives graph changes without blocking station control.
pub(crate) struct MediaRoutes {
    grant: PeerReply,
    next: Receiver<Endpoint>,
    current: Option<PeerReply>,
}
impl MediaRoutes {
    pub(crate) fn live(&self) -> bool {
        !self.grant.ended()
    }
    pub(crate) fn take(&mut self) -> Option<Endpoint> {
        let endpoint = self.next.try_recv().ok()?;
        if !self.live() {
            return None;
        }
        if let Some(old) = self
            .current
            .replace(endpoint.ingress.signals().reply_target())
        {
            old.retire();
        }
        endpoint.installed.store(true, Ordering::Release);
        Some(endpoint)
    }
}
impl Drop for MediaRoutes {
    fn drop(&mut self) {
        self.grant.close_webrtc_session();
    }
}

pub(super) struct Application {
    pub(super) port: ResourceId,
    session: ResourceId,
    pub(super) binding: ResourceId,
    pub(super) keys: Option<Arc<ReceiveStatus>>,
    transceive: bool,
    owner: Arc<GenerationWork>,
    observation: Arc<Mutex<WebRtcObservation>>,
    observer: Option<InboundObserver>,
    clock: Arc<SourceClock>,
    reply: PeerReply,
    next: SyncSender<Endpoint>,
}
impl Application {
    pub(super) fn live(&self) -> bool {
        self.owner.is_current() && !self.reply.ended()
    }
    pub(super) fn status_observation(&self) -> Option<WebRtcObservation> {
        if !self.live() {
            return None;
        }
        let mut observed = self.observation.try_lock().ok()?.clone();
        if observed.graph_generation == 0 {
            return None;
        }
        if self.transceive
            && let Some(observer) = &self.observer
            && !observer.signals().ended()
        {
            let graph = observed.graph_generation;
            let absent = || StageReading {
                rate: 48_000,
                available: false,
                measurement: None,
            };
            observed.ingress = Some(Arc::new(PeerMeasurements {
                ingress_graph_generation: Some(graph),
                decoded: StageReading {
                    rate: 48_000,
                    available: true,
                    measurement: observed.decoded.filter(|m| m.graph_generation == graph),
                },
                mix_minus: absent(),
                codec_input: absent(),
                native_ingress: observer.native_ingress(),
                provider_output: observer.provider_output(),
            }));
        }
        Some(observed)
    }

    pub(super) fn observe_ingress(&mut self, observer: InboundObserver) {
        self.observer = Some(observer);
    }
    pub(super) fn prepare(
        &self,
        plan: &native::RadioPlan,
    ) -> Result<(AudioPeer<ProcessedPeer<InboundConsumer>>, Redirect), RuntimeError> {
        let PeerAudio {
            inbound: ingress,
            input,
            output,
            outbound: egress,
        } = PeerAudio::prepare(
            48_000,
            InboundPolicy::Peer,
            &self.reply,
            self.transceive.then_some(plan),
            self.transceive,
            48_000 / 5,
        )?;
        let signal = ingress.signals().reply_target();
        signal.bind_media_isolation(Arc::clone(&plan.media_isolation));
        let admitted = Arc::new(AtomicBool::new(false));
        let peer = AudioPeer::for_application(
            &signal,
            self.transceive,
            Arc::clone(&admitted),
            input,
            output,
            MAXIMUM_FRAMES,
        )
        .map_err(RuntimeError::Link)?;
        let installed = Arc::new(AtomicBool::new(false));
        let redirect = Redirect {
            binding: self.binding,
            observer: ingress.observer(),
            observation: Arc::clone(&self.observation),
            installed: Arc::clone(&installed),
            admitted: Arc::clone(&admitted),
            endpoint: Some(Endpoint {
                ingress,
                egress,
                clock: Arc::clone(&self.clock),
                isolation: Arc::clone(&plan.media_isolation),
                installed,
                admitted,
            }),
            next: self.next.clone(),
        };
        Ok((peer, redirect))
    }
}
impl Drop for Application {
    fn drop(&mut self) {
        self.reply.close_webrtc_session();
    }
}

/// A graph exchange becomes registered only when the receiving owner has it.
pub(super) struct Redirect {
    binding: ResourceId,
    observer: InboundObserver,
    observation: Arc<Mutex<WebRtcObservation>>,
    installed: Arc<AtomicBool>,
    admitted: Arc<AtomicBool>,
    endpoint: Option<Endpoint>,
    next: SyncSender<Endpoint>,
}
impl Redirect {
    pub(super) fn submit(&mut self) -> RedirectState {
        if self.observer.signals().ended() {
            return RedirectState::Closed;
        }
        if let Some(endpoint) = self.endpoint.take() {
            match self.next.try_send(endpoint) {
                Ok(()) => {}
                Err(TrySendError::Full(endpoint)) => {
                    self.endpoint = Some(endpoint);
                    return RedirectState::Waiting;
                }
                Err(TrySendError::Disconnected(_)) => return RedirectState::Closed,
            }
        }
        if self.installed.load(Ordering::Acquire) {
            RedirectState::Installed(Receipt {
                binding: self.binding,
                observer: self.observer.clone(),
                observation: Arc::clone(&self.observation),
                admitted: Arc::clone(&self.admitted),
            })
        } else {
            RedirectState::Waiting
        }
    }
}

pub(super) enum RedirectState {
    Waiting,
    Closed,
    Installed(Receipt),
}

pub(super) struct Receipt {
    binding: ResourceId,
    observer: InboundObserver,
    observation: Arc<Mutex<WebRtcObservation>>,
    admitted: Arc<AtomicBool>,
}
impl Receipt {
    pub(super) fn admit(
        self,
        links: &mut NodeLinkControl,
        generation: u64,
    ) -> Result<Option<(ResourceId, InboundObserver)>, RuntimeError> {
        let route = self.observer.signals().reply_target();
        if !route.ended() {
            links
                .attach_ingress(PeerIdentity::webrtc(self.binding.uuid()), route.clone())
                .inspect_err(|_| route.close_webrtc_session())
                .map_err(RuntimeError::Link)?;
            {
                let mut view = self
                    .observation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                view.graph_generation = generation;
                view.anchor = None;
                view.codec_input = None;
                view.decoded = None;
            }
            self.admitted.store(true, Ordering::Release);
            return Ok(Some((self.binding, self.observer)));
        }
        Ok(None)
    }
}

pub(crate) struct PortAdmission {
    pub(crate) port: ResourceId,
    pub(crate) session: ResourceId,
    pub(crate) transceive: bool,
    pub(crate) keys: Option<Arc<ReceiveStatus>>,
    pub(crate) closed: Arc<AtomicBool>,
    pub(crate) observation: Arc<Mutex<WebRtcObservation>>,
}

impl Host {
    pub(super) fn retire_applications(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        let ports: std::collections::BTreeSet<_> = self
            .applications
            .iter()
            .filter(|app| !app.live())
            .map(|app| app.port)
            .collect();
        for id in ports {
            let local = id.to_string();
            if self.runtime.node(&local).is_none() || self.try_refresh(&local, clock)? {
                self.applications.retain(|app| app.port != id || app.live());
            }
        }
        Ok(())
    }

    /// Called by serialized control after the approved connection is transport-ready.
    pub(crate) fn attach_webrtc(
        &mut self,
        request: PortAdmission,
        owner: Arc<GenerationWork>,
        clock: RuntimeClock,
    ) -> Result<MediaRoutes, RuntimeError> {
        let PortAdmission {
            port,
            session,
            transceive,
            keys,
            closed,
            observation,
        } = request;
        let local = port.to_string();
        let current = self
            .runtime
            .node(&local)
            .and_then(|node| node.command_work())
            .ok_or(RuntimeError::MissingNode)?;
        if closed.load(Ordering::Acquire)
            || !owner.is_current()
            || !Arc::ptr_eq(&owner, &current)
            || self
                .applications
                .iter()
                .any(|app| app.session == session && app.live())
        {
            return Err(RuntimeError::Rejected);
        }
        let source_clock = site::live_station(&self.leases, &local, true)
            .map(|lease| Arc::clone(&lease.status.source_clock))
            .ok_or(RuntimeError::Unavailable)?;
        self.applications
            .try_reserve(1)
            .map_err(|_| RuntimeError::Preparation)?;
        let binding = observation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .binding;
        let grant = PeerSignals::for_revocable_webrtc(binding.uuid(), closed).reply_target();
        let (next, receive) = mpsc::sync_channel(1);
        self.applications.push(Application {
            port,
            session,
            binding,
            transceive,
            keys,
            owner,
            observation,
            observer: None,
            clock: source_clock,
            reply: grant.clone(),
            next,
        });
        let routes = MediaRoutes {
            grant,
            next: receive,
            current: None,
        };
        self.refresh(&local, clock)?;
        Ok(routes)
    }
}
