//! Negotiated SIP audio profiles and RTP framing. All operations run off audio.
use std::{net::SocketAddr, os::fd::AsRawFd, time::Instant};

/// Local receive ceiling, independent of the peer's receive packet-time preference.
pub(crate) const MAX_RECEIVE_MS: u32 = 30;
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
    /// iLBC mode30, one 240-frame/50-byte coding unit per transmit packet.
    Ilbc30,
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
    /// At most one negotiated RFC 4733 mapping per supported clock (8/16 kHz).
    pub telephone_events: [Option<TelephoneEvent>; 2],
}
/// One directional RFC 4733 mapping; receive identity never implies TX eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelephoneEvent {
    /// Payload accepted from the peer, as advertised by this endpoint.
    pub receive_payload: u8,
    /// Payload sent to the peer, as advertised by the peer.
    pub send_payload: u8,
    /// Event duration and onset clock, independent of decoded PCM.
    pub clock_rate_hz: u32,
}
impl TelephoneEvent {
    /// Symmetric mapping used by a local offer or an accepted incoming offer.
    pub const fn symmetric(payload: u8, clock_rate_hz: u32) -> Self {
        Self {
            receive_payload: payload,
            send_payload: payload,
            clock_rate_hz,
        }
    }
}
/// Numeric metadata at a terminal RTP playout rejection, never encoded content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedJitterShape {
    /// Native packet and negotiated decode workspace sizes, in their own units.
    Packet {
        /// Encoded byte count returned by native jitter.
        bytes: usize,
        /// Capacity of the negotiated encoded packet workspace.
        max_bytes: usize,
        /// Source sample frames in the native decision.
        frames: u32,
        /// Capacity of the negotiated decoder PCM workspace.
        max_frames: usize,
        /// Signed native packet offset in source sample frames.
        offset: i32,
        /// Unwrapped source coordinate of the native decision.
        timestamp: u64,
        /// Last credited wire coordinate, if one exists.
        wire_end: Option<u64>,
    },
    /// Named checked arithmetic operation and its two original operands.
    Range {
        /// Stable name of the rejecting arithmetic operation.
        check: &'static str,
        /// First original operand; no adjusted or truncated value.
        left: u64,
        /// Second original operand; no adjusted or truncated value.
        right: u64,
    },
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
    JitterShape(RejectedJitterShape),
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
    /// Encoded bytes represented by one fixed transmit packet.
    pub packet_bytes: usize,
}
impl AudioProfile {
    /// Implemented fixed-packet profiles, retaining PCMU as the preference.
    pub const ALL: [Self; 9] = [
        Self::Mulaw,
        Self::Alaw,
        Self::G722,
        Self::Gsm,
        Self::G726,
        Self::G726Aal2,
        Self::L16Narrowband,
        Self::L16Wideband,
        Self::Ilbc30,
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
            Self::Ilbc30 => (ProfileId::Ilbc, 8000, 8000, 50),
        };
        AudioParameters {
            provider_profile,
            mode_id: if self == Self::Ilbc30 { 30 } else { 0 },
            pcm_rate_hz,
            rtp_clock_hz,
            packet_frames: if self == Self::Ilbc30 {
                240
            } else {
                pcm_rate_hz / 50
            },
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
            Self::Ilbc30 => 100,
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
            Self::Ilbc30 => "iLBC",
        }
    }
    fn matches_format(self, format: &str) -> bool {
        let expected = format!("{}/{}", self.name(), self.parameters().rtp_clock_hz);
        // RFC8866 encoding names are case-insensitive; clock/channel syntax stays exact.
        format.eq_ignore_ascii_case(&expected)
            || format.eq_ignore_ascii_case(&format!("{expected}/1"))
    }
}

