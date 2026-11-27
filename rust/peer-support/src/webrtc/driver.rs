//! Drive one RTC generation through its local UDP and optional TURN sockets.

use std::{
    future::pending,
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};

use str0m::{
    Candidate, Event, Input, Output, Rtc,
    media::Mid,
    net::{Protocol, Receive, Transmit},
    rtp::{RtpWrite, Ssrc},
};
use tokio::net::{TcpStream, UdpSocket};

use super::relay::{Relay, RelayDatagram, RelayError, RelayOutput, RelaySend, RelayTransport};

const TURN_WRITE_TIME: Duration = Duration::from_secs(2);

/// One connected socket for a TURN client. A TCP socket carries plain or TLS
/// bytes as selected by the authenticated session's TURN configuration.
pub enum RelaySocket {
    /// A datagram socket already connected to the TURN server.
    Udp(UdpSocket),
    /// A stream socket already connected to the TURN server.
    Tcp(TcpStream),
}

impl RelaySocket {
    fn addresses(&self, transport: &RelayTransport) -> io::Result<(SocketAddr, SocketAddr)> {
        match (self, transport) {
            (Self::Udp(socket), RelayTransport::Udp) => {
                Ok((socket.local_addr()?, socket.peer_addr()?))
            }
            (Self::Tcp(socket), RelayTransport::Tcp | RelayTransport::Tls { .. }) => {
                Ok((socket.local_addr()?, socket.peer_addr()?))
            }
            _ => Err(invalid_transport()),
        }
    }

    async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Udp(socket) => socket.recv(buffer).await,
            Self::Tcp(socket) => loop {
                socket.readable().await?;
                match socket.try_read(buffer) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "TURN stream closed",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    result => return result,
                }
            },
        }
    }

    async fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Udp(socket) => {
                let written = socket.send(bytes).await?;
                if written != bytes.len() {
                    return Err(partial_datagram());
                }
                Ok(written)
            }
            Self::Tcp(socket) => loop {
                socket.writable().await?;
                match socket.try_write(bytes) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    result => return result,
                }
            },
        }
    }
}

struct DirectLeg {
    socket: UdpSocket,
    local: SocketAddr,
    candidate: String,
    receiver: Box<[u8]>,
    pending: Option<Transmit>,
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
    expires: Instant,
}

struct RelayLeg {
    socket: RelaySocket,
    relay: Relay,
    candidate: Candidate,
    candidate_text: String,
    receiver: Box<[u8]>,
    pending: Option<PendingWrite>,
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => pending::<()>().await,
    }
}

fn invalid_transport() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid WebRTC transport state")
}

fn relay_error(error: RelayError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("TURN relay {error:?}"))
}

fn relay_closed() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "TURN allocation closed")
}

fn partial_datagram() -> io::Error {
    io::Error::new(io::ErrorKind::WriteZero, "partial WebRTC datagram")
}

fn queue(pending: &mut Option<PendingWrite>, bytes: Vec<u8>) {
    debug_assert!(pending.is_none());
    *pending = Some(PendingWrite {
        bytes,
        offset: 0,
        expires: Instant::now() + TURN_WRITE_TIME,
    });
}

async fn flush(socket: &RelaySocket, pending: &mut Option<PendingWrite>) -> io::Result<()> {
    if let Some(write) = pending {
        tokio::time::timeout_at(write.expires.into(), async {
            while write.offset < write.bytes.len() {
                let count = socket.write(&write.bytes[write.offset..]).await?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "TURN stream stopped writing",
                    ));
                }
                write.offset += count;
            }
            Ok(())
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TURN write timed out"))??;
    }
    *pending = None;
    Ok(())
}

impl RelayLeg {
    async fn allocate(
        socket: RelaySocket,
        transport: RelayTransport,
        username: &str,
        password: &str,
    ) -> io::Result<Self> {
        let (local, server) = socket.addresses(&transport)?;
        let mut relay = Relay::new(local, server, transport, username, password, Instant::now())
            .map_err(relay_error)?;
        let mut receiver = vec![0; super::relay::SERVER_CHUNK].into_boxed_slice();
        let mut write = None;
        let candidate = loop {
            match relay.poll(Instant::now()).map_err(relay_error)? {
                RelayOutput::Network(bytes) => {
                    queue(&mut write, bytes);
                    flush(&socket, &mut write).await?;
                }
                RelayOutput::Candidate(candidate) => break candidate,
                RelayOutput::Peer(_) => continue,
                RelayOutput::Closed => return Err(relay_closed()),
                RelayOutput::WaitUntil(deadline) => {
                    tokio::select! {
                        _ = wait_until(deadline) => {}
                        received = socket.read(&mut receiver) => {
                            let size = received?;
                            relay.receive(&receiver[..size], Instant::now()).map_err(relay_error)?;
                        }
                    }
                }
            }
        };
        let candidate_text = candidate.to_sdp_string();
        Ok(Self {
            socket,
            relay,
            candidate,
            candidate_text,
            receiver,
            pending: None,
        })
    }

