//! Small format-1 state selector and bounded immutable publication journal.
use super::{
    ResourceResolution, ResourceWarning,
    candidate::ResourceId,
    catalog::{self, CatalogPath},
    decode_object,
    manifest::{ContentHash, LogicalRevision, ManifestId, ObjectReference, ObjectVersion},
};
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, str::FromStr};

/// Value-free structural failure. Invalid selecting metadata cannot default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidState;

impl std::fmt::Display for InvalidState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid state selector or publication journal")
    }
}
impl std::error::Error for InvalidState {}

type Fields = Map<String, Value>;
type Result<T> = std::result::Result<T, InvalidState>;

fn object(value: Value) -> Result<Fields> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(InvalidState),
    }
}
fn take(fields: &mut Fields, name: &str) -> Result<Value> {
    fields.remove(name).ok_or(InvalidState)
}
fn parse<T: FromStr>(value: Value) -> Result<T> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or(InvalidState)
}
fn field<T: FromStr>(fields: &mut Fields, name: &str) -> Result<T> {
    parse(take(fields, name)?)
}
fn identity(value: Value) -> Result<ResourceId> {
    let s = value.as_str().ok_or(InvalidState)?;
    let id: ResourceId = s.parse().map_err(|_| InvalidState)?;
    if id.to_string() != s {
        return Err(InvalidState);
    }
    Ok(id)
}
fn nullable<T>(value: Value, decode: impl FnOnce(Value) -> Result<T>) -> Result<Option<T>> {
    if value.is_null() {
        Ok(None)
    } else {
        decode(value).map(Some)
    }
}
fn extra(fields: Fields, warnings: &mut Vec<ResourceWarning>) {
    warnings.extend(std::iter::repeat_n(
        ResourceWarning::UnknownField,
        fields.len(),
    ));
}
fn root(bytes: &[u8]) -> Result<Fields> {
    let mut fields = decode_object(bytes).map_err(|_| InvalidState)?;
    if take(&mut fields, "format_version")?.as_u64() != Some(1) {
        return Err(InvalidState);
    }
    Ok(fields)
}

/// Trusted selection of one exact immutable manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestRef {
    /// Immutable manifest identifier.
    pub manifest: ManifestId,
    /// Logical configuration revision.
    pub revision: LogicalRevision,
    /// SHA-256 over exact bytes.
    pub sha256: ContentHash,
}
impl ManifestRef {
    fn value(&self) -> Value {
        json!({"manifest":self.manifest.as_str(),"revision":self.revision.as_str(),"sha256":self.sha256.hex()})
    }
    fn decode(v: Value, w: &mut Vec<ResourceWarning>) -> Result<Self> {
        let mut f = object(v)?;
        let item = Self {
            manifest: field(&mut f, "manifest")?,
            revision: field(&mut f, "revision")?,
            sha256: field(&mut f, "sha256")?,
        };
        extra(f, w);
        Ok(item)
    }
}
/// Historical result of attempting this desired revision; never an active process claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activation {
    /// Owner confirmation is outstanding.
    Pending,
    /// Owner confirmed adoption.
    Confirmed,
    /// Activation failed with confirmed rollback.
    Failed,
    /// Explicit recovery is required after uncertain rollback.
    RecoveryRequired,
}
impl Activation {
    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Failed => "failed",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    fn decode(v: Value) -> Result<Self> {
        match v.as_str() {
            Some("pending") => Ok(Self::Pending),
            Some("confirmed") => Ok(Self::Confirmed),
            Some("failed") => Ok(Self::Failed),
            Some("recovery_required") => Ok(Self::RecoveryRequired),
            _ => Err(InvalidState),
        }
    }
}
/// Selected immutable journal in this store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRef {
    /// Publication identity.
    pub id: ResourceId,
    /// SHA-256 over exact bytes.
    pub sha256: ContentHash,
}

