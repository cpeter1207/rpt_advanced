//! Generation-protected node ownership and control-plane scheduling.

mod aggregate;
mod composition;
pub use composition::{MessageEventIdentity, MessagePortIdentity, MessageValueSnapshot};
pub mod dtmf;
mod prepare;
mod render;
pub use aggregate::{
    CommandCompletion, CommandReceipt, CommandRenderers, CommandResponse, PreparedAdapter,
    PreparedConfigurationTransition, Runtime, RuntimeAudioOwners, RuntimeClock, RuntimeNode,
    RuntimeTransmit,
};
pub use prepare::{NativeFilePreparer, NativeMediaPreparer, NativeSpeechPreparer, RuntimeError};
pub(crate) use render::morse as morse_text;
pub use render::telemetry_speech;
mod generation;
pub mod link_schedule;
pub mod links;
mod node_host;
pub mod scheduler;
pub use generation::{GenerationSettings, GenerationWork, LifecycleError, RuntimeGeneration};
pub use node_host::{
    DeviceHandoff, DeviceUpdate, GenerationGuard, LifecycleState, LifecycleStatus, NodeControl,
    NodeHost, OwnedReceiveOwner, OwnedTransmitOwner, PairedGuard, ReceiveOwner, TransmitOwner,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod aggregate_tests;
