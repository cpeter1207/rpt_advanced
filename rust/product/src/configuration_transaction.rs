//! Durable cross-resource selection, working publication and read-only startup inspection.
pub mod import;

use crate::{
    configuration_manifest::{LoadedPublicManifest, load_public_manifest, load_public_objects},
    configuration_private::{
        CredentialEdit, Credentials, PrivateSelection, PrivateStore, SecretsState,
    },
};
use rpt_advanced_core::config::resources::candidate::{CapturedResource, ModularCandidate};
use rpt_advanced_core::config::resources::{
    ResourceWarning,
    candidate::ResourceId,
    catalog::{self, CatalogPath},
    manifest::{ContentHash, PublicManifest},
    state::{
        Activation, JournalRef, ManifestRef, PublicationJournal, StatePointer, WorkingChange,
        WorkingTarget,
    },
};
use rptadv_peer_support::sealed_records::KeyId;
use rptadv_peer_support::working_files::{
    CaptureError, CaptureLimits, CapturePath, EditGuard, FileRequirement, Fingerprint,
    WorkingSnapshot,
    immutable::{ArtifactLimits, ImmutableTarget, InstallError, artifact_sha256},
    inventory::CatalogEntry,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Persistent safety classification before any process-local owner adopts a revision.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum StartupStatus {
    /// Eligible for normal startup preparation.
    Ready,
    /// Explicit recovery is required before transmitting.
    Interrupted,
    /// The accepted graphs verified but the protected working layout could not be read.
    StorageFault,
    /// Working bytes differ from the accepted or recoverable tree.
    ExternalEdit,
    /// Private state cannot be used for mutation or activation.
    SecretsUnavailable,
}

/// Verified selected state plus historical evidence; an error from inspect is a storage fault.
pub struct Selected {
    /// Decoded historical selector and bookkeeping.
    pub state: StatePointer,
    /// Exact digest used for pointer compare-and-replace.
    pub digest: ContentHash,
    /// Fingerprint of the current complete working tree, if safely captured.
    pub observed_working: Option<ContentHash>,
    /// Manifest selected for the desired configuration.
    pub desired: LoadedPublicManifest,
    /// Private portion of the desired configuration, or its availability status.
    pub private: PrivateSelection,
    /// Distinct retained graph's authenticated private selection.
    pub last_known_good_private: Option<PrivateSelection>,
    /// Previous verified selection, when distinct from desired.
    pub last_known_good: Option<LoadedPublicManifest>,
    /// Startup eligibility derived from disk evidence.
    pub status: StartupStatus,
    /// This open store requires an owner bookkeeping retry before another save.
    pub bookkeeping_fault: bool,
    /// Value-free warnings from selected metadata.
    pub warnings: Vec<ResourceWarning>,
    journal: Option<PublicationJournal>,
}
/// A failure before the desired pointer rename is distinct from uncertain durability.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum TransactionError {
    /// The selected store could not be read or verified.
    StorageFault,
    /// Protected private graph cannot be used for this operation.
    SecretsUnavailable,
    /// The observed inventory no longer matches the expected tree.
    ExternalEdit,
    /// The selector or lifecycle precondition failed.
    Conflict,
    /// The prepared public graph or canonical targets are inconsistent.
    InvalidCandidate,
    /// The pointer or a working rename succeeded with uncertain durability.
    DurabilityUnknown,
}
/// Preserve the caller's already observed activation even if historical bookkeeping failed.
#[derive(Debug)]
pub struct ActivationWriteError {
    /// Outcome observed at the runtime owner.
    pub observed: Activation,
    /// Persistence failure after the runtime observation.
    pub error: TransactionError,
}

/// API saves demand the clean baseline. Reload/restore explicitly acknowledge an observed tree.
pub enum Admission {
    /// First publication on the explicitly provisioned handle.
    Provision {
        /// New provisioned store UUID.
        store: ResourceId,
    },
    /// Conditional ordinary save against a clean accepted revision.
    Save {
        /// Exact expected state digest.
        state: ContentHash,
    },
    /// Import an explicitly observed external working tree.
    Reload {
        /// Exact expected state digest.
        state: ContentHash,
        /// Explicitly observed working digest.
        working: ContentHash,
    },
    /// Explicit recovery or restoration against an observed tree.
    Restore {
        /// State-selected desired or last-known-good manifest providing the restored graph.
        retained: ManifestRef,
        /// Exact expected state digest.
        state: ContentHash,
        /// Explicitly observed working digest.
        working: ContentHash,
    },
}

