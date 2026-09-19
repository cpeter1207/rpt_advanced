//! Replaceable, serialized control execution with explicit task ownership.

use crate::runtime::GenerationWork;

/// One owned control-plane operation, allocated only by non-audio producers.
pub struct ControlTask {
    work: Option<GenerationWork>,
    operation: Box<dyn FnOnce() + Send>,
}

impl ControlTask {
    /// Create lifecycle work with no operating-generation dependency (such as reload).
    pub fn lifecycle(operation: impl FnOnce() + Send + 'static) -> Self {
        Self {
            work: None,
            operation: Box::new(operation),
        }
    }
    /// Create a generation-tagged operation; retired work only drops its owned payload.
    pub fn tagged(work: GenerationWork, operation: impl FnOnce() + Send + 'static) -> Self {
        Self {
            work: Some(work),
            operation: Box::new(operation),
        }
    }
    /// Execute an accepted task once on its serialized owner, suppressing stale policy.
    pub fn run(self) {
        if self.work.as_ref().is_none_or(GenerationWork::is_current) {
            (self.operation)();
        }
    }
}

/// Why a control executor could not accept ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionReason {
    /// Admission has stopped for unload or backend replacement.
    Stopped,
    /// The configured pending-work bound has been reached.
    Full,
    /// The selected backend refused the enqueue.
    Backend,
}

/// Rejection returns the original task to its submitter for cleanup or explicit retry.
pub struct RejectedTask {
    /// Original owned payload; no part of it has executed.
    pub task: ControlTask,
    /// Explicit admission failure.
    pub reason: RejectionReason,
}

/// A drain cannot wait for the calling executor itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainError {
    /// Request shutdown from an external lifecycle owner instead of a running task.
    OnExecutor,
}

/// Replaceable FIFO execution port. Never submit or drain from an audio callback.
pub trait ControlExecutor: Send + Sync {
    /// Transfer ownership only on success. A rejection must never execute inline.
    fn submit(&self, task: ControlTask) -> Result<(), RejectedTask>;
    /// Stop admission, then wait for every accepted task/callback before releasing state.
    fn stop_and_drain(&self) -> Result<(), DrainError>;
}
