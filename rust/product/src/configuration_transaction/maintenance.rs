//! Bounded local maintenance of verified immutable graphs, outside runtime/HTTP.
mod ffi;
mod record;
mod recovery;
use super::*;
#[cfg(test)]
pub(crate) use ffi::execute as execute_test_request;
use record::{Blob, GcResult, Pin, Record, Request, Survivor, Work, Working};
use rpt_advanced_core::config::resources::{
    manifest::PrivateReference, state::MAX_RETAINED_RECOVERY,
};
use rptadv_peer_support::{
    sealed_records::{RecordIdentity, SealedLimits, SealedRecord, load_systemd_key},
    working_files::{
        WriterLease,
        immutable::{
            maintenance::{ArtifactPath, ArtifactRole},
            private_records::{OpenedPrivateRecord, PreparedPrivateRecord},
        },
        transaction::new_identity,
    },
};

/// Value-free offline command failure; no supplied bytes are printable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceError {
    /// A selected or registered graph cannot be established.
    InvalidGraph,
    /// A selector, artifact or exclusive writer proof no longer matches.
    Conflict,
    /// A corresponding delivered key is unavailable or cannot authenticate.
    SecretsUnavailable,
    /// Editable working bytes do not match a safe accepted baseline.
    ExternalEdit,
    /// Publication may be visible but its durability is uncertain.
    DurabilityUnknown,
    /// Aggregate paths, bytes or explicit roots exceed their bound.
    Limits,
}
impl From<TransactionError> for MaintenanceError {
    fn from(error: TransactionError) -> Self {
        match error {
            TransactionError::SecretsUnavailable => Self::SecretsUnavailable,
            TransactionError::ExternalEdit => Self::ExternalEdit,
            TransactionError::Conflict => Self::Conflict,
            TransactionError::DurabilityUnknown => Self::DurabilityUnknown,
            _ => Self::InvalidGraph,
        }
    }
}
fn install_error(error: InstallError) -> MaintenanceError {
    match error {
        InstallError::Conflict => MaintenanceError::Conflict,
        InstallError::DurabilityUnknown => MaintenanceError::DurabilityUnknown,
        InstallError::Limits => MaintenanceError::Limits,
        _ => MaintenanceError::InvalidGraph,
    }
}
fn private_path(reference: PrivateReference) -> String {
    format!(
        "private/{}/{}.sealed.json",
        reference.record, reference.version
    )
}
fn manifest_path(reference: &ManifestRef) -> String {
    format!("manifests/{}.json", reference.manifest.as_str())
}
fn roots(state: &StatePointer) -> Vec<ManifestRef> {
    let mut result = Vec::new();
    for reference in std::iter::once(&state.desired)
        .chain(state.last_known_good.iter())
        .chain(&state.retained_recovery)
    {
        if !result.contains(reference) {
            result.push(reference.clone());
        }
    }
    result
}
fn sealed_limits(limits: CaptureLimits) -> SealedLimits {
    let cap =
        usize::try_from(limits.max_file_bytes.min(limits.max_total_bytes)).unwrap_or(usize::MAX);
    SealedLimits {
        max_plaintext_bytes: cap,
        max_document_bytes: cap,
    }
}
enum Artifact {
    Public(Vec<u8>),
    Private(OpenedPrivateRecord),
}
impl Artifact {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Public(bytes) => bytes,
            Self::Private(opened) => opened.sealed_bytes(),
        }
    }
    fn reauthenticate(&self, limits: CaptureLimits) -> Result<(), MaintenanceError> {
        if let Self::Private(opened) = self {
            authenticate(opened.identity(), opened.sealed_bytes(), limits)?;
        }
        Ok(())
    }
}
fn authenticate(
    identity: RecordIdentity,
    bytes: &[u8],
    limits: CaptureLimits,
) -> Result<(), MaintenanceError> {
    let decoded = SealedRecord::decode(identity, bytes, sealed_limits(limits))
        .map_err(|_| MaintenanceError::InvalidGraph)?;
    let key = load_systemd_key(decoded.record.key_id())
        .map_err(|_| MaintenanceError::SecretsUnavailable)?;
    decoded
        .record
        .open(&key, sealed_limits(limits))
        .map_err(|_| MaintenanceError::SecretsUnavailable)?;
    Ok(())
}
/// Exact verified graph snapshot; delivered keys are never copied.
pub struct Backup {
    state: StatePointer,
    raw_state: Vec<u8>,
    digest: ContentHash,
    artifacts: BTreeMap<String, Artifact>,
    graphs: Vec<ManifestRef>,
    keys: Vec<KeyId>,
    limits: CaptureLimits,
}
impl Backup {
    /// Selected roles and explicit physical recovery roots.
    pub fn state(&self) -> &StatePointer {
        &self.state
    }
    /// Exact original selector digest.
    pub fn digest(&self) -> ContentHash {
        self.digest
    }
    /// Exact immutable bytes in deterministic path order.
    pub fn artifacts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.artifacts
            .iter()
            .map(|(path, artifact)| (path.as_str(), artifact.bytes()))
    }
    /// Symbolic key IDs that must remain separately recoverable.
    pub fn required_keys(&self) -> &[KeyId] {
        &self.keys
    }
    fn add(&mut self, path: String, artifact: Artifact) -> Result<(), MaintenanceError> {
        if path.len() > self.limits.max_path_bytes
            || artifact.bytes().len() as u64 > self.limits.max_file_bytes
        {
            return Err(MaintenanceError::Limits);
        }
        if let Some(previous) = self.artifacts.get(&path) {
            return if previous.bytes() == artifact.bytes() {
                Ok(())
            } else {
                Err(MaintenanceError::InvalidGraph)
            };
        }
        let used = self
            .artifacts
            .values()
            .try_fold(self.raw_state.len() as u64, |sum, a| {
                sum.checked_add(a.bytes().len() as u64)
            })
            .and_then(|sum| sum.checked_add(artifact.bytes().len() as u64))
            .ok_or(MaintenanceError::Limits)?;
        if self.artifacts.len() >= self.limits.max_paths || used > self.limits.max_total_bytes {
            return Err(MaintenanceError::Limits);
        }
        if let Artifact::Private(opened) = &artifact {
            if !self.keys.contains(opened.key_id()) {
                self.keys.push(opened.key_id().clone());
                self.keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            }
        }
        self.artifacts.insert(path, artifact);
        Ok(())
    }
    fn check_role_key(&self, reference: &ManifestRef, key: &KeyId) -> Result<(), MaintenanceError> {
        let bytes = self
            .artifacts
            .get(&manifest_path(reference))
            .ok_or(MaintenanceError::InvalidGraph)?;
        let manifest = PublicManifest::decode(bytes.bytes())
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .value;
        if let Some(reference) = manifest.private() {
            let Some(Artifact::Private(opened)) = self.artifacts.get(&private_path(reference))
            else {
                return Err(MaintenanceError::InvalidGraph);
            };
            if opened.key_id() != key {
                return Err(MaintenanceError::SecretsUnavailable);
            }
        }
        Ok(())
    }
    fn reauthenticate(&self) -> Result<(), MaintenanceError> {
        for artifact in self.artifacts.values() {
            artifact.reauthenticate(self.limits)?;
        }
        Ok(())
    }
    fn preflight(
        &self,
        guard: &EditGuard<'_>,
        limits: CaptureLimits,
    ) -> Result<(), MaintenanceError> {
        self.reauthenticate()?;
        for (path, artifact) in &self.artifacts {
            if path.len() > limits.max_path_bytes
                || artifact.bytes().len() as u64 > limits.max_file_bytes.min(limits.max_total_bytes)
            {
                return Err(MaintenanceError::Limits);
            }
            guard
                .check_artifact_copy(path, artifact.bytes(), artifact_limits(limits))
                .map_err(install_error)?;
        }
        Ok(())
    }
    fn copy(&self, guard: &EditGuard<'_>) -> Result<(), MaintenanceError> {
        for (path, artifact) in &self.artifacts {
            match artifact {
                Artifact::Public(bytes) => guard
                    .install_public_copy(path, bytes, artifact_limits(self.limits))
                    .map_err(install_error)?,
                Artifact::Private(opened) => {
                    guard
                        .prepare_authenticated_private_parent(self.state.store.uuid(), opened)
                        .map_err(|_| MaintenanceError::InvalidGraph)?;
                    guard
                        .install_authenticated_private_record(opened, artifact_limits(self.limits))
                        .map_err(|_| MaintenanceError::InvalidGraph)?;
                }
            }
        }
        Ok(())
    }
    /// Write an exact backup tree, leaving editable working files and keys untouched.
    /// A fresh destination must be explicitly provisioned with this same UUID.
    pub fn write_to(
        &self,
        guard: &EditGuard<'_>,
        lease: &WriterLease,
    ) -> Result<(), MaintenanceError> {
        guard
            .check_writer_lease(lease)
            .map_err(|_| MaintenanceError::Conflict)?;
        let current = guard
            .read_state(self.limits.max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if current
            .as_deref()
            .is_some_and(|bytes| bytes != self.raw_state)
        {
            return Err(MaintenanceError::Conflict);
        }
        if current.is_none()
            && guard
                .bootstrap_identity()
                .map_err(|_| MaintenanceError::InvalidGraph)?
                != Some(self.state.store.uuid())
        {
            return Err(MaintenanceError::Conflict);
        }
        Transaction::new(guard, self.limits)
            .ordinary_writer_admission(self.state.store)
            .map_err(MaintenanceError::from)?;
        self.preflight(guard, self.limits)?;
        self.copy(guard)?;
        guard
            .check_writer_lease(lease)
            .map_err(|_| MaintenanceError::Conflict)?;
        guard
            .replace_state(
                current.as_deref().map(artifact_sha256).as_ref(),
                &self.raw_state,
                self.limits.max_file_bytes,
            )
            .map_err(install_error)?;
        guard
            .confirm_state(
                self.digest.as_bytes(),
                self.state.store.uuid(),
                self.limits.max_file_bytes,
                || Ok(()),
            )
            .map_err(install_error)
    }
}
fn artifact_limits(limits: CaptureLimits) -> ArtifactLimits {
    ArtifactLimits {
        max_path_bytes: limits.max_path_bytes,
        max_file_bytes: limits.max_file_bytes.min(limits.max_total_bytes),
    }
}
/// A restore may reuse a loader or working-planner result under equal or looser bounds.
/// A tighter receiver must capture the backup or prepare the plan under its own bounds.
fn require_compatible_limits(
    verified: CaptureLimits,
    receiver: CaptureLimits,
) -> Result<(), MaintenanceError> {
    if verified.max_paths > receiver.max_paths
        || verified.max_path_bytes > receiver.max_path_bytes
        || verified.max_file_bytes > receiver.max_file_bytes
        || verified.max_total_bytes > receiver.max_total_bytes
    {
        return Err(MaintenanceError::Limits);
    }
    Ok(())
}

