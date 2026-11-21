//! Durable, fixed-name maintenance intent and completion evidence.
use super::*;
use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::io::{self, Write};

const MAX_SEQUENCE_ITEMS: usize = 8192;

type Result<T> = std::result::Result<T, MaintenanceError>;
pub(super) fn hex(hash: ContentHash) -> String {
    hash.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn document(bytes: &[u8]) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| MaintenanceError::InvalidGraph)
}
fn sequence<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Vec<T>, D::Error> {
    struct Items<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Items<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded record items")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element()? {
                if items.len() >= MAX_SEQUENCE_ITEMS {
                    return Err(A::Error::custom("maintenance item limit"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    d.deserialize_seq(Items(std::marker::PhantomData))
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pin {
    pub manifest: String,
    pub revision: String,
    pub sha256: String,
}
impl Pin {
    pub fn from_ref(r: &ManifestRef) -> Self {
        Self {
            manifest: r.manifest.as_str().into(),
            revision: r.revision.as_str().into(),
            sha256: hex(r.sha256),
        }
    }
    pub fn reference(&self) -> Result<ManifestRef> {
        Ok(ManifestRef {
            manifest: self
                .manifest
                .parse()
                .map_err(|_| MaintenanceError::InvalidGraph)?,
            revision: self
                .revision
                .parse()
                .map_err(|_| MaintenanceError::InvalidGraph)?,
            sha256: self
                .sha256
                .parse()
                .map_err(|_| MaintenanceError::InvalidGraph)?,
        })
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Gc {
        before: String,
    },
    Rotate {
        before: String,
        old_key: String,
        new_key: String,
    },
    Retain {
        before: String,
        key: String,
        #[serde(deserialize_with = "sequence")]
        pins: Vec<Pin>,
    },
    Restore {
        before: Option<String>,
        observed: String,
        key: String,
        source: Option<String>,
    },
}
impl Request {
    pub fn before(&self) -> Result<Option<ContentHash>> {
        let text = match self {
            Self::Gc { before } | Self::Rotate { before, .. } | Self::Retain { before, .. } => {
                Some(before)
            }
            Self::Restore { before, .. } => before.as_ref(),
        };
        text.map(|s| s.parse().map_err(|_| MaintenanceError::InvalidGraph))
            .transpose()
    }
    fn validate(&self) -> Result<()> {
        self.before()?;
        let key = |s: &str| {
            KeyId::new(s)
                .map(|_| ())
                .map_err(|_| MaintenanceError::InvalidGraph)
        };
        match self {
            Self::Gc { .. } => Ok(()),
            Self::Rotate {
                old_key, new_key, ..
            } => {
                key(old_key)?;
                if old_key == new_key {
                    return Err(MaintenanceError::InvalidGraph);
                }
                key(new_key)
            }
            Self::Retain { key: id, pins, .. } => {
                key(id)?;
                if pins.len() > MAX_RETAINED_RECOVERY {
                    return Err(MaintenanceError::Limits);
                }
                for (i, pin) in pins.iter().enumerate() {
                    pin.reference()?;
                    if pins[..i].contains(pin) {
                        return Err(MaintenanceError::InvalidGraph);
                    }
                }
                Ok(())
            }
            Self::Restore {
                key: id,
                observed,
                source,
                ..
            } => {
                key(id)?;
                observed
                    .parse::<ContentHash>()
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                if source
                    .as_ref()
                    .is_some_and(|s| s.is_empty() || s.len() > 4096)
                {
                    return Err(MaintenanceError::InvalidGraph);
                }
                Ok(())
            }
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Bounds {
    paths: usize,
    path_bytes: usize,
    file_bytes: u64,
    total_bytes: u64,
}
impl From<CaptureLimits> for Bounds {
    fn from(l: CaptureLimits) -> Self {
        Self {
            paths: l.max_paths,
            path_bytes: l.max_path_bytes,
            file_bytes: l.max_file_bytes,
            total_bytes: l.max_total_bytes,
        }
    }
}
impl Bounds {
    pub fn limits(&self) -> CaptureLimits {
        CaptureLimits {
            max_paths: self.paths,
            max_path_bytes: self.path_bytes,
            max_file_bytes: self.file_bytes,
            max_total_bytes: self.total_bytes,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Working {
    pub digest: String,
    #[serde(deserialize_with = "sequence")]
    pub directories: Vec<PathBuf>,
}
impl Working {
    pub fn capture(s: &WorkingSnapshot) -> Result<Self> {
        Ok(Self {
            digest: hex(digest(s)),
            directories: s
                .directories()
                .ok_or(MaintenanceError::InvalidGraph)?
                .to_vec(),
        })
    }
    pub fn target(&self, manifest: &PublicManifest, journal: &PublicationJournal) -> Self {
        let mut dirs = self.directories.iter().cloned().collect::<BTreeSet<_>>();
        for dir in &journal.prune_directories {
            dirs.remove(Path::new(dir));
        }
        for file in targets(manifest).keys() {
            for dir in Path::new(file)
                .ancestors()
                .skip(1)
                .filter(|d| !d.as_os_str().is_empty())
            {
                dirs.insert(dir.to_owned());
            }
        }
        Self {
            digest: hex(journal.target_working),
            directories: dirs.into_iter().collect(),
        }
    }
    /// Only journal-created or journal-pruned directories may differ during replay.
    pub fn permits_transition(&self, target: &Self, snapshot: &WorkingSnapshot) -> bool {
        let Some(observed) = snapshot.directories() else {
            return false;
        };
        let original = &self.directories;
        let target = &target.directories;
        let unexpected = observed
            .iter()
            .any(|d| original.binary_search(d).is_err() && target.binary_search(d).is_err());
        let missing = original
            .iter()
            .any(|d| target.binary_search(d).is_ok() && observed.binary_search(d).is_err());
        !unexpected && !missing
    }
    pub fn check(&self, s: &WorkingSnapshot) -> Result<()> {
        if self.digest == hex(digest(s)) && Some(self.directories.as_slice()) == s.directories() {
            Ok(())
        } else {
            Err(MaintenanceError::ExternalEdit)
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Blob {
    pub path: String,
    pub bytes: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Work {
    Gc {
        #[serde(deserialize_with = "sequence")]
        inventory: Vec<String>,
        #[serde(deserialize_with = "sequence")]
        registered: Vec<Blob>,
        #[serde(deserialize_with = "sequence")]
        candidates: Vec<String>,
    },
    Rotate {
        #[serde(deserialize_with = "sequence")]
        ciphertext: Vec<Blob>,
        #[serde(deserialize_with = "sequence")]
        manifests: Vec<Blob>,
    },
    Retain,
    Restore {
        source_digest: String,
        source_bounds: Bounds,
        manifest: Blob,
        journal: Blob,
        pending_state: String,
        target_working: Working,
    },
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Survivor {
    pub path: String,
    pub sha256: Option<String>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GcResult {
    pub removed: usize,
    #[serde(deserialize_with = "sequence")]
    pub survivors: Vec<Survivor>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    store: String,
    pub request: Request,
    pub bounds: Bounds,
    pub working: Working,
    pub final_state: String,
    pub gc: Option<GcResult>,
    pub pending: Option<Work>,
}
impl Record {
    pub fn new(
        store: ResourceId,
        limits: CaptureLimits,
        request: Request,
        working: Working,
        final_state: &[u8],
        gc: Option<GcResult>,
        work: Work,
    ) -> Result<Self> {
        let r = Self {
            version: 1,
            store: store.to_string(),
            request,
            bounds: limits.into(),
            working,
            final_state: document(final_state)?,
            gc,
            pending: Some(work),
        };
        r.validate(store, limits)?;
        Ok(r)
    }
    pub fn state(&self) -> Result<StatePointer> {
        let d = StatePointer::decode(self.final_state.as_bytes())
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if !d.warnings.is_empty() || d.value.store.to_string() != self.store {
            return Err(MaintenanceError::InvalidGraph);
        }
        Ok(d.value)
    }
    pub fn validate(&self, store: ResourceId, limits: CaptureLimits) -> Result<()> {
        if self.version != 1 || self.store != store.to_string() {
            return Err(MaintenanceError::InvalidGraph);
        }
        let proven = self.bounds.limits();
        let b = CaptureLimits {
            max_paths: limits.max_paths.min(proven.max_paths),
            max_path_bytes: limits.max_path_bytes.min(proven.max_path_bytes),
            max_file_bytes: limits.max_file_bytes.min(proven.max_file_bytes),
            max_total_bytes: limits.max_total_bytes.min(proven.max_total_bytes),
        };
        let max_items = b.max_paths.min(MAX_SEQUENCE_ITEMS);
        let digest = |s: &str| {
            s.parse::<ContentHash>()
                .map(|_| ())
                .map_err(|_| MaintenanceError::InvalidGraph)
        };
        let path = |s: &str| {
            if s.len() > b.max_path_bytes {
                return Err(MaintenanceError::Limits);
            }
            ArtifactPath::parse(s)
                .map(|_| ())
                .ok_or(MaintenanceError::InvalidGraph)
        };
        let blob = |v: &Blob| {
            path(&v.path)?;
            if v.bytes.len() as u64 > b.max_file_bytes {
                return Err(MaintenanceError::Limits);
            }
            Ok(())
        };
        self.request.validate()?;
        let terminal = self.state()?;
        digest(&self.working.digest)?;
        if self.final_state.len() as u64 > b.max_file_bytes
            || self.working.directories.len() > max_items
            || self
                .working
                .directories
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.working.directories.iter().any(|p| {
                p.as_os_str().len() > b.max_path_bytes
                    || !p.to_str().is_some_and(|s| {
                        matches!(catalog::classify(s), Some(CatalogPath::Directory))
                    })
            })
        {
            return Err(MaintenanceError::Limits);
        }
        if matches!(self.request, Request::Gc { .. }) != self.gc.is_some() {
            return Err(MaintenanceError::InvalidGraph);
        }
        match &self.request {
            Request::Gc { .. }
                if self.request.before()? != Some(hash(self.final_state.as_bytes())) =>
            {
                return Err(MaintenanceError::InvalidGraph);
            }
            Request::Retain { pins, .. }
                if terminal
                    .retained_recovery
                    .iter()
                    .map(Pin::from_ref)
                    .ne(pins.iter().cloned()) =>
            {
                return Err(MaintenanceError::InvalidGraph);
            }
            _ => {}
        }
        if let Some(gc) = &self.gc {
            if gc.survivors.len() > max_items
                || gc
                    .removed
                    .checked_add(gc.survivors.len())
                    .is_none_or(|n| n > b.max_paths)
            {
                return Err(MaintenanceError::Limits);
            }
            let mut last = None;
            for f in &gc.survivors {
                path(&f.path)?;
                if last.is_some_and(|s| s >= f.path.as_str()) {
                    return Err(MaintenanceError::InvalidGraph);
                }
                last = Some(f.path.as_str());
                if let Some(h) = &f.sha256 {
                    digest(h)?;
                }
            }
        }
        match (&self.request, &self.pending) {
            (_, None) | (Request::Retain { .. }, Some(Work::Retain)) => (),
            (
                Request::Gc { .. },
                Some(Work::Gc {
                    inventory,
                    registered,
                    candidates,
                }),
            ) => {
                if [inventory.len(), registered.len(), candidates.len()]
                    .into_iter()
                    .any(|n| n > max_items)
                {
                    return Err(MaintenanceError::Limits);
                }
                for p in inventory.iter().chain(candidates) {
                    path(p)?;
                }
                for v in registered {
                    blob(v)?;
                }
            }
            (
                Request::Rotate { .. },
                Some(Work::Rotate {
                    ciphertext,
                    manifests,
                }),
            ) => {
                if ciphertext.len() > max_items
                    || manifests.len() > max_items
                    || ciphertext
                        .len()
                        .checked_add(manifests.len())
                        .is_none_or(|n| n > b.max_paths)
                {
                    return Err(MaintenanceError::Limits);
                }
                for v in ciphertext.iter().chain(manifests) {
                    blob(v)?;
                }
            }
            (
                Request::Restore { observed, .. },
                Some(Work::Restore {
                    source_digest,
                    source_bounds,
                    manifest,
                    journal,
                    pending_state,
                    target_working,
                }),
            ) => {
                digest(source_digest)?;
                require_compatible_limits(source_bounds.limits(), proven)?;
                blob(manifest)?;
                blob(journal)?;
                if pending_state.len() as u64 > b.max_file_bytes
                    || target_working.directories.len() > max_items
                {
                    return Err(MaintenanceError::Limits);
                }
                let d = StatePointer::decode(pending_state.as_bytes())
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                let j = PublicationJournal::decode(
                    journal.bytes.as_bytes(),
                    b.max_paths,
                    b.max_path_bytes,
                )
                .map_err(|_| MaintenanceError::InvalidGraph)?;
                let m = PublicManifest::decode(manifest.bytes.as_bytes())
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                let mut clean = d.value;
                let link = clean.journal.take().ok_or(MaintenanceError::InvalidGraph)?;
                clean.clean_working = Some(j.value.target_working);
                if !d.warnings.is_empty()
                    || !m.warnings.is_empty()
                    || !j.warnings.is_empty()
                    || clean != terminal
                    || j.value.store != store
                    || j.value.prior != self.request.before()?
                    || j.value.id != link.id
                    || hash(journal.bytes.as_bytes()) != link.sha256
                    || journal.path != format!("journals/{}.json", link.id)
                    || j.value.desired != clean.desired
                    || manifest.path != manifest_path(&clean.desired)
                    || hash(manifest.bytes.as_bytes()) != clean.desired.sha256
                    || m.value.id() != &clean.desired.manifest
                    || m.value.revision() != &clean.desired.revision
                    || hex(j.value.source_working) != *observed
                    || self.working.digest != *observed
                    || target_working.digest != hex(j.value.target_working)
                    || *target_working != self.working.target(&m.value, &j.value)
                {
                    return Err(MaintenanceError::InvalidGraph);
                }
            }
            _ => return Err(MaintenanceError::InvalidGraph),
        }
        Ok(())
    }
}

pub(super) fn limit(limits: CaptureLimits) -> u64 {
    limits.max_total_bytes.min(64 * 1024 * 1024)
}
pub(super) fn encode<T: Serialize>(value: &T, cap: u64) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        remaining: u64,
    }
    impl Write for Bounded {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(data.len() as u64)
                .ok_or_else(|| io::Error::other("record limit"))?;
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut w = Bounded {
        bytes: Vec::new(),
        remaining: cap,
    };
    serde_json::to_writer(&mut w, value).map_err(|_| MaintenanceError::Limits)?;
    Ok(w.bytes)
}
pub(super) fn read(
    guard: &EditGuard<'_>,
    store: ResourceId,
    limits: CaptureLimits,
) -> Result<Option<(Record, ContentHash)>> {
    let Some(bytes) = guard.read_maintenance(limit(limits)).map_err(|e| {
        if e == CaptureError::Limits {
            MaintenanceError::Limits
        } else {
            MaintenanceError::InvalidGraph
        }
    })?
    else {
        return Ok(None);
    };
    let r: Record = serde_json::from_slice(&bytes).map_err(|_| MaintenanceError::InvalidGraph)?;
    r.validate(store, limits)?;
    if encode(&r, limit(limits))? != bytes {
        return Err(MaintenanceError::InvalidGraph);
    }
    Ok(Some((r, hash(&bytes))))
}
