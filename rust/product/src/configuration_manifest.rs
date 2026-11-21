//! Verified readback of explicitly selected immutable public configuration.

use rpt_advanced_core::config::resources::{
    ResourceResolution, ResourceWarning,
    candidate::{CandidateError, CapturedResource, ModularCandidate, ResourceBinding},
    manifest::{ContentHash, LogicalRevision, ManifestError, ManifestId, PublicManifest},
};
use rptadv_peer_support::working_files::{
    CaptureError, CaptureLimits, EditGuard,
    immutable::{ArtifactLimits, ImmutableTarget, ReadError},
};
use std::fmt;

/// Value-free failure loading the selected immutable public graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicLoadError {
    /// The protected artifact could not be read and verified.
    Read(ReadError),
    /// The public manifest could not be decoded.
    Manifest(ManifestError),
    /// The verified manifest declares a different identifier.
    ManifestIdMismatch,
    /// The verified manifest declares a different logical revision.
    RevisionMismatch,
    /// An object declares an incorrect identity or contains reserved private fields.
    InvalidEnvelope,
    /// The host rejected a derived immutable target.
    InvalidTarget,
}

impl fmt::Display for PublicLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => error.fmt(formatter),
            Self::Manifest(error) => error.fmt(formatter),
            Self::ManifestIdMismatch => formatter.write_str("selected public manifest ID mismatch"),
            Self::RevisionMismatch => {
                formatter.write_str("selected public manifest revision mismatch")
            }
            Self::InvalidTarget => formatter.write_str("invalid derived public artifact target"),
            Self::InvalidEnvelope => formatter.write_str("invalid public object envelope"),
        }
    }
}

impl std::error::Error for PublicLoadError {}

/// An owned, byte-verified public manifest and its selected immutable objects.
pub struct LoadedPublicManifest {
    manifest: ResourceResolution<PublicManifest>,
    raw_manifest: Vec<u8>,
    objects: Vec<(ResourceBinding, Vec<u8>)>,
}

impl LoadedPublicManifest {
    /// Access the normalized manifest and its validated registry.
    pub fn manifest(&self) -> &PublicManifest {
        &self.manifest.value
    }

    /// Borrow the exact verified manifest bytes for internal staging.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.raw_manifest
    }

    /// Recoverable warnings from interpreting the public manifest.
    pub fn manifest_warnings(&self) -> &[ResourceWarning] {
        &self.manifest.warnings
    }

    /// Borrow exact verified immutable bytes for confirming previously unselected references.
    pub(crate) fn verified_objects(
        &self,
    ) -> impl Iterator<
        Item = (
            rpt_advanced_core::config::resources::candidate::ResourceId,
            &[u8],
        ),
    > + '_ {
        self.objects
            .iter()
            .map(|(b, artifact)| (b.id, artifact.as_slice()))
    }

    /// Exact verified public object sizes, for bounded working publication planning.
    pub(crate) fn object_sizes(
        &self,
    ) -> impl Iterator<
        Item = (
            rpt_advanced_core::config::resources::candidate::ResourceId,
            usize,
        ),
    > + '_ {
        self.objects
            .iter()
            .map(|(binding, artifact)| (binding.id, artifact.len()))
    }

    /// Check resource identities and envelopes, resolve local metadata, and stage
    /// opaque section bodies for later domain validation.
    pub fn assemble_candidate(&self) -> Result<ModularCandidate<'_>, CandidateError> {
        let captures = self
            .objects
            .iter()
            .map(|(binding, artifact)| CapturedResource {
                binding: *binding,
                json: artifact.as_slice(),
            })
            .collect::<Vec<_>>();
        ModularCandidate::assemble(self.manifest().registry(), &captures)
    }
}

impl fmt::Debug for LoadedPublicManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LoadedPublicManifest(..)")
    }
}

/// Read and hash-verify a caller-selected manifest and all its public objects.
///
/// Limits cover the manifest and its registered rows, including empty slots.
/// The caller selects the manifest's ID, logical revision and trusted hash.
pub fn load_public_manifest(
    guard: &EditGuard<'_>,
    manifest_id: &ManifestId,
    revision: &LogicalRevision,
    expected_sha256: &ContentHash,
    limits: CaptureLimits,
) -> Result<LoadedPublicManifest, PublicLoadError> {
    load_public_manifest_with(
        manifest_id,
        revision,
        expected_sha256,
        limits,
        |_, target, expected, bounds| read_public(guard, target, expected, bounds),
    )
}

