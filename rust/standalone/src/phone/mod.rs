//! Native phone transports; no radio or mailbox authorization lives here.
mod codec;
/// Bounded native SIP media descriptions and packet framing.
pub mod media;

/// Composition-owned SIP registration, independent of media and radio callbacks.
pub(crate) mod registration;

#[cfg(test)]
pub(crate) mod sip_test_registrar;

pub(crate) mod call_media;
