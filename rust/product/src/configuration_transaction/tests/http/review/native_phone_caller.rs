//! Independent wire caller for the managed native account and RF callback fixture.
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::private_phone::registrar;

#[path = "native_phone_mix_minus.rs"]
mod mix_minus;

pub(super) struct Caller<'a> {
    signaling: &'a UdpSocket,
    contact: SocketAddr,
    dialog: u32,
    to: String,
    media: UdpSocket,
    destination: SocketAddr,
    sequence: u16,
    timestamp: u32,
    event_type: Option<u8>,
    next: Instant,
}

#[derive(Clone, Copy)]
struct PromptPacket {
    time: Instant,
    timestamp: u32,
    sequence: u16,
    ssrc: u32,
    audible: bool,
}

fn request(
    socket: &UdpSocket,
    contact: SocketAddr,
    dialog: u32,
    method: &str,
    to: &str,
    body: &str,
) {
    let from = socket.local_addr().unwrap();
    let cseq = if method == "BYE" { 2 } else { 1 };
    let content = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/sdp\r\n"
    };
    let message = format!(
        "{method} sip:desk@{contact} SIP/2.0\r\nVia: SIP/2.0/UDP {from};branch=z9hG4bK-private-{dialog}-{method};rport\r\nMax-Forwards: 70\r\nFrom: <sip:caller@fixture.invalid>;tag=grant{dialog}\r\nTo: {to}\r\nCall-ID: grant{dialog}@fixture.invalid\r\nCSeq: {cseq} {method}\r\nContact: <sip:caller@{from}>\r\n{content}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.send_to(message.as_bytes(), contact).unwrap();
}

impl<'a> Caller<'a> {
    /// Adopt the fixture's RTP leg after a locally originated SIP dialog.
    pub(super) fn answered(
        signaling: &'a UdpSocket,
        contact: SocketAddr,
        media: UdpSocket,
        destination: SocketAddr,
        event_type: Option<u8>,
    ) -> Self {
        Self {
            signaling,
            contact,
            dialog: 0,
            to: String::new(),
            media,
            destination,
            sequence: 1,
            timestamp: 8000,
            event_type,
            next: Instant::now(),
        }
    }

    /// Receive three silent 20 ms PCMU packets before the caller transmits.
    pub(super) fn idle_rtp(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut wire = [0; 2048];
        let mut prior: Option<(u16, u32, u32)> = None;
        let mut observed = 0;
        let mut continuous = true;
        while observed < 3 && continuous {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if remaining.is_zero() {
                break;
            }
            self.media.set_read_timeout(Some(remaining)).unwrap();
            let (length, source) = match self.media.recv_from(&mut wire) {
                Ok(packet) => packet,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("idle SIP fixture RTP read failed: {error}"),
            };
            observed += 1;
            continuous = source == self.destination
                && length == 172
                && wire[0] == 0x80
                && wire[1] & 0x7f == 0
                && wire[12..length].iter().all(|octet| octet & 0x7f == 0x7f);
            if continuous {
                let sequence = u16::from_be_bytes(wire[2..4].try_into().unwrap());
                let timestamp = u32::from_be_bytes(wire[4..8].try_into().unwrap());
                let ssrc = u32::from_be_bytes(wire[8..12].try_into().unwrap());
                continuous = prior.is_none_or(|(old_sequence, old_timestamp, old_ssrc)| {
                    sequence == old_sequence.wrapping_add(1)
                        && timestamp == old_timestamp.wrapping_add(160)
                        && ssrc == old_ssrc
                });
                prior = Some((sequence, timestamp, ssrc));
            }
        }
        let continuous_silence = observed == 3 && continuous;
        eprintln!("IDLE_SIP_RTP packet_count={observed} continuous_silence={continuous_silence}");
        continuous_silence
    }

