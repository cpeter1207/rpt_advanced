//! WebRTC provider tasks retired with the management service.

use super::{
    Approval, CandidateWork, Direction, Fault, Id, OfferWork, Receipt, Transport, WorkKey,
};
use crate::{
    host::webrtc::{MediaRoutes, PortAdmission},
    lifecycle::{Engine, MediaAdmission},
    management::live::WebRtcObservation,
};
use rptadv_jitter_client::{JITTER_LIBRARY_NAME, JitterProvider};
use rptadv_peer_support::{
    https::BrowserPrincipal,
    sealed_records::SecretBytes,
    webrtc::{
        self, ChannelId, ChannelKind, Event, MediaKind, Mid, Pt, SdpOffer, UdpDriver,
        opus::OpusProvider,
    },
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    runtime,
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, interval, sleep_until},
};

mod playout;
mod receive;
mod turn;
pub(crate) use turn::TurnAccess;

const AUTHORIZATION_LIFETIME: Duration = Duration::from_secs(60);
const SETUP_LIFETIME: Duration = Duration::from_secs(20);
const TIMING_LIFETIME_MS: u16 = 100;
const DATA_MESSAGES_PER_SECOND: [u16; 2] = [128, 256];

const READY: u8 = 6;
const SETUP_STAGES: [&str; READY as usize] = [
    "socket bind", "candidate exchange", "ICE and DTLS", "DCEP negotiation",
    "station admission", "station audio attachment",
];

struct Running {
    key: WorkKey,
    closed: Arc<AtomicBool>,
    authorization: watch::Sender<Instant>,
    candidates: mpsc::Sender<CandidateWork>,
    receipts: mpsc::Receiver<Receipt>,
    worker: JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// The production media owner runs one serial task per session on the shared executor.
pub(crate) struct Sessions {
    station: Weak<Engine>,
    binding: IpAddr,
    turn: Option<Arc<TurnAccess>>,
    opus: Arc<OpusProvider>,
    jitter: Option<Arc<JitterProvider>>,
    running: HashMap<Id, Running>,
    executor: runtime::Runtime,
}

impl Sessions {
    pub(crate) fn new(
        station: Weak<Engine>,
        binding: IpAddr,
        turn: Option<TurnAccess>,
    ) -> Result<Self, Fault> {
        let opus = OpusProvider::load().map_err(|_| Fault::Unavailable)?;
        let jitter = opus
            .decoder(receive::MAX_RTP_PAYLOAD)
            .ok()
            .filter(|decoder| decoder.supports_provenance())
            .and_then(|_| {
                let directory = Path::new(env!("RPT_ADVANCED_LIBDIR"));
                // This is the same trusted installation path used for native providers.
                unsafe { JitterProvider::load(&directory.join(JITTER_LIBRARY_NAME)) }.ok()
            });
        let executor = runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rpt-webrtc")
            .build()
            .map_err(|_| Fault::Unavailable)?;
        Ok(Self {
            station,
            binding,
            turn: turn.map(Arc::new),
            opus: Arc::new(opus),
            jitter,
            executor,
            running: HashMap::new(),
        })
    }

    fn current(&mut self, key: WorkKey) -> Result<&mut Running, Fault> {
        self.running
            .get_mut(&key.session)
            .filter(|running| running.key == key)
            .ok_or(Fault::Gone)
    }
}

impl Transport for Sessions {
    fn approve(
        &mut self,
        _principal: &BrowserPrincipal,
        port: Id,
        direction: Direction,
        revision: &str,
        station_generation: u64,
        now: Instant,
    ) -> Result<Approval, Fault> {
        let engine = self.station.upgrade().ok_or(Fault::Unavailable)?;
        let active = engine.modular_observed();
        if !active.status_ready
            || active.active_revision.as_deref() != Some(revision)
            || active.owners.get(&port) != Some(&station_generation)
        {
            return Err(Fault::Unavailable);
        }
        if direction == Direction::Transceive && self.jitter.is_none() {
            return Err(Fault::Unavailable);
        }
        Ok(Approval {
            direction,
            until: now + AUTHORIZATION_LIFETIME,
        })
    }

