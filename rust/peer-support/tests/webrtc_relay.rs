//! Exercise the controller relay adapter against an isolated coturn instance.

#![cfg(target_os = "linux")]

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rptadv_peer_support::webrtc::relay::{
    Relay, RelayDatagram, RelayError, RelayOutput, RelaySend, RelayTransport,
};
use rptadv_peer_support::webrtc::{self, Event, RelaySocket, UdpDriver};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, ServerName},
};
use str0m::{
    media::{Direction, Frequency, MediaKind, MediaTime},
    net::{Protocol, Transmit},
    rtp::{RtpWrite, SeqNo},
};

const WAIT: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(10);

enum Socket {
    Udp(UdpSocket),
    Tcp(TcpStream),
}

impl Socket {
    fn connect(server: SocketAddr, udp: bool) -> Self {
        if udp {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.connect(server).unwrap();
            socket.set_read_timeout(Some(TICK)).unwrap();
            socket.set_write_timeout(Some(WAIT)).unwrap();
            Self::Udp(socket)
        } else {
            let socket = TcpStream::connect_timeout(&server, WAIT).unwrap();
            socket.set_read_timeout(Some(TICK)).unwrap();
            socket.set_write_timeout(Some(WAIT)).unwrap();
            Self::Tcp(socket)
        }
    }

    fn local(&self) -> SocketAddr {
        match self {
            Self::Udp(socket) => socket.local_addr().unwrap(),
            Self::Tcp(socket) => socket.local_addr().unwrap(),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        match self {
            Self::Udp(socket) => assert_eq!(socket.send(bytes).unwrap(), bytes.len()),
            Self::Tcp(socket) => socket.write_all(bytes).unwrap(),
        }
    }

    fn read(&mut self, bytes: &mut [u8]) -> Option<usize> {
        let result = match self {
            Self::Udp(socket) => socket.recv(bytes),
            Self::Tcp(socket) => socket.read(bytes),
        };
        match result {
            Ok(0) => panic!("TURN leg ended before the test completed"),
            Ok(count) => Some(count),
            Err(error) if temporary(error.kind()) => None,
            Err(error) => panic!("TURN leg read failed: {error}"),
        }
    }
}

fn temporary(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

struct Leg {
    socket: Socket,
    relay: Relay,
    address: Option<SocketAddr>,
    inbound: Option<RelayDatagram>,
    closed: bool,
}

impl Leg {
    fn pump(&mut self) {
        for _ in 0..64 {
            match self.relay.poll(Instant::now()).unwrap() {
                RelayOutput::Network(bytes) => self.socket.write(&bytes),
                RelayOutput::Candidate(candidate) => {
                    assert!(self.address.replace(candidate.addr()).is_none());
                }
                RelayOutput::Peer(datagram) => self.receive(datagram),
                RelayOutput::Closed => {
                    self.closed = true;
                    return;
                }
                RelayOutput::WaitUntil(Some(deadline)) if deadline <= Instant::now() => continue,
                RelayOutput::WaitUntil(_) => {
                    let mut buffer = [0; 8192];
                    if let Some(size) = self.socket.read(&mut buffer)
                        && let Some(datagram) =
                            self.relay.receive(&buffer[..size], Instant::now()).unwrap()
                    {
                        self.receive(datagram);
                    }
                    return;
                }
            }
        }
        panic!("relay exceeded its poll budget");
    }

    fn receive(&mut self, datagram: RelayDatagram) {
        assert!(self.inbound.replace(datagram).is_none());
    }

    fn until<T>(&mut self, stage: &str, mut ready: impl FnMut(&mut Self) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(result) = ready(self) {
                return result;
            }
            assert!(
                Instant::now() < deadline && !self.closed,
                "relay failed at {stage}"
            );
            self.pump();
        }
    }
}

fn exercise(directory: &Path, name: &str, address: &str, transport: RelayTransport) {
    let server = address.parse().unwrap();
    let socket = Socket::connect(server, matches!(&transport, RelayTransport::Udp));
    let credentials =
        std::fs::read_to_string(directory.join(format!("{name}.credentials"))).unwrap();
    let mut credentials = credentials.lines();
    let now = Instant::now();
    let relay = Relay::new(
        socket.local(),
        server,
        transport,
        credentials.next().unwrap(),
        credentials.next().unwrap(),
        now,
    )
    .unwrap();
    let mut leg = Leg {
        relay,
        socket,
        address: None,
        inbound: None,
        closed: false,
    };
    let relay = leg.until("allocation", |leg| leg.address);
    assert!(relay.ip().is_loopback());
    assert!((49160..=49175).contains(&relay.port()));
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(TICK)).unwrap();
    let peer_addr = peer.local_addr().unwrap();

