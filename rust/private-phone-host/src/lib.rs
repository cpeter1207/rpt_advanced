//! Private telephone transport ownership shared by the standalone and Asterisk hosts.
#[allow(warnings, missing_docs, clippy::all)]
pub mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}
pub mod accounts;
mod progress;
mod sip;
#[cfg(test)]
#[path = "../tests/support/sip_registrar.rs"]
mod test_sip_registrar;

mod jitter;
pub use jitter::JitterProvider;

/// The private provider or its current call could not complete the operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// A checked provider operation or native lease is unavailable.
    Provider,
    /// The selected registration or call is no longer authoritative.
    Retired,
    /// The caller supplied an inconsistent or out-of-range native operation.
    Input,
}

pub mod calls;

pub mod phone;
pub mod sip_provider;

pub mod selection;

pub mod frontend;

mod iax;