/// Count exact bytes once per canonical selected path. Superseded journals are not roots.
struct SelectedBudget<'a> {
    limits: CaptureLimits,
    files: BTreeMap<String, &'a [u8]>,
    total: u64,
}

impl<'a> SelectedBudget<'a> {
    fn from_backups(
        limits: CaptureLimits,
        backups: impl IntoIterator<Item = &'a Backup>,
    ) -> Result<Self, MaintenanceError> {
        let mut budget = Self {
            limits,
            files: BTreeMap::new(),
            total: 0,
        };
        for backup in backups {
            for (path, bytes) in backup
                .artifacts()
                .filter(|(path, _)| !path.starts_with("journals/"))
            {
                budget.add(path.to_owned(), bytes)?;
            }
        }
        Ok(budget)
    }

    fn add(&mut self, path: String, bytes: &'a [u8]) -> Result<(), MaintenanceError> {
        if let Some(previous) = self.files.get(&path) {
            return if *previous == bytes {
                Ok(())
            } else {
                Err(MaintenanceError::Conflict)
            };
        }
        self.total = self
            .total
            .checked_add(bytes.len() as u64)
            .ok_or(MaintenanceError::Limits)?;
        // Reserve one path for the selector itself.
        if self.files.len().saturating_add(2) > self.limits.max_paths
            || path.len() > self.limits.max_path_bytes
            || bytes.len() as u64 > self.limits.max_file_bytes
            || self.total > self.limits.max_total_bytes
        {
            return Err(MaintenanceError::Limits);
        }
        self.files.insert(path, bytes);
        Ok(())
    }

    fn selector_limit(&self) -> u64 {
        self.limits
            .max_file_bytes
            .min(self.limits.max_total_bytes - self.total)
    }
}

/// Deterministic deletion candidates, bound to exact selector and registration evidence.
pub struct GarbagePlan {
    state: ContentHash,
    paths: Vec<String>,
    inventory: Vec<String>,
    registered: Backup,
}
impl GarbagePlan {
    /// Canonical immutable files proven unreachable from every selected role.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }
}
/// Fresh physical identities retained in memory for exact interrupted-publication retry.
pub struct RotationPlan {
    before: ContentHash,
    state: StatePointer,
    private: Vec<PreparedPrivateRecord>,
    manifests: Vec<(String, Vec<u8>)>,
    working: WorkingSnapshot,
    old_key: KeyId,
    new_key: KeyId,
}
impl RotationPlan {
    fn check_budget(
        &self,
        original: &Backup,
        limits: CaptureLimits,
    ) -> Result<(), MaintenanceError> {
        let mut budget = SelectedBudget::from_backups(limits, [original])?;
        for record in &self.private {
            let id = record.identity();
            let path = format!(
                "private/{}/{}.sealed.json",
                id.scope().record_uuid(),
                id.object_version()
            );
            budget.add(path, record.sealed_bytes())?;
        }
        for (path, bytes) in &self.manifests {
            budget.add(path.clone(), bytes)?;
        }
        check_pointer_budget(&self.state, digest(&self.working), budget.selector_limit())
            .map_err(|_| MaintenanceError::Limits)
    }
}

