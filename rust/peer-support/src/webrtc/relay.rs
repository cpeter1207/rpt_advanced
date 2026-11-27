//! One TURN allocation on the RTC owner's serial I/O loop.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use str0m::{
    Candidate,
    net::{Protocol, Transmit as RtcTransmit},
};
use turn_client_proto::{
    api::{CreatePermissionError, TurnConfig, TurnEvent, TurnPeerData, TurnPollRet, TurnRecvRet},
    prelude::TurnClientApi,
    stun::agent::Transmit,
    tcp::TurnClientTcp,
    types::{Instant as TurnTime, TransportType, TurnCredentials},
    udp::TurnClientUdp,
};
use turn_client_rustls::TurnClientRustls;

turn_client_proto::impl_client!(
    Client,
    (Udp, TurnClientUdp),
    (Tcp, TurnClientTcp),
    (Tls, TurnClientRustls)
);

const UDP: TransportType = TransportType::Udp;
pub(super) const SERVER_CHUNK: usize = 16 * 1024 + 256;
const PEER_DATAGRAM: usize = 4096;

/// How the owner connects to the TURN service. The allocation itself relays UDP.
#[derive(Clone)]
pub enum RelayTransport {
    /// One connected datagram socket.
    Udp,
    /// One connected stream socket.
    Tcp,
    /// One connected stream socket, protected by this adapter's TLS client.
    Tls {
        /// Configured, certificate-verified TURN service name.
        name: ServerName<'static>,
        /// Trust roots approved for this TURN service.
        roots: RootCertStore,
    },
}

impl RelayTransport {
    /// Select the exact TLS server name and an explicitly trusted certificate issuer.
    pub fn tls(
        name: String,
        issuer: rustls::pki_types::CertificateDer<'static>,
    ) -> Result<Self, RelayError> {
        let name = ServerName::try_from(name).map_err(|_| RelayError::Configuration)?;
        let mut roots = RootCertStore::empty();
        roots.add(issuer).map_err(|_| RelayError::Configuration)?;
        Ok(Self::Tls { name, roots })
    }
}

/// Failure categories carrying no credentials, packet data or addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayError {
    /// The connected server socket or credentials were incomplete.
    Configuration,
    /// TLS client configuration could not be prepared.
    Tls,
    /// The input exceeded the permitted server read or peer datagram size.
    TooLarge,
    /// The RTC transmission does not use this allocation.
    Route,
    /// An allocation was refused or could not form an ICE candidate.
    Allocation,
    /// The TURN client could not advance the requested operation.
    Protocol,
}

/// One TURN-carried datagram ready for str0m's UDP receive input.
pub struct RelayDatagram {
    /// Peer address observed by the relay.
    pub source: SocketAddr,
    /// This session's allocated relay address.
    pub destination: SocketAddr,
    /// The unmodified ICE, DTLS or SRTP datagram.
    pub bytes: Vec<u8>,
}

/// One item from the relay's serial poll.
pub enum RelayOutput {
    /// Write these bytes on the connected socket. TLS bytes are already encrypted.
    Network(Vec<u8>),
    /// Add and trickle this UDP relay candidate in the RTC generation.
    Candidate(Candidate),
    /// Deliver this datagram to the RTC instance.
    Peer(RelayDatagram),
    /// Wait on the socket and, when present, the monotonic deadline.
    WaitUntil(Option<Instant>),
    /// The allocation and its candidate are no longer usable.
    Closed,
}

/// Result of submitting a validated str0m transmission.
pub enum RelaySend {
    /// Write these bytes now on the connected server socket.
    Network(Vec<u8>),
    /// The TURN client will supply network bytes on a subsequent poll.
    Queued,
    /// Permission is pending. The RTC protocol can retry its own check.
    AwaitingPermission,
}

/// Own one allocation and its TLS state, with no additional send or receive queue.
pub struct Relay {
    client: Client,
    start: Instant,
    allocated: Option<SocketAddr>,
    closing: bool,
    closed: bool,
}

