//! Incoming link access-list parsing and deny-first authorization.

use std::fmt;

/// A malformed comma-separated node list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessListError;

impl fmt::Display for AccessListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid node access list")
    }
}

impl std::error::Error for AccessListError {}

/// A validated incoming-link allow/deny policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPolicy {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl AccessPolicy {
    /// Construct a policy from validated decimal node lists.
    pub fn new(allow: &str, deny: &str) -> Result<Self, AccessListError> {
        Ok(Self {
            allow: parse_list(allow)?,
            deny: parse_list(deny)?,
        })
    }

    /// Return whether a list has only decimal node tokens and spaces or tabs.
    pub fn list_valid(list: &str) -> bool {
        parse_list(list).is_ok()
    }

    /// Return whether an authenticated node is permitted by this policy.
    pub fn allows(&self, node: &str, verified: bool) -> bool {
        verified
            && !self.deny.iter().any(|entry| entry == node)
            && (self.allow.is_empty() || self.allow.iter().any(|entry| entry == node))
    }

    /// Borrow the validated allowlist entries.
    pub fn allow_entries(&self) -> &[String] {
        &self.allow
    }

    /// Borrow the validated denylist entries.
    pub fn deny_entries(&self) -> &[String] {
        &self.deny
    }
}

fn parse_list(list: &str) -> Result<Vec<String>, AccessListError> {
    let bytes = list.as_bytes();
    let mut cursor = 0;
    skip_spaces(bytes, &mut cursor);
    if cursor == bytes.len() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    loop {
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if start == cursor {
            return Err(AccessListError);
        }
        entries.push(list[start..cursor].to_owned());
        skip_spaces(bytes, &mut cursor);
        if cursor == bytes.len() {
            return Ok(entries);
        }
        if bytes[cursor] != b',' {
            return Err(AccessListError);
        }
        cursor += 1;
        skip_spaces(bytes, &mut cursor);
        if cursor == bytes.len() {
            return Err(AccessListError);
        }
    }
}

fn skip_spaces(bytes: &[u8], cursor: &mut usize) {
    while *cursor < bytes.len() && matches!(bytes[*cursor], b' ' | b'\t') {
        *cursor += 1;
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;