/// Runtime-independent durable desired/LKG and publication evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePointer {
    /// Provisioned store UUID.
    pub store: ResourceId,
    /// Desired manifest selection.
    pub desired: ManifestRef,
    /// Last manifest confirmed by the runtime owner.
    pub last_known_good: Option<ManifestRef>,
    /// Historical activation outcome.
    pub activation: Activation,
    /// Outstanding publication journal.
    pub journal: Option<JournalRef>,
    /// Full verified working fingerprint after publication.
    pub clean_working: Option<ContentHash>,
}
impl StatePointer {
    fn validate(&self) -> Result<()> {
        if self.clean_working.is_some() == self.journal.is_some()
            || self.journal.is_some() && self.activation != Activation::Pending
            || self.activation == Activation::Confirmed
                && self.last_known_good.as_ref() != Some(&self.desired)
            || self.last_known_good.as_ref().is_some_and(|lkg| {
                (lkg.manifest == self.desired.manifest || lkg.revision == self.desired.revision)
                    && *lkg != self.desired
            })
        {
            return Err(InvalidState);
        }
        Ok(())
    }
    /// Encode the complete state without runtime generations or object bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(json!({"format_version":1,"store_uuid":self.store.to_string(),"desired":self.desired.value(),
            "last_known_good":self.last_known_good.as_ref().map(ManifestRef::value),
            "activation":{"state":self.activation.name(),"target_revision":self.desired.revision.as_str()},
            "publication_journal":self.journal.as_ref().map(|r|json!({"id":r.id.to_string(),"sha256":r.sha256.hex()})),
            "clean_working_sha256":self.clean_working.map(|h|h.hex())
        }).to_string().into_bytes())
    }
    /// Decode exact field types; nullable selectors require an explicit JSON null.
    pub fn decode(bytes: &[u8]) -> Result<ResourceResolution<Self>> {
        let mut f = root(bytes)?;
        let mut w = Vec::new();
        let store = identity(take(&mut f, "store_uuid")?)?;
        let desired = ManifestRef::decode(take(&mut f, "desired")?, &mut w)?;
        let last_known_good = nullable(take(&mut f, "last_known_good")?, |v| {
            ManifestRef::decode(v, &mut w)
        })?;
        let mut a = object(take(&mut f, "activation")?)?;
        let activation = Activation::decode(take(&mut a, "state")?)?;
        if field::<LogicalRevision>(&mut a, "target_revision")? != desired.revision {
            return Err(InvalidState);
        }
        extra(a, &mut w);
        let journal = nullable(take(&mut f, "publication_journal")?, |v| {
            let mut j = object(v)?;
            let r = JournalRef {
                id: identity(take(&mut j, "id")?)?,
                sha256: field(&mut j, "sha256")?,
            };
            extra(j, &mut w);
            Ok(r)
        })?;
        let clean_working = nullable(take(&mut f, "clean_working_sha256")?, parse)?;
        extra(f, &mut w);
        let value = Self {
            store,
            desired,
            last_known_good,
            activation,
            journal,
            clean_working,
        };
        value.validate()?;
        Ok(ResourceResolution { value, warnings: w })
    }
}

