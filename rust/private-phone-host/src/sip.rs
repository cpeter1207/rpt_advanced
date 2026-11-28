//! The same Sofia reactor owns its native accounts, calls, RTP and callback registrations.
pub(crate) mod call_media;
mod codec;
pub(crate) mod media;
pub(crate) mod registration;
#[cfg(test)]
mod route_tests;