impl MediaDescription {
    /// Select one supported audio stream without accepting security downgrade.
    pub fn parse(source: &[u8]) -> Result<Self, MediaError> {
        Self::parse_description(source, false)
    }
    /// Answers retain offered formats with directional payload numbering. An
    /// offered payload cannot be reinterpreted as another format or clock.
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
            )
        }) {
            return Err(invalid);
        }
        // ptime is a preference, not a wire-shape requirement. Peer maxptime
        // bounds our fixed TX packet; our independent RX ceiling is advertised.
        let mut maximum = MAX_RECEIVE_MS;
        for attribute in attributes().filter(|a| matches!(a.key.as_str(), "ptime" | "maxptime")) {
            let whole = attribute
                .value
                .as_deref()
                .and_then(packet_time_floor)
                .ok_or(invalid)?;
            if attribute.key == "maxptime" {
                maximum = maximum.min(whole);
            }
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
        let mut telephone_events = [None; 2];
        for value in &name.formats {
            // Unknown incoming formats do not invalidate usable audio; every
            // answer format must correspond to a format we actually offered.
            let Some(payload) = value.parse::<u8>().ok().filter(|pt| *pt <= 127) else {
                if answer {
                    return Err(invalid);
                }
                continue;
            };
            let mut mapping = None;
            for attribute in attributes().filter(|a| a.key == "rtpmap") {
                let (pt, format) = attribute
                    .value
                    .as_deref()
                    .and_then(|v| v.split_once(' '))
                    .ok_or(invalid)?;
                if pt == value && mapping.replace(format).is_some() {
                    return Err(invalid);
                }
            }
            let codec = AudioProfile::ALL.into_iter().find(|codec| {
                if payload < 96 {
                    codec.payload() == payload && mapping.is_none_or(|v| codec.matches_format(v))
                } else {
                    mapping.is_some_and(|v| codec.matches_format(v))
                }
            });
            if payload < 96
                && AudioProfile::ALL.iter().any(|c| c.payload() == payload)
                && codec.is_none()
            {
                return Err(invalid);
            }
            let event_clock = match mapping {
                Some(value)
                    if value.eq_ignore_ascii_case("telephone-event/8000")
                        || value.eq_ignore_ascii_case("telephone-event/8000/1") =>
                {
                    Some(8000)
                }
                Some(value)
                    if value.eq_ignore_ascii_case("telephone-event/16000")
                        || value.eq_ignore_ascii_case("telephone-event/16000/1") =>
                {
                    Some(16000)
                }
                _ => None,
            }
            .filter(|_| (96..=127).contains(&payload))
            .filter(|_| {
                attributes()
                    .filter(|a| a.key == "fmtp")
                    .filter_map(|a| a.value.as_deref()?.split_once(' '))
                    .filter(|(pt, _)| *pt == value)
                    .all(|(_, events)| complete_dtmf_range(events))
            });
            if answer {
                if codec.is_none() && event_clock.is_none() {
                    return Err(invalid);
                }
                if let Some(offered) = AudioProfile::ALL
                    .into_iter()
                    .find(|c| c.payload() == payload)
                {
                    if codec != Some(offered) {
                        return Err(invalid);
                    }
                }
                if matches!(payload, 101 | 102)
                    && event_clock != Some(if payload == 101 { 8000 } else { 16000 })
                {
                    return Err(invalid);
                }
            }
            if codec == Some(AudioProfile::Ilbc30) {
                let mut mode = None;
                for attribute in attributes().filter(|a| a.key == "fmtp") {
                    let (pt, parameters) = attribute
                        .value
                        .as_deref()
                        .and_then(|v| v.split_once(' '))
                        .ok_or(invalid)?;
                    if pt != value {
                        continue;
                    }
                    for parameter in parameters.split(';') {
                        let (key, value) = parameter
                            .trim()
                            .split_once('=')
                            .unwrap_or((parameter.trim(), ""));
                        let value = value.trim();
                        if key.trim().eq_ignore_ascii_case("mode")
                            && (!matches!(value, "20" | "30") || mode.replace(value).is_some())
                        {
                            return Err(invalid);
                        }
                    }
                }
                // RFC3952's lower-bandwidth common mode is30 even for a mode20
                // peer preference. Absent mode also retains the30ms default.
            }
            if selected.is_none() {
                selected = codec
                    .filter(|codec| {
                        let profile = codec.parameters();
                        profile.packet_frames * 1000 / profile.pcm_rate_hz <= maximum
                    })
                    .map(|codec| (codec, payload));
            }
            if let Some(clock_rate_hz) = event_clock {
                let index = usize::from(clock_rate_hz == 16000);
                if telephone_events[index].is_some() {
                    // One offered mapping per rate: an answer cannot add aliases.
                    if answer {
                        return Err(invalid);
                    }
                } else {
                    telephone_events[index] = Some(TelephoneEvent {
                        receive_payload: if answer { 101 + index as u8 } else { payload },
                        send_payload: payload,
                        clock_rate_hz,
                    });
                }
            }
        }
        let (codec, payload) = selected.ok_or(invalid)?;
        if !answer {
            // An answerer genuinely omits unmatched events. An offerer must
            // retain valid mixed answers for RX while falling back only for TX.
            for event in &mut telephone_events {
                if event.is_some_and(|e| e.clock_rate_hz != codec.parameters().rtp_clock_hz) {
                    *event = None;
                }
            }
        }
        Ok(Self {
            remote: SocketAddr::new(ip, port),
            codec,
            receive_payload: if answer { codec.payload() } else { payload },
            send_payload: payload,
            telephone_events,
        })
    }
    /// Native event TX is allowed only at the selected audio RTP clock. Mixed
    /// answers keep native RX and use the existing synthesized in-band TX path.
    pub fn transmit_event(&self) -> Option<TelephoneEvent> {
        self.telephone_events
            .iter()
            .flatten()
            .copied()
            .find(|event| event.clock_rate_hz == self.codec.parameters().rtp_clock_hz)
    }
    /// Receive and transmit decisions are independent; the product preserves RX
    /// native events even when its outgoing control must synthesize audio tones.
    pub(crate) fn digit_capabilities(&self) -> u32 {
        if self.telephone_events.iter().all(Option::is_none) {
            64
        } else if self.transmit_event().is_none() {
            512
        } else {
            0
        }
    }
    fn valid_payloads(&self) -> bool {
        let audio = |pt| pt == self.codec.payload() || (96..=127).contains(&pt);
        if !audio(self.receive_payload) || !audio(self.send_payload) {
            return false;
        }
        for (index, event) in self.telephone_events.iter().enumerate() {
            if let Some(event) = event {
                if event.clock_rate_hz != [8000, 16000][index]
                    || !(96..=127).contains(&event.receive_payload)
                    || !(96..=127).contains(&event.send_payload)
                    || event.receive_payload == self.receive_payload
                    || event.send_payload == self.send_payload
                {
                    return false;
                }
            }
        }
        !matches!(self.telephone_events, [Some(a), Some(b)]
            if a.receive_payload == b.receive_payload || a.send_payload == b.send_payload)
    }
    /// Answer the selected stream at a local media address.
    pub fn answer(&self, local: SocketAddr, session: u64) -> Result<Vec<u8>, MediaError> {
        Self::describe(
            local,
            session,
            &[(self.codec, self.receive_payload)],
            self.telephone_events,
        )
    }
    /// Offer implemented profiles with one event mapping per supported RTP clock.
    pub(crate) fn offer(local: SocketAddr, session: u64) -> Result<Vec<u8>, MediaError> {
        let codecs = AudioProfile::ALL.map(|codec| (codec, codec.payload()));
        Self::describe(
            local,
            session,
            &codecs,
            [
                Some(TelephoneEvent::symmetric(101, 8000)),
                Some(TelephoneEvent::symmetric(102, 16000)),
            ],
        )
    }
    fn describe(
        local: SocketAddr,
        session: u64,
        codecs: &[(AudioProfile, u8)],
        events: [Option<TelephoneEvent>; 2],
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
        for (codec, payload) in codecs {
            if *codec == AudioProfile::Ilbc30 {
                use std::fmt::Write;
                write!(attributes, "a=fmtp:{payload} mode=30\r\n").expect("String formatting");
            }
        }
        // A mixed-duration offer cannot associate one ptime with each format.
        // The selected answer advertises its actual fixed TX cadence.
        if let [(codec, _)] = codecs {
            use std::fmt::Write;
            let profile = codec.parameters();
            let duration = profile.packet_frames * 1000 / profile.pcm_rate_hz;
            write!(attributes, "a=ptime:{duration}\r\n").expect("String formatting");
        }
        for event in events.into_iter().flatten() {
            let pt = event.receive_payload;
            let rate = event.clock_rate_hz;
            use std::fmt::Write;
            write!(formats, " {pt}").expect("String formatting");
            write!(
                attributes,
                "a=rtpmap:{pt} telephone-event/{rate}\r\na=fmtp:{pt} 0-15\r\n"
            )
            .expect("String formatting");
        }
        Ok(format!("v=0\r\no=- {session} {session} IN {family} {}\r\ns=rpt-advanced\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {formats}\r\n{attributes}a=maxptime:{MAX_RECEIVE_MS}\r\na=sendrecv\r\n", local.ip(), local.ip(), local.port()).into_bytes())
    }
}