/// Complete guarded desired-state publication, including the explicitly staged private changes.
pub struct Publication<'a> {
    /// Exact state and working-tree preconditions.
    pub admission: Admission,
    /// Captured complete working tree.
    pub source: &'a WorkingSnapshot,
    /// Validated registry and selected immutable objects.
    pub manifest: &'a PublicManifest,
    /// New or changed public object bytes.
    pub objects: &'a [(ResourceId, &'a [u8])],
    /// Fresh durable publication journal identity.
    pub journal_id: ResourceId,
    /// Write-only changes bound to their final resource owners.
    pub edits: &'a [CredentialEdit],
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(artifact_sha256(bytes))
}
fn digest(snapshot: &WorkingSnapshot) -> ContentHash {
    ContentHash::from_bytes(*snapshot.fingerprint().as_bytes())
}
fn capture_error(e: CaptureError) -> TransactionError {
    match e {
        CaptureError::Changed => TransactionError::ExternalEdit,
        _ => TransactionError::StorageFault,
    }
}
fn write_error(e: InstallError) -> TransactionError {
    match e {
        InstallError::Conflict => TransactionError::Conflict,
        InstallError::DurabilityUnknown => TransactionError::DurabilityUnknown,
        InstallError::Store(e) => capture_error(e),
        _ => TransactionError::StorageFault,
    }
}
fn targets(manifest: &PublicManifest) -> BTreeMap<String, WorkingTarget> {
    manifest
        .registry()
        .bindings()
        .filter_map(|binding| {
            Some((
                catalog::relative_path(binding),
                WorkingTarget {
                    resource: binding.id,
                    reference: manifest.reference(binding.id)?.clone(),
                },
            ))
        })
        .collect()
}
fn fingerprint(targets: &BTreeMap<String, WorkingTarget>) -> ContentHash {
    let hashes = targets
        .iter()
        .map(|(p, t)| (p.clone(), *t.reference.sha256().as_bytes()))
        .collect();
    ContentHash::from_bytes(*Fingerprint::full(&hashes).as_bytes())
}
fn changes(
    snapshot: &WorkingSnapshot,
    targets: &BTreeMap<String, WorkingTarget>,
    graph: &LoadedPublicManifest,
    prune: &[String],
    limits: CaptureLimits,
) -> Result<Vec<WorkingChange>, TransactionError> {
    let mut dirs: BTreeSet<&Path> = snapshot
        .directories()
        .ok_or(TransactionError::InvalidCandidate)?
        .iter()
        .map(PathBuf::as_path)
        .collect();
    for dir in prune {
        dirs.remove(Path::new(dir));
    }
    for path in targets.keys() {
        if path.len() > limits.max_path_bytes {
            return Err(TransactionError::InvalidCandidate);
        }
        dirs.extend(
            Path::new(path)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty()),
        );
    }
    if targets.len().saturating_add(dirs.len()) > limits.max_paths {
        return Err(TransactionError::InvalidCandidate);
    }
    let sizes: BTreeMap<_, _> = graph.object_sizes().collect();
    let mut old: BTreeMap<_, _> = snapshot
        .files()
        .map(|(p, b)| (p.to_str().expect("catalog path").to_owned(), hash(b)))
        .collect();
    let mut changes = Vec::new();
    for (path, t) in targets {
        let source = old.remove(path);
        if source.as_ref() != Some(t.reference.sha256()) {
            changes.push(WorkingChange {
                path: path.clone(),
                source,
                target: Some(t.clone()),
            });
        }
    }
    changes.extend(old.into_iter().map(|(path, h)| WorkingChange {
        path,
        source: Some(h),
        target: None,
    }));
    // Shrink before growth: each interrupted file inventory remains within
    // the same count and total-byte limits as the source and target.
    changes.sort_by_key(|c| match &c.target {
        None => 0,
        Some(t) => match snapshot.file(Path::new(&c.path)) {
            Some(b) if sizes[&t.resource] <= b.len() => 1,
            Some(_) => 2,
            None => 3,
        },
    });
    Ok(changes)
}

fn port_directory(path: &Path) -> Option<String> {
    let mut parts = path.strip_prefix("ports").ok()?.components();
    Some(format!("ports/{}", parts.next()?.as_os_str().to_str()?))
}

fn retired_directories(
    source: &WorkingSnapshot,
    mut prior: BTreeSet<String>,
    desired: &PublicManifest,
) -> Result<Vec<String>, TransactionError> {
    for (path, _) in source.files() {
        if let Some(root) = port_directory(path).filter(|p| p.starts_with("ports/new-")) {
            prior.insert(root);
        }
    }
    for (_, id) in desired.registry().port_keys() {
        prior.remove(&format!("ports/{id}"));
    }
    Ok(source
        .directories()
        .ok_or(TransactionError::InvalidCandidate)?
        .iter()
        .rev()
        .filter(|p| port_directory(p).is_some_and(|root| prior.contains(&root)))
        .map(|p| p.to_str().expect("catalog directory").to_owned())
        .collect())
}

