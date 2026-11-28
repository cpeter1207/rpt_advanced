//! Named direct-trigger streams.

/// Resolved operator-authored DTMF stream; credentials must never be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedDtmfMacroSettings {
    /// Descriptive label, independent of the direct trigger.
    pub name: String,
    /// Full star-prefixed command trigger.
    pub trigger: String,
    /// Require the source's existing administrative grant throughout execution.
    pub protected: bool,
    /// Explicitly terminated commands and comma pauses.
    pub commands: String,
}