impl Relay {
    /// Prepare a TURN client for an already connected server leg.
    ///
    /// The caller owns endpoint selection and socket I/O. Supply the actual
    /// connected socket addresses and short-lived credentials for this session.
    pub fn new(
        local: SocketAddr,
        server: SocketAddr,
        transport: RelayTransport,
        username: &str,
        password: &str,
        now: Instant,
    ) -> Result<Self, RelayError> {
        if local.port() == 0
            || server.port() == 0
            || local.ip().is_unspecified()
            || server.ip().is_unspecified()
            || username.is_empty()
            || password.is_empty()
        {
            return Err(RelayError::Configuration);
        }
        let config = TurnConfig::new(TurnCredentials::new(username, password));
        let client = match transport {
            RelayTransport::Udp => Client::Udp(TurnClientUdp::allocate(local, server, config)),
            RelayTransport::Tcp => Client::Tcp(TurnClientTcp::allocate(local, server, config)),
            RelayTransport::Tls { name, roots } => {
                let tls = ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|_| RelayError::Tls)?
                .with_root_certificates(roots)
                .with_no_client_auth();
                Client::Tls(TurnClientRustls::allocate(
                    local,
                    server,
                    config,
                    name,
                    Arc::new(tls),
                ))
            }
        };
        Ok(Self {
            client,
            start: now,
            allocated: None,
            closing: false,
            closed: false,
        })
    }

    fn turn_time(&self, now: Instant) -> TurnTime {
        TurnTime::ZERO + now.saturating_duration_since(self.start)
    }

    fn network<T: AsRef<[u8]>>(&self, packet: Transmit<T>) -> Result<Vec<u8>, RelayError> {
        if packet.transport != self.client.transport()
            || packet.from != self.client.local_addr()
            || packet.to != self.client.remote_addr()
        {
            return Err(RelayError::Route);
        }
        Ok(packet.data.as_ref().to_vec())
    }

    fn peer<T: AsRef<[u8]> + std::fmt::Debug>(
        &self,
        packet: TurnPeerData<T>,
    ) -> Option<RelayDatagram> {
        if self.closing || packet.transport != UDP || packet.data().len() > PEER_DATAGRAM {
            return None;
        }
        Some(RelayDatagram {
            source: packet.peer,
            destination: self.allocated?,
            bytes: packet.data().to_vec(),
        })
    }

    /// Consume a bounded read from the exact connected server socket.
    ///
    /// Drain poll output before another read. A peer datagram returned directly
    /// here precedes any further peer output from that read.
    pub fn receive(
        &mut self,
        data: &[u8],
        now: Instant,
    ) -> Result<Option<RelayDatagram>, RelayError> {
        if data.len() > SERVER_CHUNK {
            return Err(RelayError::TooLarge);
        }
        if self.closed || data.is_empty() {
            return Ok(None);
        }
        let input = Transmit::new(
            data,
            self.client.transport(),
            self.client.remote_addr(),
            self.client.local_addr(),
        );
        let time = self.turn_time(now);
        match self.client.recv(input, time) {
            TurnRecvRet::PeerData(packet) => Ok(self.peer(packet)),
            _ => Ok(None),
        }
    }

    /// Poll one item. Keep polling after an item until a deadline or closure.
    pub fn poll(&mut self, now: Instant) -> Result<RelayOutput, RelayError> {
        if self.closed {
            return Ok(RelayOutput::Closed);
        }
        let time = self.turn_time(now);
        let deadline = match self.client.poll(time) {
            TurnPollRet::WaitUntil(deadline) => deadline,
            TurnPollRet::Closed => {
                self.closed = true;
                self.allocated = None;
                return Ok(RelayOutput::Closed);
            }
            _ => return Err(RelayError::Protocol),
        };
        let event = self.client.poll_event();
        match event {
            Some(TurnEvent::AllocationCreated(UDP, address)) if !self.closing => {
                let candidate =
                    Candidate::relayed(address, self.client.local_addr(), Protocol::Udp)
                        .map_err(|_| RelayError::Allocation)?;
                self.allocated = Some(address);
                return Ok(RelayOutput::Candidate(candidate));
            }
            Some(TurnEvent::AllocationCreateFailed(_)) => return Err(RelayError::Allocation),
            _ => {}
        }
        let peer = self.client.poll_recv(time);
        let drained_peer = peer.is_some();
        if let Some(datagram) = peer.and_then(|packet| self.peer(packet)) {
            return Ok(RelayOutput::Peer(datagram));
        }
        if let Some(packet) = self.client.poll_transmit(time) {
            return self.network(packet).map(RelayOutput::Network);
        }
        let next = if event.is_some() || drained_peer {
            Some(now)
        } else {
            deadline
                .checked_duration_since(TurnTime::ZERO)
                .and_then(|duration| self.start.checked_add(duration))
        };
        Ok(RelayOutput::WaitUntil(next))
    }

    /// Route one str0m UDP datagram with this allocation as its source.
    ///
    /// An initial check may arrive before TURN creates permission. The adapter
    /// does not retain the datagram, and str0m keeps ownership of ICE retries.
    pub fn send(&mut self, packet: &RtcTransmit, now: Instant) -> Result<RelaySend, RelayError> {
        if self.closing || packet.proto != Protocol::Udp || Some(packet.source) != self.allocated {
            return Err(RelayError::Route);
        }
        if packet.contents.len() > PEER_DATAGRAM {
            return Err(RelayError::TooLarge);
        }
        let destination = packet.destination;
        let time = self.turn_time(now);
        if !self.client.have_permission(UDP, destination.ip()) {
            return match self.client.create_permission(UDP, destination.ip(), time) {
                Ok(()) | Err(CreatePermissionError::AlreadyExists) => {
                    Ok(RelaySend::AwaitingPermission)
                }
                Err(_) => Err(RelayError::Protocol),
            };
        }
        match self
            .client
            .send_to(UDP, destination, &packet.contents[..], time)
        {
            Ok(Some(packet)) => self.network(packet.build()).map(RelaySend::Network),
            Ok(None) => Ok(RelaySend::Queued),
            Err(_) => Err(RelayError::Protocol),
        }
    }

    /// Withdraw the allocation on shutdown, without holding the session open indefinitely.
    pub fn close(&mut self, now: Instant) {
        if self.closing || self.closed {
            return;
        }
        self.closing = true;
        let time = self.turn_time(now);
        if self.client.delete(time).is_err() {
            self.closed = true;
            self.allocated = None;
        }
    }
}
