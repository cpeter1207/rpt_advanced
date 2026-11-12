//! Exact product reload publication contract shared by both host adapters.

/// Terminal product disposition. Unknown statuses are unsafe, never rejection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReloadOutcome {
    /// Publication and operational follow-up completed.
    #[default]
    Committed,
    /// Preparation was rejected without publishing the candidate.
    Rejected,
    /// Candidate published, but operational follow-up failed.
    CommittedWithError,
    /// Coherent publication/rollback is not known; retain owners and close admission.
    Fatal(i32),
}
impl ReloadOutcome {
    /// Decode ABI8 status; only one denotes an accepted transaction still pending.
    pub fn from_status(status: i32) -> Option<Self> {
        match status {
            1 => None,
            0 => Some(Self::Committed),
            -1 => Some(Self::Rejected),
            -2 => Some(Self::CommittedWithError),
            other => Some(Self::Fatal(other)),
        }
    }
    /// Encode the terminal status without discarding an unknown foreign diagnosis.
    pub fn status(self) -> i32 {
        match self {
            Self::Committed => 0,
            Self::Rejected => -1,
            Self::CommittedWithError => -2,
            Self::Fatal(status) => status,
        }
    }
    /// Candidate ownership must be adopted even when later operations failed.
    pub fn published(self) -> bool {
        matches!(self, Self::Committed | Self::CommittedWithError)
    }
}
