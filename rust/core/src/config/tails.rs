//! Checked release-triggered station media.
use super::ResolvedMessageSettings;

/// One named tail, with the existing shared message/media defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTailSettings {
    /// Complete ordered content and inherited source settings.
    pub message: ResolvedMessageSettings,
    /// Minimum milliseconds since successful completion; zero allows every release.
    pub interval_ms: u64,
    /// Milliseconds after actual unkey; zero retains PTT at ordinary hang expiry.
    pub delay_ms: u64,
}
