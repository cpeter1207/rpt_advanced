//! Owned JSON resource decoding and site-to-port inheritance.

pub mod candidate;

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use unic_langid::LanguageIdentifier;

/// The required parent of a materialized metadata body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeParent {
    /// Site settings inherit only the built-in defaults.
    BuiltIn,
    /// Port settings inherit from the site.
    Site,
}

/// Resource input from which no deterministic configuration can be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonResourceError {
    /// Malformed JSON or ambiguous object member names.
    InvalidJson {
        /// One-based line reported by the decoder.
        line: usize,
        /// Column reported by the decoder.
        column: usize,
    },
    /// The document root is not a JSON object.
    ExpectedObject,
    /// The scope lacks a nonempty local display name.
    InvalidName,
    /// The scope lacks its required explicit parent.
    InvalidParent(ScopeParent),
}

impl fmt::Display for JsonResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { line, column } => {
                write!(
                    formatter,
                    "invalid JSON or duplicate object key at line {line}, column {column}"
                )
            }
            Self::ExpectedObject => formatter.write_str("expected a JSON object"),
            Self::InvalidName => formatter.write_str("name must be a nonempty string"),
            Self::InvalidParent(ScopeParent::BuiltIn) => {
                formatter.write_str("site inherit_from must be explicitly null")
            }
            Self::InvalidParent(ScopeParent::Site) => {
                formatter.write_str("port inherit_from must be explicitly \"site\"")
            }
        }
    }
}

impl std::error::Error for JsonResourceError {}

/// Recoverable JSON input ignored by the owning resource's decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceWarning {
    /// An unsupported member name, which may contain arbitrary input.
    UnknownField,
    /// An invalid value identified only by its schema-defined field name.
    InvalidValue(&'static str),
}

impl fmt::Display for ResourceWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownField => formatter.write_str("ignored an unknown JSON field"),
            Self::InvalidValue(field) => {
                write!(
                    formatter,
                    "ignored invalid {field} value and retained inherited or built-in settings"
                )
            }
        }
    }
}

/// A decoded resource and its value-free warnings for the configuration host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceResolution<T> {
    /// The resource with invalid optional fields left absent.
    pub value: T,
    /// Warnings to log with the enclosing resource's context.
    pub warnings: Vec<ResourceWarning>,
}

/// Exact local port identity, independent of network accounts and paths.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortKey(String);

impl PortKey {
    /// Preserve an arbitrary JSON string as a case-sensitive identity.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the unchanged identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The writable metadata body for one established scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMetadata {
    /// Friendly display name specific to this scope.
    pub name: String,
    /// Explicit parent for this scope.
    pub inherit_from: ScopeParent,
    /// Local enable override.
    pub enabled: Option<bool>,
    /// Local callsign override, including an explicit empty callsign.
    pub callsign: Option<String>,
    /// Local canonical Fluent locale override.
    pub language: Option<String>,
}

impl LocalMetadata {
    fn decode(
        input: &[u8],
        inherit_from: ScopeParent,
    ) -> Result<ResourceResolution<Self>, JsonResourceError> {
        Self::normalize(decode_object(input)?, inherit_from)
    }

    fn normalize(
        mut fields: Map<String, Value>,
        inherit_from: ScopeParent,
    ) -> Result<ResourceResolution<Self>, JsonResourceError> {
        let name = fields
            .remove("name")
            .and_then(into_string)
            .filter(|name| !name.is_empty())
            .ok_or(JsonResourceError::InvalidName)?;
        let supplied_parent = match fields.remove("inherit_from") {
            Some(Value::Null) => Some(ScopeParent::BuiltIn),
            Some(Value::String(parent)) if parent == "site" => Some(ScopeParent::Site),
            _ => None,
        };
        if supplied_parent != Some(inherit_from) {
            return Err(JsonResourceError::InvalidParent(inherit_from));
        }

        let mut warnings = Vec::new();
        let value = Self {
            name,
            inherit_from,
            enabled: optional(
                &mut fields,
                "enabled",
                |value| value.as_bool(),
                &mut warnings,
            ),
            callsign: optional(
                &mut fields,
                "callsign",
                |value| into_string(value).filter(|value| value.chars().count() <= 63),
                &mut warnings,
            ),
            language: optional(
                &mut fields,
                "language",
                |value| {
                    value
                        .as_str()?
                        .parse::<LanguageIdentifier>()
                        .ok()
                        .map(|language| language.to_string())
                },
                &mut warnings,
            ),
        };
        warnings.extend(fields.into_iter().map(|_| ResourceWarning::UnknownField));
        Ok(ResourceResolution { value, warnings })
    }

