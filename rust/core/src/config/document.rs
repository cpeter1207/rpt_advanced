//! Owned configuration document storage.

use super::{ConfigError, parse, scope};

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
    section_lines: Vec<usize>,
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
                    document.section_lines.push(line_number);
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
                    return Err(ConfigError::syntax(
                        line_number,
                        "malformed section or option",
                    ));
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

    /// Copy relevant radio/profile source without interpreting any shared setting.
    ///
    /// This opaque identity keeps unrelated controller policy out of radio reload
    /// comparisons. Selected named sections and fallback scopes remain available
    /// to the shared provider, which owns resolution and validation.
    pub(crate) fn processing_configuration(&self, node: &str) -> String {
        let mut result = format!("[{node}]\n");
        for entry in self
            .section_entries(node)
            .filter(|entry| entry.key.to_ascii_lowercase().ends_with("_profile"))
        {
            result.push_str(&format!("{}={}\n", entry.key, entry.value));
        }
        for (index, section) in self.sections.iter().enumerate() {
            let parsed = scope::parse_scope(section).expect("schema validated processing sections");
            let selected = section.split_once(' ').is_some_and(|(kind, name)| {
                self.section_entries(node).any(|entry| {
                    entry.key.eq_ignore_ascii_case(&format!("{kind}_profile"))
                        && entry.value.eq_ignore_ascii_case(name)
                })
            });
            let applicable = match parsed.kind {
                scope::ScopeKind::ProcessingDefault
                | scope::ScopeKind::RadioDefault
                | scope::ScopeKind::RemoteDefault => true,
                scope::ScopeKind::ProcessingNode => {
                    parsed
                        .node
                        .is_some_and(|name| name.eq_ignore_ascii_case(node))
                        || selected
                }
                scope::ScopeKind::RadioNode | scope::ScopeKind::RemoteNode => parsed
                    .node
                    .is_some_and(|name| name.eq_ignore_ascii_case(node)),
                _ => false,
            };
            if !applicable {
                continue;
            }
            result.push_str(&format!("[{section}]\n"));
            let start = self.section_lines[index];
            let end = self
                .section_lines
                .get(index + 1)
                .copied()
                .unwrap_or(usize::MAX);
            for entry in self
                .entries
                .iter()
                .filter(|entry| entry.line > start && entry.line < end)
            {
                result.push_str(&format!("{}={}\n", entry.key, entry.value));
            }
        }
        result
    }

    /// Return declared node sections in source order.
    pub fn nodes(&self) -> Vec<&str> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(index, section)| {
                matches!(
                    scope::parse_scope(section),
                    Ok(parsed) if parsed.kind == scope::ScopeKind::Node
                ) && !self.sections[..*index].contains(section)
            })
            .map(|(_, section)| section.as_str())
            .collect()
    }

    /// Return matching named sections in source order.
    pub fn named_sections(&self, family: &str, node: Option<&str>) -> Vec<&str> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(index, section)| {
                let Ok(parsed) = scope::parse_scope(section) else {
                    return false;
                };
                scope::is_named(parsed.kind)
                    && section.split(' ').next() == Some(family)
                    && parsed.node == node
                    && !self.sections[..*index].contains(section)
            })
            .map(|(_, section)| section.as_str())
            .collect()
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
        self.entries
            .iter()
            .filter(move |entry| entry.section == section)
    }

    pub(crate) fn section_line(&self, index: usize) -> usize {
        self.section_lines[index]
    }
}
