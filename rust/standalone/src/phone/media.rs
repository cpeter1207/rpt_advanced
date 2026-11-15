//! Negotiated SIP audio profiles and RTP framing. All operations run off audio.
use std::{net::SocketAddr, os::fd::AsRawFd, time::Instant};

#[cfg(test)]
mod tests;
/// Implemented SIP wire profiles; unavailable codecs are never advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProfile {
    /// RTP payload 0, PCMU at 8000 Hz.
    Mulaw,
    /// RTP payload 8, PCMA at 8000 Hz.
    Alaw,
    /// RTP payload 9, 16 kHz PCM with the standard 8 kHz RTP clock.
    G722,
    /// GSM full-rate, one standard 33-byte frame per packet.
    Gsm,
    /// Standard RTP G726-32 low-nibble-first packing.
    G726,
    /// AAL2 G726-32 high-nibble-first packing.
    G726Aal2,
    /// Network-order signed linear PCM at 8 kHz.
    L16Narrowband,
    /// Network-order signed linear PCM at 16 kHz.
    L16Wideband,
}
/// Negotiated unencrypted media; no implicit SRTP downgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// Remote media endpoint from SDP.
    pub remote: SocketAddr,
    /// Selected codec wire profile.
    pub codec: AudioProfile,
    /// Payload accepted from the peer, independent of the transmit mapping.
    pub receive_payload: u8,
    /// Payload sent to the peer, independent of the receive mapping.
    pub send_payload: u8,
    /// Negotiated RFC 4733 payload at 8000 Hz, if present.
    pub telephone_event: Option<u8>,
}
/// Redacted media failures, never SDP or packet bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    /// Description is malformed or requires an unsupported profile.
    UnsupportedDescription,
    /// Malformed or mismatched RTP packet.
    InvalidPacket,
    /// Socket I/O failed.
    Network,
    /// Active RTP socket failed; numeric errno only (zero when unavailable).
    Socket(i32),
    /// Released jitter provider operation failed.
    Jitter,
    /// Released jitter decision does not match the negotiated packet shape.
    JitterShape,
    /// Product source sink violated its ABI or accepted-credit contract.
    SourceContract,
    /// The shared media owner lock was poisoned.
    Owner,
    /// Released codec provider operation failed.
    Codec,
    /// The nonwrapping transmit generation is exhausted; replace the call owner.
    Retired,
}
/// Explicit codec and wire clocks; encoded bytes never stand in for PCM frames.
#[derive(Debug, Clone, Copy)]
pub struct AudioParameters {
    /// Existing protocol-neutral codec identity.
    pub provider_profile: rptadv_codec_client::ProfileId,
    /// Selected provider-local mode.
    pub mode_id: u32,
    /// PCM clock used by the converter, jitter source and product ring.
    pub pcm_rate_hz: u32,
    /// RTP clock used only by packet timestamps.
    pub rtp_clock_hz: u32,
    /// PCM samples represented by one transmit packet.
    pub packet_frames: u32,
    /// Maximum encoded bytes in the selected packet profile.
    pub packet_bytes: usize,
}
impl AudioProfile {
    /// Implemented fixed-packet profiles, retaining PCMU as the preference.
    pub const ALL: [Self; 8] = [
        Self::Mulaw,
        Self::Alaw,
        Self::G722,
        Self::Gsm,
        Self::G726,
        Self::G726Aal2,
        Self::L16Narrowband,
        Self::L16Wideband,
    ];
    /// Current bounded packet policy, validated against the released provider.
    pub fn parameters(self) -> AudioParameters {
        use rptadv_codec_client::ProfileId;
        let (provider_profile, pcm_rate_hz, rtp_clock_hz, packet_bytes) = match self {
            Self::Mulaw => (ProfileId::G711Mu, 8000, 8000, 160),
            Self::Alaw => (ProfileId::G711A, 8000, 8000, 160),
            Self::G722 => (ProfileId::G722, 16000, 8000, 160),
            Self::Gsm => (ProfileId::GsmFr, 8000, 8000, 33),
            Self::G726 => (ProfileId::G726, 8000, 8000, 80),
            Self::G726Aal2 => (ProfileId::G726Aal2, 8000, 8000, 80),
            Self::L16Narrowband => (ProfileId::Pcm8, 8000, 8000, 320),
            Self::L16Wideband => (ProfileId::Pcm16, 16000, 16000, 640),
        };
        AudioParameters {
            provider_profile,
            mode_id: 0,
            pcm_rate_hz,
            rtp_clock_hz,
            packet_frames: pcm_rate_hz / 50,
            packet_bytes,
        }
    }
    /// Static assignment or this endpoint's default dynamic offer mapping.
    pub fn payload(self) -> u8 {
        match self {
            Self::Mulaw => 0,
            Self::Alaw => 8,
            Self::G722 => 9,
            Self::Gsm => 3,
            Self::G726 => 96,
            Self::G726Aal2 => 97,
            Self::L16Narrowband => 98,
            Self::L16Wideband => 99,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Mulaw => "PCMU",
            Self::Alaw => "PCMA",
            Self::G722 => "G722",
            Self::Gsm => "GSM",
            Self::G726 => "G726-32",
            Self::G726Aal2 => "AAL2-G726-32",
            Self::L16Narrowband | Self::L16Wideband => "L16",
        }
    }
    fn matches_format(self, format: &str) -> bool {
        let expected = format!("{}/{}", self.name(), self.parameters().rtp_clock_hz);
        format == expected || format == format!("{expected}/1")
    }
}