// Validate the full positive decimal spelling while retaining only the whole
// milliseconds needed to compare our integer20/30ms TX periods. Saturation never
// skips suffix validation. Accept trailing fractional zeroes as a receiver extension.
fn packet_time_floor(value: &str) -> Option<u32> {
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    if whole.is_empty() || fraction.is_some_and(str::is_empty) {
        return None;
    }
    let mut positive = false;
    let mut result = 0;
    for byte in whole.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        positive |= byte != b'0';
        result = (result * 10 + u32::from(byte - b'0')).min(MAX_RECEIVE_MS);
    }
    for byte in fraction.unwrap_or("").bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        positive |= byte != b'0';
    }
    positive.then_some(result)
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
    /// One inspected complete-unit encoded packet within the local receive ceiling.
    Audio {
        /// Original wrapping packet sequence number.
        sequence: u16,
        /// Original wrapping source sample clock.
        timestamp: u32,
        /// Original synchronization source.
        ssrc: u32,
        /// Actual encoded payload bytes copied into the receive workspace.
        length: usize,
        /// Native inspected PCM frames, independent of RTP clock ticks.
        span: u32,
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
    /// Exactly one fixed packet from the negotiated released encoder.
    pub payload: &'a [u8],
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
    events: [Option<(u32, u32, u8, bool)>; 16],
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
            || !media.valid_payloads()
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
                wire: vec![0; 12 + media.codec.parameters().packet_bytes].into_boxed_slice(),
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
        if media.remote.is_ipv4() != self.media.remote.is_ipv4() || !media.valid_payloads() {
            return Err(MediaError::UnsupportedDescription);
        }
        self.socket
            .connect(media.remote)
            .await
            .map_err(|_| MediaError::Network)?;
        self.discard_incoming()?;
        self.egress.header.payload_type = media.send_payload;
        self.egress.wire = vec![0; 12 + media.codec.parameters().packet_bytes].into_boxed_slice();
        self.media = media;
        Ok(self)
    }
    /// Drop datagrams from the previous phase. A real empty observation advances the receipt floor.
    pub(super) fn discard_incoming(&mut self) -> Result<(), MediaError> {
        let mut wire = [0; 2048];
        for _ in 0..64 {
            match receive_datagram(&self.socket, &mut wire, 0, &mut self.receipt_floor) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.source = None;
                    self.events = [None; 16];
                    self.next_event = 0;
                    return Ok(());
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
        let Some(event) = self.media.transmit_event() else {
            return -1;
        };
        self.egress.begin_digit(
            digit,
            event.send_payload,
            event.clock_rate_hz,
            self.elapsed_ms(),
        )
    }
    /// Service at most one due event report on the existing serial media turn.
    pub(crate) fn poll_digit(&mut self) -> Result<(), MediaError> {
        self.egress
            .poll_digit(self.elapsed_ms(), |wire| self.socket.try_send(wire))
    }
    /// Poll on the existing SIP owner without blocking signaling or retained source credit.
    /// The receipt floor precedes every packet since the last proven empty socket;
    /// it cannot advance merely because Tokio has not dispatched readiness yet.
    pub(crate) fn try_receive(
        &mut self,
        output: &mut [u8],
        inspect: &impl Fn(&[u8]) -> Result<u32, MediaError>,
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
                .receive_wire(&wire[..length], output, inspect)
                .map(|event| Some((event, self.receipt_floor))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(MediaError::Socket(error.raw_os_error().unwrap_or(0))),
        }
    }
    fn receive_wire(
        &mut self,
        wire: &[u8],
        output: &mut [u8],
        inspect: &impl Fn(&[u8]) -> Result<u32, MediaError>,
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
            if output.len() < payload.len() {
                return Err(MediaError::InvalidPacket);
            }
            let span = inspect(payload)?;
            output[..payload.len()].copy_from_slice(payload);
            RtpEvent::Audio {
                sequence: header.sequence_number,
                timestamp: header.timestamp,
                ssrc: header.ssrc,
                length: payload.len(),
                span,
            }
        } else if let Some(event) = self
            .media
            .telephone_events
            .iter()
            .flatten()
            .find(|event| event.receive_payload == header.payload_type)
        {
            self.digit(event.clock_rate_hz, header.timestamp, payload)?
        } else {
            return Err(MediaError::InvalidPacket);
        };
        self.source = Some(header.ssrc);
        Ok(event)
    }
    fn digit(
        &mut self,
        clock_rate_hz: u32,
        timestamp: u32,
        payload: &[u8],
    ) -> Result<RtpEvent, MediaError> {
        let [event, flags, high, low] = payload else {
            return Err(MediaError::InvalidPacket);
        };
        if *event > 15 || flags & 0x40 != 0 || u16::from_be_bytes([*high, *low]) == 0 {
            return Err(MediaError::InvalidPacket);
        }
        let ended = flags & 0x80 != 0;
        let digit = b"0123456789*#ABCD"[usize::from(*event)];
        if let Some((_, _, _, previous_end)) =
            self.events
                .iter_mut()
                .flatten()
                .find(|(clock, time, symbol, _)| {
                    *clock == clock_rate_hz && *time == timestamp && symbol == event
                })
        {
            if *previous_end || !ended {
                return Ok(RtpEvent::None);
            }
            *previous_end = true;
        } else {
            self.events[self.next_event] = Some((clock_rate_hz, timestamp, *event, ended));
            self.next_event = (self.next_event + 1) % self.events.len();
        }
        Ok(RtpEvent::Digit { digit, ended })
    }
}
/// Independent contexts through the existing protocol-neutral codec client.
pub use super::codec::{AudioCodec, DecodeProvenance};