    async fn receive(&mut self) -> io::Result<Option<RelayDatagram>> {
        let size = self.socket.read(&mut self.receiver).await?;
        self.relay
            .receive(&self.receiver[..size], Instant::now())
            .map_err(relay_error)
    }

    async fn withdraw(&mut self) -> io::Result<()> {
        // The caller has retired media. Drop the socket if it still owns
        // unsent bytes, including a partially written TLS record.
        if self.pending.is_some() {
            return Ok(());
        }
        self.relay.close(Instant::now());
        loop {
            flush(&self.socket, &mut self.pending).await?;
            match self.relay.poll(Instant::now()).map_err(relay_error)? {
                RelayOutput::Network(bytes) => queue(&mut self.pending, bytes),
                RelayOutput::Closed => return Ok(()),
                RelayOutput::WaitUntil(deadline) => {
                    tokio::select! {
                        _ = wait_until(deadline) => {}
                        received = self.receive() => { received?; }
                    }
                }
                RelayOutput::Candidate(_) | RelayOutput::Peer(_) => {}
            }
        }
    }
}

/// One RTC generation driven serially by its direct socket, its TURN socket,
/// or both. The station owner authorizes and consumes the resulting events.
pub struct UdpDriver {
    rtc: Rtc,
    direct: Option<DirectLeg>,
    relay: Option<RelayLeg>,
    retired_relay: Option<(SocketAddr, String)>,
}

impl UdpDriver {
    /// Register the already-bound direct UDP socket's actual host candidate.
    pub fn new(mut rtc: Rtc, socket: UdpSocket) -> io::Result<Self> {
        let local = socket.local_addr()?;
        let candidate = Candidate::host(local, "udp").map_err(|_| invalid_transport())?;
        let candidate = rtc
            .add_local_candidate(candidate)
            .ok_or_else(invalid_transport)?
            .to_sdp_string();
        Ok(Self {
            rtc,
            direct: Some(DirectLeg {
                socket,
                local,
                candidate,
                receiver: vec![0; usize::from(u16::MAX)].into_boxed_slice(),
                pending: None,
            }),
            relay: None,
            retired_relay: None,
        })
    }

    /// Construct a relay-only driver and gather one UDP relay candidate.
    pub async fn new_relay(
        rtc: Rtc,
        socket: RelaySocket,
        transport: RelayTransport,
        username: &str,
        password: &str,
        deadline: Instant,
    ) -> io::Result<Self> {
        let mut driver = Self {
            rtc,
            direct: None,
            relay: None,
            retired_relay: None,
        };
        driver
            .add_relay(socket, transport, username, password, deadline)
            .await?;
        Ok(driver)
    }

