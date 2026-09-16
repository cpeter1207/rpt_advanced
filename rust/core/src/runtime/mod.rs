//! Generation-protected node ownership and control-plane scheduling.

mod aggregate;
pub mod dtmf;
mod prepare;
mod render;
pub use aggregate::{
    PreparedAdapter, Runtime, RuntimeAudioOwners, RuntimeClock, RuntimeDispatch, RuntimeNode,
    RuntimeTransmit,
};
pub use prepare::{NativeFilePreparer, NativeMediaPreparer, NativeSpeechPreparer, RuntimeError};
pub use render::telemetry_speech;
mod generation;
pub mod link_schedule;
pub mod links;
mod node_host;
pub mod scheduler;
pub use generation::{GenerationSettings, GenerationWork, LifecycleError, RuntimeGeneration};
pub use node_host::{
    DeviceHandoff, GenerationGuard, LifecycleState, LifecycleStatus, NodeControl, NodeHost,
    OwnedReceiveOwner, OwnedTransmitOwner, PairedGuard, ReceiveOwner, TransmitOwner,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod aggregate_tests;