impl MediaDescription {
    /// Select one supported audio stream without accepting security downgrade.
    pub fn parse(source: &[u8]) -> Result<Self, MediaError> {
        Self::parse_description(source, false)
    }
    /// Answers may only retain offered payloads; invalid advertised events are
    /// not equivalent to genuinely omitting RFC4733 and choosing inband DTMF.
    pub(crate) fn parse_answer(source: &[u8]) -> Result<Self, MediaError> {
        Self::parse_description(source, true)
    }
    fn parse_description(source: &[u8], answer: bool) -> Result<Self, MediaError> {
        use sdp::description::session::SessionDescription;
        let invalid = MediaError::UnsupportedDescription;
        if source.len() > 16_384 {
            return Err(invalid);
        }
        let session = SessionDescription::unmarshal(&mut std::io::Cursor::new(source))
            .map_err(|_| invalid)?;
        let [media] = session.media_descriptions.as_slice() else {
            return Err(invalid);
        };
        let name = &media.media_name;
        if name.media != "audio"
            || name.protos != ["RTP", "AVP"]
            || name.port.range.is_some()
            || session.encryption_key.is_some()
            || media.encryption_key.is_some()
        {
            return Err(invalid);
        }
        if answer
            && name
                .formats
                .iter()
                .any(|pt| !matches!(pt.as_str(), "0" | "8" | "9" | "101"))
        {
            return Err(invalid);
        }
        let attributes = || session.attributes.iter().chain(&media.attributes);
        if attributes().any(|a| {
            matches!(
                a.key.as_str(),
                "crypto"
                    | "fingerprint"
                    | "setup"
                    | "ice-ufrag"
                    | "ice-pwd"
                    | "candidate"
                    | "rtcp-mux"
            ) || (a.key == "ptime" && a.value.as_deref() != Some("20"))
                || (a.key == "maxptime"
                    && a.value
                        .as_deref()
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_none_or(|maximum| maximum < 20))
        }) {
            return Err(invalid);
        }
        let direction = media
            .attributes
            .iter()
            .rev()
            .chain(session.attributes.iter().rev())
            .find(|a| {
                matches!(
                    a.key.as_str(),
                    "sendrecv" | "sendonly" | "recvonly" | "inactive"
                )
            });
        if direction.is_some_and(|a| a.key != "sendrecv") {
            return Err(invalid);
        }
        let connection = media
            .connection_information
            .as_ref()
            .or(session.connection_information.as_ref())
            .ok_or(invalid)?;
        let address = connection.address.as_ref().ok_or(invalid)?;
        let ip: std::net::IpAddr = address.address.parse().map_err(|_| invalid)?;
        let port = u16::try_from(name.port.value)
            .ok()
            .filter(|p| *p != 0)
            .ok_or(invalid)?;
        if connection.network_type != "IN"
            || address.ttl.is_some()
            || address.range.is_some()
            || ip.is_unspecified()
            || ip.is_multicast()
            || connection.address_type != if ip.is_ipv4() { "IP4" } else { "IP6" }
        {
            return Err(invalid);
        }
        let mut selected = None;
        for value in &name.formats {
            // Unsupported offered formats do not invalidate a usable audio stream.
            // Answers are restricted to our advertised formats above.
            let Some(payload) = value.parse::<u8>().ok().filter(|pt| *pt <= 127) else {
                continue;
            };
            let mut mapping = None;
            for attribute in attributes().filter(|a| a.key == "rtpmap") {
                let (pt, format) = attribute
                    .value
                    .as_deref()
                    .and_then(|v| v.split_once(' '))
                    .ok_or(invalid)?;
                if pt == value {
                    if mapping.replace(format).is_some() {
                        return Err(invalid);
                    }
                }
            }
            let codec = AudioProfile::ALL.into_iter().find(|codec| {
                if payload < 96 {
                    codec.payload() == payload
                        && mapping.is_none_or(|value| codec.matches_format(value))
                } else {
                    mapping.is_some_and(|value| codec.matches_format(value))
                }
            });
            if payload < 96
                && AudioProfile::ALL
                    .iter()
                    .any(|codec| codec.payload() == payload)
                && codec.is_none()
            {
                return Err(invalid);
            }
            if answer {
                let valid = match payload {
                    0 | 8 | 9 => codec.is_some_and(|codec| codec.payload() == payload),
                    101 => matches!(
                        mapping,
                        Some("telephone-event/8000" | "telephone-event/8000/1")
                    ),
                    _ => false,
                };
                if !valid {
                    return Err(invalid);
                }
            }
            if selected.is_none() {
                selected = codec.map(|codec| (codec, payload));
            }
        }
        let (codec, payload) = selected.ok_or(invalid)?;
        let telephone_event = attributes().filter(|a| a.key == "rtpmap").find_map(|a| {
            let (pt, format) = a.value.as_deref()?.split_once(' ')?;
            let number = pt.parse::<u8>().ok()?;
            let events_supported = attributes()
                .filter(|a| a.key == "fmtp")
                .filter_map(|a| a.value.as_deref()?.split_once(' '))
                .filter(|(payload, _)| *payload == pt)
                .all(|(_, events)| complete_dtmf_range(events));
            ((96..=127).contains(&number)
                && name.formats.iter().any(|f| f == pt)
                && matches!(format, "telephone-event/8000" | "telephone-event/8000/1")
                && events_supported)
                .then_some(number)
        });
        if answer && name.formats.iter().any(|pt| pt == "101") && telephone_event != Some(101) {
            return Err(invalid);
        }
        Ok(Self {
            remote: SocketAddr::new(ip, port),
            codec,
            receive_payload: payload,
            send_payload: payload,
            telephone_event,
        })
    }
    /// Answer the selected stream at a local media address.
    pub fn answer(&self, local: SocketAddr, session: u64) -> Result<Vec<u8>, MediaError> {
        Self::describe(
            local,
            session,
            &[(self.codec, self.receive_payload)],
            self.telephone_event,
        )
    }
    /// Offer the composition-verified profiles and the fixed telephone-event map.
    pub(crate) fn offer(local: SocketAddr, session: u64) -> Result<Vec<u8>, MediaError> {
        // Batch A remains unadvertised until its call-control/wire qualification.
        Self::describe(
            local,
            session,
            &[
                (AudioProfile::Mulaw, 0),
                (AudioProfile::Alaw, 8),
                (AudioProfile::G722, 9),
            ],
            Some(101),
        )
    }
    fn describe(
        local: SocketAddr,
        session: u64,
        codecs: &[(AudioProfile, u8)],
        event: Option<u8>,
    ) -> Result<Vec<u8>, MediaError> {
        if local.ip().is_unspecified() || local.ip().is_multicast() || local.port() == 0 {
            return Err(MediaError::UnsupportedDescription);
        }
        let family = if local.is_ipv4() { "IP4" } else { "IP6" };
        let mut formats = codecs
            .iter()
            .map(|(_, payload)| payload.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let mut attributes = codecs
            .iter()
            .map(|(codec, payload)| {
                format!(
                    "a=rtpmap:{} {}/{}\r\n",
                    payload,
                    codec.name(),
                    codec.parameters().rtp_clock_hz
                )
            })
            .collect::<String>();
        if let Some(pt) = event {
            use std::fmt::Write;
            write!(formats, " {pt}").expect("String formatting");
            write!(
                attributes,
                "a=rtpmap:{pt} telephone-event/8000\r\na=fmtp:{pt} 0-15\r\n"
            )
            .expect("String formatting");
        }
        Ok(format!("v=0\r\no=- {session} {session} IN {family} {}\r\ns=rpt-advanced\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {formats}\r\n{attributes}a=ptime:20\r\na=sendrecv\r\n", local.ip(), local.ip(), local.port()).into_bytes())
    }
}

// A negotiated RFC 4733 format must cover every telephone keypad symbol.
fn complete_dtmf_range(value: &str) -> bool {
    let mut symbols = 0u16;
    for range in value.split(',') {
        let (first, last) = range.split_once('-').unwrap_or((range, range));
        let (Ok(first), Ok(last)) = (first.parse::<u8>(), last.parse::<u8>()) else {
            return false;
        };
        if first > last {
            return false;
        }
        for event in first..=last.min(15) {
            symbols |= 1u16 << event;
        }
    }
    symbols == u16::MAX
}

/// One ingress event; packet order belongs to the existing jitter owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpEvent {
    /// Duplicate telephone event, with no newly actionable transition.
    None,
    /// One complete encoded packet in the selected 20 ms profile.
    Audio {
        /// Original wrapping packet sequence number.
        sequence: u16,
        /// Original wrapping source sample clock.
        timestamp: u32,
        /// Original synchronization source.
        ssrc: u32,
    },
    /// Telephone event onset or first end, independently of audio admission.
    Digit {
        /// ASCII DTMF symbol.
        digit: u8,
        /// Whether this is the first end for that event.
        ended: bool,
    },
}
/// Encoded source media; timing is fixed before its first submission.
#[derive(Clone, Copy)]
pub struct TimedAudio<'a> {
    /// Exactly one 20 ms packet from the negotiated released encoder.
    pub payload: &'a [u8; 160],
    /// First packet following a source discontinuity.
    pub marker: bool,
    /// Original wrapping RTP-clock position, including discarded gaps.
    pub timestamp: u32,
    /// Immutable deadline in this endpoint's nonwrapping elapsed milliseconds.
    pub deadline_ms: u32,
    /// Exact nonzero generation returned by the most recent retirement.
    pub epoch: u64,
}
/// Result of one nonblocking submission; no result claims remote delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStatus {
    /// Complete UDP datagram accepted by the socket.
    Sent,
    /// No acceptance; wait for writable readiness or the original deadline.
    WouldBlock,
    /// No acceptance; source deadline or generation is no longer current.
    Expired,
}