/// Derive historical bookkeeping from an actual outcome, including LKG promotion.
fn with_observed_activation(mut state: StatePointer, observed: Activation) -> StatePointer {
    state.activation = observed;
    if observed == Activation::Confirmed {
        state.last_known_good = Some(state.desired.clone());
    }
    state
}

fn check_pointer_budget(
    state: &StatePointer,
    working: ContentHash,
    limit: u64,
) -> Result<(), TransactionError> {
    let fits = |s: &StatePointer| -> Result<(), TransactionError> {
        if s.encode()
            .map_err(|_| TransactionError::InvalidCandidate)?
            .len() as u64
            > limit
        {
            return Err(TransactionError::InvalidCandidate);
        }
        Ok(())
    };
    fits(state)?;
    let mut clean = state.clone();
    clean.journal = None;
    clean.clean_working = Some(working);
    let promoted = with_observed_activation(clean.clone(), Activation::Confirmed);
    // An unsuccessful observation can follow either the first clean selection
    // or a later startup attempt after confirmation promoted the LKG.
    for baseline in [clean, promoted] {
        fits(&baseline)?;
        for observed in [Activation::Failed, Activation::RecoveryRequired] {
            fits(&with_observed_activation(baseline.clone(), observed))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Boundary {
    PointerBarrier,
    DesiredSync,
    FinalSync,
    WorkingFileSync,
    WorkingDirSync,
    OrphanSync,
    Objects,
    PrivateArtifacts,
    Manifest,
    Journal,
    Pointer,
    Working,
    Clean,
}

/// The actual runtime coordinator owns this guard across capture, validation, prepare and commit.
pub struct Transaction<'a, 'store> {
    #[cfg(test)]
    boundary: Option<&'a dyn Fn(Boundary) -> Result<(), TransactionError>>,
    guard: &'a EditGuard<'store>,
    write_key: Option<&'a KeyId>,
    limits: CaptureLimits,
}

impl<'a, 'store> Transaction<'a, 'store> {
    /// Bind the guarded store and the limits applied to each complete read pass.
    pub fn new(guard: &'a EditGuard<'store>, limits: CaptureLimits) -> Self {
        Self {
            guard,
            limits,
            write_key: None,
            #[cfg(test)]
            boundary: None,
        }
    }
    /// Production hosts must select their external write key even for a public-only graph.
    /// Missing material blocks mutations while inspect can still return public resources.
    /// A different selected index key requires stopped-writer maintenance.
    pub fn with_write_key(mut self, key: &'a KeyId) -> Self {
        self.write_key = Some(key);
        self
    }

    fn private_store(
        &self,
        store: ResourceId,
        bytes: u64,
        paths: usize,
    ) -> PrivateStore<'_, 'store> {
        let mut limits = self.limits;
        limits.max_total_bytes = limits.max_total_bytes.saturating_sub(bytes);
        limits.max_paths = limits.max_paths.saturating_sub(paths);
        PrivateStore {
            guard: self.guard,
            store,
            write_key: self.write_key,
            limits,
        }
    }
    fn private_selection(
        &self,
        store: ResourceId,
        graph: &LoadedPublicManifest,
    ) -> PrivateSelection {
        let used = graph.object_sizes().map(|(_, n)| n as u64).sum::<u64>()
            + graph.manifest_bytes().len() as u64;
        let owners = graph
            .assemble_candidate()
            .map_err(|_| SecretsState::Error)
            .and_then(|c| c.credential_owners().map_err(|_| SecretsState::Error));
        self.private_store(
            store,
            used,
            1 + graph.manifest().registry().bindings().len(),
        )
        .select(graph.manifest().private(), owners)
    }

    /// Exercise a real persistence boundary using the fixture supplied by the caller.
    #[cfg(test)]
    pub(crate) fn with_checkpoint(
        mut self,
        checkpoint: &'a dyn Fn(Boundary) -> Result<(), TransactionError>,
    ) -> Self {
        self.boundary = Some(checkpoint);
        self
    }
    #[cfg(test)]
    fn at(&self, point: Boundary) -> Result<(), TransactionError> {
        self.boundary.map_or(Ok(()), |callback| callback(point))
    }
    fn artifact_limits(&self) -> ArtifactLimits {
        ArtifactLimits {
            max_file_bytes: self.limits.max_file_bytes.min(self.limits.max_total_bytes),
            max_path_bytes: self.limits.max_path_bytes,
        }
    }
    /// Read the full catalog; the helper and receiver use this exact operation.
    pub fn inventory(&self) -> Result<WorkingSnapshot, TransactionError> {
        self.guard
            .inventory(self.limits, |p| match catalog::classify(p.to_str()?)? {
                CatalogPath::Directory => Some(CatalogEntry::Directory),
                CatalogPath::File(_) => Some(CatalogEntry::File),
            })
            .map_err(|error| match error {
                CaptureError::UnsafePath | CaptureError::Limits => TransactionError::ExternalEdit,
                other => capture_error(other),
            })
    }
    fn load(&self, r: &ManifestRef) -> Result<LoadedPublicManifest, TransactionError> {
        let graph =
            load_public_manifest(self.guard, &r.manifest, &r.revision, &r.sha256, self.limits)
                .map_err(|_| TransactionError::StorageFault)?;
        graph
            .assemble_candidate()
            .map_err(|_| TransactionError::StorageFault)?;
        Ok(graph)
    }
    /// Inspect only the selector and its verified graphs. Never repairs any file.
    pub fn inspect(&self) -> Result<Selected, TransactionError> {
        let bytes = self
            .guard
            .read_state(self.artifact_limits().max_file_bytes)
            .map_err(capture_error)?
            .ok_or(TransactionError::StorageFault)?;
        let decoded = StatePointer::decode(&bytes).map_err(|_| TransactionError::StorageFault)?;
        let state = decoded.value;
        let mut warnings = decoded.warnings;
        let desired = self.load(&state.desired)?;
        warnings.extend_from_slice(desired.manifest_warnings());
        let last_known_good = state
            .last_known_good
            .as_ref()
            .filter(|r| *r != &state.desired)
            .map(|r| self.load(r))
            .transpose()?;
        if let Some(graph) = &last_known_good {
            warnings.extend_from_slice(graph.manifest_warnings());
        }
        let target = targets(desired.manifest());
        let expected = fingerprint(&target);
        let journal = state
            .journal
            .as_ref()
            .map(|r| {
                let data = self
                    .guard
                    .read_journal(r.id.uuid(), r.sha256.as_bytes(), self.artifact_limits())
                    .map_err(|_| TransactionError::StorageFault)?;
                let decoded = PublicationJournal::decode(
                    &data,
                    self.limits.max_paths,
                    self.limits.max_path_bytes,
                )
                .map_err(|_| TransactionError::StorageFault)?;
                warnings.extend(decoded.warnings);
                let j = decoded.value;
                if j.id != r.id
                    || j.store != state.store
                    || j.prior.is_none() && state.last_known_good.is_some()
                    || j.desired != state.desired
                    || j.target_working != expected
                    || j.prune_directories.iter().any(|d| {
                        let prefix = format!("{d}/");
                        target
                            .range(prefix.clone()..)
                            .next()
                            .is_some_and(|(p, _)| p.starts_with(&prefix))
                    })
                    || j.changes
                        .iter()
                        .any(|c| c.target.as_ref() != target.get(&c.path))
                {
                    return Err(TransactionError::StorageFault);
                }
                Ok(j)
            })
            .transpose()?;
        if state.clean_working.is_some_and(|clean| clean != expected) {
            return Err(TransactionError::StorageFault);
        }
        let mut observed_working = None;
        let status = match self.inventory() {
            Err(TransactionError::ExternalEdit) => StartupStatus::ExternalEdit,
            Err(_) => StartupStatus::StorageFault,
            Ok(snapshot) => {
                observed_working = Some(digest(&snapshot));
                let consistent = journal.as_ref().map_or_else(
                    || observed_working == Some(expected),
                    |j| reconcilable(&snapshot, j),
                );
                if !consistent {
                    StartupStatus::ExternalEdit
                } else if journal.is_some() || state.activation != Activation::Confirmed {
                    StartupStatus::Interrupted
                } else {
                    StartupStatus::Ready
                }
            }
        };
        let private = self.private_selection(state.store, &desired);
        let last_known_good_private = last_known_good
            .as_ref()
            .map(|g| self.private_selection(state.store, g));
        let private_warnings = private.warning_count()
            + last_known_good_private
                .as_ref()
                .map_or(0, PrivateSelection::warning_count);
        warnings.extend(std::iter::repeat_n(
            ResourceWarning::UnknownField,
            private_warnings,
        ));
        let status = if private.state() != SecretsState::Ready
            || last_known_good_private
                .as_ref()
                .is_some_and(|p| p.state() != SecretsState::Ready)
        {
            StartupStatus::SecretsUnavailable
        } else {
            status
        };
        Ok(Selected {
            private,
            last_known_good_private,
            observed_working,
            state,
            digest: hash(&bytes),
            desired,
            last_known_good,
            status,
            bookkeeping_fault: self.guard.bookkeeping_fault(),
            warnings,
            journal,
        })
    }

    /// Install normalized prepared public objects, verify the complete immutable graph,
    /// publish its journal, then compare the old pointer and full source inventory at the switch.
    pub fn commit(
        &self,
        admission: Admission,
        source: &WorkingSnapshot,
        manifest: &PublicManifest,
        objects: &[(ResourceId, &[u8])],
        journal_id: ResourceId,
    ) -> Result<StatePointer, TransactionError> {
        self.commit_credentials(admission, source, manifest, objects, journal_id, &[])
            .map(|(state, _)| state)
    }

    /// Publish final public settings and explicit write-only edits in one selector switch.
    /// The returned count is the number of invalid private setting values ignored.
    pub fn commit_credentials(
        &self,
        admission: Admission,
        source: &WorkingSnapshot,
        manifest: &PublicManifest,
        objects: &[(ResourceId, &[u8])],
        journal_id: ResourceId,
        edits: &[CredentialEdit],
    ) -> Result<(StatePointer, usize), TransactionError> {
        self.commit_prepared(
            Publication {
                admission,
                source,
                manifest,
                objects,
                journal_id,
                edits,
            },
            |_| Ok(()),
        )
    }

    /// Allow a caller to finish effect-free runtime preparation against the verified final
    /// credentials before the journal or selector is published. Plaintext stays under its
    /// typed owner and an error leaves the previously selected configuration intact.
    pub fn commit_prepared(
        &self,
        publication: Publication<'_>,
        finish: impl FnOnce(&Credentials) -> Result<(), TransactionError>,
    ) -> Result<(StatePointer, usize), TransactionError> {
        let Publication {
            admission,
            source,
            manifest,
            objects,
            journal_id,
            edits,
        } = publication;
        if let Some(key) = self.write_key {
            rptadv_peer_support::sealed_records::load_systemd_key(key)
                .map_err(|_| TransactionError::SecretsUnavailable)?;
        }
        if self.guard.bookkeeping_fault() {
            return Err(TransactionError::StorageFault);
        }
        let source_hash = digest(source);
        let mut previous_ports = BTreeSet::new();
        let restoring = matches!(admission, Admission::Restore { .. });
        let mut retained_private = Credentials::default();
        let (store, prior, lkg) = match admission {
            Admission::Provision { store } => {
                if self.guard.bootstrap_identity().map_err(capture_error)? != Some(store.uuid())
                    || source.files().next().is_some()
                    || self
                        .guard
                        .read_state(self.artifact_limits().max_file_bytes)
                        .map_err(capture_error)?
                        .is_some()
                {
                    return Err(TransactionError::Conflict);
                }
                (store, None, None)
            }
            _ => {
                let old = self.inspect()?;
                if old.status == StartupStatus::StorageFault {
                    return Err(TransactionError::StorageFault);
                }
                if old.status == StartupStatus::SecretsUnavailable {
                    return Err(TransactionError::SecretsUnavailable);
                }
                let mut use_lkg = false;
                let settled = old.state.journal.is_none();
                let (expect, working, admitted) = match admission {
                    Admission::Save { state } => (
                        state,
                        old.state.clean_working,
                        settled && old.state.activation == Activation::Confirmed,
                    ),
                    // Confirmed rollback settles the previous activation. An explicit,
                    // fingerprinted reload can replace that failed desired graph.
                    Admission::Reload { state, working } => (
                        state,
                        Some(working),
                        settled
                            && matches!(
                                old.state.activation,
                                Activation::Confirmed | Activation::Failed
                            ),
                    ),
                    Admission::Restore {
                        state,
                        working,
                        retained,
                    } => {
                        if !edits.is_empty() {
                            return Err(TransactionError::InvalidCandidate);
                        }
                        use_lkg = retained != old.state.desired;
                        let source = if retained == old.state.desired {
                            Some(&old.desired)
                        } else if old.state.last_known_good.as_ref() == Some(&retained) {
                            old.last_known_good.as_ref()
                        } else {
                            None
                        };
                        if !source.is_some_and(|g| g.manifest().same_configuration(manifest)) {
                            return Err(TransactionError::InvalidCandidate);
                        }
                        (state, Some(working), true)
                    }
                    Admission::Provision { .. } => unreachable!(),
                };
                if expect != old.digest
                    || !admitted
                    || manifest.revision() == &old.state.desired.revision
                    || manifest.id() == &old.state.desired.manifest
                {
                    return Err(TransactionError::Conflict);
                }
                if working != Some(source_hash) {
                    return Err(TransactionError::ExternalEdit);
                }
                previous_ports.extend(
                    old.desired
                        .manifest()
                        .registry()
                        .port_keys()
                        .map(|(_, id)| format!("ports/{id}")),
                );
                if let Some(j) = &old.journal {
                    previous_ports.extend(
                        j.prune_directories
                            .iter()
                            .filter_map(|p| port_directory(Path::new(p))),
                    );
                }
                self.guard
                    .confirm_state(
                        old.digest.as_bytes(),
                        old.state.store.uuid(),
                        self.artifact_limits().max_file_bytes,
                        || Ok(()),
                    )
                    .map_err(write_error)?;
                retained_private = if use_lkg {
                    old.last_known_good_private
                        .ok_or(TransactionError::InvalidCandidate)?
                } else {
                    old.private
                }
                .into_credentials()
                .map_err(|_| TransactionError::SecretsUnavailable)?;
                (old.state.store, Some(old.digest), old.state.last_known_good)
            }
        };
        let limits = self.artifact_limits();
        if manifest.registry().bindings().len() >= self.limits.max_paths
            || objects.len() > self.limits.max_paths
        {
            return Err(TransactionError::InvalidCandidate);
        }
        let mut remaining = self.limits.max_total_bytes;
        let mut installed = BTreeSet::new();
        let mut pending = Vec::with_capacity(objects.len());
        for &(id, bytes) in objects {
            if bytes.len() as u64 > remaining.min(limits.max_file_bytes) {
                return Err(TransactionError::InvalidCandidate);
            }
            let binding = manifest
                .registry()
                .by_id(id)
                .ok_or(TransactionError::InvalidCandidate)?;
            if !crate::configuration_private::public_envelope(binding, bytes) {
                return Err(TransactionError::InvalidCandidate);
            }
            let r = manifest
                .reference(id)
                .ok_or(TransactionError::InvalidCandidate)?;
            if !installed.insert(id) || hash(bytes) != *r.sha256() {
                return Err(TransactionError::InvalidCandidate);
            }
            remaining -= bytes.len() as u64;
            let t = ImmutableTarget::public_object(&id.to_string(), r.version().as_str())
                .map_err(|_| TransactionError::InvalidCandidate)?;
            pending.push((id, t, bytes));
        }
        for (id, t, bytes) in pending {
            self.guard
                .prepare_object_parent(id.uuid())
                .map_err(capture_error)?;
            self.guard
                .install_immutable(&t, bytes, limits)
                .map_err(|_| TransactionError::StorageFault)?;
        }
        #[cfg(test)]
        self.at(Boundary::Objects)?;
        let verified_objects = load_public_objects(self.guard, manifest, self.limits, 0)
            .map_err(|_| TransactionError::StorageFault)?;
        let captures = verified_objects
            .iter()
            .map(|(binding, obj)| CapturedResource {
                binding: *binding,
                json: obj.bytes(),
            })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(manifest.registry(), &captures)
            .map_err(|_| TransactionError::InvalidCandidate)?;
        let owners = candidate
            .credential_owners()
            .map_err(|_| TransactionError::InvalidCandidate)?;
        let private_store = self.private_store(
            store,
            captures.iter().map(|c| c.json.len() as u64).sum(),
            1 + manifest.registry().bindings().len(),
        );
        let (private, warnings) = if restoring {
            (manifest.private(), 0)
        } else {
            if manifest.private().is_some() && manifest.private() != retained_private.reference() {
                return Err(TransactionError::InvalidCandidate);
            }
            private_store.prepare(&retained_private, &owners, edits)?
        };
        #[cfg(test)]
        self.at(Boundary::PrivateArtifacts)?;
        let bytes = manifest.encode_with_private(private);
        let r = ManifestRef {
            manifest: manifest.id().clone(),
            revision: manifest.revision().clone(),
            sha256: hash(&bytes),
        };
        let target = ImmutableTarget::manifest(r.manifest.as_str())
            .map_err(|_| TransactionError::InvalidCandidate)?;
        self.guard
            .install_immutable(&target, &bytes, limits)
            .map_err(|_| TransactionError::StorageFault)?;
        #[cfg(test)]
        self.at(Boundary::Manifest)?;
        let verified = self.load(&r)?;
        let private = self.private_selection(store, &verified);
        let credentials = private
            .credentials()
            .map_err(|_| TransactionError::SecretsUnavailable)?;
        finish(credentials)?;
        for (id, bytes) in verified
            .verified_objects()
            .filter(|(id, _)| !installed.contains(id))
        {
            let reference = manifest
                .reference(id)
                .ok_or(TransactionError::InvalidCandidate)?;
            let target =
                ImmutableTarget::public_object(&id.to_string(), reference.version().as_str())
                    .map_err(|_| TransactionError::InvalidCandidate)?;
            self.guard
                .prepare_object_parent(id.uuid())
                .map_err(capture_error)?;
            #[cfg(test)]
            self.at(Boundary::OrphanSync)?;
            self.guard
                .install_immutable(&target, bytes, limits)
                .map_err(|_| TransactionError::StorageFault)?;
        }
        let targets = targets(verified.manifest());
        let prune = retired_directories(source, previous_ports, manifest)?;
        let j = PublicationJournal {
            id: journal_id,
            store,
            prior,
            desired: r.clone(),
            source_working: source_hash,
            target_working: fingerprint(&targets),
            changes: changes(source, &targets, &verified, &prune, self.limits)?,
            prune_directories: prune,
        };
        let data = j
            .encode(self.limits.max_paths, self.limits.max_path_bytes)
            .map_err(|_| TransactionError::InvalidCandidate)?;
        let sha256 = self
            .guard
            .install_journal(journal_id.uuid(), &data, limits)
            .map_err(|_| TransactionError::StorageFault)?;
        let state = StatePointer {
            store,
            desired: r,
            last_known_good: lkg,
            activation: Activation::Pending,
            journal: Some(JournalRef {
                id: journal_id,
                sha256: ContentHash::from_bytes(sha256),
            }),
            clean_working: None,
        };
        check_pointer_budget(&state, j.target_working, limits.max_file_bytes)?;
        #[cfg(test)]
        self.at(Boundary::Journal)?;
        self.switch(prior, &state, Some(source_hash), source.directories())?;
        #[cfg(test)]
        self.at(Boundary::Pointer)?;
        Ok((state, warnings))
    }
    fn switch(
        &self,
        old: Option<ContentHash>,
        state: &StatePointer,
        working: Option<ContentHash>,
        directories: Option<&[PathBuf]>,
    ) -> Result<(), TransactionError> {
        let bytes = state
            .encode()
            .map_err(|_| TransactionError::InvalidCandidate)?;
        self.guard
            .replace_state_checked(
                old.as_ref().map(ContentHash::as_bytes),
                &bytes,
                self.artifact_limits().max_file_bytes,
                || {
                    if let Some(expected) = working {
                        let actual = self.inventory().map_err(|_| CaptureError::Changed)?;
                        if digest(&actual) != expected
                            || directories.is_some_and(|d| actual.directories() != Some(d))
                        {
                            return Err(CaptureError::Changed.into());
                        }
                        if state.journal.is_none() {
                            self.guard.sync_inventory(&actual, |_entry| {
                                #[cfg(test)]
                                self.at(match _entry {
                                    CatalogEntry::File => Boundary::WorkingFileSync,
                                    CatalogEntry::Directory => Boundary::WorkingDirSync,
                                })
                                .map_err(|_| InstallError::DurabilityUnknown)?;
                                Ok(())
                            })?;
                            let fresh = self.inventory().map_err(|_| CaptureError::Changed)?;
                            if digest(&fresh) != expected
                                || fresh.directories() != actual.directories()
                            {
                                return Err(CaptureError::Changed.into());
                            }
                            self.guard.verify_current(&actual)?;
                        }
                    }
                    Ok(())
                },
                || {
                    #[cfg(test)]
                    self.at(if state.journal.is_some() {
                        Boundary::DesiredSync
                    } else {
                        Boundary::FinalSync
                    })
                    .map_err(|_| InstallError::DurabilityUnknown)?;
                    Ok(())
                },
            )
            .map_err(write_error)
    }

    /// Publish only selected journal changes and finalize the clean fingerprint.
    /// Repeating after any partial write validates the recorded source or already-target.
    pub fn reconcile(&self) -> Result<(), TransactionError> {
        let mut selected = self.inspect()?;
        if selected.status == StartupStatus::SecretsUnavailable {
            return Err(TransactionError::SecretsUnavailable);
        }
        if selected.status == StartupStatus::StorageFault {
            return Err(TransactionError::StorageFault);
        }
        if selected.status == StartupStatus::ExternalEdit {
            return Err(TransactionError::ExternalEdit);
        }
        self.guard
            .confirm_state(
                selected.digest.as_bytes(),
                selected.state.store.uuid(),
                self.artifact_limits().max_file_bytes,
                || {
                    #[cfg(test)]
                    self.at(Boundary::PointerBarrier)
                        .map_err(|_| InstallError::DurabilityUnknown)?;
                    Ok(())
                },
            )
            .map_err(write_error)?;
        let Some(j) = selected.journal else {
            return Ok(());
        };
        for change in j.changes.iter().filter(|c| c.target.is_none()) {
            self.publish_change(change)?;
        }
        for dir in &j.prune_directories {
            let path = CapturePath::new(dir, FileRequirement::Optional).map_err(capture_error)?;
            self.guard.prune_directory(&path).map_err(|e| {
                if e == InstallError::Conflict {
                    TransactionError::ExternalEdit
                } else {
                    write_error(e)
                }
            })?;
        }
        for change in j.changes.iter().filter(|c| c.target.is_some()) {
            self.publish_change(change)?;
        }
        selected.state.journal = None;
        selected.state.clean_working = Some(j.target_working);
        self.switch(
            Some(selected.digest),
            &selected.state,
            Some(j.target_working),
            None,
        )?;
        #[cfg(test)]
        self.at(Boundary::Clean)?;
        Ok(())
    }

    fn publish_change(&self, change: &WorkingChange) -> Result<(), TransactionError> {
        let target = change
            .target
            .as_ref()
            .map(|t| {
                let location = ImmutableTarget::public_object(
                    &t.resource.to_string(),
                    t.reference.version().as_str(),
                )
                .map_err(|_| TransactionError::StorageFault)?;
                self.guard
                    .read_immutable(
                        &location,
                        t.reference.sha256().as_bytes(),
                        self.artifact_limits(),
                    )
                    .map_err(|_| TransactionError::StorageFault)
            })
            .transpose()?;
        let path =
            CapturePath::new(&change.path, FileRequirement::Optional).map_err(capture_error)?;
        self.guard
            .reconcile_file(
                &path,
                change.source.as_ref().map(ContentHash::as_bytes),
                target.as_ref(),
                self.limits.max_file_bytes,
            )
            .map_err(|e| {
                if e == InstallError::Conflict {
                    TransactionError::ExternalEdit
                } else {
                    write_error(e)
                }
            })?;
        #[cfg(test)]
        self.at(Boundary::Working)?;
        Ok(())
    }

    /// Persist a real owner's observed outcome without touching working files.
    pub fn record_activation(
        &self,
        selected: &Selected,
        observed: Activation,
    ) -> Result<(), ActivationWriteError> {
        if observed == Activation::Pending {
            return Err(ActivationWriteError {
                observed,
                error: TransactionError::Conflict,
            });
        }
        let result = (|| {
            let current = self.inspect()?;
            if current.status == StartupStatus::SecretsUnavailable {
                return Err(TransactionError::SecretsUnavailable);
            }
            if current.digest != selected.digest {
                return Err(TransactionError::Conflict);
            }
            let state = current.state;
            if state.store != selected.state.store
                || state.desired != selected.state.desired
                || state.journal.is_some()
            {
                return Err(TransactionError::Conflict);
            }
            let next = with_observed_activation(state, observed);
            self.switch(Some(selected.digest), &next, None, None)
        })();
        if result != Err(TransactionError::SecretsUnavailable) {
            self.guard.set_bookkeeping_fault(result.is_err());
        }
        result.map_err(|error| ActivationWriteError { observed, error })
    }
}

/// Undo/redo only in a hash map. Both reconstructed inventories must match
/// the journal, so an unrelated addition or deletion also blocks publication.
fn reconcilable(snapshot: &WorkingSnapshot, j: &PublicationJournal) -> bool {
    let mut source: BTreeMap<_, _> = snapshot
        .files()
        .map(|(p, b)| {
            (
                p.to_str().expect("catalog path").to_owned(),
                artifact_sha256(b),
            )
        })
        .collect();
    let mut target = source.clone();
    for c in &j.changes {
        let old = c.source.as_ref().map(ContentHash::as_bytes);
        let new = c.target.as_ref().map(|t| t.reference.sha256().as_bytes());
        let current = source.get(&c.path);
        if current != old && current != new {
            return false;
        }
        for (map, hash) in [(&mut source, old), (&mut target, new)] {
            if let Some(h) = hash {
                map.insert(c.path.clone(), *h);
            } else {
                map.remove(&c.path);
            }
        }
    }
    Fingerprint::full(&source).as_bytes() == j.source_working.as_bytes()
        && Fingerprint::full(&target).as_bytes() == j.target_working.as_bytes()
}

#[cfg(test)]
mod tests;
