#![deny(warnings, missing_docs)]

//! Public-Asterisk channel and codec ownership, with canonical float PCM inward.

#[allow(warnings, missing_docs, clippy::all)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/asterisk.rs"));
}

mod pcm;

pub mod application;
pub mod cli;
pub mod codec;
pub mod connection;
pub mod lifecycle;
pub mod link;
pub mod module;
pub mod product;
pub mod radio;
pub mod services;

/// Failure at the external Asterisk boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An owned worker could not be started or joined.
    Thread,
    /// Incoming public channel metadata or handoff is not acceptable.
    Admission,
    /// Asterisk rejected radio activation.
    Call,
    /// Public application or CLI registration failed.
    Registration,
    /// Required channel technology is not loaded.
    MissingTechnology,
    /// The radio advertises no native format.
    MissingFormat,
    /// Local audio must be exactly 48 kHz signed linear.
    UnsupportedFormat,
    /// Asterisk could not reserve the radio.
    Reservation,
    /// Asterisk rejected the read or write format.
    ChannelFormat,
    /// No usable bidirectional peer codec exists.
    NoCandidates,
    /// Setup could not allocate bounded storage or an Asterisk object.
    Allocation,
    /// Asterisk rejected a single-format offer.
    Offer,
    /// No translator path is available.
    Translation,
    /// Channel read returned no frame.
    Hangup,
    /// Voice payload, format or bounded sample count is invalid.
    InvalidFrame,
    /// Asterisk rejected a PTT change.
    Indication,
    /// Asterisk rejected output audio.
    Write,
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
