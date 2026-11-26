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
/// Bound physical station reservations and their control callbacks.
pub mod radio_host;
/// ASL3-compatible HTTPS registration kept outside real-time audio callbacks.
#[path = "../../peer-support/frontend/registration/mod.rs"]
pub mod registration;
/// Product start/reload/stop lifetime for the standalone process.
pub mod runtime;
mod station_plan;

/// Validate the managed service bootstrap and selected configuration.
pub mod configuration;
pub use configuration::{ConfigError, check_config};
