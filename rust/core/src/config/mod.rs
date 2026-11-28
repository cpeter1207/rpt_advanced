//! Checked JSON resource profiles and their effective station settings.

mod dtmf_macros;
mod echolink;
mod messages;
mod parse;
mod phone_mailboxes;
pub mod resources;
mod settings;
mod site;
mod tails;
pub use site::{
    AlarmCondition, AlarmSettings, Calibration, FanMode, FanSettings, InputSettings,
    OutputSettings, ResolvedSiteSettings, SensorSettings, SiteAction, SiteBackend, SiteEndpoint,
};
pub use tails::ResolvedTailSettings;

#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod resources_tests;
#[cfg(test)]
mod tests;

pub use dtmf_macros::ResolvedDtmfMacroSettings;
pub use echolink::{EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport};
pub(crate) use messages::selector_valid as message_selector_valid;
pub use messages::{BindingSource, IdentitySource, ResolvedMessageSettings};
pub use phone_mailboxes::{valid_mailbox_pin_hash, verify_mailbox_pin};
pub use settings::valid_admin_hash;
pub use settings::{
    LinkLookupMethod, NodeId, ResolvedAnnouncementSettings, ResolvedCourtesySettings,
    ResolvedDtmfRegenerationSettings, ResolvedIdentifierSettings, ResolvedMorseSettings,
    ResolvedNodeSettings, ResolvedPermanentLinkSettings, ResolvedSpeechSettings,
    ResolvedTimeSettings,
};

/// A recoverable unknown option or invalid value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning {
    /// One-based source line that supplied the ignored input.
    pub line: usize,
    /// Section containing the warning.
    pub section: String,
    /// Option that was ignored.
    pub key: String,
    /// Supplied value when it is safe to report.
    pub value: String,
    /// Human-readable fallback reason.
    pub message: String,
    /// Effective inherited/default fallback after ignoring the input.
    pub fallback: String,
}

impl ConfigWarning {
    pub(crate) fn new(
        line: usize,
        section: &str,
        key: &str,
        value: &str,
        message: &str,
        fallback: &str,
    ) -> Self {
        Self {
            line,
            section: section.to_owned(),
            key: key.to_owned(),
            // URLs may carry private query parameters; never echo them in diagnostics.
            value: if key == "commands" && section.starts_with("dtmf_macro ") {
                "<redacted command stream>".to_owned()
            } else if section.starts_with("mailbox ")
                || matches!(key, "directory_password" | "proxy_password")
            {
                "<redacted secret>".to_owned()
            } else if matches!(key, "statpost_url" | "iax_registration_url") {
                "<redacted URL>".to_owned()
            } else {
                value.to_owned()
            },
            message: message.to_owned(),
            fallback: if key == "commands"
                && section.starts_with("dtmf_macro ")
                && fallback != "macro unavailable"
            {
                "<redacted command stream>".to_owned()
            } else {
                fallback.to_owned()
            },
        }
    }
}

/// A typed result plus nonfatal configuration warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution<T> {
    /// Resolved value.
    pub value: T,
    /// Unknown or invalid options that retained defaults.
    pub warnings: Vec<ConfigWarning>,
}

/// Invalid configuration owner or resource relationship.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Invalid configuration relationship.
    Structure {
        /// Section whose relationships are invalid.
        section: String,
        /// Specific structural diagnostic.
        message: String,
    },
}

impl ConfigError {
    pub(crate) fn structure(section: &str, message: &str) -> Self {
        Self::Structure {
            section: section.to_owned(),
            message: message.to_owned(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Structure { section, message } => write!(formatter, "{section}: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}