    let mut outgoing = Transmit {
        proto: Protocol::Udp,
        source: leg.socket.local(),
        destination: peer_addr,
        contents: b"outbound".to_vec().into(),
    };
    assert!(matches!(
        leg.relay.send(&outgoing, Instant::now()),
        Err(RelayError::Route)
    ));
    outgoing.source = relay;
    assert!(matches!(
        leg.relay.send(&outgoing, Instant::now()).unwrap(),
        RelaySend::AwaitingPermission
    ));
    leg.until("peer permission", |leg| {
        match leg.relay.send(&outgoing, Instant::now()).unwrap() {
            RelaySend::AwaitingPermission => None,
            RelaySend::Queued => Some(()),
            RelaySend::Network(bytes) => {
                leg.socket.write(&bytes);
                Some(())
            }
        }
    });
    let mut data = [0; 32];
    let (size, source) = leg.until("outbound datagram", |_| match peer.recv_from(&mut data) {
        Ok(received) => Some(received),
        Err(error) if temporary(error.kind()) => None,
        Err(error) => panic!("test peer could not receive: {error}"),
    });
    assert_eq!(source, relay);
    assert_eq!(&data[..size], b"outbound");

    assert_eq!(peer.send_to(b"return", relay).unwrap(), 6);
    let datagram = leg.until("peer datagram", |leg| leg.inbound.take());
    assert_eq!(datagram.destination, relay);
    assert_eq!(datagram.source, peer_addr);
    assert_eq!(datagram.bytes, b"return");
    println!("{name}: one relay candidate, permission, and both datagram directions verified");
}

fn fixture_directory() -> PathBuf {
    let interfaces = std::fs::read_to_string("/proc/net/dev").unwrap();
    assert_eq!(
        interfaces
            .lines()
            .skip(2)
            .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
            .collect::<Vec<_>>(),
        ["lo"]
    );
    PathBuf::from(std::env::var_os("RPTADV_TURN_FIXTURE_DIR").expect("use run-turn.py"))
}

#[test]
#[ignore = "requires the loopback-only coturn fixture"]
fn relay_adapter_uses_udp_tcp_and_verified_tls() {
    let directory = fixture_directory();
    exercise(&directory, "udp", "127.0.0.1:3478", RelayTransport::Udp);
    exercise(&directory, "tcp", "127.0.0.1:3478", RelayTransport::Tcp);
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            std::fs::read(directory.join("ca.der")).unwrap(),
        ))
        .unwrap();
    exercise(
        &directory,
        "tls",
        "127.0.0.1:5349",
        RelayTransport::Tls {
            name: ServerName::try_from("localhost").unwrap(),
            roots,
        },
    );
}