/// A replacement selects the manifest's verified immutable version for this resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkingTarget {
    /// Immutable public object owner.
    pub resource: ResourceId,
    /// Verified object version and digest.
    pub reference: ObjectReference,
}
/// One catalog-relative change. None denotes absence, distinct from empty bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct WorkingChange {
    /// Catalog-relative working path.
    pub path: String,
    /// Expected source hash or absence.
    pub source: Option<ContentHash>,
    /// Replacement object or removal.
    pub target: Option<WorkingTarget>,
}
/// Immutable intent without a circular digest reference to the new state selector.
pub struct PublicationJournal {
    /// Publication identity.
    pub id: ResourceId,
    /// Provisioned store UUID.
    pub store: ResourceId,
    /// Expected prior pointer hash or explicit provisioning absence.
    pub prior: Option<ContentHash>,
    /// Desired manifest selection.
    pub desired: ManifestRef,
    /// Complete source inventory fingerprint.
    pub source_working: ContentHash,
    /// Complete target inventory fingerprint.
    pub target_working: ContentHash,
    /// Protected directories of retired ports, in child-before-parent removal order.
    pub prune_directories: Vec<String>,
    /// Bounded working changes.
    pub changes: Vec<WorkingChange>,
}
impl PublicationJournal {
    fn validate(&self, max_paths: usize, max_path_bytes: usize) -> Result<()> {
        if self
            .changes
            .len()
            .saturating_add(self.prune_directories.len())
            > max_paths.saturating_mul(2)
            || self.prune_directories.len() > max_paths
        {
            return Err(InvalidState);
        }
        let mut paths = BTreeSet::new();
        for dir in &self.prune_directories {
            if dir.len() > max_path_bytes
                || !dir.starts_with("ports/")
                || !matches!(catalog::classify(dir), Some(CatalogPath::Directory))
                || !paths.insert(dir)
            {
                return Err(InvalidState);
            }
        }
        if self
            .prune_directories
            .windows(2)
            .any(|pair| std::path::Path::new(&pair[0]) <= std::path::Path::new(&pair[1]))
        {
            return Err(InvalidState);
        }
        for change in &self.changes {
            if change.path.len() > max_path_bytes
                || !matches!(catalog::classify(&change.path), Some(CatalogPath::File(_)))
                || !paths.insert(&change.path)
                || change.source.as_ref() == change.target.as_ref().map(|t| t.reference.sha256())
            {
                return Err(InvalidState);
            }
        }
        Ok(())
    }
    /// Encode only bounded changes to recognized working paths.
    pub fn encode(&self, max_paths: usize, max_path_bytes: usize) -> Result<Vec<u8>> {
        self.validate(max_paths, max_path_bytes)?;
        let changes=self.changes.iter().map(|c|json!({"path":c.path,"source_sha256":c.source.map(|h|h.hex()),
            "target":c.target.as_ref().map(|t|json!({"resource_uuid":t.resource.to_string(),"version":t.reference.version().as_str(),"sha256":t.reference.sha256().hex()}))
        })).collect::<Vec<_>>();
        Ok(json!({"format_version":1,"id":self.id.to_string(),"store_uuid":self.store.to_string(),
            "prior_state_sha256":self.prior.map(|h|h.hex()),"desired":self.desired.value(),"source_working_sha256":self.source_working.hex(),
            "target_working_sha256":self.target_working.hex(),"changes":changes,"prune_directories":self.prune_directories}).to_string().into_bytes())
    }
    /// Decode with path and count bounds before any changes may be attempted.
    pub fn decode(
        bytes: &[u8],
        max_paths: usize,
        max_path_bytes: usize,
    ) -> Result<ResourceResolution<Self>> {
        let mut f = root(bytes)?;
        let mut w = Vec::new();
        let id = identity(take(&mut f, "id")?)?;
        let store = identity(take(&mut f, "store_uuid")?)?;
        let prior = nullable(take(&mut f, "prior_state_sha256")?, parse)?;
        let desired = ManifestRef::decode(take(&mut f, "desired")?, &mut w)?;
        let source_working = field(&mut f, "source_working_sha256")?;
        let target_working = field(&mut f, "target_working_sha256")?;
        let Value::Array(rows) = take(&mut f, "changes")? else {
            return Err(InvalidState);
        };
        let Value::Array(dirs) = take(&mut f, "prune_directories")? else {
            return Err(InvalidState);
        };
        if rows.len().saturating_add(dirs.len()) > max_paths.saturating_mul(2)
            || dirs.len() > max_paths
        {
            return Err(InvalidState);
        }
        let prune_directories = dirs
            .into_iter()
            .map(|v| match v {
                Value::String(s) => Ok(s),
                _ => Err(InvalidState),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut changes = Vec::with_capacity(rows.len());
        for row in rows {
            let mut c = object(row)?;
            let Value::String(path) = take(&mut c, "path")? else {
                return Err(InvalidState);
            };
            let source = nullable(take(&mut c, "source_sha256")?, parse)?;
            let target = nullable(take(&mut c, "target")?, |v| {
                let mut t = object(v)?;
                let resource = identity(take(&mut t, "resource_uuid")?)?;
                let version: ObjectVersion = field(&mut t, "version")?;
                let reference = ObjectReference::new(version, field(&mut t, "sha256")?);
                extra(t, &mut w);
                Ok(WorkingTarget {
                    resource,
                    reference,
                })
            })?;
            extra(c, &mut w);
            changes.push(WorkingChange {
                path,
                source,
                target,
            });
        }
        extra(f, &mut w);
        let value = Self {
            id,
            store,
            prior,
            desired,
            source_working,
            target_working,
            changes,
            prune_directories,
        };
        value.validate(max_paths, max_path_bytes)?;
        Ok(ResourceResolution { value, warnings: w })
    }
}
