//! Reference-only public configuration manifests.

use super::{
    JsonResourceError, PortKey, ResourceResolution, ResourceWarning,
    candidate::{
        CandidateError, ResourceBinding, ResourceId, ResourceKind, ResourceRegistry, ResourceScope,
    },
    decode_object, into_string,
};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, fmt, str::FromStr};

const FORMAT_VERSION: u64 = 1;

/// Structural failure constructing or decoding a public manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// Duplicate-aware JSON decoding failed.
    Decode(JsonResourceError),
    /// The public manifest format version is missing or unsupported.
    UnsupportedFormat,
    /// The manifest contains unsupported private reachability.
    UnsupportedPrivate,
    /// A required structural field is missing or invalid.
    InvalidField(&'static str),
    /// Additional rows repeat a resource UUID.
    DuplicateResourceId,
    /// A metadata row is missing or inconsistent with the scope registry.
    InvalidMetadata,
    /// A metadata row or port protocol has no object reference.
    MissingReference,
    /// The resource registry rejected the supplied bindings.
    Registry(CandidateError),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => error.fmt(formatter),
            Self::UnsupportedFormat => formatter.write_str("unsupported public manifest format"),
            Self::UnsupportedPrivate => {
                formatter.write_str("private manifest references are unsupported")
            }
            Self::InvalidField(field) => write!(formatter, "invalid manifest field {field}"),
            Self::DuplicateResourceId => formatter.write_str("duplicate manifest resource ID"),
            Self::InvalidMetadata => {
                formatter.write_str("missing or inconsistent manifest metadata row")
            }
            Self::MissingReference => {
                formatter.write_str("required manifest object reference is missing")
            }
            Self::Registry(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ManifestError {}

macro_rules! component_type {
    ($(#[$doc:meta])* $name:ident, $field:literal) => {
        $(#[$doc])*
        #[derive(Clone, Eq, PartialEq)]
        pub struct $name(String);

        impl FromStr for $name {
            type Err = ManifestError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if matches!(value, "" | "." | "..") || value.contains(['/', '\0']) {
                    return Err(ManifestError::InvalidField($field));
                }
                Ok(Self(value.to_owned()))
            }
        }

        impl $name {
            /// Borrow the exact identifier text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}

component_type!(
    /// Server-assigned immutable manifest identifier.
    ManifestId, "id"
);
component_type!(
    /// Logical configuration revision, independent of artifact versions.
    LogicalRevision, "revision"
);
component_type!(
    /// Server-assigned version of one immutable resource object.
    ObjectVersion, "version"
);

/// SHA-256 content hash represented as exact digest bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wrap a digest already computed over exact bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for ContentHash {
    type Err = ManifestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut digest = [0; 32];
        let invalid = ManifestError::InvalidField("sha256");
        if text.len() != digest.len() * 2 {
            return Err(invalid);
        }
        let digit = |value| match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            _ => Err(invalid),
        };
        for (byte, pair) in digest.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            *byte = digit(pair[0])? << 4 | digit(pair[1])?;
        }
        Ok(Self(digest))
    }
}

impl ContentHash {
    pub(super) fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.0
            .iter()
            .flat_map(|byte| {
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 0x0f)]),
                ]
            })
            .collect()
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentHash(..)")
    }
}

/// A version and hash for one resource UUID's immutable object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectReference {
    version: ObjectVersion,
    sha256: ContentHash,
}

impl ObjectReference {
    /// Form the reference from its separately validated parts.
    pub fn new(version: ObjectVersion, sha256: ContentHash) -> Self {
        Self { version, sha256 }
    }

    /// Return the stored object's version.
    pub fn version(&self) -> &ObjectVersion {
        &self.version
    }

    /// Return the digest of the exact stored bytes.
    pub fn sha256(&self) -> &ContentHash {
        &self.sha256
    }
}

/// Supported public staging graph, with references and no configuration bodies.
pub struct PublicManifest {
    id: ManifestId,
    revision: LogicalRevision,
    registry: ResourceRegistry,
    references: BTreeMap<ResourceId, ObjectReference>,
}

impl PublicManifest {
    /// Construct the graph from explicit bindings and optional object references.
    ///
    /// Include one metadata row with a reference for each registered scope.
    /// A port protocol row always carries a reference. Reference-free ordinary
    /// slots retain their IDs, and reference-free site protocol defaults are absent.
    pub fn new(
        id: ManifestId,
        revision: LogicalRevision,
        site: ResourceId,
        ports: &[(ResourceId, PortKey)],
        rows: impl IntoIterator<Item = (ResourceBinding, Option<ObjectReference>)>,
    ) -> Result<Self, ManifestError> {
        Self::with_members(id, revision, site, ports, rows, &[])
    }