/// Fully authenticated backup restoration with fixed identities for interrupted retries.
pub struct RestorePlan {
    backup: Backup,
    source_locator: Option<String>,
    prepared_limits: CaptureLimits,
    before: Option<ContentHash>,
    working: WorkingSnapshot,
    manifest: (String, Vec<u8>),
    journal: (String, Vec<u8>),
    state: StatePointer,
    key: KeyId,
}
impl RestorePlan {
    fn clean_state(&self, working: ContentHash) -> StatePointer {
        let mut clean = self.state.clone();
        clean.journal = None;
        clean.clean_working = Some(working);
        clean
    }

    /// Validate all selected roles, pending and clean budgets, and immutable conflicts before copying.
    fn preflight(
        &self,
        maintenance: &Maintenance<'_, '_>,
        previous: Option<&Backup>,
        working: ContentHash,
    ) -> Result<(), MaintenanceError> {
        self.backup
            .check_role_key(&self.backup.state.desired, &self.key)?;
        if let Some(lkg) = &self.state.last_known_good {
            previous
                .ok_or(MaintenanceError::Conflict)?
                .check_role_key(lkg, &self.key)?;
        }
        let mut budget = SelectedBudget::from_backups(
            maintenance.tx.limits,
            std::iter::once(&self.backup).chain(previous),
        )?;
        budget.add(self.manifest.0.clone(), &self.manifest.1)?;
        check_pointer_budget(&self.clean_state(working), working, budget.selector_limit())
            .map_err(|_| MaintenanceError::Limits)?;
        budget.add(self.journal.0.clone(), &self.journal.1)?;
        let state = self.state.encode().map_err(|_| MaintenanceError::Limits)?;
        if state.len() as u64 > budget.selector_limit() {
            return Err(MaintenanceError::Limits);
        }

        self.backup
            .preflight(maintenance.tx.guard, maintenance.tx.limits)?;
        for (path, bytes) in [&self.manifest, &self.journal] {
            if self
                .backup
                .artifacts
                .get(path)
                .is_some_and(|artifact| artifact.bytes() != bytes.as_slice())
            {
                return Err(MaintenanceError::Conflict);
            }
            maintenance
                .tx
                .guard
                .check_artifact_copy(path, bytes, maintenance.tx.artifact_limits())
                .map_err(install_error)?;
        }
        Ok(())
    }
}
#[cfg(test)]
impl RestorePlan {
    pub(crate) fn generated_paths(&self) -> [&str; 2] {
        [&self.manifest.0, &self.journal.0]
    }
}
/// Held edit guard; destructive calls also demand an opaque writer lifetime.
pub struct Maintenance<'a, 's> {
    tx: Transaction<'a, 's>,
    store: ResourceId,
}
impl<'a, 's> Maintenance<'a, 's> {
    /// Bind an explicit trusted store UUID and aggregate read bounds.
    pub fn new(guard: &'a EditGuard<'s>, store: ResourceId, limits: CaptureLimits) -> Self {
        Self {
            tx: Transaction::new(guard, limits),
            store,
        }
    }
    fn proof(&self, lease: &WriterLease) -> Result<(), MaintenanceError> {
        self.tx
            .guard
            .check_writer_lease(lease)
            .map_err(|_| MaintenanceError::Conflict)
    }
    fn reservation(&self) -> Result<Option<(Record, ContentHash)>, MaintenanceError> {
        let entry = record::read(self.tx.guard, self.store, self.tx.limits)?;
        if entry.is_some() {
            self.verify_store_identity(true)?;
        }
        Ok(entry)
    }

    fn verify_store_identity(&self, strict: bool) -> Result<(), MaintenanceError> {
        if !self.tx.guard.accepts_identity(self.store.uuid()) {
            return Err(MaintenanceError::Conflict);
        }
        let raw = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if let Some(raw) = raw {
            let selected =
                StatePointer::decode(&raw).map_err(|_| MaintenanceError::InvalidGraph)?;
            if (strict && !selected.warnings.is_empty()) || selected.value.store != self.store {
                return Err(MaintenanceError::InvalidGraph);
            }
        } else if !self
            .tx
            .guard
            .has_bootstrap_marker(self.store.uuid())
            .map_err(|_| MaintenanceError::InvalidGraph)?
        {
            return Err(MaintenanceError::Conflict);
        }
        Ok(())
    }

    pub(super) fn admit_ordinary(&self) -> Result<(), TransactionError> {
        if let Some((record, digest)) = self
            .reservation()
            .map_err(|_| TransactionError::StorageFault)?
        {
            if record.pending.is_some() {
                return Err(TransactionError::Conflict);
            }
            self.tx
                .guard
                .confirm_maintenance(digest.as_bytes(), record::limit(self.tx.limits), || Ok(()))
                .map_err(|_| TransactionError::DurabilityUnknown)?;
        } else {
            self.verify_store_identity(false)
                .map_err(|_| TransactionError::StorageFault)?;
        }
        Ok(())
    }

    fn check_state(&self, expected: ContentHash) -> Result<(), MaintenanceError> {
        let current = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if current.as_deref().map(hash) != Some(expected) {
            return Err(MaintenanceError::Conflict);
        }
        Ok(())
    }
    fn check_bootstrap(&self) -> Result<(), MaintenanceError> {
        if self
            .tx
            .guard
            .bootstrap_identity()
            .map_err(|_| MaintenanceError::InvalidGraph)?
            != Some(self.store.uuid())
        {
            return Err(MaintenanceError::Conflict);
        }
        Ok(())
    }
    fn baseline(&self) -> Result<WorkingSnapshot, MaintenanceError> {
        let selected = self.tx.inspect().map_err(MaintenanceError::from)?;
        if selected.state.store != self.store {
            return Err(MaintenanceError::InvalidGraph);
        }
        match selected.status {
            StartupStatus::SecretsUnavailable => return Err(MaintenanceError::SecretsUnavailable),
            StartupStatus::ExternalEdit | StartupStatus::StorageFault => {
                return Err(MaintenanceError::ExternalEdit);
            }
            _ => {}
        }
        if selected.bookkeeping_fault {
            return Err(MaintenanceError::Conflict);
        }
        self.tx.inventory().map_err(MaintenanceError::from)
    }
    fn add_graph(
        &self,
        backup: &mut Backup,
        reference: &ManifestRef,
    ) -> Result<(), MaintenanceError> {
        self.add_graph_with(backup, reference, &BTreeMap::new())
    }

    fn load_with(
        &self,
        reference: &ManifestRef,
        missing: &BTreeMap<&str, &str>,
    ) -> Result<LoadedPublicManifest, MaintenanceError> {
        use crate::configuration_manifest::{PublicLoadError, load_public_manifest_with};
        use rptadv_peer_support::working_files::immutable::ReadError;
        let graph = load_public_manifest_with(
            &reference.manifest,
            &reference.revision,
            &reference.sha256,
            self.tx.limits,
            |path, target, expected, limits| match self.tx.guard.read_immutable(
                target,
                expected.as_bytes(),
                limits,
            ) {
                Ok(artifact) => Ok(artifact.into_bytes()),
                Err(ReadError::Store(CaptureError::Missing)) if missing.contains_key(path) => {
                    let bytes = missing[path].as_bytes();
                    if bytes.len() as u64 > limits.max_file_bytes
                        || path.len() > limits.max_path_bytes
                    {
                        return Err(PublicLoadError::Read(CaptureError::Limits.into()));
                    }
                    if hash(bytes) != *expected {
                        return Err(PublicLoadError::Read(ReadError::DigestMismatch));
                    }
                    Ok(bytes.to_vec())
                }
                Err(error) => Err(PublicLoadError::Read(error)),
            },
        )
        .map_err(|_| MaintenanceError::InvalidGraph)?;
        graph
            .assemble_candidate()
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        Ok(graph)
    }

