//! Verified readback of explicitly selected immutable public configuration.

use rpt_advanced_core::config::resources::{
    ResourceResolution, ResourceWarning,
    candidate::{CandidateError, CapturedResource, ModularCandidate, ResourceBinding},
    manifest::{ContentHash, LogicalRevision, ManifestError, ManifestId, PublicManifest},
};
use rptadv_peer_support::working_files::{
    CaptureError, CaptureLimits, EditGuard,
    immutable::{ArtifactLimits, ImmutableArtifact, ImmutableTarget, ReadError},
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
        }
    }
}

impl std::error::Error for PublicLoadError {}

/// An owned, byte-verified public manifest and its selected immutable objects.
pub struct LoadedPublicManifest {
    manifest: ResourceResolution<PublicManifest>,
    raw_manifest: ImmutableArtifact,
    objects: Vec<(ResourceBinding, ImmutableArtifact)>,
}

impl LoadedPublicManifest {
    /// Access the normalized manifest and its validated registry.
    pub fn manifest(&self) -> &PublicManifest {
        &self.manifest.value
    }

    /// Borrow the exact verified manifest bytes for internal staging.
    pub fn manifest_bytes(&self) -> &[u8] {
        self.raw_manifest.bytes()
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
            .map(|(b, artifact)| (b.id, artifact.bytes()))
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
            .map(|(binding, artifact)| (binding.id, artifact.bytes().len()))
    }

    /// Check resource identities and envelopes, resolve local metadata, and stage
    /// opaque section bodies for later domain validation.
    pub fn assemble_candidate(&self) -> Result<ModularCandidate<'_>, CandidateError> {
        let captures = self
            .objects
            .iter()
            .map(|(binding, artifact)| CapturedResource {
                binding: *binding,
                json: artifact.bytes(),
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
    let path_count_limit = PublicLoadError::Read(CaptureError::Limits.into());
    if limits.max_paths == 0 {
        return Err(path_count_limit);
    }

    let mut remaining = limits.max_total_bytes;
    let mut read = |target: &ImmutableTarget,
                    sha256: &ContentHash|
     -> Result<ImmutableArtifact, PublicLoadError> {
        let artifact = guard
            .read_immutable(
                target,
                sha256.as_bytes(),
                ArtifactLimits {
                    max_path_bytes: limits.max_path_bytes,
                    max_file_bytes: limits.max_file_bytes.min(remaining),
                },
            )
            .map_err(PublicLoadError::Read)?;
        remaining -= artifact.bytes().len() as u64;
        Ok(artifact)
    };

    let target = ImmutableTarget::manifest(manifest_id.as_str())
        .map_err(|_| PublicLoadError::InvalidTarget)?;
    let raw_manifest = read(&target, expected_sha256)?;
    let manifest =
        PublicManifest::decode(raw_manifest.bytes()).map_err(PublicLoadError::Manifest)?;
    if manifest.value.id() != manifest_id {
        return Err(PublicLoadError::ManifestIdMismatch);
    }
    if manifest.value.revision() != revision {
        return Err(PublicLoadError::RevisionMismatch);
    }

    let bindings = manifest.value.registry().bindings();
    if bindings.len() >= limits.max_paths {
        return Err(path_count_limit);
    }
    let mut objects = Vec::new();
    for binding in bindings {
        if let Some(reference) = manifest.value.reference(binding.id) {
            let target = ImmutableTarget::public_object(
                &binding.id.to_string(),
                reference.version().as_str(),
            )
            .map_err(|_| PublicLoadError::InvalidTarget)?;
            objects.push((binding, read(&target, reference.sha256())?));
        }
    }

    Ok(LoadedPublicManifest {
        manifest,
        raw_manifest,
        objects,
    })
}

#[cfg(test)]
mod tests;
