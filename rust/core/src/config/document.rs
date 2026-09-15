//! Owned configuration document storage.

use super::{parse, ConfigError};

/// One option retained in source order with owned strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Section containing this option.
    pub section: String,
    /// Option name.
    pub key: String,
    /// Explicit value; an empty value clears an inherited value.
    pub value: String,
    /// One-based physical source line.
    pub line: usize,
}

/// A complete parsed configuration whose contents do not borrow the input text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigDocument {
    sections: Vec<String>,
    entries: Vec<ConfigEntry>,
}

impl ConfigDocument {
    /// Parse sections and options, retaining every string in owned storage.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut document = Self::default();
        let mut section: Option<String> = None;
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            if line.contains('\0') {
                return Err(ConfigError::syntax(line_number, "embedded null byte"));
            }
            match parse::parse_line(line) {
                Ok(parse::ParsedLine::Empty) => {}
                Ok(parse::ParsedLine::Section(name)) => {
                    section = Some(name.to_owned());
                    document.sections.push(name.to_owned());
                }
                Ok(parse::ParsedLine::Option(key, value)) => {
                    let Some(section_name) = section.as_ref() else {
                        return Err(ConfigError::syntax(line_number, "option before section"));
                    };
                    document.entries.push(ConfigEntry {
                        section: section_name.clone(),
                        key: key.to_owned(),
                        value: value.to_owned(),
                        line: line_number,
                    });
                }
                Err(()) => {
                    return Err(ConfigError::syntax(line_number, "malformed section or option"));
                }
            }
        }
        Ok(document)
    }

    /// Return sections in source order, including sections with no options.
    pub fn sections(&self) -> &[String] {
        &self.sections
    }

    /// Return options in source order.
    pub fn entries(&self) -> &[ConfigEntry] {
        &self.entries
    }

    pub(crate) fn lookup(&self, key: &str, scopes: &[&str]) -> Option<&str> {
        scopes.iter().find_map(|scope| {
            self.entries
                .iter()
                .rev()
                .find(|entry| entry.section == *scope && entry.key == key)
                .map(|entry| entry.value.as_str())
        })
    }

    pub(crate) fn section_entries(&self, section: &str) -> impl Iterator<Item = &ConfigEntry> {
        self.entries.iter().filter(move |entry| entry.section == section)
    }
}
