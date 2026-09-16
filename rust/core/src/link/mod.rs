//! AllStarLink protocol and control-owned link policy, independent of transport.
mod audio;
mod hub;
mod peer;
mod protocol;
mod routing;
mod signals;
mod topology;

pub use audio::{AudioPeer, LinkAudio, LinkAudioStatus, LinkDispatcher, PeerInput};
pub use hub::{AdmissionError, LinkManager, LinkStatus, Mode, RetryAttempt};
pub use peer::{Peer, ReceiveState};
pub use protocol::{Protocol, Route};
pub use routing::{MixSource, NativeMixer};
pub use signals::PeerSignals;
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
