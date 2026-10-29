//! Core task adapter over the shared dynamic control-execution client.
pub use control::ClientError;
use rpt_advanced_core::control::{
    ControlExecutor, ControlTask, DrainError, RejectedTask, RejectionReason,
};
use rptadv_peer_support::control;

/// Existing generated control capability; no public product signature changes.
pub type Descriptor = crate::abi::rptadv_control_descriptor_v1;

/// The product supplies generation-aware tasks; shared support owns ABI/lifecycle.
pub struct ControlClient(control::Client);
impl ControlClient {
    /// Validate/open the selected existing backend.
    /// # Safety
    /// Descriptor spans/code remain live through this owner's off-executor drop.
    /// Its size prefix is truthful and lifecycle never joins from audio/control callbacks.
    pub unsafe fn open(
        descriptor: *const Descriptor,
        name: &str,
        capacity: usize,
    ) -> Result<Self, ClientError> {
        unsafe { control::Client::open(descriptor.cast(), name, capacity) }.map(Self)
    }
}
impl ControlExecutor for ControlClient {
    fn submit(&self, task: ControlTask) -> Result<(), RejectedTask> {
        self.0
            .submit(task, ControlTask::run)
            .map_err(|rejected| RejectedTask {
                task: rejected.payload,
                reason: match rejected.reason {
                    control::RejectionReason::Stopped => RejectionReason::Stopped,
                    control::RejectionReason::Full => RejectionReason::Full,
                    control::RejectionReason::Backend => RejectionReason::Backend,
                },
            })
    }
    fn stop_and_drain(&self) -> Result<(), DrainError> {
        self.0.stop_and_drain().map_err(|_| DrainError::OnExecutor)
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