    fn effective(&self, site: Option<&Self>) -> EffectiveMetadata {
        EffectiveMetadata {
            name: self.name.clone(),
            inherit_from: self.inherit_from,
            enabled: self
                .enabled
                .or_else(|| site.and_then(|site| site.enabled))
                .unwrap_or(true),
            callsign: self
                .callsign
                .as_ref()
                .or_else(|| site.and_then(|site| site.callsign.as_ref()))
                .cloned()
                .unwrap_or_default(),
            language: self
                .language
                .as_ref()
                .or_else(|| site.and_then(|site| site.language.as_ref()))
                .cloned()
                .unwrap_or_else(|| "en-US".to_owned()),
        }
    }
}

fn into_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        _ => None,
    }
}

fn optional<T>(
    fields: &mut Map<String, Value>,
    field: &'static str,
    parse: impl FnOnce(Value) -> Option<T>,
    warnings: &mut Vec<ResourceWarning>,
) -> Option<T> {
    let value = parse(fields.remove(field)?);
    if value.is_none() {
        warnings.push(ResourceWarning::InvalidValue(field));
    }
    value
}

/// Effective common settings with display identity from the local scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveMetadata {
    /// Local display name.
    pub name: String,
    /// Local parent relationship.
    pub inherit_from: ScopeParent,
    /// Whether the scope is enabled after applying defaults.
    pub enabled: bool,
    /// Callsign after applying defaults.
    pub callsign: String,
    /// Canonical Fluent locale after applying defaults.
    pub language: String,
}

/// Validated site metadata body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteMetadata(LocalMetadata);

impl SiteMetadata {
    /// Decode a full site metadata body with an explicit null parent.
    pub fn decode(input: &[u8]) -> Result<ResourceResolution<Self>, JsonResourceError> {
        let ResourceResolution { value, warnings } =
            LocalMetadata::decode(input, ScopeParent::BuiltIn)?;
        Ok(ResourceResolution {
            value: Self(value),
            warnings,
        })
    }

    /// Borrow the sparse metadata that was supplied locally.
    pub fn local(&self) -> &LocalMetadata {
        &self.0
    }

    /// Apply the built-in defaults.
    pub fn effective(&self) -> EffectiveMetadata {
        self.0.effective(None)
    }
}

/// Validated port metadata body. The registry owns the port ID and key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortMetadata(LocalMetadata);

impl PortMetadata {
    /// Decode a full port metadata body with an explicit site parent.
    pub fn decode(input: &[u8]) -> Result<ResourceResolution<Self>, JsonResourceError> {
        let ResourceResolution { value, warnings } =
            LocalMetadata::decode(input, ScopeParent::Site)?;
        Ok(ResourceResolution {
            value: Self(value),
            warnings,
        })
    }

    /// Borrow the sparse metadata that was supplied locally.
    pub fn local(&self) -> &LocalMetadata {
        &self.0
    }

    /// Apply the built-in defaults, site defaults, then port overrides.
    pub fn effective(&self, site: &SiteMetadata) -> EffectiveMetadata {
        self.0.effective(Some(site.local()))
    }
}

/// Decode one unvalidated resource object, rejecting duplicate member names at every depth.
///
/// The owning schema filters these raw fields before creating a public view.
pub fn decode_object(input: &[u8]) -> Result<Map<String, Value>, JsonResourceError> {
    let Value::Object(fields) =
        rptadv_json::decode_value(input).map_err(|error| JsonResourceError::InvalidJson {
            line: error.line,
            column: error.column,
        })?
    else {
        return Err(JsonResourceError::ExpectedObject);
    };
    Ok(fields)
}

/// Apply sparse overrides to one already validated section or named-definition map.
///
/// Objects merge recursively, including empty override objects. Arrays and
/// scalars replace the inherited value. Protocol instance membership and
/// mailbox identity use their own rules outside this function.
pub fn inherit_section(
    inherited: &Map<String, Value>,
    local: &Map<String, Value>,
) -> Map<String, Value> {
    let mut effective = inherited.clone();
    merge_objects(&mut effective, local);
    effective
}

fn merge_objects(effective: &mut Map<String, Value>, local: &Map<String, Value>) {
    for (key, value) in local {
        if let (Some(Value::Object(inherited)), Value::Object(overrides)) =
            (effective.get_mut(key), value)
        {
            merge_objects(inherited, overrides);
        } else {
            effective.insert(key.clone(), value.clone());
        }
    }
}

/// Resolve mailbox identities by whole-record replacement.
///
/// The caller supplies one record per identity containing its profile and
/// credential binding together, preventing cross-scope field inheritance.
pub fn inherit_mailboxes<T: Clone>(
    inherited: &BTreeMap<String, T>,
    local: &BTreeMap<String, T>,
) -> BTreeMap<String, T> {
    let mut effective = inherited.clone();
    effective.extend(local.clone());
    effective
}