    /// Detect the physical-edge fixture's 1 kHz waveform in actual G.711 media.
    pub(super) fn station_audio(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut wire = [0; 2048];
        let mut detected = 0;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            self.media.set_read_timeout(Some(remaining)).unwrap();
            let (length, source) = match self.media.recv_from(&mut wire) {
                Ok(packet) => packet,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("fixture RTP read failed: {error}"),
            };
            assert_eq!(source, self.destination);
            assert!(length >= 12 && wire[0] == 0x80 && wire[1] & 0x7f == 0);
            let mut phases = [0i64; 2];
            for (index, &code) in wire[12..length].iter().enumerate() {
                let bits = !code;
                let magnitude =
                    i64::from(((i32::from(bits & 15) * 8 + 132) << ((bits >> 4) & 7)) - 132);
                let pcm = if bits & 0x80 != 0 {
                    -magnitude
                } else {
                    magnitude
                };
                for (phase, output) in phases.iter_mut().enumerate() {
                    *output += if (index + 2 * phase) % 8 < 4 {
                        pcm
                    } else {
                        -pcm
                    };
                }
            }
            if length > 12
                && phases
                    .into_iter()
                    .any(|phase| phase.abs() > 200 * (length - 12) as i64)
            {
                detected += 1;
                if detected == 3 {
                    return true;
                }
            }
        }
        false
    }

    /// Observe fresh, sustained 1 kHz after the incoming login and attachment prompts.
    pub(super) fn fresh_station_audio(&mut self) -> bool {
        // The three 20 WPM Morse fallback prompts take 29.58 seconds in total.
        // Allow time for playback to drain and for the fresh station tone.
        self.fresh_station_audio_within(Duration::from_secs(40))
    }

    fn fresh_station_audio_within(&mut self, budget: Duration) -> bool {
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        let mut wire = [0; 2048];
        media.set_nonblocking(true).unwrap();
        let drained = (0..512).any(|_| match media.recv_from(&mut wire) {
            Ok((_, source)) => {
                assert_eq!(source, destination);
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(error) => panic!("fixture RTP backlog read failed: {error}"),
        });
        media.set_nonblocking(false).unwrap();
        assert!(drained, "fixture RTP backlog did not drain");

        self.during(true, || {
            Self::receive_fresh_station_audio(&media, destination, &mut wire, budget)
        })
    }

    fn receive_fresh_station_audio(
        media: &UdpSocket,
        destination: SocketAddr,
        wire: &mut [u8; 2048],
        budget: Duration,
    ) -> bool {
        const FRAMES: usize = 160;
        const QUARTER: f64 = std::f64::consts::FRAC_1_SQRT_2;
        const COSINE: [f64; 8] = [1.0, QUARTER, 0.0, -QUARTER, -1.0, -QUARTER, 0.0, QUARTER];
        let deadline = Instant::now() + budget;
        let mut previous: Option<(u16, u32, u32)> = None;
        let mut reference = [0.0; 2];
        let mut consecutive = 0;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            media.set_read_timeout(Some(remaining)).unwrap();
            let (length, source) = match media.recv_from(wire) {
                Ok(packet) => packet,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("fixture fresh RTP read failed: {error}"),
            };
            assert_eq!(source, destination);
            if length != 12 + FRAMES || wire[0] != 0x80 || wire[1] & 0x7f != 0 {
                previous = None;
                continue;
            }
            let (mut cosine, mut sine, mut energy) = (0.0, 0.0, 0.0);
            for (index, &code) in wire[12..length].iter().enumerate() {
                let bits = !code;
                let magnitude =
                    f64::from(((i32::from(bits & 15) * 8 + 132) << ((bits >> 4) & 7)) - 132);
                let sample = if bits & 0x80 != 0 {
                    -magnitude
                } else {
                    magnitude
                };
                cosine += sample * COSINE[index % 8];
                sine += sample * COSINE[(index + 6) % 8];
                energy += sample * sample;
            }
            let power = cosine * cosine + sine * sine;
            // Require audible PCM with at least 88% of its energy in the 1 kHz bin.
            if energy < 40_000.0 * FRAMES as f64 || 2.0 * power < 0.88 * FRAMES as f64 * energy {
                previous = None;
                continue;
            }
            let phase = [cosine / power.sqrt(), sine / power.sqrt()];
            let sequence = u16::from_be_bytes(wire[2..4].try_into().unwrap());
            let timestamp = u32::from_be_bytes(wire[4..8].try_into().unwrap());
            let ssrc = u32::from_be_bytes(wire[8..12].try_into().unwrap());
            if previous.is_some_and(|(seq, time, stream)| {
                seq.wrapping_add(1) == sequence
                    && time.wrapping_add(FRAMES as u32) == timestamp
                    && stream == ssrc
            }) && phase[0] * reference[0] + phase[1] * reference[1] > 0.95
            {
                consecutive += 1;
            } else {
                reference = phase;
                consecutive = 1;
            }
            previous = Some((sequence, timestamp, ssrc));
            if consecutive == 12 {
                return true;
            }
        }
        false
    }

    /// Drain immediate tail packets, then require a quiet retired RTP leg.
    pub(super) fn media_quiet(&self) -> bool {
        let deadline = Instant::now() + Duration::from_millis(700);
        let tail = Instant::now() + Duration::from_millis(200);
        let mut wire = [0; 2048];
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            self.media.set_read_timeout(Some(remaining)).unwrap();
            match self.media.recv_from(&mut wire) {
                Ok((_, source)) => {
                    assert_eq!(source, self.destination);
                    if Instant::now() > tail {
                        return false;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return true;
                }
                Err(error) => panic!("fixture RTP drain failed: {error}"),
            }
        }
        true
    }

    pub(super) fn connect(
        signaling: &'a UdpSocket,
        contact: SocketAddr,
        dialog: u32,
        telephone_events: bool,
    ) -> Self {
        let media = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = media.local_addr().unwrap().port();
        let event_offer = if telephone_events { " 101" } else { "" };
        let events = if telephone_events {
            "a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\n"
        } else {
            ""
        };
        let body = format!(
            "v=0\r\no=caller 1 1 IN IP4 127.0.0.1\r\ns=private\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP 0{event_offer}\r\na=rtpmap:0 PCMU/8000\r\n{events}a=ptime:20\r\na=sendrecv\r\n"
        );
        request(
            signaling,
            contact,
            dialog,
            "INVITE",
            "<sip:desk@fixture.invalid>",
            &body,
        );
        let mut buffer = [0; 8192];
        let response = loop {
            let (response, sender) = registrar::receive_register(signaling, &mut buffer).unwrap();
            assert_eq!(sender, contact);
            assert_eq!(
                registrar::header(&response, "Call-ID"),
                format!("grant{dialog}@fixture.invalid")
            );
            if response.starts_with("SIP/2.0 1") {
                continue;
            }
            assert!(
                response.starts_with("SIP/2.0 200 "),
                "native SIP invite failed"
            );
            break response;
        };
        let to = registrar::header(&response, "To").to_owned();
        let line = response
            .lines()
            .find_map(|line| line.strip_prefix("m=audio "))
            .unwrap();
        let mut media_fields = line.split_whitespace();
        let port: u16 = media_fields.next().unwrap().parse().unwrap();
        assert_eq!(media_fields.next(), Some("RTP/AVP"));
        let accepted: Vec<_> = media_fields.collect();
        assert!(accepted.contains(&"0"));
        let event_type: Option<u8> = response.lines().find_map(|line| {
            let (payload, encoding) = line.strip_prefix("a=rtpmap:")?.split_once(' ')?;
            (encoding.eq_ignore_ascii_case("telephone-event/8000") && accepted.contains(&payload))
                .then(|| payload.parse().unwrap())
        });
        assert_eq!(
            event_type.is_some(),
            telephone_events,
            "the actual SDP answer must match the offered digit transport"
        );
        let address = response
            .lines()
            .find_map(|line| line.strip_prefix("c=IN IP4 "))
            .unwrap();
        let destination: SocketAddr = (address.parse::<std::net::Ipv4Addr>().unwrap(), port).into();
        assert!(destination.ip().is_loopback());
        request(signaling, contact, dialog, "ACK", &to, "");
        Self {
            signaling,
            contact,
            dialog,
            to,
            media,
            destination,
            sequence: 1,
            timestamp: 8000,
            event_type,
            next: Instant::now(),
        }
    }

    fn packet(&mut self, kind: u8, marker: bool, timestamp: u32, payload: &[u8]) {
        std::thread::sleep(self.next.saturating_duration_since(Instant::now()));
        let mut packet = vec![0x80, kind | if marker { 0x80 } else { 0 }];
        packet.extend(self.sequence.to_be_bytes());
        packet.extend(timestamp.to_be_bytes());
        packet.extend(0x48c011ab_u32.to_be_bytes());
        packet.extend_from_slice(payload);
        self.media.send_to(&packet, self.destination).unwrap();
        self.sequence = self.sequence.wrapping_add(1);
        self.next = Instant::now() + Duration::from_millis(20);
    }

    pub(super) fn digits(&mut self, digits: &str) {
        for digit in digits.bytes() {
            let Some(event_type) = self.event_type else {
                self.audio_digit(digit);
                continue;
            };
            let code = match digit {
                b'0'..=b'9' => digit - b'0',
                b'*' => 10,
                b'#' => 11,
                _ => panic!("unsupported telephone-event fixture symbol"),
            };
            let start = self.timestamp;
            for (step, duration) in [160_u16, 320, 480, 640, 640, 640].into_iter().enumerate() {
                let [hi, lo] = duration.to_be_bytes();
                self.packet(
                    event_type,
                    step == 0,
                    start,
                    &[code, 11 | if step >= 3 { 0x80 } else { 0 }, hi, lo],
                );
            }
            self.timestamp = self.timestamp.wrapping_add(960);
            self.audio(3, true);
        }
    }

    fn audio_digit(&mut self, digit: u8) {
        const KEYS: &[u8] = b"123A456B789C*0#D";
        let key = KEYS.iter().position(|symbol| *symbol == digit).unwrap();
        let row = [697., 770., 852., 941.][key / 4];
        let column = [1209., 1336., 1477., 1633.][key % 4];
        // Known signed G.711 mu-law wire levels form two simultaneous test tones.
        // No RFC 4733 payload is offered or sent on this dialog.
        for part in 0..6 {
            let pcmu: [u8; 160] = std::array::from_fn(|offset| {
                let t = std::f64::consts::TAU * (part * 160 + offset) as f64 / 8_000.;
                let wave = (row * t).sin() + (column * t).sin();
                let level = match wave.abs() {
                    x if x < 0.24 => 0xff,
                    x if x < 0.75 => 0xbf,
                    x if x < 1.45 => 0xaf,
                    _ => 0x9f,
                };
                if wave.is_sign_negative() {
                    level & 0x7f
                } else {
                    level
                }
            });
            self.packet(0, part == 0, self.timestamp, &pcmu);
            self.timestamp = self.timestamp.wrapping_add(160);
        }
        self.audio(5, true);
    }

    pub(super) fn audio(&mut self, blocks: usize, silence: bool) {
        // PCMU octets on the wire. The product must decode and mix them on its real owners.
        let pcmu: [u8; 160] = std::array::from_fn(|n| {
            if silence {
                0xff
            } else if n % 16 < 8 {
                0xa2
            } else {
                0x22
            }
        });
        for _ in 0..blocks {
            self.packet(0, false, self.timestamp, &pcmu);
            self.timestamp = self.timestamp.wrapping_add(160);
        }
    }

    /// A post-answer marker distinct from the provisional 500 Hz fixture.
    pub(super) fn established_800hz(&mut self, blocks: usize) {
        let pcmu: [u8; 160] = std::array::from_fn(|i| if i % 10 < 5 { 0xa2 } else { 0x22 });
        for _ in 0..blocks {
            self.packet(0, false, self.timestamp, &pcmu);
            self.timestamp = self.timestamp.wrapping_add(160);
        }
    }

    pub(super) fn spoken_yes(&mut self) {
        for block in include_bytes!("speech-yes.ulaw").chunks_exact(160) {
            self.packet(0, false, self.timestamp, block);
            self.timestamp = self.timestamp.wrapping_add(160);
        }
    }

    /// Keep the independent source's packet clock running throughout a management request.
    pub(super) fn during<T>(&mut self, silence: bool, operation: impl FnOnce() -> T) -> T {
        std::thread::scope(|scope| {
            let (done, running) = std::sync::mpsc::channel::<()>();
            let wire = scope.spawn(move || {
                let mut blocks = 0;
                while let Err(std::sync::mpsc::TryRecvError::Empty) = running.try_recv() {
                    self.audio(1, silence);
                    blocks += 1;
                }
                blocks
            });
            let result = operation();
            drop(done);
            let blocks = wire.join().unwrap();
            eprintln!("sent {blocks} timestamped PCMU packets during the management edit");
            result
        })
    }

    /// A deliberate period without packets advances the wire source clock as well.
    pub(super) fn resume_audio_clock(&mut self) {
        let now = Instant::now();
        let skipped = now.saturating_duration_since(self.next).as_millis() / 20;
        self.timestamp = self
            .timestamp
            .wrapping_add(u32::try_from(skipped * 160).unwrap());
        self.next = now;
    }

    /// Compare audible segments on the call's RTP and receipt clocks.
    pub(super) fn private_prompt_gap(&mut self) -> (u32, u128) {
        use std::sync::atomic::{AtomicBool, Ordering};

        let last = self.drain_private_prompt(Duration::from_secs(20));
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;

        // Continue reading during the controlled pause. A provider may send
        // silence here, while an audible remainder would invalidate the boundary.
        self.during(true, || {
            assert!(
                Self::audible_prompt_until(
                    &media,
                    destination,
                    Instant::now() + Duration::from_secs(1),
                )
                .is_none(),
                "the prior prompt was still audible in the controlled pause"
            );
        });

        let terminating_mailbox = AtomicBool::new(false);
        let first = std::thread::scope(|scope| {
            let listener = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(4);
                let packet = Self::audible_prompt_until(&media, destination, deadline)
                    .expect("audible password prompt");
                assert!(
                    terminating_mailbox.load(Ordering::Acquire),
                    "unexpected audible prompt before submitting the mailbox"
                );
                packet
            });
            self.digits("741");
            terminating_mailbox.store(true, Ordering::Release);
            self.digits("#");
            listener.join().unwrap()
        });
        assert_eq!(first.ssrc, last.ssrc);
        let sequence_delta = first.sequence.wrapping_sub(last.sequence);
        assert!((1..=u16::MAX / 2).contains(&sequence_delta));
        (
            first.timestamp.wrapping_sub(last.timestamp),
            first.time.duration_since(last.time).as_millis(),
        )
    }

    /// Check the initial no-input timer, then enter each field after its prompt.
    pub(super) fn wait_before_login(&mut self) -> Option<u128> {
        let last = self.drain_private_prompt(Duration::from_secs(20));
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        let retry = self.during(true, || {
            Self::audible_prompt_until(&media, destination, last.time + Duration::from_secs(5))
        });
        if retry.is_some() {
            self.drain_private_prompt(Duration::from_secs(20));
        }
        self.digits("741#");
        // The unfixed host may append Retry immediately after Password; allow both
        // finite fallback streams to drain without approaching the call admission cap.
        self.drain_private_prompt(Duration::from_secs(30));
        self.digits("1234#");
        retry.map(|packet| packet.time.duration_since(last.time).as_millis())
    }

    pub(super) fn audible_greeting(&mut self) -> Instant {
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        self.during(true, || {
            Self::audible_prompt_until(&media, destination, Instant::now() + Duration::from_secs(4))
        })
        .expect("private greeting must be playing")
        .time
    }

    /// Check a currently audible greeting without requiring a second prompt producer.
    pub(super) fn barge_during_greeting(&mut self, other: &mut Self) -> (bool, bool) {
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        let target = other.during(true, || {
            let age_ms = self.audible_greeting().elapsed().as_millis();
            assert!(age_ms <= 100, "target greeting was not heard immediately before the digit");
            eprintln!("PRIVATE_LOGIN_BARGE_READY target_audible=true target_age_ms={age_ms} unrelated_programme=true");

            self.digits("7");
            // Anything admitted before the digit may already be in the socket.
            self.audio(12, true);
            Self::drain_prompt_backlog(&media, destination);
            self.during(true, || {
                Self::audible_prompt_until(
                    &media, destination, Instant::now() + Duration::from_millis(650),
                ).is_some()
            })
        });
        let unrelated = self.during(true, || {
            other.fresh_station_audio_within(Duration::from_secs(2))
        });
        (target, unrelated)
    }

    fn drain_prompt_backlog(media: &UdpSocket, destination: SocketAddr) {
        media.set_nonblocking(true).unwrap();
        let mut wire = [0; 2048];
        let drained = (0..512).any(|_| match media.recv_from(&mut wire) {
            Ok((_, source)) => {
                assert_eq!(source, destination);
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(error) => panic!("fixture private RTP backlog read failed: {error}"),
        });
        media.set_nonblocking(false).unwrap();
        assert!(drained, "fixture private RTP backlog did not drain");
    }

    fn drain_private_prompt(&mut self, budget: Duration) -> PromptPacket {
        // The nominal Morse word gap is 420 ms. Continuous encoded silence
        // does not extend the boundary, and neither does a missing datagram.
        const QUIET: Duration = Duration::from_secs(1);
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        self.during(true, || {
            let deadline = Instant::now() + budget;
            let mut last: Option<PromptPacket> = None;
            loop {
                let boundary = last.map(|packet| packet.time + QUIET);
                let now = Instant::now();
                if boundary.is_some_and(|end| end <= now && end <= deadline) {
                    return last.expect("an audible private prompt");
                }
                let until = boundary.map_or(deadline, |end| end.min(deadline));
                let timeout = until
                    .checked_duration_since(now)
                    .filter(|left| !left.is_zero())
                    .expect("private prompt did not finish");
                if let Some(packet) = Self::private_prompt_packet(&media, destination, timeout)
                    && packet.audible
                {
                    last = Some(packet);
                }
            }
        })
    }

    fn audible_prompt_until(
        media: &UdpSocket,
        destination: SocketAddr,
        deadline: Instant,
    ) -> Option<PromptPacket> {
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if left.is_zero() {
                break;
            }
            let packet = Self::private_prompt_packet(media, destination, left)?;
            if packet.audible {
                return Some(packet);
            }
        }
        None
    }

    fn private_prompt_packet(
        media: &UdpSocket,
        destination: SocketAddr,
        timeout: Duration,
    ) -> Option<PromptPacket> {
        media.set_read_timeout(Some(timeout)).unwrap();
        let mut wire = [0; 2048];
        let (length, source) = match media.recv_from(&mut wire) {
            Ok(packet) => packet,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return None;
            }
            Err(error) => panic!("fixture private RTP read failed: {error}"),
        };
        let time = Instant::now();
        assert_eq!(source, destination);
        assert_eq!(
            length, 172,
            "the negotiated PCMU packet contains 160 samples"
        );
        assert_eq!(wire[0], 0x80);
        assert_eq!(wire[1] & 0x7f, 0);
        Some(PromptPacket {
            time,
            timestamp: u32::from_be_bytes(wire[4..8].try_into().unwrap()),
            sequence: u16::from_be_bytes(wire[2..4].try_into().unwrap()),
            ssrc: u32::from_be_bytes(wire[8..12].try_into().unwrap()),
            audible: wire[12..length]
                .iter()
                .any(|code| !matches!(code, 0xff | 0x7f)),
        })
    }

    /// Hold the same answered leg while the fixture toggles its physical carrier.
    pub(super) fn rtp_handover(&mut self, carrier: &std::path::Path) -> bool {
        let media = self.media.try_clone().unwrap();
        let destination = self.destination;
        let (initial_silence, programme, resumed_silence, monotonic) = self.during(true, || {
            let mut previous: Option<PromptPacket> = None;
            let mut monotonic = true;
            let mut observe = |audible| {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut consecutive = 0;
                while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                    if left.is_zero() {
                        break;
                    }
                    let Some(packet) = Self::private_prompt_packet(&media, destination, left)
                    else {
                        break;
                    };
                    if let Some(prior) = previous {
                        monotonic &= packet.ssrc == prior.ssrc
                            && packet.sequence == prior.sequence.wrapping_add(1)
                            && (packet.timestamp.wrapping_sub(prior.timestamp) as i32) >= 160;
                    }
                    previous = Some(packet);
                    consecutive = if packet.audible == audible {
                        consecutive + 1
                    } else {
                        0
                    };
                    if consecutive == 3 {
                        return true;
                    }
                }
                false
            };
            let initial_silence = observe(false);
            std::fs::write(carrier, []).unwrap();
            let programme = observe(true);
            std::fs::remove_file(carrier).unwrap();
            let resumed_silence = observe(false);
            (initial_silence, programme, resumed_silence, monotonic)
        });
        eprintln!(
            "RTP_HANDOVER initial_silence={initial_silence} programme={programme} resumed_silence={resumed_silence} monotonic={monotonic}"
        );
        initial_silence && programme && resumed_silence && monotonic
    }

    pub(super) fn receive_private_audio(&self) -> usize {
        self.media
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut data = [0; 2048];
        while Instant::now() < deadline {
            let (len, sender) = self.media.recv_from(&mut data).unwrap();
            assert_eq!(sender, self.destination);
            assert!(len >= 12);
            assert_eq!(
                data[0], 0x80,
                "the negotiated private source uses plain RTP headers"
            );
            assert_eq!(
                data[1] & 0x7f,
                0,
                "private digits must never echo to the caller"
            );
            if data[12..len].iter().any(|o| !matches!(o, 0xff | 0x7f)) {
                return len - 12;
            }
        }
        panic!("private prompt never delivered audible PCMU");
    }

    pub(super) fn expect_revocation(self) {
        self.read_revocation().unwrap();
    }

    pub(super) fn read_revocation(&self) -> std::io::Result<()> {
        let mut buffer = [0; 8192];
        let (bye, sender) = registrar::receive_register(self.signaling, &mut buffer)?;
        assert_eq!(sender, self.contact);
        assert!(bye.starts_with("BYE "));
        assert_eq!(
            registrar::header(&bye, "Call-ID"),
            format!("grant{}@fixture.invalid", self.dialog)
        );
        self.signaling
            .send_to(registrar::response(&bye, 200, false).as_bytes(), sender)?;
        Ok(())
    }

    pub(super) fn hangup(self) {
        request(
            self.signaling,
            self.contact,
            self.dialog,
            "BYE",
            &self.to,
            "",
        );
        let mut buffer = [0; 8192];
        let (reply, sender) = registrar::receive_register(self.signaling, &mut buffer).unwrap();
        assert_eq!(sender, self.contact);
        assert!(reply.starts_with("SIP/2.0 200 "));
        assert_eq!(registrar::header(&reply, "CSeq"), "2 BYE");
    }
}
