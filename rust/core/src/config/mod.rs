//! Owned configuration parsing, schema validation, and inherited settings.

mod document;
mod parse;
mod schema;
mod scope;
mod settings;

#[cfg(test)]
mod document_tests;
#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod scope_tests;
#[cfg(test)]
mod tests;

pub use document::{ConfigDocument, ConfigEntry};
pub use schema::Schema;
pub use settings::{
    LinkLookupMethod, NodeId, ResolvedAnnouncementSettings, ResolvedCourtesySettings,
    ResolvedEventSettings, ResolvedIdentifierSettings, ResolvedMacroSettings,
    ResolvedMorseSettings, ResolvedNodeSettings, ResolvedPermanentLinkSettings,
    ResolvedScheduleSettings, ResolvedSpeechSettings, ResolvedTemplateSettings,
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
            value: value.to_owned(),
            message: message.to_owned(),
            fallback: fallback.to_owned(),
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

/// Irrecoverable syntax or document-structure failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Malformed line with its one-based source line.
    Syntax {
        /// One-based physical source line.
        line: usize,
        /// Specific syntax diagnostic.
        message: String,
    },
    /// Invalid section/definition structure.
    Structure {
        /// Section whose relationships are invalid.
        section: String,
        /// Specific structural diagnostic.
        message: String,
    },
}

impl ConfigError {
    pub(crate) fn syntax(line: usize, message: &str) -> Self {
        Self::Syntax {
            line,
            message: message.to_owned(),
        }
    }

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
            Self::Syntax { line, message } => write!(formatter, "line {line}: {message}"),
            Self::Structure { section, message } => write!(formatter, "{section}: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}
