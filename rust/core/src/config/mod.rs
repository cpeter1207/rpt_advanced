//! Owned configuration parsing, schema validation, and inherited settings.

mod document;
mod parse;
mod schema;
mod settings;

#[cfg(test)]
mod tests;

pub use document::{ConfigDocument, ConfigEntry};
pub use schema::Schema;
pub use settings::{LinkLookupMethod, NodeId, ResolvedNodeSettings};

/// A recoverable unknown option or invalid value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Section containing the warning.
    pub section: String,
    /// Option that was ignored.
    pub key: String,
    /// Human-readable fallback reason.
    pub message: String,
}

impl ConfigWarning {
    pub(crate) fn new(section: &str, key: &str, message: &str) -> Self {
        Self { section: section.to_owned(), key: key.to_owned(), message: message.to_owned() }
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
    Syntax { line: usize, message: String },
    /// Invalid section/definition structure.
    Structure { section: String, message: String },
}

impl ConfigError {
    pub(crate) fn syntax(line: usize, message: &str) -> Self {
        Self::Syntax { line, message: message.to_owned() }
    }

    pub(crate) fn structure(section: &str, message: &str) -> Self {
        Self::Structure { section: section.to_owned(), message: message.to_owned() }
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