/// One immutable datagram retained only after socket backpressure. Audio and
/// telephone events share this one slot, SSRC and sequence-number owner.
struct PendingDatagram {
    length: usize,
    audio: bool,
    timestamp: u32,
    marker: bool,
    deadline_ms: u32,
}
impl PendingDatagram {
    fn matches(&self, wire: &[u8], packet: &Packet<'_>) -> bool {
        self.timestamp == packet.timestamp
            && self.marker == packet.marker
            && self.deadline_ms == packet.deadline_ms
            && wire[1] & 0x7f == packet.payload_type
            && wire[12..self.length] == *packet.payload
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
    clock_rate_hz: u32,
    started_ms: u32,
    next_ms: u32,
    final_reports: u8,
}

/// Serial packet framing, separate from socket readiness.
struct RtpEgress {
    header: rtp::header::Header,
    // One setup-sized reusable wire buffer, shared by audio and event retries.
    wire: Box<[u8]>,
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
        if audio.payload.len() + 12 != self.wire.len() {
            return Err(MediaError::InvalidPacket);
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
    fn begin_digit(&mut self, digit: u8, payload_type: u8, clock_rate_hz: u32, now_ms: u32) -> i32 {
        let Some(symbol) = b"0123456789*#ABCD".iter().position(|value| *value == digit) else {
            return -1;
        };
        if self.epoch == 0
            || now_ms.checked_add(650).is_none()
            || !(96..=127).contains(&payload_type)
            || !matches!(clock_rate_hz, 8000 | 16000)
        {
            return -1;
        }
        if self.digit.is_some() {
            return 1;
        }
        self.digit = Some(TelephoneDigit {
            symbol: symbol as u8,
            payload_type,
            clock_rate_hz,
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
        let duration_ms = (digit.next_ms - digit.started_ms + 20).min(200);
        let duration = (duration_ms * (digit.clock_rate_hz / 1000)) as u16;
        let ended = duration_ms == 200;
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
                timestamp: digit.started_ms.wrapping_mul(digit.clock_rate_hz / 1000),
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
            if !pending.matches(&self.wire, &packet) {
                return Err(MediaError::InvalidPacket);
            }
        }
        if now_ms >= packet.deadline_ms {
            self.pending = None;
            return Ok(SendStatus::Expired);
        }
        if packet.deadline_ms - now_ms > 200 || packet.payload.len() > self.wire.len() - 12 {
            return Err(MediaError::InvalidPacket);
        }
        if self.pending.is_none() {
            self.header.marker = packet.marker;
            self.header.timestamp = packet.timestamp;
            let audio_payload =
                std::mem::replace(&mut self.header.payload_type, packet.payload_type);
            let marshalled = self.header.marshal_to(&mut self.wire[..12]);
            self.header.payload_type = audio_payload;
            marshalled.map_err(|_| MediaError::InvalidPacket)?;
            let length = 12 + packet.payload.len();
            self.wire[12..length].copy_from_slice(packet.payload);
            self.pending = Some(PendingDatagram {
                length,
                audio: packet.audio,
                timestamp: packet.timestamp,
                marker: packet.marker,
                deadline_ms: packet.deadline_ms,
            });
        }
        let pending = self.pending.as_ref().expect("prepared datagram");
        match send(&self.wire[..pending.length]) {
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