    fn offer(
        &mut self,
        work: OfferWork,
        revision: &str,
        station_generation: u64,
        until: Instant,
    ) -> Result<(), Fault> {
        let key = work.key;
        let (permit, inhibit) = self
            .station
            .upgrade()
            .and_then(|engine| engine.webrtc_admission(revision))
            .ok_or(Fault::Unavailable)?;
        let text = std::str::from_utf8(work.sdp.expose_bytes()).map_err(|_| Fault::Invalid)?;
        let offer = SdpOffer::from_sdp_string(text).map_err(|_| Fault::Invalid)?;
        let (rtc, answer) =
            webrtc::accept_offer(Instant::now(), offer).map_err(|_| Fault::Invalid)?;
        let answer = SecretBytes::new(answer.to_sdp_string().into_bytes());
        if self.running.contains_key(&key.session) {
            return Err(Fault::Conflict);
        }
        self.running
            .try_reserve(1)
            .map_err(|_| Fault::Unavailable)?;
        let closed = Arc::new(AtomicBool::new(false));
        let (authorization, grant) = watch::channel(until);
        let binding = work.binding;
        let progress = Arc::new(AtomicU8::new(0));
        let observation = Arc::new(Mutex::new(WebRtcObservation {
            binding,
            connection_generation: key.ice_generation,
            transceive: work.direction == Direction::Transceive,
            graph_generation: 0,
            anchor: None,
            codec_input: None,
            decoded: None,
            ingress: None,
            sender_rate: None,
            receiver_quality: Vec::new(),
        }));
        let (candidates, incoming) = mpsc::channel(16);
        let (send_receipts, receipts) = mpsc::channel(16);
        let start = Start {
            key,
            station: self.station.clone(),
            station_generation,
            permit,
            inhibit,
            revision: revision.to_owned(),
            listen_address: self.binding,
            turn: self.turn.clone(),
            opus: Arc::clone(&self.opus),
            direction: work.direction,
            jitter: self.jitter.clone(),
            authorization: grant,
            binding,
            epochs: work.epochs,
            observation,
            candidates: incoming,
            receipts: send_receipts,
            closed: Arc::clone(&closed),
            progress: Arc::clone(&progress),
        };
        let station = self.station.clone();
        let canceled = Arc::clone(&closed);
        let worker = self.executor.spawn(async move {
            // A dropped receipt sender reports terminal state without filling the queue.
            let _ = serve(start, rtc, answer).await;
            if let Some(&stage) = SETUP_STAGES.get(usize::from(progress.load(Ordering::Acquire)))
                && !canceled.load(Ordering::Acquire)
                && let Some(engine) = station.upgrade()
            {
                engine.webrtc_setup_notice(stage);
            }
        });
        self.running.insert(
            key.session,
            Running {
                key,
                closed,
                authorization,
                candidates,
                receipts,
                worker,
            },
        );
        Ok(())
    }

    fn candidate(&mut self, work: CandidateWork) -> Result<(), Fault> {
        self.current(work.key.offer)?
            .candidates
            .try_send(work)
            .map_err(|_| Fault::Unavailable)
    }

    fn renew(&mut self, key: WorkKey, until: Instant) -> Result<(), Fault> {
        self.current(key)?
            .authorization
            .send(until)
            .map_err(|_| Fault::Gone)
    }

    fn close(&mut self, key: WorkKey) {
        if self.current(key).is_ok() {
            self.running.remove(&key.session);
        }
    }