/// Advance the conservative socket-ingress bound only on an actual kernel
/// EAGAIN. Capturing before the syscall never rejuvenates a queued datagram.
fn receive_datagram(
    socket: &tokio::net::UdpSocket,
    wire: &mut [u8],
    flags: i32,
    floor: &mut Instant,
) -> std::io::Result<usize> {
    let before = Instant::now();
    // SAFETY: the live single-owner UDP descriptor and writable slice remain
    // valid for this nonblocking syscall; MSG_PEEK never consumes queued data.
    let size = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            wire.as_mut_ptr().cast(),
            wire.len(),
            libc::MSG_DONTWAIT | flags,
        )
    };
    if size >= 0 {
        return Ok(size as usize);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        *floor = before;
    }
    Err(error)
}

/// Serial datagram owner, separate from packet jitter and PCM processing.
pub struct RtpEndpoint {
    socket: tokio::net::UdpSocket,
    media: MediaDescription,
    egress: RtpEgress,
    started: std::time::Instant,
    receipt_floor: Instant,
    source: Option<u32>,
    events: [Option<(u32, u8, bool)>; 16],
    next_event: usize,
}
impl RtpEndpoint {
    /// Bind one exact negotiated remote with caller-provided SSRC and sequence.
    pub async fn bind(
        local: SocketAddr,
        media: MediaDescription,
        ssrc: u32,
        sequence: u16,
    ) -> Result<Self, MediaError> {
        if media.remote.port() == 0
            || media.remote.ip().is_unspecified()
            || media.remote.ip().is_multicast()
            || media.receive_payload != media.codec.payload()
            || media.send_payload != media.codec.payload()
            || media
                .telephone_event
                .is_some_and(|payload| !(96..=127).contains(&payload))
        {
            return Err(MediaError::UnsupportedDescription);
        }
        // Every possible receipt follows this initial monotonic lower bound.
        let receipt_floor = Instant::now();
        let socket = tokio::net::UdpSocket::bind(local)
            .await
            .map_err(|_| MediaError::Network)?;
        socket
            .connect(media.remote)
            .await
            .map_err(|_| MediaError::Network)?;
        Ok(Self {
            socket,
            egress: RtpEgress {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: media.send_payload,
                    sequence_number: sequence,
                    ssrc,
                    ..Default::default()
                },
                pending: None,
                epoch: 1,
                digit: None,
            },
            started: std::time::Instant::now(),
            receipt_floor,
            media,
            source: None,
            events: [None; 16],
            next_event: 0,
        })
    }
    /// Adopt an answer on this same offered socket only after a proven empty cut.
    /// This bounded drain is not RTP admission: old SSRC/event state cannot survive.
    pub(crate) async fn select_answer(
        mut self,
        media: MediaDescription,
    ) -> Result<Self, MediaError> {
        if media.remote.is_ipv4() != self.media.remote.is_ipv4()
            || media.telephone_event.is_some_and(|pt| pt != 101)
        {
            return Err(MediaError::UnsupportedDescription);
        }
        self.socket
            .connect(media.remote)
            .await
            .map_err(|_| MediaError::Network)?;
        let mut wire = [0; 2048];
        for _ in 0..64 {
            match receive_datagram(&self.socket, &mut wire, 0, &mut self.receipt_floor) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.egress.header.payload_type = media.send_payload;
                    self.media = media;
                    self.source = None;
                    self.events = [None; 16];
                    self.next_event = 0;
                    return Ok(self);
                }
                Err(error) => return Err(MediaError::Socket(error.raw_os_error().unwrap_or(0))),
            }
        }
        Err(MediaError::Network)
    }
    /// Actual listening address, including an allocated ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr, MediaError> {
        self.socket.local_addr().map_err(|_| MediaError::Network)
    }
    /// Nonwrapping call time used by the product's original-age deadline mapping.
    /// At exhaustion, checked deadline creation fails rather than rejuvenating media.
    pub fn elapsed_ms(&self) -> u32 {
        u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX)
    }
    /// Current transmit generation; zero means retirement exhausted the owner.
    pub fn tx_epoch(&self) -> u64 {
        self.egress.epoch
    }
    /// Drop staged media before acknowledging a source gap, finish or cancellation.
    /// Already submitted UDP datagrams cannot be recalled.
    pub fn retire_audio(&mut self) -> Result<u64, MediaError> {
        self.egress.retire()
    }
    /// Submit once without awaiting readiness or changing the source clock.
    ///
    /// The caller maps the oldest original source birth to an immutable deadline
    /// with at most 200 ms remaining, as the existing private Outbound owner does.
    /// Retry exactly the same request after WouldBlock. Expired means no socket
    /// acceptance; the caller retires converter/encoder history before new media.
    pub fn try_send_audio(&mut self, audio: TimedAudio<'_>) -> Result<SendStatus, MediaError> {
        let now_ms = self.elapsed_ms();
        self.egress
            .send(audio, now_ms, |wire| self.socket.try_send(wire))
    }
    /// Admit one completed symbol without another queue. Zero accepts, one
    /// retains caller custody, and minus one refuses invalid/unnegotiated input.
    pub(crate) fn send_digit(&mut self, digit: u8) -> i32 {
        let Some(payload) = self.media.telephone_event else {
            return -1;
        };
        self.egress.begin_digit(digit, payload, self.elapsed_ms())
    }
    /// Service at most one due event report on the existing serial media turn.
    pub(crate) fn poll_digit(&mut self) -> Result<(), MediaError> {
        self.egress
            .poll_digit(self.elapsed_ms(), |wire| self.socket.try_send(wire))
    }
    /// True only while a real WouldBlock retains an uncommitted datagram.
    pub fn wants_writable(&self) -> bool {
        self.egress.pending.is_some()
    }
    /// Borrow the existing Tokio readiness future only after WouldBlock.
    /// The serial owner also services the retained deadline and call retirement.
    pub async fn writable(&self) -> Result<(), MediaError> {
        self.socket
            .writable()
            .await
            .map_err(|_| MediaError::Network)
    }
    /// Receive a source-filtered packet without reordering it before jitter.
    pub async fn receive(&mut self, output: &mut [u8; 160]) -> Result<RtpEvent, MediaError> {
        loop {
            self.socket
                .readable()
                .await
                .map_err(|_| MediaError::Network)?;
            if let Some((event, _)) = self.try_receive(output)? {
                return Ok(event);
            }
        }
    }
    /// Poll on the existing SIP owner without blocking signaling or retained source credit.
    /// The receipt floor precedes every packet since the last proven empty socket;
    /// it cannot advance merely because Tokio has not dispatched readiness yet.
    pub(crate) fn try_receive(
        &mut self,
        output: &mut [u8; 160],
    ) -> Result<Option<(RtpEvent, Instant)>, MediaError> {
        let mut wire = [0; 2048];
        let mut attempted = false;
        let result = self.socket.try_io(tokio::io::Interest::READABLE, || {
            attempted = true;
            receive_datagram(&self.socket, &mut wire, 0, &mut self.receipt_floor)
        });
        if !attempted {
            // A cached WouldBlock is not an empty-socket observation. A real
            // nonconsuming probe keeps idle bounds current without stealing
            // data or introducing a second socket/receiver.
            match receive_datagram(
                &self.socket,
                &mut [0; 1],
                libc::MSG_PEEK,
                &mut self.receipt_floor,
            ) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(MediaError::Socket(error.raw_os_error().unwrap_or(0))),
            }
        }
        match result {
            Ok(length) => self
                .receive_wire(&wire[..length], output)
                .map(|event| Some((event, self.receipt_floor))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(MediaError::Socket(error.raw_os_error().unwrap_or(0))),
        }
    }
    fn receive_wire(
        &mut self,
        wire: &[u8],
        output: &mut [u8; 160],
    ) -> Result<RtpEvent, MediaError> {
        use webrtc_util::marshal::Unmarshal;
        if wire.len() == 2048 {
            return Err(MediaError::InvalidPacket);
        }
        let mut payload = wire;
        let header =
            rtp::header::Header::unmarshal(&mut payload).map_err(|_| MediaError::InvalidPacket)?;
        if header.version != 2 || self.source.is_some_and(|source| source != header.ssrc) {
            return Err(MediaError::InvalidPacket);
        }
        if header.padding {
            let padding = usize::from(*payload.last().ok_or(MediaError::InvalidPacket)?);
            if padding == 0 || padding > payload.len() {
                return Err(MediaError::InvalidPacket);
            }
            payload = &payload[..payload.len() - padding];
        }
        let event = if header.payload_type == self.media.receive_payload {
            let packet: &[u8; 160] = payload.try_into().map_err(|_| MediaError::InvalidPacket)?;
            output.copy_from_slice(packet);
            RtpEvent::Audio {
                sequence: header.sequence_number,
                timestamp: header.timestamp,
                ssrc: header.ssrc,
            }
        } else if self.media.telephone_event == Some(header.payload_type) {
            self.digit(header.timestamp, payload)?
        } else {
            return Err(MediaError::InvalidPacket);
        };
        self.source = Some(header.ssrc);
        Ok(event)
    }
    fn digit(&mut self, timestamp: u32, payload: &[u8]) -> Result<RtpEvent, MediaError> {
        let [event, flags, high, low] = payload else {
            return Err(MediaError::InvalidPacket);
        };
        if *event > 15 || flags & 0x40 != 0 || u16::from_be_bytes([*high, *low]) == 0 {
            return Err(MediaError::InvalidPacket);
        }
        let ended = flags & 0x80 != 0;
        let digit = b"0123456789*#ABCD"[usize::from(*event)];
        if let Some((_, _, previous_end)) = self
            .events
            .iter_mut()
            .flatten()
            .find(|(time, symbol, _)| *time == timestamp && symbol == event)
        {
            if *previous_end || !ended {
                return Ok(RtpEvent::None);
            }
            *previous_end = true;
        } else {
            self.events[self.next_event] = Some((timestamp, *event, ended));
            self.next_event = (self.next_event + 1) % self.events.len();
        }
        Ok(RtpEvent::Digit { digit, ended })
    }
}
/// Independent contexts through the existing protocol-neutral codec client.
pub use super::codec::AudioCodec;