    /// Gather the one configured TURN leg. With a direct leg present, str0m
    /// keeps both candidates and chooses the pair through its normal ICE policy.
    /// The direct path stays available if gathering fails.
    pub async fn add_relay(
        &mut self,
        socket: RelaySocket,
        transport: RelayTransport,
        username: &str,
        password: &str,
        deadline: Instant,
    ) -> io::Result<()> {
        if self.relay.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "TURN leg already present",
            ));
        }
        let leg = tokio::time::timeout_at(
            deadline.into(),
            RelayLeg::allocate(socket, transport, username, password),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TURN allocation timed out"))??;
        self.rtc
            .add_local_candidate(leg.candidate.clone())
            .ok_or_else(invalid_transport)?;
        self.relay = Some(leg);
        Ok(())
    }

    /// Primary SDP candidate initially advertised by the driver.
    /// Use relay_candidate to inspect whether its TURN leg still exists.
    pub fn candidate(&self) -> &str {
        self.direct
            .as_ref()
            .map(|leg| leg.candidate.as_str())
            .or_else(|| self.relay_candidate())
            .or_else(|| self.retired_relay.as_ref().map(|(_, text)| text.as_str()))
            .expect("live transport has a candidate")
    }

    /// Additional or sole SDP TURN candidate to publish over authorized signaling.
    pub fn relay_candidate(&self) -> Option<&str> {
        self.relay.as_ref().map(|leg| leg.candidate_text.as_str())
    }

    /// Apply already-authorized Trickle ICE input to the active transport.
    pub fn add_remote_candidate(&mut self, text: &str) -> io::Result<()> {
        self.rtc.add_remote_candidate(
            Candidate::from_sdp_string(text).map_err(|_| invalid_transport())?,
        );
        Ok(())
    }

    /// Queue prepared RTP for a negotiated track and return its actual SSRC.
    /// This only transfers ownership to the transport.
    pub fn write_rtp(&mut self, media: Mid, packet: RtpWrite) -> io::Result<Ssrc> {
        let mut direct = self.rtc.direct_api();
        let stream = direct
            .stream_tx_by_mid(media, None)
            .ok_or_else(invalid_transport)?;
        stream.write_rtp(packet);
        Ok(stream.ssrc())
    }

    fn detach_relay(&mut self) -> Option<RelayLeg> {
        let relay = self.relay.take()?;
        self.retired_relay = Some((relay.candidate.addr(), relay.candidate_text.clone()));
        self.rtc.direct_api().invalidate_candidate(&relay.candidate);
        Some(relay)
    }

    fn relay_failed(&mut self, error: io::Error) -> io::Result<()> {
        self.detach_relay();
        if self.direct.is_some() {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Stop relay ingress for this generation and attempt to withdraw the
    /// allocation before the owner's deadline. Cancellation still drops the leg.
    pub async fn close_relay(&mut self, deadline: Instant) -> io::Result<()> {
        let Some(mut relay) = self.detach_relay() else {
            return Ok(());
        };
        tokio::time::timeout_at(deadline.into(), relay.withdraw())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TURN withdrawal timed out"))?
    }

    fn receive(&mut self, packet: RelayDatagram) -> io::Result<()> {
        if let Ok(input) = Receive::new(
            Protocol::Udp,
            packet.source,
            packet.destination,
            &packet.bytes,
        ) {
            self.rtc
                .handle_input(Input::Receive(Instant::now(), input))
                .map_err(|_| invalid_transport())?;
        }
        Ok(())
    }

    fn route(&mut self, packet: Transmit) -> io::Result<()> {
        if packet.proto != Protocol::Udp {
            return Err(invalid_transport());
        }
        if let Some(leg) = &mut self.direct
            && packet.source == leg.local
        {
            leg.pending = Some(packet);
        } else if let Some(leg) = &mut self.relay
            && packet.source == leg.candidate.addr()
        {
            match leg.relay.send(&packet, Instant::now()) {
                Ok(RelaySend::Network(bytes)) => queue(&mut leg.pending, bytes),
                Ok(RelaySend::Queued | RelaySend::AwaitingPermission) => {}
                Err(error) => self.relay_failed(relay_error(error))?,
            }
        } else if self.retired_relay.as_ref().map(|(address, _)| *address) != Some(packet.source) {
            return Err(invalid_transport());
        }
        Ok(())
    }

    /// Run network I/O and monotonic timers until one transport event.
    /// Cancelling the wait preserves an owned datagram and any partial TURN write.
    pub async fn next_event(&mut self) -> io::Result<Event> {
        loop {
            if let Some(leg) = &mut self.direct
                && let Some(packet) = &leg.pending
            {
                let written = leg
                    .socket
                    .send_to(&packet.contents, packet.destination)
                    .await?;
                if written != packet.contents.len() {
                    return Err(partial_datagram());
                }
                leg.pending = None;
            }
            if let Some(leg) = &mut self.relay
                && let Err(error) = flush(&leg.socket, &mut leg.pending).await
            {
                self.relay_failed(error)?;
            }
            let rtc_deadline = match self.rtc.poll_output().map_err(|_| invalid_transport())? {
                Output::Event(event) => return Ok(event),
                Output::Transmit(packet) => {
                    self.route(packet)?;
                    continue;
                }
                Output::Timeout(deadline) => deadline,
            };
            let relay_deadline = if let Some(leg) = &mut self.relay {
                match leg.relay.poll(Instant::now()) {
                    Ok(RelayOutput::WaitUntil(deadline)) => deadline,
                    Ok(RelayOutput::Network(bytes)) => {
                        queue(&mut leg.pending, bytes);
                        continue;
                    }
                    Ok(RelayOutput::Peer(packet)) => {
                        self.receive(packet)?;
                        continue;
                    }
                    Ok(RelayOutput::Closed) => {
                        self.relay_failed(relay_closed())?;
                        continue;
                    }
                    Ok(RelayOutput::Candidate(_)) => {
                        self.relay_failed(invalid_transport())?;
                        continue;
                    }
                    Err(error) => {
                        self.relay_failed(relay_error(error))?;
                        continue;
                    }
                }
            } else {
                None
            };
            self.wait_for_input(rtc_deadline, relay_deadline).await?;
        }
    }

    async fn wait_for_input(
        &mut self,
        rtc_deadline: Instant,
        relay_deadline: Option<Instant>,
    ) -> io::Result<()> {
        let direct = async {
            match &mut self.direct {
                Some(leg) => leg.socket.recv_from(&mut leg.receiver).await,
                None => pending().await,
            }
        };
        let relay = async {
            match &mut self.relay {
                Some(leg) => leg.receive().await,
                None => pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = wait_until(Some(rtc_deadline)) => {
                self.rtc.handle_input(Input::Timeout(Instant::now())).map_err(|_| invalid_transport())
            }
            _ = wait_until(relay_deadline) => Ok(()),
            received = direct => {
                let (size, source) = received?;
                let leg = self.direct.as_ref().expect("receiving direct socket");
                if let Ok(packet) = Receive::new(Protocol::Udp, source, leg.local, &leg.receiver[..size]) {
                    self.rtc.handle_input(Input::Receive(Instant::now(), packet)).map_err(|_| invalid_transport())?;
                }
                Ok(())
            }
            received = relay => match received {
                Ok(Some(packet)) => self.receive(packet),
                Ok(None) => Ok(()),
                Err(error) => self.relay_failed(error),
            }
        }
    }
}