async fn relay_driver(rtc: webrtc::Rtc, directory: &Path, leg: &str) -> UdpDriver {
    let plain = "127.0.0.1:3478";
    let (socket, transport) = match leg {
        "udp" => {
            let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            socket.connect(plain).await.unwrap();
            (RelaySocket::Udp(socket), RelayTransport::Udp)
        }
        "tcp" => {
            let socket = tokio::net::TcpStream::connect(plain).await.unwrap();
            (RelaySocket::Tcp(socket), RelayTransport::Tcp)
        }
        "tls" => {
            let socket = tokio::net::TcpStream::connect("127.0.0.1:5349")
                .await
                .unwrap();
            let mut roots = RootCertStore::empty();
            roots
                .add(CertificateDer::from(
                    std::fs::read(directory.join("ca.der")).unwrap(),
                ))
                .unwrap();
            let transport = RelayTransport::Tls {
                name: ServerName::try_from("localhost").unwrap(),
                roots,
            };
            (RelaySocket::Tcp(socket), transport)
        }
        _ => unreachable!(),
    };
    let credentials =
        std::fs::read_to_string(directory.join(format!("{leg}.credentials"))).unwrap();
    let mut credentials = credentials.lines();
    UdpDriver::new_relay(
        rtc,
        socket,
        transport,
        credentials.next().unwrap(),
        credentials.next().unwrap(),
        Instant::now() + WAIT,
    )
    .await
    .unwrap()
}

#[test]
#[ignore = "requires the loopback-only coturn fixture"]
fn driver_exchanges_ice_dtls_and_rtp_using_only_relays() {
    let directory = fixture_directory();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for leg in ["udp", "tcp", "tls"] {
            let now = Instant::now();
            let mut caller = webrtc::transport(now);
            let mut change = caller.sdp_api();
            let media = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
            let (offer, pending) = change.apply().unwrap();
            let (remote, answer) = webrtc::accept_offer(now, offer).unwrap();
            caller.sdp_api().accept_answer(pending, answer).unwrap();
            let opus = caller.codec_config().params()[0].pt();
            let (mut caller, mut remote) = tokio::join!(
                relay_driver(caller, &directory, leg),
                relay_driver(remote, &directory, leg),
            );
            assert!(caller.candidate().contains(" typ relay"));
            assert!(remote.candidate().contains(" typ relay"));
            assert_ne!(caller.candidate(), remote.candidate());
            caller.add_remote_candidate(remote.candidate()).unwrap();
            remote.add_remote_candidate(caller.candidate()).unwrap();

            tokio::time::timeout(WAIT, async {
                let mut connected = [false; 2];
                while !connected.iter().all(|value| *value) {
                    tokio::select! {
                        event = caller.next_event() => {
                            connected[0] |= matches!(event.unwrap(), Event::Connected);
                        }
                        event = remote.next_event() => {
                            connected[1] |= matches!(event.unwrap(), Event::Connected);
                        }
                    }
                }
            }).await.expect("relayed ICE and DTLS");

            let frame = RtpWrite::new(opus, SeqNo::from(9), 9_600, Instant::now(), &[0xf8, 0xff, 0xfe][..]);
            let source = caller.write_rtp(media, frame).unwrap();
            tokio::time::timeout(WAIT, async {
                loop {
                    tokio::select! {
                        event = caller.next_event() => { event.unwrap(); }
                        event = remote.next_event() => {
                            if let Event::RtpPacket(packet) = event.unwrap() {
                                assert_eq!(packet.header.ssrc, source);
                                assert_eq!(packet.time, MediaTime::new(9_600, Frequency::FORTY_EIGHT_KHZ));
                                assert_eq!(packet.payload.as_ref(), &[0xf8, 0xff, 0xfe]);
                                break;
                            }
                        }
                    }
                }
            }).await.expect("RTP reached the peer through both allocations");

            let closing = Instant::now() + WAIT;
            let (left, right) = tokio::join!(caller.close_relay(closing), remote.close_relay(closing));
            left.unwrap();
            right.unwrap();
            assert!(caller.relay_candidate().is_none());
            assert!(remote.relay_candidate().is_none());
            println!("{leg}: both relay-only RTC endpoints connected and exchanged protected RTP");
        }
    });
}
