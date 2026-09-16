#![deny(warnings, missing_docs)]

//! Portable product lifecycle and radio-controller orchestration.

#[allow(warnings, missing_docs, clippy::all)]
pub(crate) mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}

pub mod control;
pub mod host;
pub mod lifecycle;
pub mod link;
pub mod media;
pub mod services;
pub mod worker;

#[cfg(test)]
mod fixture;

/// Failure at a host-services boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An owned worker could not be started or joined.
    Thread,
    /// A public host object was rejected or unavailable.
    Admission,
    /// A requested operation failed.
    Operation,
    /// Storage or an external resource could not be prepared.
    Allocation,
    /// A requested device or peer could not be reserved.
    Reservation,
    /// A peer or device rate/format is unsupported.
    UnsupportedFormat,
    /// PCM conversion failed.
    Translation,
    /// The host ended a live channel.
    Hangup,
    /// Input or output PCM was malformed.
    InvalidFrame,
    /// The host rejected output.
    Write,
}
