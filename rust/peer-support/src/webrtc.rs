//! WebRTC transport setup and data-channel negotiation for an authorized controller session.

use std::time::Instant;

/// Versioned controller records for the negotiated data channels.
pub mod channel;
mod driver;
/// Released mono Opus codec for the WebRTC media owner.
pub mod opus;
/// Sample-timed receiver RF quality observations.
pub mod quality;
/// Connection-owned TURN allocation and relayed datagrams.
pub mod relay;
/// Relative clock intervals reported by authenticated RTCP sender feedback.
pub mod reports;
mod timing;
pub use driver::{ConnectionState, IceRestartBasis, RelaySocket, UdpDriver};
pub use timing::{MediaAnchor, ReceiveGate, ReceiveLease, native_delta_ns};

pub use str0m::change::{SdpAnswer, SdpOffer};
pub use str0m::channel::{ChannelConfig, ChannelId, Reliability};
pub use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt, SenderFeedback};
pub use str0m::rtp::{RtpPacket, RtpWrite, SeqNo, Ssrc};
pub use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcError};

/// Protocol announced in both DCEP channel descriptors.
pub const CHANNEL_PROTOCOL: &str = "rptadv.webrtc.v1";

/// Purpose of a supported in-band data channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelKind {
    /// Reliable ordered decisions, media anchors and explicit release.
    Control,
    /// Unordered, expiring sample-key lease updates.
    Timing,
}

/// Prepare an Opus RTP endpoint using full ICE and verified DTLS fingerprints.
pub fn transport(now: Instant) -> Rtc {
    Rtc::builder()
        .set_ice_lite(false)
        .set_fingerprint_verification(true)
        .set_rtp_mode(true)
        .clear_codecs()
        .enable_opus(true, false)
        .build(now)
}

/// Accept a bounded offer already admitted by the session owner. Connection
/// establishment, media permission and attachment are separate decisions.
pub fn accept_offer(now: Instant, offer: SdpOffer) -> Result<(Rtc, SdpAnswer), RtcError> {
    let mut connection = transport(now);
    let answer = connection.sdp_api().accept_offer(offer)?;
    Ok((connection, answer))
}

/// Classify an in-band channel only when its reliability and version match the
/// owner's timing contract. The owner still binds the channel to a live grant.
pub fn channel_kind(
    config: &ChannelConfig,
    maximum_timing_lifetime_ms: u16,
) -> Option<ChannelKind> {
    if config.protocol != CHANNEL_PROTOCOL || config.negotiated.is_some() {
        return None;
    }
    match (config.label.as_str(), config.ordered, config.reliability) {
        ("control", true, Reliability::Reliable) => Some(ChannelKind::Control),
        ("timing", false, Reliability::MaxPacketLifetime { lifetime })
            if lifetime > 0 && lifetime <= maximum_timing_lifetime_ms =>
        {
            Some(ChannelKind::Timing)
        }
        _ => None,
    }
}