    /// Construct a reference-only graph with ordered named members.
    pub fn with_members(
        id: ManifestId,
        revision: LogicalRevision,
        site: ResourceId,
        ports: &[(ResourceId, PortKey)],
        rows: impl IntoIterator<Item = (ResourceBinding, Option<ObjectReference>)>,
        members: &[(ResourceId, PortKey)],
    ) -> Result<Self, ManifestError> {
        let mut unique_rows = BTreeMap::new();
        for (binding, reference) in rows {
            if reference.is_none()
                && (binding.kind == ResourceKind::Metadata
                    || binding.kind.is_named()
                    || binding.kind.has_optional_presence()
                        && matches!(binding.scope, ResourceScope::Port(_)))
            {
                return Err(ManifestError::MissingReference);
            }
            if unique_rows
                .insert(binding.id, (binding, reference))
                .is_some()
            {
                return Err(ManifestError::DuplicateResourceId);
            }
        }

        let additional: Vec<_> = unique_rows
            .values()
            .map(|&(binding, _)| binding)
            .filter(|binding| binding.kind != ResourceKind::Metadata && !binding.kind.is_named())
            .collect();
        let named = members
            .iter()
            .map(|(id, key)| {
                let (binding, _) = unique_rows
                    .get(id)
                    .ok_or(ManifestError::InvalidField("members"))?;
                Ok((*binding, key.clone()))
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let registry = ResourceRegistry::with_members(site, ports, &additional, &named)
            .map_err(ManifestError::Registry)?;
        // The registry already includes a metadata slot for every scope. All
        // explicit rows must match their slots and account for those metadata IDs.
        if unique_rows.len() != registry.bindings().len()
            || unique_rows
                .values()
                .any(|&(binding, _)| registry.by_id(binding.id) != Some(binding))
        {
            return Err(ManifestError::InvalidMetadata);
        }
        let references = unique_rows
            .into_iter()
            .filter_map(|(id, (_, reference))| reference.map(|reference| (id, reference)))
            .collect();
        Ok(Self {
            id,
            revision,
            registry,
            references,
        })
    }

    /// Interpret duplicate-aware JSON for the supported public manifest format.
    pub fn decode(input: &[u8]) -> Result<ResourceResolution<Self>, ManifestError> {
        let mut root = decode_object(input).map_err(ManifestError::Decode)?;
        if root
            .remove("format_version")
            .and_then(|value| value.as_u64())
            != Some(FORMAT_VERSION)
        {
            return Err(ManifestError::UnsupportedFormat);
        }
        if root.contains_key("private") {
            return Err(ManifestError::UnsupportedPrivate);
        }

        let id = parse_field(root.remove("id"), "id")?;
        let revision = parse_field(root.remove("revision"), "revision")?;
        let site = parse_field(root.remove("site_uuid"), "site_uuid")?;
        let ports = required_object(root.remove("port_keys"), "port_keys")?
            .into_iter()
            .map(|(key, value)| Ok((parse_field(Some(value), "port_keys")?, PortKey::new(key))))
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let order = root.remove("port_order");
        let resources = required_object(root.remove("resources"), "resources")?;
        let members = match root.remove("members") {
            None => Vec::new(),
            Some(Value::Array(members)) => members
                .into_iter()
                .map(|row| {
                    let Value::Array(pair) = row else {
                        return Err(ManifestError::InvalidField("members"));
                    };
                    let [id, Value::String(key)]: [Value; 2] = pair
                        .try_into()
                        .map_err(|_| ManifestError::InvalidField("members"))?
                    else {
                        return Err(ManifestError::InvalidField("members"));
                    };
                    Ok((parse_field(Some(id), "members")?, PortKey::new(key)))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(ManifestError::InvalidField("members")),
        };
        let mut warnings = vec![ResourceWarning::UnknownField; root.len()];
        let rows = resources
            .into_iter()
            .map(|(id, value)| decode_row(&id, value, site, &mut warnings))
            .collect::<Result<Vec<_>, ManifestError>>()?;
        let mut value = Self::with_members(id, revision, site, &ports, rows, &members)?;
        // A zero/one-member collection has only one possible ordering.
        if order.is_some() || ports.len() > 1 {
            let invalid = ManifestError::InvalidField("port_order");
            let Some(Value::Array(ids)) = order else {
                return Err(invalid);
            };
            if ids.len() != ports.len() {
                return Err(invalid);
            }
            let mut known: BTreeMap<_, _> = ports.into_iter().collect();
            value.registry.port_order = ids
                .into_iter()
                .map(|v| {
                    let id: ResourceId = parse_field(Some(v), "port_order")?;
                    Ok((id, known.remove(&id).ok_or(invalid)?))
                })
                .collect::<Result<Vec<_>, ManifestError>>()?;
        }
        Ok(ResourceResolution { value, warnings })
    }

    /// Emit deterministic reference-only JSON with canonical UUIDs and object paths.
    pub fn encode(&self) -> Vec<u8> {
        let port_keys = self
            .registry
            .port_keys()
            .map(|(key, id)| (key.as_str().to_owned(), Value::String(id.to_string())))
            .collect::<Map<_, _>>();
        let resources = self
            .registry
            .bindings()
            .map(|binding| {
                let mut row = Map::from_iter([
                    ("kind".to_owned(), Value::from(binding.kind.as_str())),
                    (
                        "owner".to_owned(),
                        Value::String(binding.scope.id().to_string()),
                    ),
                ]);
                if let Some(reference) = self.reference(binding.id) {
                    row.extend([
                        (
                            "version".to_owned(),
                            Value::from(reference.version().as_str()),
                        ),
                        (
                            "object".to_owned(),
                            Value::String(object_path(binding.id, reference.version())),
                        ),
                        ("sha256".to_owned(), Value::String(reference.sha256().hex())),
                    ]);
                }
                (binding.id.to_string(), Value::Object(row))
            })
            .collect::<Map<_, _>>();
        let mut encoded = json!({
            "format_version": FORMAT_VERSION,
            "id": self.id.as_str(),
            "revision": self.revision.as_str(),
            "site_uuid": self.registry.site_id().to_string(),
            "port_keys": port_keys,
            "resources": resources,
        });
        if self.registry.port_order.len() > 1 {
            encoded["port_order"] = json!(
                self.registry
                    .port_order
                    .iter()
                    .map(|(id, _)| id.to_string())
                    .collect::<Vec<_>>()
            );
        }
        if !self.registry.members().is_empty() {
            encoded["members"] = Value::Array(
                self.registry
                    .members()
                    .iter()
                    .map(|(binding, key)| json!([binding.id.to_string(), key.as_str()]))
                    .collect(),
            );
        }
        encoded.to_string().into_bytes()
    }

    /// Compare the complete retained configuration independently of manifest ID and logical revision.
    pub fn same_configuration(&self, other: &Self) -> bool {
        self.registry == other.registry && self.references == other.references
    }

    /// Return the immutable manifest identifier.
    pub fn id(&self) -> &ManifestId {
        &self.id
    }

    /// Return the logical configuration revision.
    pub fn revision(&self) -> &LogicalRevision {
        &self.revision
    }

    /// Return the validated registry for this staging graph.
    pub fn registry(&self) -> &ResourceRegistry {
        &self.registry
    }

    /// Find the immutable object for a populated binding.
    pub fn reference(&self, id: ResourceId) -> Option<&ObjectReference> {
        self.references.get(&id)
    }
}

impl fmt::Debug for PublicManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicManifest(..)")
    }
}

fn decode_row(
    id: &str,
    value: Value,
    site: ResourceId,
    warnings: &mut Vec<ResourceWarning>,
) -> Result<(ResourceBinding, Option<ObjectReference>), ManifestError> {
    let id = id
        .parse()
        .map_err(|_| ManifestError::InvalidField("resources"))?;
    let mut row = required_object(Some(value), "resources")?;
    let kind = row
        .remove("kind")
        .as_ref()
        .and_then(Value::as_str)
        .and_then(ResourceKind::from_wire)
        .ok_or(ManifestError::InvalidField("kind"))?;
    let owner = parse_field(row.remove("owner"), "owner")?;
    let scope = if owner == site {
        ResourceScope::Site(owner)
    } else {
        ResourceScope::Port(owner)
    };
    let reference = match (
        row.remove("version"),
        row.remove("object"),
        row.remove("sha256"),
    ) {
        (None, None, None) => None,
        (Some(version), Some(path), Some(sha256)) => {
            let version = parse_field(Some(version), "version")?;
            if path.as_str() != Some(object_path(id, &version).as_str()) {
                return Err(ManifestError::InvalidField("object"));
            }
            Some(ObjectReference::new(
                version,
                parse_field(Some(sha256), "sha256")?,
            ))
        }
        _ => return Err(ManifestError::InvalidField("reference")),
    };
    warnings.extend(std::iter::repeat_n(
        ResourceWarning::UnknownField,
        row.len(),
    ));
    Ok((ResourceBinding { id, scope, kind }, reference))
}

fn required_object(
    value: Option<Value>,
    field: &'static str,
) -> Result<Map<String, Value>, ManifestError> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        _ => Err(ManifestError::InvalidField(field)),
    }
}

fn parse_field<T: FromStr>(value: Option<Value>, field: &'static str) -> Result<T, ManifestError> {
    value
        .and_then(into_string)
        .and_then(|value| value.parse().ok())
        .ok_or(ManifestError::InvalidField(field))
}

fn object_path(id: ResourceId, version: &ObjectVersion) -> String {
    format!("objects/{id}/{}.json", version.as_str())
}

#[cfg(test)]
mod tests;