    fn next_receipt(&mut self) -> Option<Receipt> {
        let mut ended = None;
        for running in self.running.values_mut() {
            match running.receipts.try_recv() {
                Ok(receipt) => return Some(receipt),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    ended = Some(running.key);
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) if running.worker.is_finished() => {
                    ended = Some(running.key);
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
        }
        ended.map(|key| {
            self.running.remove(&key.session);
            Receipt::Closed(key)
        })
    }
}

struct Start {
    key: WorkKey,
    station: Weak<Engine>,
    station_generation: u64,
    permit: MediaAdmission,
    inhibit: Arc<AtomicBool>,
    revision: String,
    listen_address: IpAddr,
    turn: Option<Arc<TurnAccess>>,
    opus: Arc<OpusProvider>,
    direction: Direction,
    jitter: Option<Arc<JitterProvider>>,
    authorization: watch::Receiver<Instant>,
    binding: Id,
    epochs: Arc<AtomicU64>,
    observation: Arc<Mutex<WebRtcObservation>>,
    candidates: mpsc::Receiver<CandidateWork>,
    receipts: mpsc::Sender<Receipt>,
    closed: Arc<AtomicBool>,
    progress: Arc<AtomicU8>,
}

fn report(outbox: &mpsc::Sender<Receipt>, receipt: Receipt) -> Result<(), ()> {
    outbox.try_send(receipt).map_err(|_| ())
}

async fn serve(session: Start, rtc: webrtc::Rtc, answer: SecretBytes) -> Result<(), ()> {
    let key = session.key;
    let socket = UdpSocket::bind(SocketAddr::new(session.listen_address, 0))
        .await
        .map_err(|_| ())?;
    let mut driver = UdpDriver::new(rtc, socket).map_err(|_| ())?;
    session.progress.store(1, Ordering::Release);
    report(&session.receipts, Receipt::Answer { key, sdp: answer })?;
    report(
        &session.receipts,
        Receipt::LocalCandidate {
            key,
            sequence: 0,
            contents: Some(SecretBytes::new(driver.candidate().as_bytes().to_vec())),
        },
    )?;
    let mut next_sequence = 1;
    let mut relay_expiry = None;
    if let Some(turn) = &session.turn
        && turn
            .gather(&mut driver, session.listen_address)
            .await
            .is_ok()
        && let Some(candidate) = driver.relay_candidate()
    {
        report(
            &session.receipts,
            Receipt::LocalCandidate {
                key,
                sequence: next_sequence,
                contents: Some(SecretBytes::new(candidate.as_bytes().to_vec())),
            },
        )?;
        next_sequence += 1;
        relay_expiry = Some(turn.expires());
    }
    report(
        &session.receipts,
        Receipt::LocalCandidate {
            key,
            sequence: next_sequence,
            contents: None,
        },
    )?;
    session.progress.store(2, Ordering::Release);
    let outcome = media(session, &mut driver, relay_expiry).await;
    let _ = driver
        .close_relay(Instant::now() + Duration::from_millis(250))
        .await;
    outcome
}

async fn media(
    mut session: Start,
    driver: &mut UdpDriver,
    relay_expiry: Option<Instant>,
) -> Result<(), ()> {
    let key = session.key;
    let mut channels = [None::<ChannelId>; 2];
    let mut message_window = Instant::now();
    let mut received_messages = [0; 2];
    let mut track = None::<(Mid, Pt)>;
    let mut routes = None::<MediaRoutes>;
    let mut playout = playout::Playout::new(
        &session.opus,
        key,
        session.binding,
        Arc::clone(&session.epochs),
        Arc::clone(&session.observation),
    )?;
    let ready = webrtc::channel::Control::Ready {
        binding: session.binding.to_string(),
        ice_generation: webrtc::channel::Coordinate(key.ice_generation),
    }
    .encode()
    .ok_or(())?;
    let mut input = if session.direction == Direction::Transceive {
        Some(receive::Receiver::new(
            &session.opus,
            session.jitter.as_ref().ok_or(())?,
            key,
            session.binding,
            Arc::clone(&session.observation),
        )?)
    } else {
        None
    };
    let mut endpoint = None::<crate::host::webrtc::Endpoint>;
    let mut announced = false;
    let start_deadline = Instant::now() + SETUP_LIFETIME;
    let mut pace = interval(Duration::from_millis(10));
    pace.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let until = *session.authorization.borrow_and_update();
        let until = relay_expiry.map_or(until, |expiry| expiry.min(until));
        let wake = if announced {
            until
        } else {
            until.min(start_deadline)
        };
        if Instant::now() >= wake
            || session.authorization.has_changed().is_err()
            || session.closed.load(Ordering::Acquire)
            || !session.permit.current()
            || (routes.is_some() && !driver.connection_state().secure_connected())
            || session.inhibit.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let lease_wakeup = input
            .as_ref()
            .and_then(receive::Receiver::deadline)
            .unwrap_or(wake);
        tokio::select! {
            _ = sleep_until(wake.into()) => return Ok(()),
            _ = sleep_until(lease_wakeup.into()), if lease_wakeup < wake => {
                if let Some(endpoint) = &endpoint && let Some(input) = &mut input {
                    input.expire(Instant::now(), endpoint)?;
                }
            }
            authorized = session.authorization.changed() => authorized.map_err(|_| ())?,
            candidate = session.candidates.recv() => {
                let candidate = candidate.ok_or(())?;
                if let Some(contents) = candidate.contents {
                    let text = std::str::from_utf8(contents.expose_bytes()).map_err(|_| ())?;
                    driver.add_remote_candidate(text).map_err(|_| ())?;
                }
                report(&session.receipts, Receipt::CandidateAccepted(candidate.key))?;
            }
            _ = pace.tick() => {
                if let Some(routes) = &mut routes {
                    if !routes.live() {
                        return Ok(());
                    }
                    if let Some(next) = routes.take() {
                        if let Some(input) = &mut input {
                            input.endpoint_changed();
                        }
                        endpoint = Some(next);
                        playout.discontinue()?;
                    }
                    if let Some(endpoint) = &mut endpoint
                        && endpoint.admitted()
                        && let Some(track) = track
                    {
                        if !announced {
                            if !driver.write_channel(ChannelKind::Control, false, &ready, 4096, 4096)
                                .map_err(|_| ())?
                            {
                                continue;
                            }
                            report(&session.receipts, Receipt::Attached(key))?;
                            announced = true;
                            session.progress.store(READY, Ordering::Release);
                        }
                        if Instant::now() >= wake
                            || !session.permit.current()
                            || session.inhibit.load(Ordering::Acquire)
                        {
                            return Ok(());
                        }
                        if let Some(input) = &mut input {
                            input.tick(Instant::now(), endpoint)?;
                        }
                        playout.send(endpoint, driver, track)?;
                    }
                } else if track.is_some() && channels.iter().all(Option::is_some)
                    && driver.connection_state().secure_connected()
                {
                    let Some(engine) = session.station.upgrade() else { return Ok(()) };
                    let station = session.station_generation;
                    let direction = session.direction;
                    let revision = session.revision.clone();
                    let clock = engine.clock();
                    session.progress.store(4, Ordering::Release);
                    let closed = Arc::clone(&session.closed);
                    let observation = Arc::clone(&session.observation);
                    routes = Some(tokio::task::spawn_blocking(move || {
                        engine.run(move |host| {
                            if closed.load(Ordering::Acquire) || Instant::now() >= wake {
                                return Err(rpt_advanced_core::runtime::RuntimeError::Rejected);
                            }
                            let local = key.port.to_string();
                            if !host.modular_revision_matches(&revision) {
                                return Err(rpt_advanced_core::runtime::RuntimeError::Rejected);
                            }
                            let owner = host.runtime.node(&local).and_then(|node| node.command_work())
                                .filter(|owner| owner.id() == station)
                                .ok_or(rpt_advanced_core::runtime::RuntimeError::Rejected)?;
                            host.attach_webrtc(PortAdmission {
                                port: key.port,
                                session: key.session,
                                transceive: direction == Direction::Transceive,
                                closed,
                                observation,
                            }, owner, clock)
                        })
                    }).await.map_err(|_| ())?.map_err(|_| ())?);
                    session.progress.store(5, Ordering::Release);
                }
            }
            event = driver.next_event() => {
                match event.map_err(|_| ())? {
                    Event::MediaAdded(media) if media.kind == MediaKind::Audio => {
                        let expected = if session.direction == Direction::Transceive {
                            webrtc::Direction::SendRecv
                        } else {
                            webrtc::Direction::SendOnly
                        };
                        if track.is_some() || media.direction != expected {
                            return Err(());
                        }
                        let pt = driver.opus_payload(media.mid).ok_or(())?;
                        track = Some((media.mid, pt));
                    }
                    Event::Connected => {
                        session.progress.fetch_max(3, Ordering::Release);
                    }
                    Event::ChannelOpen(id, _) => match driver.bind_channel(id, TIMING_LIFETIME_MS) {
                        Some(ChannelKind::Control) => channels[0] = Some(id),
                        Some(ChannelKind::Timing) => channels[1] = Some(id),
                        None => {}
                    },
                    Event::ChannelData(message) => {
                        if let Some(slot) = channels.iter().position(|id| *id == Some(message.id)) {
                            let now = Instant::now();
                            if now.duration_since(message_window) >= Duration::from_secs(1) {
                                message_window = now;
                                received_messages.fill(0);
                            }
                            received_messages[slot] += 1;
                            if received_messages[slot] > DATA_MESSAGES_PER_SECOND[slot] {
                                return Err(());
                            }
                            if !message.binary
                                && let Some(input) = &mut input
                                && let Some(endpoint) = &endpoint
                            {
                                if slot == 0 {
                                    input.control(&message.data, endpoint, driver, now)?;
                                } else {
                                    input.timing(&message.data, endpoint, now)?;
                                }
                            }
                        }
                    }
                    Event::RtpPacket(packet) => {
                        if let Some(input) = &mut input
                            && let Some(endpoint) = &endpoint
                            && let Some((mid, payload)) = track
                            && driver.receive_track(packet.header.ssrc) == Some(mid)
                        {
                            input.packet(&packet, driver, payload, endpoint, Instant::now())?;
                        }
                    }
                    Event::SenderFeedback(report) => {
                        if let Some(input) = &mut input
                            && track.is_some_and(|(mid, _)| mid == report.mid)
                        {
                            input.sender_feedback(&report);
                        }
                    }
                    Event::ChannelClose(id) if channels.contains(&Some(id)) => return Ok(()),
                    Event::Closed => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}