fn read_public(
    guard: &EditGuard<'_>,
    target: &ImmutableTarget,
    expected: &ContentHash,
    limits: ArtifactLimits,
) -> Result<Vec<u8>, PublicLoadError> {
    guard
        .read_immutable(target, expected.as_bytes(), limits)
        .map(|artifact| artifact.into_bytes())
        .map_err(PublicLoadError::Read)
}

pub(crate) fn load_public_manifest_with(
    manifest_id: &ManifestId,
    revision: &LogicalRevision,
    expected_sha256: &ContentHash,
    limits: CaptureLimits,
    read: impl Fn(
        &str,
        &ImmutableTarget,
        &ContentHash,
        ArtifactLimits,
    ) -> Result<Vec<u8>, PublicLoadError>,
) -> Result<LoadedPublicManifest, PublicLoadError> {
    let target = ImmutableTarget::manifest(manifest_id.as_str())
        .map_err(|_| PublicLoadError::InvalidTarget)?;
    let raw_manifest = read(
        &format!("manifests/{}.json", manifest_id.as_str()),
        &target,
        expected_sha256,
        ArtifactLimits {
            max_file_bytes: limits.max_file_bytes.min(limits.max_total_bytes),
            max_path_bytes: limits.max_path_bytes,
        },
    )?;
    let manifest = PublicManifest::decode(&raw_manifest).map_err(PublicLoadError::Manifest)?;
    if manifest.value.id() != manifest_id {
        return Err(PublicLoadError::ManifestIdMismatch);
    }
    if manifest.value.revision() != revision {
        return Err(PublicLoadError::RevisionMismatch);
    }

    let objects =
        load_public_objects_with(&read, &manifest.value, limits, raw_manifest.len() as u64)?;
    Ok(LoadedPublicManifest {
        manifest,
        raw_manifest,
        objects,
    })
}

/// Verify the objects of an in-memory candidate before its encrypted index is selected.
pub(crate) fn load_public_objects(
    guard: &EditGuard<'_>,
    manifest: &PublicManifest,
    limits: CaptureLimits,
    used: u64,
) -> Result<Vec<(ResourceBinding, Vec<u8>)>, PublicLoadError> {
    load_public_objects_with(
        &|_, target, expected, bounds| read_public(guard, target, expected, bounds),
        manifest,
        limits,
        used,
    )
}

fn load_public_objects_with(
    read: &impl Fn(
        &str,
        &ImmutableTarget,
        &ContentHash,
        ArtifactLimits,
    ) -> Result<Vec<u8>, PublicLoadError>,
    manifest: &PublicManifest,
    limits: CaptureLimits,
    used: u64,
) -> Result<Vec<(ResourceBinding, Vec<u8>)>, PublicLoadError> {
    let path_count_limit = PublicLoadError::Read(CaptureError::Limits.into());
    if limits.max_paths == 0 {
        return Err(path_count_limit);
    }

    let mut remaining = limits
        .max_total_bytes
        .checked_sub(used)
        .ok_or(path_count_limit)?;
    let bindings = manifest.registry().bindings();
    if bindings.len() >= limits.max_paths {
        return Err(path_count_limit);
    }
    let mut objects = Vec::new();
    for binding in bindings {
        if let Some(reference) = manifest.reference(binding.id) {
            let target = ImmutableTarget::public_object(
                &binding.id.to_string(),
                reference.version().as_str(),
            )
            .map_err(|_| PublicLoadError::InvalidTarget)?;
            let path = format!(
                "objects/{}/{}.json",
                binding.id,
                reference.version().as_str()
            );
            let artifact = read(
                &path,
                &target,
                reference.sha256(),
                ArtifactLimits {
                    max_path_bytes: limits.max_path_bytes,
                    max_file_bytes: limits.max_file_bytes.min(remaining),
                },
            )?;
            remaining = remaining
                .checked_sub(artifact.len() as u64)
                .ok_or(path_count_limit)?;
            if !crate::configuration_private::public_envelope(binding, artifact.as_slice()) {
                return Err(PublicLoadError::InvalidEnvelope);
            }
            objects.push((binding, artifact));
        }
    }

    Ok(objects)
}

#[cfg(test)]
mod tests;