/// One immutable datagram retained only after socket backpressure. Audio and
/// telephone events share this one slot, SSRC and sequence-number owner.
struct PendingDatagram {
    wire: [u8; 172],
    length: usize,
    audio: bool,
    timestamp: u32,
    marker: bool,
    deadline_ms: u32,
}
impl PendingDatagram {
    fn matches(&self, packet: &Packet<'_>) -> bool {
        self.timestamp == packet.timestamp
            && self.marker == packet.marker
            && self.deadline_ms == packet.deadline_ms
            && self.wire[1] & 0x7f == packet.payload_type
            && self.wire[12..self.length] == *packet.payload
    }
}

/// One borrowed packet request; source deadline and payload never change on retry.
struct Packet<'a> {
    payload: &'a [u8],
    payload_type: u8,
    timestamp: u32,
    marker: bool,
    deadline_ms: u32,
    audio: bool,
}

/// A completed digit is regenerated as a finite 200 ms event with a 50 ms gap,
/// matching the existing default DTMF regeneration shape. No symbol is logged.
struct TelephoneDigit {
    symbol: u8,
    payload_type: u8,
    started_ms: u32,
    next_ms: u32,
    final_reports: u8,
}

/// Serial packet framing, separate from socket readiness.
struct RtpEgress {
    header: rtp::header::Header,
    pending: Option<PendingDatagram>,
    epoch: u64,
    digit: Option<TelephoneDigit>,
}
impl RtpEgress {
    fn retire(&mut self) -> Result<u64, MediaError> {
        // An audio-source cut does not revoke separately admitted call control.
        if self.pending.as_ref().is_some_and(|pending| pending.audio) {
            self.pending = None;
        }
        self.epoch = self
            .epoch
            .checked_add(1)
            .filter(|_| self.epoch != 0)
            .unwrap_or(0);
        if self.epoch == 0 {
            self.digit = None;
            self.pending = None;
            Err(MediaError::Retired)
        } else {
            Ok(self.epoch)
        }
    }
    fn send(
        &mut self,
        audio: TimedAudio<'_>,
        now_ms: u32,
        send: impl FnOnce(&[u8]) -> std::io::Result<usize>,
    ) -> Result<SendStatus, MediaError> {
        if self.epoch == 0 || audio.epoch != self.epoch {
            return Ok(SendStatus::Expired);
        }
        self.packet(
            Packet {
                payload: audio.payload,
                payload_type: self.header.payload_type,
                timestamp: audio.timestamp,
                marker: audio.marker,
                deadline_ms: audio.deadline_ms,
                audio: true,
            },
            now_ms,
            send,
        )
    }
    fn begin_digit(&mut self, digit: u8, payload_type: u8, now_ms: u32) -> i32 {
        let Some(symbol) = b"0123456789*#ABCD".iter().position(|value| *value == digit) else {
            return -1;
        };
        if self.epoch == 0
            || now_ms.checked_add(650).is_none()
            || !(96..=127).contains(&payload_type)
        {
            return -1;
        }
        if self.digit.is_some() {
            return 1;
        }
        self.digit = Some(TelephoneDigit {
            symbol: symbol as u8,
            payload_type,
            started_ms: now_ms,
            next_ms: now_ms,
            final_reports: 0,
        });
        0
    }
    fn poll_digit(
        &mut self,
        now_ms: u32,
        send: impl FnOnce(&[u8]) -> std::io::Result<usize>,
    ) -> Result<(), MediaError> {
        let Some(digit) = &self.digit else {
            return Ok(());
        };
        if digit.final_reports == 3 {
            if now_ms >= digit.next_ms {
                self.digit = None;
            }
            return Ok(());
        }
        if now_ms >= digit.started_ms + 450 {
            self.digit = None;
            if self.pending.as_ref().is_some_and(|pending| !pending.audio) {
                self.pending = None;
            }
            return Ok(());
        }
        if now_ms < digit.next_ms {
            return Ok(());
        }
        let duration = (digit.next_ms - digit.started_ms + 20).min(200) as u16 * 8;
        let ended = duration == 1600;
        let payload = [
            digit.symbol,
            12 | if ended { 0x80 } else { 0 },
            (duration >> 8) as u8,
            duration as u8,
        ];
        let result = self.packet(
            Packet {
                payload: &payload,
                payload_type: digit.payload_type,
                timestamp: digit.started_ms.wrapping_mul(8),
                marker: digit.next_ms == digit.started_ms,
                deadline_ms: (digit.next_ms + 200).min(digit.started_ms + 450),
                audio: false,
            },
            now_ms,
            send,
        )?;
        match result {
            SendStatus::Sent => {
                let digit = self.digit.as_mut().expect("retained event");
                digit.final_reports += u8::from(ended);
                // Never emit a catch-up burst after a late serial turn.
                digit.next_ms = now_ms + if digit.final_reports == 3 { 50 } else { 20 };
            }
            SendStatus::WouldBlock => {}
            SendStatus::Expired => self.digit = None,
        }
        Ok(())
    }
    fn packet(
        &mut self,
        packet: Packet<'_>,
        now_ms: u32,
        send: impl FnOnce(&[u8]) -> std::io::Result<usize>,
    ) -> Result<SendStatus, MediaError> {
        use webrtc_util::marshal::Marshal;
        if self.epoch == 0 {
            return Ok(SendStatus::Expired);
        }
        if let Some(pending) = &self.pending {
            if pending.audio != packet.audio {
                return Ok(SendStatus::WouldBlock);
            }
            if !pending.matches(&packet) {
                return Err(MediaError::InvalidPacket);
            }
        }
        if now_ms >= packet.deadline_ms {
            self.pending = None;
            return Ok(SendStatus::Expired);
        }
        if packet.deadline_ms - now_ms > 200 || packet.payload.len() > 160 {
            return Err(MediaError::InvalidPacket);
        }
        if self.pending.is_none() {
            self.header.marker = packet.marker;
            self.header.timestamp = packet.timestamp;
            let audio_payload =
                std::mem::replace(&mut self.header.payload_type, packet.payload_type);
            let mut wire = [0; 172];
            let marshalled = self.header.marshal_to(&mut wire[..12]);
            self.header.payload_type = audio_payload;
            marshalled.map_err(|_| MediaError::InvalidPacket)?;
            let length = 12 + packet.payload.len();
            wire[12..length].copy_from_slice(packet.payload);
            self.pending = Some(PendingDatagram {
                wire,
                length,
                audio: packet.audio,
                timestamp: packet.timestamp,
                marker: packet.marker,
                deadline_ms: packet.deadline_ms,
            });
        }
        let pending = self.pending.as_ref().expect("prepared datagram");
        match send(&pending.wire[..pending.length]) {
            Ok(_) => {
                self.pending = None;
                self.header.sequence_number = self.header.sequence_number.wrapping_add(1);
                Ok(SendStatus::Sent)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(SendStatus::WouldBlock)
            }
            Err(error) => {
                self.pending = None;
                Err(MediaError::Socket(error.raw_os_error().unwrap_or(0)))
            }
        }
    }
}
