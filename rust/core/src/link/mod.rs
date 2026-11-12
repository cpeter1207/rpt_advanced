//! AllStarLink protocol and control-owned link policy, independent of transport.
mod audio;
mod echo_telemetry;
mod hub;
mod identity;
mod peer;
mod phone;
mod protocol;
mod signals;
mod text;
#[cfg(test)]
#[path = "text_tests.rs"]
mod text_tests;
mod topology;

pub use audio::{
    AudioPeer, DispatchProgress, GroupMemberSelection, GroupSelection, LinkAudio, LinkAudioStatus,
    LinkDispatcher, LocalMedia, LocalTextInput, PeerInput, RenderedPeerBlock, SampleProvenance,
    SourceCoordinate, SourcePosition, SourceTimeBound, TimedPeerSample,
};
pub use echo_telemetry::EchoTelemetryPolicy;
pub use hub::{AdmissionError, LinkManager, LinkStatus, Mode, RetryAttempt};
pub use identity::{EchoAlias, IdentityError, PeerAddress, PeerIdentity, ProtocolId};
pub use peer::{Peer, ReceivePolicy, ReceiveState};
pub use phone::{MailboxId, PhoneAccountId, PhoneCallId, PhoneSource};
pub use protocol::{Protocol, Route};
pub use signals::{PeerReply, PeerSignals, RetirementDomain};
pub use text::{ClassifiedText, HumanMessage, TextAudience, TextOrigin, classify_asl};
pub use topology::TopologyManager;

fn identity(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn decimal_identity(name: &str) -> bool {
    identity(name) && name.bytes().all(|b| b.is_ascii_digit())
}

/// Whether an IAX DTMF end value is a conventional completed digit.
pub fn valid_digit(digit: char) -> bool {
    "0123456789ABCD*#".contains(digit)
}
