//! Loopback-only IAX test fixture: one socket/thread, bounded reliable state per call.
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Counts {
    pub media: AtomicU64,
    pub samples: AtomicU64,
    pub send_errors: AtomicU64,
    pub out_of_order: AtomicU64,
    pub retransmissions: AtomicU64,
}
pub(super) struct Simulator {
    pub address: SocketAddr,
    pub receive_buffer_bytes: i32,
    pub counts: Arc<Counts>,
    pub ingress_enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
struct Pending {
    bytes: Vec<u8>,
    sent: Instant,
}
struct Call {
    client: u16,
    server: u16,
    receive: u8,
    send: u8,
    phase: u8,
    epoch: Instant,
    pending: VecDeque<Pending>,
    ingress: u64,
    keyed: bool,
}
fn full(
    (source, destination): (u16, u16),
    timestamp: u32,
    (send, receive): (u8, u8),
    (kind, subclass): (u8, u8),
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + payload.len());
    bytes.extend_from_slice(&(source | 0x8000).to_be_bytes());
    bytes.extend_from_slice(&destination.to_be_bytes());
    bytes.extend_from_slice(&timestamp.to_be_bytes());
    bytes.extend_from_slice(&[send, receive, kind, subclass]);
    bytes.extend_from_slice(payload);
    bytes
}
fn send(socket: &UdpSocket, address: SocketAddr, bytes: &[u8], counts: &Counts) {
    if socket.send_to(bytes, address).is_err() {
        counts.send_errors.fetch_add(1, Ordering::Relaxed);
    }
}
impl Call {
    fn reliable(
        &mut self,
        socket: &UdpSocket,
        address: SocketAddr,
        kind: (u8, u8),
        timestamp: u32,
        payload: &[u8],
        counts: &Counts,
    ) {
        if self.pending.len() == 32 {
            return;
        }
        let bytes = full(
            (self.server, self.client),
            timestamp,
            (self.send, self.receive),
            kind,
            payload,
        );
        self.send = self.send.wrapping_add(1);
        send(socket, address, &bytes, counts);
        self.pending.push_back(Pending {
            bytes,
            sent: Instant::now(),
        });
    }
    fn receive(&mut self, socket: &UdpSocket, address: SocketAddr, bytes: &[u8], counts: &Counts) {
        if bytes[2] & 0x80 != 0 && counts.retransmissions.fetch_add(1, Ordering::Relaxed) < 16 {
            eprintln!(
                "sim retry call={} expected={} got={} kind={}/{}",
                self.client, self.receive, bytes[8], bytes[10], bytes[11]
            );
        }
        let acknowledgment = bytes[9];
        while self.pending.front().is_some_and(|pending| {
            let distance = acknowledgment.wrapping_sub(pending.bytes[8]);
            distance != 0 && distance < 128
        }) {
            self.pending.pop_front();
        }
        let timestamp = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let ack = bytes[10..12] == [6, 4];
        if !ack {
            let current = bytes[8] == self.receive;
            if current {
                self.receive = self.receive.wrapping_add(1);
            } else if self.receive.wrapping_sub(bytes[8]) >= 128
                && counts.out_of_order.fetch_add(1, Ordering::Relaxed) < 16
            {
                eprintln!(
                    "sim gap call={} expected={} got={} ack={} kind={}/{}",
                    self.client, self.receive, bytes[8], bytes[9], bytes[10], bytes[11]
                );
            }
            send(
                socket,
                address,
                &full(
                    (self.server, self.client),
                    timestamp,
                    (acknowledgment, self.receive),
                    (6, 4),
                    &[],
                ),
                counts,
            );
            if current {
                if bytes[10] == 2 {
                    counts.media.fetch_add(1, Ordering::Relaxed);
                    counts
                        .samples
                        .fetch_add((bytes.len() - 12) as u64, Ordering::Relaxed);
                }
                match (bytes[10], bytes[11]) {
                    (6, 2) => self.reliable(socket, address, (6, 3), timestamp, &[], counts),
                    (6, 11) => self.reliable(socket, address, (6, 12), timestamp, &[], counts),
                    _ => {}
                }
            }
        }
        if self.pending.is_empty() {
            match self.phase {
                0 => {
                    self.phase = 1;
                    self.reliable(socket, address, (4, 4), 1, &[], counts);
                }
                1 => {
                    self.phase = 2;
                    self.reliable(socket, address, (7, 0), 2, b"!NEWKEY!", counts);
                }
                _ => {}
            }
        }
    }
}
impl Simulator {
    pub fn start(peers: usize) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        // One simulator socket represents every remote node. Give it room for
        // their simultaneous topology replies; report the kernel's actual bound.
        let mut receive_buffer_bytes = peers.saturating_mul(4096).min(i32::MAX as usize) as i32;
        let mut length = size_of::<i32>() as libc::socklen_t;
        unsafe {
            assert_eq!(
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    std::ptr::from_ref(&receive_buffer_bytes).cast(),
                    length
                ),
                0
            );
            assert_eq!(
                libc::getsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    std::ptr::from_mut(&mut receive_buffer_bytes).cast(),
                    &mut length
                ),
                0
            );
        }
        socket.set_nonblocking(true).unwrap();
        let address = socket.local_addr().unwrap();
        let counts = Arc::new(Counts::default());
        let ingress_enabled = Arc::new(AtomicBool::new(false));
        let receiving = ingress_enabled.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let statistics = counts.clone();
        let stopping = stop.clone();
        let thread = thread::Builder::new()
            .name("load-udp".into())
            .spawn(move || {
                let mut calls: HashMap<(SocketAddr, u16), Call> = HashMap::new();
                let mut bytes = [0; 2048];
                let mut maintenance = Instant::now();
                while !stopping.load(Ordering::Relaxed) {
                    let mut received = 0;
                    for _ in 0..512 {
                        let (length, address) = match socket.recv_from(&mut bytes) {
                            Ok(packet) => packet,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => panic!("simulator receive: {error}"),
                        };
                        received += 1;
                        if length < 4 {
                            continue;
                        }
                        let source = u16::from_be_bytes(bytes[..2].try_into().unwrap());
                        if source & 0x8000 == 0 {
                            if calls.contains_key(&(address, source)) {
                                statistics.media.fetch_add(1, Ordering::Relaxed);
                                statistics
                                    .samples
                                    .fetch_add((length - 4) as u64, Ordering::Relaxed);
                            }
                            continue;
                        }
                        if length < 12 {
                            continue;
                        }
                        let source = source & 0x7fff;
                        let key = (address, source);
                        if bytes[10..12] == [6, 1] && !calls.contains_key(&key) {
                            // Call numbers are scoped to a remote socket; reuse one server ID.
                            let mut call = Call {
                                client: source,
                                server: 1,
                                receive: 1,
                                send: 0,
                                phase: 0,
                                epoch: Instant::now(),
                                pending: VecDeque::new(),
                                ingress: 0,
                                keyed: false,
                            };
                            call.reliable(
                                &socket,
                                address,
                                (6, 7),
                                0,
                                &[9, 4, 0, 0, 0, 4],
                                &statistics,
                            );
                            calls.insert(key, call);
                        } else if let Some(call) = calls.get_mut(&key) {
                            call.receive(&socket, address, &bytes[..length], &statistics);
                        }
                    }
                    if maintenance.elapsed() >= Duration::from_millis(20) {
                        maintenance = Instant::now();
                        for ((address, _), call) in &mut calls {
                            for pending in &mut call.pending {
                                if pending.sent.elapsed() >= Duration::from_millis(200) {
                                    pending.bytes[2] |= 0x80;
                                    pending.bytes[9] = call.receive;
                                    send(&socket, *address, &pending.bytes, &statistics);
                                    pending.sent = maintenance;
                                }
                            }
                            // Four peers supply one second of concurrent ingress every five seconds.
                            let ms = call.epoch.elapsed().as_millis() as u64;
                            if receiving.load(Ordering::Relaxed)
                                && call.client <= 4
                                && call.phase == 2
                                && ms / 20 != call.ingress
                            {
                                call.ingress = ms / 20;
                                let keyed = ms % 5000 < 1000;
                                if call.keyed != keyed {
                                    call.keyed = keyed;
                                    call.reliable(
                                        &socket,
                                        *address,
                                        (4, if keyed { 12 } else { 13 }),
                                        ms as u32,
                                        &[],
                                        &statistics,
                                    );
                                }
                                if keyed {
                                    call.reliable(
                                        &socket,
                                        *address,
                                        (2, 4),
                                        ms as u32,
                                        &[0xd0; 160],
                                        &statistics,
                                    );
                                }
                            }
                        }
                    }
                    if received == 0 {
                        thread::sleep(Duration::from_micros(200));
                    }
                }
            })
            .unwrap();
        Self {
            address,
            receive_buffer_bytes,
            counts,
            ingress_enabled,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Simulator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
