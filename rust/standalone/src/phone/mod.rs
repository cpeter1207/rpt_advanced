//! Native phone transports; no radio or mailbox authorization lives here.
mod codec;
/// Bounded native SIP media descriptions and packet framing.
pub mod media;

/// Composition-owned SIP registration, independent of media and radio callbacks.
pub(crate) mod registration;
