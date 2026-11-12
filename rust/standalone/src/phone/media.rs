//! Narrow 8 kHz G.711 phone profile. All operations run off audio.
use std::net::SocketAddr;

#[cfg(test)]
mod tests;
/// Selected static RTP audio format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum G711 {
    /// RTP payload 0, PCMU at 8000 Hz.
    Mulaw,
    /// RTP payload 8, PCMA at 8000 Hz.
    Alaw,
}
/// Negotiated unencrypted media; no implicit SRTP downgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// Remote media endpoint from SDP.
    pub remote: SocketAddr,
    /// Selected static G.711 format.
    pub codec: G711,
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
    /// Released G.711 provider operation failed.
    Codec,
    /// The nonwrapping transmit generation is exhausted; replace the call owner.
    Retired,
}
impl G711 {
    fn payload(self) -> u8 {
        match self {
            Self::Mulaw => 0,
            Self::Alaw => 8,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Mulaw => "PCMU",
            Self::Alaw => "PCMA",
        }
    }
}
impl MediaDescription {
    /// Select one supported audio stream without accepting security downgrade.
    pub fn parse(source: &[u8]) -> Result<Self, MediaError> {
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
        let codec = name
            .formats
            .iter()
            .find_map(|value| match value.as_str() {
                "0" => Some(G711::Mulaw),
                "8" => Some(G711::Alaw),
                _ => None,
            })
            .ok_or(invalid)?;
        let expected = format!("{}/8000", codec.name());
        let payload = codec.payload().to_string();
        for attribute in attributes().filter(|a| a.key == "rtpmap") {
            let (pt, format) = attribute
                .value
                .as_deref()
                .and_then(|v| v.split_once(' '))
                .ok_or(invalid)?;
            if pt == payload && format != expected && format != format!("{expected}/1") {
                return Err(invalid);
            }
        }
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
        Ok(Self {
            remote: SocketAddr::new(ip, port),
            codec,
            telephone_event,
        })
    }
    /// Answer the selected stream at a local media address.
    pub fn answer(&self, local: SocketAddr, session: u64) -> Result<Vec<u8>, MediaError> {
        if local.ip().is_unspecified() || local.ip().is_multicast() || local.port() == 0 {
            return Err(MediaError::UnsupportedDescription);
        }
        let family = if local.is_ipv4() { "IP4" } else { "IP6" };
        let payload = self.codec.payload();
        let (event_format, event_attributes) = self
            .telephone_event
            .map(|pt| {
                (
                    format!(" {pt}"),
                    format!("a=rtpmap:{pt} telephone-event/8000\r\na=fmtp:{pt} 0-15\r\n"),
                )
            })
            .unwrap_or_default();
        Ok(format!("v=0\r\no=- {session} {session} IN {family} {}\r\ns=rpt-advanced\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {payload}{event_format}\r\na=rtpmap:{payload} {}/8000\r\na=ptime:20\r\na=sendrecv\r\n{event_attributes}", local.ip(), local.ip(), local.port(), self.codec.name()).into_bytes())
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
    /// One complete encoded 20 ms G.711 packet.
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
    /// Exactly one 20 ms packet from the released G.711 encoder.
    pub payload: &'a [u8; 160],
    /// First packet following a source discontinuity.
    pub marker: bool,
    /// Original wrapping 8 kHz source sample position, including discarded gaps.
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

/// Serial datagram owner, separate from packet jitter and PCM processing.
pub struct RtpEndpoint {
    socket: tokio::net::UdpSocket,
    media: MediaDescription,
    egress: RtpEgress,
    started: std::time::Instant,
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
            || media
                .telephone_event
                .is_some_and(|payload| !(96..=127).contains(&payload))
        {
            return Err(MediaError::UnsupportedDescription);
        }
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
                    payload_type: media.codec.payload(),
                    sequence_number: sequence,
                    ssrc,
                    ..Default::default()
                },
                pending: None,
                epoch: 1,
            },
            started: std::time::Instant::now(),
            media,
            source: None,
            events: [None; 16],
            next_event: 0,
        })
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
        use webrtc_util::marshal::Unmarshal;
        let mut wire = [0; 2048];
        let length = self
            .socket
            .recv(&mut wire)
            .await
            .map_err(|_| MediaError::Network)?;
        if length == wire.len() {
            return Err(MediaError::InvalidPacket);
        }
        let mut payload = &wire[..length];
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
        let event = if header.payload_type == self.media.codec.payload() {
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
/// Independent encode/decode contexts from the released dynamic G.711 provider.
pub use super::codec::G711Codec;

/// One immutable datagram retained only after socket backpressure.
struct PendingAudio {
    wire: [u8; 172],
    timestamp: u32,
    marker: bool,
    deadline_ms: u32,
}
impl PendingAudio {
    fn matches(&self, audio: TimedAudio<'_>) -> bool {
        self.timestamp == audio.timestamp
            && self.marker == audio.marker
            && self.deadline_ms == audio.deadline_ms
            && self.wire[12..] == audio.payload[..]
    }
}

/// Serial packet framing, separate from socket readiness.
struct RtpEgress {
    header: rtp::header::Header,
    pending: Option<PendingAudio>,
    epoch: u64,
}
impl RtpEgress {
    fn retire(&mut self) -> Result<u64, MediaError> {
        self.pending = None;
        self.epoch = self
            .epoch
            .checked_add(1)
            .filter(|_| self.epoch != 0)
            .unwrap_or(0);
        if self.epoch == 0 {
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
        use webrtc_util::marshal::Marshal;
        if self.epoch == 0 || audio.epoch != self.epoch {
            return Ok(SendStatus::Expired);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.matches(audio))
        {
            return Err(MediaError::InvalidPacket);
        }
        if now_ms >= audio.deadline_ms {
            self.pending = None;
            return Ok(SendStatus::Expired);
        }
        if audio.deadline_ms - now_ms > 200 {
            return Err(MediaError::InvalidPacket);
        }
        if self.pending.is_none() {
            self.header.marker = audio.marker;
            self.header.timestamp = audio.timestamp;
            let mut wire = [0; 172];
            self.header
                .marshal_to(&mut wire[..12])
                .map_err(|_| MediaError::InvalidPacket)?;
            wire[12..].copy_from_slice(audio.payload);
            self.pending = Some(PendingAudio {
                wire,
                timestamp: audio.timestamp,
                marker: audio.marker,
                deadline_ms: audio.deadline_ms,
            });
        }
        // UDP submits this fixed 172-byte datagram atomically or returns an error.
        match send(&self.pending.as_ref().expect("prepared datagram").wire) {
            Ok(_) => {
                self.pending = None;
                self.header.sequence_number = self.header.sequence_number.wrapping_add(1);
                Ok(SendStatus::Sent)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(SendStatus::WouldBlock)
            }
            Err(_) => {
                self.pending = None;
                Err(MediaError::Network)
            }
        }
    }
}
