//! Standalone process setup and lifecycle.
#![deny(warnings, missing_docs)]

#[allow(warnings, missing_docs, clippy::all)]
pub(crate) mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}

/// Standalone AllStarLink destination lookup without Asterisk resolver APIs.
pub mod directory;
/// Signal-driven standalone process lifecycle.
pub mod foreground;
/// Asterisk-free implementations of product host services.
pub mod host_services;
/// Dynamically loaded outbound AllStarLink-compatible IAX2 client.
pub mod iax;
/// Runtime provider loading and version validation.
pub mod providers;
/// Safe ordered activation of one standalone CM119 radio path.
pub mod radio_activation;
/// Standalone product requests resolved to configured radio-channel reservations.
pub mod radio_host;
/// ASL3-compatible HTTPS registration kept outside real-time audio callbacks.
pub mod registration;
/// Product start/reload/stop lifetime for the standalone process.
pub mod runtime;
/// Secure IAX credentials loaded separately from the ordinary node configuration.
pub mod secrets;

/// Copied configuration records supplied by the shared controller product.
pub mod configuration;
/// Owned native station requests copied from the product's synchronous visitor.
pub mod native_configuration;
pub use configuration::{ConfigError, ResolvedRadioNode, check_config, resolve_radio_nodes};