    fn add_graph_with(
        &self,
        backup: &mut Backup,
        reference: &ManifestRef,
        missing: &BTreeMap<&str, &str>,
    ) -> Result<(), MaintenanceError> {
        if backup.graphs.contains(reference) {
            return Ok(());
        }
        let graph = self.load_with(reference, missing)?;
        let (private_store, owners) = self.tx.private_graph_owner(self.store, &graph);
        let private =
            private_store.select_with(graph.manifest().private(), owners, |r, remaining| {
                private_store.open_with_witness(
                    r,
                    remaining,
                    missing.get(private_path(r).as_str()).map(|b| b.as_bytes()),
                )
            });
        if !graph.manifest_warnings().is_empty() || private.warning_count() != 0 {
            return Err(MaintenanceError::InvalidGraph);
        }
        let credentials = private
            .credentials()
            .map_err(|_| MaintenanceError::SecretsUnavailable)?;
        backup.add(
            manifest_path(reference),
            Artifact::Public(graph.manifest_bytes().to_vec()),
        )?;
        for (id, bytes) in graph.verified_objects() {
            let object = graph
                .manifest()
                .reference(id)
                .ok_or(MaintenanceError::InvalidGraph)?;
            backup.add(
                format!("objects/{}/{}.json", id, object.version().as_str()),
                Artifact::Public(bytes.to_vec()),
            )?;
        }
        let mut remaining = self.tx.limits.max_total_bytes;
        let private_store = self.tx.private_store(self.store, 0, 0);
        for reference in credentials.references() {
            let opened = private_store
                .open_with_witness(
                    reference,
                    &mut remaining,
                    missing
                        .get(private_path(reference).as_str())
                        .map(|b| b.as_bytes()),
                )
                .map_err(|_| MaintenanceError::SecretsUnavailable)?;
            backup.add(private_path(reference), Artifact::Private(opened))?;
        }
        backup.graphs.push(reference.clone());
        Ok(())
    }
    /// Capture every selected manifest/index/leaf and exact journal without writer downtime.
    pub fn capture(&self) -> Result<Backup, MaintenanceError> {
        let selected = self.tx.inspect().map_err(MaintenanceError::from)?;
        self.capture_selected(selected)
    }
    fn capture_selected(&self, selected: Selected) -> Result<Backup, MaintenanceError> {
        // Inspection warnings here concern the selector, manifests, journal and private records.
        if selected.state.store != self.store || !selected.warnings.is_empty() {
            return Err(MaintenanceError::InvalidGraph);
        }
        let raw_state = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .ok_or(MaintenanceError::InvalidGraph)?;
        if hash(&raw_state) != selected.digest {
            return Err(MaintenanceError::Conflict);
        }
        let mut result = Backup {
            state: selected.state,
            raw_state,
            digest: selected.digest,
            artifacts: BTreeMap::new(),
            graphs: Vec::new(),
            keys: Vec::new(),
            limits: self.tx.limits,
        };
        for reference in roots(&result.state) {
            self.add_graph(&mut result, &reference)?;
        }
        if let Some(reference) = &result.state.journal {
            let path = format!("journals/{}.json", reference.id);
            let bytes = self
                .tx
                .guard
                .read_journal(
                    reference.id.uuid(),
                    reference.sha256.as_bytes(),
                    self.tx.artifact_limits(),
                )
                .map_err(|_| MaintenanceError::InvalidGraph)?;
            result.add(path, Artifact::Public(bytes))?;
        }
        self.check_state(result.digest)?;
        Ok(result)
    }
    fn registered_bytes(
        &self,
        target: ArtifactPath,
        missing: &BTreeMap<&str, &str>,
    ) -> Result<Vec<u8>, MaintenanceError> {
        let path = target.relative().to_owned();
        match self
            .tx
            .guard
            .capture_artifact(target, self.tx.artifact_limits())
        {
            Ok(a) => Ok(a.bytes().to_vec()),
            Err(CaptureError::Missing) => missing
                .get(path.as_str())
                .map(|b| b.as_bytes().to_vec())
                .ok_or(MaintenanceError::InvalidGraph),
            Err(_) => Err(MaintenanceError::InvalidGraph),
        }
    }
    fn registered(
        &self,
        backup: &mut Backup,
        inventory: &[String],
        missing: &BTreeMap<&str, &str>,
    ) -> Result<(), MaintenanceError> {
        for path in inventory {
            let target = ArtifactPath::parse(path).ok_or(MaintenanceError::InvalidGraph)?;
            match target.role() {
                ArtifactRole::Manifest => {
                    let raw = self.registered_bytes(target, missing)?;

                    let manifest = PublicManifest::decode(raw.as_slice())
                        .map_err(|_| MaintenanceError::InvalidGraph)?
                        .value;
                    let reference = ManifestRef {
                        manifest: manifest.id().clone(),
                        revision: manifest.revision().clone(),
                        sha256: hash(raw.as_slice()),
                    };
                    if manifest_path(&reference) != *path {
                        return Err(MaintenanceError::InvalidGraph);
                    }
                    self.add_graph_with(backup, &reference, missing)?;
                }
                ArtifactRole::Journal => {
                    let raw = self.registered_bytes(target, missing)?;

                    let journal = PublicationJournal::decode(
                        raw.as_slice(),
                        self.tx.limits.max_paths,
                        self.tx.limits.max_path_bytes,
                    )
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                    if !journal.warnings.is_empty() {
                        return Err(MaintenanceError::InvalidGraph);
                    }
                    let journal = journal.value;
                    if format!("journals/{}.json", journal.id) != *path
                        || journal.store != self.store
                    {
                        return Err(MaintenanceError::InvalidGraph);
                    }
                    let graph = self.load_with(&journal.desired, missing)?;
                    let expected = targets(graph.manifest());
                    if journal.target_working != fingerprint(&expected)
                        || journal
                            .changes
                            .iter()
                            .any(|change| change.target.as_ref() != expected.get(&change.path))
                        || journal.prune_directories.iter().any(|dir| {
                            expected
                                .keys()
                                .any(|path| path.starts_with(&format!("{dir}/")))
                        })
                    {
                        return Err(MaintenanceError::InvalidGraph);
                    }
                    self.add_graph_with(backup, &journal.desired, missing)?;
                    backup.add(path.clone(), Artifact::Public(raw.as_slice().to_vec()))?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Verify all registration evidence before planning exact unreachable-only deletion.
    /// Unknown paths are preserved. A selected journal conservatively prevents deletion.
    pub fn plan_gc(&self, lease: &WriterLease) -> Result<GarbagePlan, MaintenanceError> {
        self.proof(lease)?;
        self.baseline()?;
        let mut registered = self.capture()?;
        let reachable: BTreeSet<_> = registered.artifacts.keys().cloned().collect();
        let inventory = self
            .tx
            .guard
            .discover_artifacts(self.tx.limits)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        self.registered(&mut registered, &inventory, &BTreeMap::new())?;
        let paths = if registered.state.journal.is_some() {
            Vec::new()
        } else {
            registered
                .artifacts
                .keys()
                .filter(|path| !reachable.contains(*path))
                .cloned()
                .collect()
        };
        self.check_state(registered.digest)?;
        Ok(GarbagePlan {
            state: registered.digest,
            paths,
            inventory,
            registered,
        })
    }
    /// Recheck every root and candidate before any unlink; retry missing planned leaves safely.
    pub fn collect(&self, lease: &WriterLease, plan: &GarbagePlan) -> Result<(), MaintenanceError> {
        self.proof(lease)?;
        self.check_state(plan.state)?;
        self.baseline()?;
        self.capture()?;
        plan.registered.reauthenticate()?;
        let current = self
            .tx
            .guard
            .discover_artifacts(self.tx.limits)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if current.iter().any(|path| !plan.inventory.contains(path))
            || plan
                .inventory
                .iter()
                .any(|path| !current.contains(path) && !plan.paths.contains(path))
        {
            return Err(MaintenanceError::Conflict);
        }
        for (path, artifact) in &plan.registered.artifacts {
            if current.contains(path) {
                let actual = self
                    .tx
                    .guard
                    .capture_artifact(
                        ArtifactPath::parse(path).ok_or(MaintenanceError::InvalidGraph)?,
                        self.tx.artifact_limits(),
                    )
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                if actual.bytes() != artifact.bytes() {
                    return Err(MaintenanceError::Conflict);
                }
            }
        }
        let record = self.gc_record(plan)?;
        let Some(intent) = self.reserve(lease, &record)? else {
            return Ok(());
        };
        self.tx
            .confirm_selector(plan.state, self.store)
            .map_err(MaintenanceError::from)?;
        let mut order = plan.paths.iter().collect::<Vec<_>>();
        let mut indices = BTreeSet::new();
        for r in &plan.registered.graphs {
            let manifest =
                PublicManifest::decode(plan.registered.artifacts[&manifest_path(r)].bytes())
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
            if let Some(index) = manifest.value.private() {
                indices.insert(private_path(index));
            }
        }
        order.sort_by_key(|path| {
            if path.starts_with("manifests/") {
                3
            } else if path.starts_with("journals/") {
                2
            } else if indices.contains(*path) {
                1
            } else {
                0
            }
        });
        for path in order {
            let expected = artifact_sha256(
                plan.registered
                    .artifacts
                    .get(path)
                    .ok_or(MaintenanceError::InvalidGraph)?
                    .bytes(),
            );
            self.tx
                .guard
                .remove_artifact_checked(lease, path, &expected, self.tx.artifact_limits(), || {
                    self.check_state(plan.state)
                        .map_err(|_| InstallError::Conflict)
                })
                .map_err(install_error)?;
            #[cfg(test)]
            self.tx
                .at(Boundary::Garbage)
                .map_err(MaintenanceError::from)?;
        }
        self.finish(lease, record, intent).map(|_| ())
    }
    /// Authenticate every retained graph and seal fresh versions under a separate delivered key.
    pub fn prepare_rotation(
        &self,
        lease: &WriterLease,
        old: &KeyId,
        new: &KeyId,
    ) -> Result<RotationPlan, MaintenanceError> {
        self.rotation_with(lease, old, new, None)
    }

    fn rotation_with(
        &self,
        lease: &WriterLease,
        old: &KeyId,
        new: &KeyId,
        frozen: Option<(&[Blob], &[Blob])>,
    ) -> Result<RotationPlan, MaintenanceError> {
        self.proof(lease)?;
        if old == new {
            return Err(MaintenanceError::Conflict);
        }
        let working = self.baseline()?;
        let backup = self.capture()?;
        if backup.state.journal.is_some() {
            return Err(MaintenanceError::Conflict);
        }
        backup.reauthenticate()?;
        load_systemd_key(old).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        let key = load_systemd_key(new).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        backup.check_role_key(&backup.state.desired, old)?;
        let original = roots(&backup.state);
        let historical: Vec<_> = backup
            .state
            .retained_recovery
            .iter()
            .filter(|r| {
                *r != &backup.state.desired && Some(*r) != backup.state.last_known_good.as_ref()
            })
            .cloned()
            .collect();
        if original.len().saturating_add(historical.len()) > MAX_RETAINED_RECOVERY {
            return Err(MaintenanceError::Limits);
        }
        let mut indices = BTreeSet::new();
        for reference in &backup.graphs {
            let manifest = PublicManifest::decode(
                backup
                    .artifacts
                    .get(&manifest_path(reference))
                    .ok_or(MaintenanceError::InvalidGraph)?
                    .bytes(),
            )
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .value;
            if let Some(reference) = manifest.private() {
                indices.insert(private_path(reference));
            }
        }
        let mut replacements = BTreeMap::new();
        let mut private = Vec::new();
        let mut sealed_index = 0;
        let mut seal = |opened: &OpenedPrivateRecord,
                        plaintext: &rptadv_peer_support::sealed_records::SecretBytes|
         -> Result<PreparedPrivateRecord, MaintenanceError> {
            if let Some((sealed, _)) = frozen {
                let blob = sealed
                    .get(sealed_index)
                    .ok_or(MaintenanceError::InvalidGraph)?;
                sealed_index += 1;
                let scope = *opened.identity().scope();
                let prefix = format!("private/{}/", scope.record_uuid());
                let version = blob
                    .path
                    .strip_prefix(&prefix)
                    .and_then(|s| s.strip_suffix(".sealed.json"))
                    .ok_or(MaintenanceError::InvalidGraph)?;
                let identity = RecordIdentity::new(scope, version)
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                let bytes = blob.bytes.as_bytes();
                let verified = OpenedPrivateRecord::from_witness(
                    identity,
                    hash(bytes).as_bytes(),
                    bytes,
                    sealed_limits(self.tx.limits),
                )
                .map_err(|_| MaintenanceError::SecretsUnavailable)?;
                if verified.key_id() != new
                    || !verified.warnings().is_empty()
                    || verified.plaintext().expose_bytes() != plaintext.expose_bytes()
                {
                    return Err(MaintenanceError::InvalidGraph);
                }
                return Ok(verified.into_prepared());
            }
            PreparedPrivateRecord::seal(
                &key,
                opened.identity().scope(),
                plaintext,
                sealed_limits(self.tx.limits),
            )
            .map_err(|_| MaintenanceError::InvalidGraph)
        };
        let mut prepare = |path: &str,
                           opened: &OpenedPrivateRecord,
                           plaintext: &rptadv_peer_support::sealed_records::SecretBytes|
         -> Result<(), MaintenanceError> {
            let prepared = seal(opened, plaintext)?;
            let identity = prepared.identity();
            let reference = PrivateReference {
                record: identity
                    .scope()
                    .record_uuid()
                    .to_string()
                    .parse()
                    .map_err(|_| MaintenanceError::InvalidGraph)?,
                version: identity
                    .object_version()
                    .to_string()
                    .parse()
                    .map_err(|_| MaintenanceError::InvalidGraph)?,
                sha256: hash(prepared.sealed_bytes()),
            };
            replacements.insert(path.to_owned(), reference);
            private.push(prepared);
            Ok(())
        };
        for (path, artifact) in &backup.artifacts {
            if let Artifact::Private(opened) = artifact
                && !indices.contains(path)
            {
                prepare(path, opened, opened.plaintext())?;
            }
        }
        for path in &indices {
            let Some(Artifact::Private(opened)) = backup.artifacts.get(path) else {
                return Err(MaintenanceError::InvalidGraph);
            };
            let rows = rotated_index(opened, &replacements)?;
            let prepared = seal(opened, &rows)?;
            let identity = prepared.identity();
            replacements.insert(
                path.clone(),
                PrivateReference {
                    record: identity
                        .scope()
                        .record_uuid()
                        .to_string()
                        .parse()
                        .map_err(|_| MaintenanceError::InvalidGraph)?,
                    version: identity
                        .object_version()
                        .to_string()
                        .parse()
                        .map_err(|_| MaintenanceError::InvalidGraph)?,
                    sha256: hash(prepared.sealed_bytes()),
                },
            );
            private.push(prepared);
        }
        let mut manifests = Vec::new();
        let mut physical = BTreeMap::new();
        for reference in &backup.graphs {
            let mut value = rptadv_json::decode_value(
                backup
                    .artifacts
                    .get(&manifest_path(reference))
                    .ok_or(MaintenanceError::InvalidGraph)?
                    .bytes(),
            )
            .map_err(|_| MaintenanceError::InvalidGraph)?;
            let old_manifest = PublicManifest::decode(
                backup
                    .artifacts
                    .get(&manifest_path(reference))
                    .ok_or(MaintenanceError::InvalidGraph)?
                    .bytes(),
            )
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .value;
            let id = if let Some((_, saved)) = frozen {
                let blob = saved
                    .get(manifests.len())
                    .ok_or(MaintenanceError::InvalidGraph)?;
                let parsed = PublicManifest::decode(blob.bytes.as_bytes())
                    .map_err(|_| MaintenanceError::InvalidGraph)?;
                parsed.value.id().as_str().to_owned()
            } else {
                new_identity()
                    .map_err(|_| MaintenanceError::InvalidGraph)?
                    .to_string()
            };
            value["id"] = serde_json::Value::String(id.clone());
            if let Some(root) = old_manifest.private() {
                value["private"] = replacements
                    .get(&private_path(root))
                    .ok_or(MaintenanceError::InvalidGraph)?
                    .encode();
            }
            let bytes = serde_json::to_vec(&value).map_err(|_| MaintenanceError::InvalidGraph)?;
            let next = ManifestRef {
                manifest: id.parse().map_err(|_| MaintenanceError::InvalidGraph)?,
                revision: reference.revision.clone(),
                sha256: hash(&bytes),
            };
            manifests.push((manifest_path(&next), bytes));
            physical.insert(manifest_path(reference), next);
        }
        let translated = |reference: &ManifestRef| {
            physical
                .get(&manifest_path(reference))
                .cloned()
                .ok_or(MaintenanceError::InvalidGraph)
        };
        let mut state = backup.state.clone();
        state.desired = translated(&state.desired)?;
        state.last_known_good = state.last_known_good.as_ref().map(translated).transpose()?;
        state.retained_recovery = original;
        for reference in historical {
            let reference = translated(&reference)?;
            if !state.retained_recovery.contains(&reference) {
                state.retained_recovery.push(reference);
            }
        }
        let plan = RotationPlan {
            before: backup.digest,
            state,
            private,
            manifests,
            working,
            old_key: old.clone(),
            new_key: new.clone(),
        };
        plan.check_budget(&backup, self.tx.limits)?;
        self.check_state(backup.digest)?;
        Ok(plan)
    }
    /// Publish replacements at the existing state commit point; old bytes remain immutable.
    pub fn rotate(&self, lease: &WriterLease, plan: &RotationPlan) -> Result<(), MaintenanceError> {
        self.proof(lease)?;
        load_systemd_key(&plan.old_key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        load_systemd_key(&plan.new_key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        for prepared in &plan.private {
            authenticate(prepared.identity(), prepared.sealed_bytes(), self.tx.limits)?;
        }
        let bytes = plan
            .state
            .encode()
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        let next = hash(&bytes);
        let current = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .ok_or(MaintenanceError::InvalidGraph)?;
        let actual = hash(&current);
        if actual != plan.before && actual != next {
            return Err(MaintenanceError::Conflict);
        }
        self.baseline()?;
        plan.check_budget(&self.capture()?, self.tx.limits)?;
        let observed = self.tx.inventory().map_err(MaintenanceError::from)?;
        if digest(&observed) != digest(&plan.working)
            || observed.directories() != plan.working.directories()
        {
            return Err(MaintenanceError::ExternalEdit);
        }
        for prepared in &plan.private {
            let id = prepared.identity();
            let path = format!(
                "private/{}/{}.sealed.json",
                id.scope().record_uuid(),
                id.object_version()
            );
            self.tx
                .guard
                .check_artifact_copy(&path, prepared.sealed_bytes(), self.tx.artifact_limits())
                .map_err(install_error)?;
        }
        for (path, bytes) in &plan.manifests {
            self.tx
                .guard
                .check_artifact_copy(path, bytes, self.tx.artifact_limits())
                .map_err(install_error)?;
        }
        let record = self.rotation_record(plan)?;
        let Some(intent) = self.reserve(lease, &record)? else {
            return Ok(());
        };
        for prepared in &plan.private {
            self.tx
                .guard
                .prepare_private_parent(self.store.uuid(), prepared)
                .map_err(|_| MaintenanceError::InvalidGraph)?;
            self.tx
                .guard
                .install_private_record(prepared, self.tx.artifact_limits())
                .map_err(|_| MaintenanceError::InvalidGraph)?;
        }
        #[cfg(test)]
        self.tx
            .at(Boundary::PrivateArtifacts)
            .map_err(MaintenanceError::from)?;
        for (path, bytes) in &plan.manifests {
            self.tx
                .guard
                .install_public_copy(path, bytes, self.tx.artifact_limits())
                .map_err(install_error)?;
        }
        #[cfg(test)]
        self.tx
            .at(Boundary::Manifest)
            .map_err(MaintenanceError::from)?;
        if actual == plan.before {
            self.tx
                .switch(
                    Some(plan.before),
                    &plan.state,
                    Some(digest(&plan.working)),
                    plan.working.directories(),
                )
                .map_err(MaintenanceError::from)?;
            #[cfg(test)]
            self.tx
                .at(Boundary::Pointer)
                .map_err(MaintenanceError::from)?;
        } else {
            self.tx
                .guard
                .confirm_state(
                    next.as_bytes(),
                    self.store.uuid(),
                    self.tx.artifact_limits().max_file_bytes,
                    || Ok(()),
                )
                .map_err(install_error)?;
        }
        self.finish(lease, record, intent).map(|_| ())
    }
    /// Explicitly replace recovery roots, preserving configuration revisions and activation.
    pub fn retain(
        &self,
        lease: &WriterLease,
        expected: ContentHash,
        references: &[ManifestRef],
        key: &KeyId,
    ) -> Result<(), MaintenanceError> {
        let request = Self::pin_request(expected, references, key);
        if self.resume(lease, &request, &mut None)?.is_some() {
            return Ok(());
        }
        self.retain_inner(lease, expected, references, key)
    }

    fn pin_request(expected: ContentHash, references: &[ManifestRef], key: &KeyId) -> Request {
        let mut pins = Vec::new();
        for r in references {
            let p = Pin::from_ref(r);
            if !pins.contains(&p) {
                pins.push(p);
            }
        }
        Request::Retain {
            before: record::hex(expected),
            key: key.as_str().into(),
            pins,
        }
    }

    fn retain_inner(
        &self,
        lease: &WriterLease,
        expected: ContentHash,
        references: &[ManifestRef],
        key: &KeyId,
    ) -> Result<(), MaintenanceError> {
        self.proof(lease)?;
        load_systemd_key(key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        let tx = Transaction::new(self.tx.guard, self.tx.limits).with_write_key(key);
        #[cfg(test)]
        let tx = if let Some(checkpoint) = self.tx.boundary {
            tx.with_checkpoint(checkpoint)
        } else {
            tx
        };
        if tx.inspect().map_err(MaintenanceError::from)?.status == StartupStatus::SecretsUnavailable
        {
            return Err(MaintenanceError::SecretsUnavailable);
        }
        let working = self.baseline()?;
        let mut backup = self.capture()?;
        if backup.digest != expected {
            return Err(MaintenanceError::Conflict);
        }
        let mut retained = Vec::new();
        for reference in references {
            if !retained.contains(reference) {
                retained.push(reference.clone());
            }
        }
        if retained.len() > MAX_RETAINED_RECOVERY {
            return Err(MaintenanceError::Limits);
        }
        for reference in &retained {
            self.add_graph(&mut backup, reference)?;
        }
        let mut state = backup.state.clone();
        state.retained_recovery = retained;
        if state
            .encode()
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .len() as u64
            > self.tx.limits.max_file_bytes
        {
            return Err(MaintenanceError::Limits);
        }
        let record = Record::new(
            self.store,
            self.tx.limits,
            Self::pin_request(expected, references, key),
            Working::capture(&working)?,
            &state.encode().map_err(|_| MaintenanceError::InvalidGraph)?,
            None,
            Work::Retain,
        )?;
        let Some(intent) = self.reserve(lease, &record)? else {
            return Ok(());
        };
        tx.switch(
            Some(expected),
            &state,
            Some(digest(&working)),
            working.directories(),
        )
        .map_err(MaintenanceError::from)?;
        self.finish(lease, record, intent).map(|_| ())
    }

    /// Validate a complete backup and exact destination state/working preconditions before copying.
    /// The backup is consumed into an exact-retry plan. Delivered keys remain external.
    pub fn prepare_restore(
        &self,
        lease: &WriterLease,
        backup: Backup,
        expected: Option<ContentHash>,
        working: ContentHash,
        key: &KeyId,
    ) -> Result<RestorePlan, MaintenanceError> {
        self.restore_with(lease, backup, expected, working, key, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_with(
        &self,
        lease: &WriterLease,
        backup: Backup,
        expected: Option<ContentHash>,
        working: ContentHash,
        key: &KeyId,
        locator: Option<String>,
        frozen: Option<(&Blob, &Blob)>,
    ) -> Result<RestorePlan, MaintenanceError> {
        self.proof(lease)?;
        if backup.state.store != self.store {
            return Err(MaintenanceError::InvalidGraph);
        }
        require_compatible_limits(backup.limits, self.tx.limits)?;
        let raw = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        if raw.as_deref().map(hash) != expected {
            return Err(MaintenanceError::Conflict);
        }
        let source = self.tx.inventory().map_err(MaintenanceError::from)?;
        if digest(&source) != working {
            return Err(MaintenanceError::ExternalEdit);
        }
        let (previous, previous_ports) = if expected.is_some() {
            let selected = self.tx.inspect().map_err(MaintenanceError::from)?;
            let ports = selected.previous_ports();
            (Some(self.capture_selected(selected)?), ports)
        } else {
            self.check_bootstrap()?;
            (None, BTreeSet::new())
        };
        load_systemd_key(key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        let mut value = rptadv_json::decode_value(
            backup
                .artifacts
                .get(&manifest_path(&backup.state.desired))
                .ok_or(MaintenanceError::InvalidGraph)?
                .bytes(),
        )
        .map_err(|_| MaintenanceError::InvalidGraph)?;
        let (manifest_id, revision, journal_id) = if let Some((m, j)) = frozen {
            let manifest = PublicManifest::decode(m.bytes.as_bytes())
                .map_err(|_| MaintenanceError::InvalidGraph)?;
            let journal = PublicationJournal::decode(
                j.bytes.as_bytes(),
                self.tx.limits.max_paths,
                self.tx.limits.max_path_bytes,
            )
            .map_err(|_| MaintenanceError::InvalidGraph)?;
            if !manifest.warnings.is_empty() || !journal.warnings.is_empty() {
                return Err(MaintenanceError::InvalidGraph);
            }
            (
                manifest.value.id().as_str().to_owned(),
                manifest.value.revision().as_str().to_owned(),
                journal.value.id,
            )
        } else {
            let fresh = || {
                new_identity()
                    .map(|id| id.to_string())
                    .map_err(|_| MaintenanceError::InvalidGraph)
            };
            (
                fresh()?,
                fresh()?,
                fresh()?
                    .parse()
                    .map_err(|_| MaintenanceError::InvalidGraph)?,
            )
        };
        value["id"] = serde_json::Value::String(manifest_id);
        value["revision"] = serde_json::Value::String(revision);
        let manifest_bytes =
            serde_json::to_vec(&value).map_err(|_| MaintenanceError::InvalidGraph)?;
        let manifest = PublicManifest::decode(&manifest_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?
            .value;
        let desired = ManifestRef {
            manifest: manifest.id().clone(),
            revision: manifest.revision().clone(),
            sha256: hash(&manifest_bytes),
        };
        let target = targets(&manifest);
        let sizes = manifest
            .registry()
            .bindings()
            .filter_map(|binding| {
                let reference = manifest.reference(binding.id)?;
                let path = format!(
                    "objects/{}/{}.json",
                    binding.id,
                    reference.version().as_str()
                );
                Some(
                    backup
                        .artifacts
                        .get(&path)
                        .map(|artifact| (binding.id, artifact.bytes().len()))
                        .ok_or(MaintenanceError::InvalidGraph),
                )
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let prune = retired_directories(&source, previous_ports, &manifest)
            .map_err(MaintenanceError::from)?;
        let journal = PublicationJournal {
            id: journal_id,
            store: self.store,
            prior: expected,
            desired: desired.clone(),
            source_working: working,
            target_working: fingerprint(&target),
            changes: changes_sized(&source, &target, sizes, &prune, self.tx.limits)
                .map_err(MaintenanceError::from)?,
            prune_directories: prune,
        };
        let journal_bytes = journal
            .encode(self.tx.limits.max_paths, self.tx.limits.max_path_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        let mut retained = roots(&backup.state);
        if let Some(previous) = &previous {
            for reference in roots(&previous.state) {
                if !retained.contains(&reference) {
                    retained.push(reference);
                }
            }
        }
        let state = StatePointer {
            store: self.store,
            desired,
            last_known_good: previous
                .as_ref()
                .and_then(|p| p.state.last_known_good.clone()),
            retained_recovery: retained,
            activation: Activation::Pending,
            journal: Some(JournalRef {
                id: journal.id,
                sha256: hash(&journal_bytes),
            }),
            clean_working: None,
        };
        let manifest = (manifest_path(&state.desired), manifest_bytes);
        let journal_path = format!("journals/{}.json", journal.id);
        let plan = RestorePlan {
            backup,
            source_locator: locator,
            prepared_limits: self.tx.limits,
            before: expected,
            working: source,
            manifest,
            journal: (journal_path, journal_bytes),
            state,
            key: key.clone(),
        };
        plan.preflight(self, previous.as_ref(), journal.target_working)?;
        Ok(plan)
    }

    /// Select a fresh deliberate restore, then reuse the existing bounded journal replay.
    /// Completion is saved/published, with historical activation still pending.
    pub fn restore(
        &self,
        lease: &WriterLease,
        plan: &RestorePlan,
    ) -> Result<StatePointer, MaintenanceError> {
        self.proof(lease)?;
        if plan.state.store != self.store {
            return Err(MaintenanceError::Conflict);
        }
        require_compatible_limits(plan.prepared_limits, self.tx.limits)?;
        load_systemd_key(&plan.key).map_err(|_| MaintenanceError::SecretsUnavailable)?;
        let raw = self
            .tx
            .guard
            .read_state(self.tx.artifact_limits().max_file_bytes)
            .map_err(|_| MaintenanceError::InvalidGraph)?;
        let actual = raw.as_deref().map(hash);
        let pending = hash(
            &plan
                .state
                .encode()
                .map_err(|_| MaintenanceError::InvalidGraph)?,
        );
        let journal = PublicationJournal::decode(
            &plan.journal.1,
            self.tx.limits.max_paths,
            self.tx.limits.max_path_bytes,
        )
        .map_err(|_| MaintenanceError::InvalidGraph)?
        .value;
        let clean = plan.clean_state(journal.target_working);
        let finished = hash(&clean.encode().map_err(|_| MaintenanceError::InvalidGraph)?);
        if actual != plan.before && actual != Some(pending) && actual != Some(finished) {
            return Err(MaintenanceError::Conflict);
        }
        let previous = if actual.is_some() {
            Some(self.capture()?)
        } else {
            self.check_bootstrap()?;
            None
        };
        plan.preflight(self, previous.as_ref(), journal.target_working)?;
        if actual == plan.before {
            let current = self.tx.inventory().map_err(MaintenanceError::from)?;
            if digest(&current) != digest(&plan.working)
                || current.directories() != plan.working.directories()
            {
                return Err(MaintenanceError::ExternalEdit);
            }
        }
        let record = self.restore_record(plan)?;
        if let Some(result) = self.resume_published_restore(lease, &record)? {
            return Ok(result);
        }
        let Some(intent) = self.reserve(lease, &record)? else {
            return record.state();
        };
        plan.backup.copy(self.tx.guard)?;
        #[cfg(test)]
        self.tx
            .at(Boundary::Objects)
            .map_err(MaintenanceError::from)?;
        self.tx
            .guard
            .install_public_copy(
                &plan.manifest.0,
                &plan.manifest.1,
                self.tx.artifact_limits(),
            )
            .map_err(install_error)?;
        #[cfg(test)]
        self.tx
            .at(Boundary::Manifest)
            .map_err(MaintenanceError::from)?;
        self.tx
            .guard
            .install_public_copy(&plan.journal.0, &plan.journal.1, self.tx.artifact_limits())
            .map_err(install_error)?;
        #[cfg(test)]
        self.tx
            .at(Boundary::Journal)
            .map_err(MaintenanceError::from)?;
        if actual == plan.before {
            self.tx
                .switch(
                    plan.before,
                    &plan.state,
                    Some(digest(&plan.working)),
                    plan.working.directories(),
                )
                .map_err(MaintenanceError::from)?;
            #[cfg(test)]
            self.tx
                .at(Boundary::Pointer)
                .map_err(MaintenanceError::from)?;
        }
        self.reconcile_restore(&record, &plan.key)?;
        self.finish(lease, record, intent)?;
        Ok(clean)
    }

    #[cfg(test)]
    pub(crate) fn with_checkpoint(
        mut self,
        checkpoint: &'a dyn Fn(Boundary) -> Result<(), TransactionError>,
    ) -> Self {
        self.tx = self.tx.with_checkpoint(checkpoint);
        self
    }
}

fn rotated_index(
    opened: &OpenedPrivateRecord,
    replacements: &BTreeMap<String, PrivateReference>,
) -> Result<rptadv_peer_support::sealed_records::SecretBytes, MaintenanceError> {
    use rptadv_json::{BorrowedChildren, decode_borrowed};
    let invalid = MaintenanceError::InvalidGraph;
    let root = decode_borrowed(opened.plaintext().expose_bytes()).map_err(|_| invalid)?;
    let BorrowedChildren::Object(fields) = root.children else {
        return Err(invalid);
    };
    let Some(records) = fields.get("records") else {
        return Err(invalid);
    };
    let BorrowedChildren::Array(rows) = &records.children else {
        return Err(invalid);
    };
    let mut output = Vec::new();
    for row in rows {
        let BorrowedChildren::Array(pair) = &row.children else {
            return Err(invalid);
        };
        let [owner, reference] = pair.as_slice() else {
            return Err(invalid);
        };
        let BorrowedChildren::Object(fields) = &reference.children else {
            return Err(invalid);
        };
        let text = |name: &str| {
            serde_json::from_str::<String>(fields.get(name).ok_or(invalid)?.raw())
                .map_err(|_| invalid)
        };
        let record = text("record")?;
        let version = text("version")?;
        let path = format!("private/{record}/{version}.sealed.json");
        let reference = replacements.get(&path).ok_or(invalid)?;
        let owner = rptadv_json::decode_value(owner.raw().as_bytes()).map_err(|_| invalid)?;
        output.push(serde_json::json!([owner, reference.encode()]));
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"format_version":1,"records":output}))
        .map_err(|_| invalid)?;
    Ok(rptadv_peer_support::sealed_records::SecretBytes::new(bytes))
}
