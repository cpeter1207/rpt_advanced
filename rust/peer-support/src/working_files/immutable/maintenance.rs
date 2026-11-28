//! Canonical protected artifact operations; reachability policy stays in the product.
use super::*;
use crate::working_files::{CaptureLimits, WriterLease};
use rustix::fs::Dir;
use std::{collections::BTreeSet, ffi::OsStr, os::unix::ffi::OsStrExt};

/// Registered immutable namespace role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactRole {
    /// Public resource version.
    PublicObject,
    /// Reference-only public manifest.
    Manifest,
    /// Immutable publication journal.
    Journal,
    /// Encrypted index or leaf.
    Private,
}
/// Validated canonical artifact path; it never exposes a raw private installation target.
pub struct ArtifactPath {
    relative: String,
    target: ImmutableTarget,
    role: ArtifactRole,
}
impl ArtifactPath {
    /// Admit exactly a registered namespace, canonical UUID shards and fixed suffixes.
    pub fn parse(path: &str) -> Option<Self> {
        let parts: Vec<_> = path.split('/').collect();
        let canonical = |s: &str| Uuid::parse_str(s).ok().filter(|id| id.to_string() == s);
        let (target, role) = match parts.as_slice() {
            ["objects", resource, leaf] => {
                canonical(resource)?;
                (
                    ImmutableTarget::public_object(resource, leaf.strip_suffix(".json")?).ok()?,
                    ArtifactRole::PublicObject,
                )
            }
            ["private", resource, leaf] => {
                canonical(resource)?;
                let version = leaf.strip_suffix(".sealed.json")?;
                canonical(version)?;
                (
                    ImmutableTarget {
                        namespace: "private",
                        resource: Some((*resource).to_owned()),
                        leaf: (*leaf).to_owned(),
                    },
                    ArtifactRole::Private,
                )
            }
            ["manifests", leaf] => (
                ImmutableTarget::manifest(leaf.strip_suffix(".json")?).ok()?,
                ArtifactRole::Manifest,
            ),
            ["journals", leaf] => {
                let id = canonical(leaf.strip_suffix(".json")?)?;
                (ImmutableTarget::journal(id).ok()?, ArtifactRole::Journal)
            }
            _ => return None,
        };
        Some(Self {
            relative: path.to_owned(),
            target,
            role,
        })
    }
    /// Canonical store-relative path.
    pub fn relative(&self) -> &str {
        &self.relative
    }
    /// Registered namespace role.
    pub fn role(&self) -> ArtifactRole {
        self.role
    }
}
/// Pinned, exact-byte captured immutable artifact.
pub struct CapturedArtifact {
    path: ArtifactPath,
    artifact: ImmutableArtifact,
}
impl CapturedArtifact {
    /// Canonical protected location.
    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }
    /// Exact stored ciphertext/public bytes.
    pub fn bytes(&self) -> &[u8] {
        self.artifact.bytes()
    }
    /// SHA-256 over those exact bytes.
    pub fn sha256(&self) -> &[u8; 32] {
        self.artifact.sha256()
    }
}
impl EditGuard<'_> {
    /// Enumerate only canonical immutable paths, preserving unknown entries.
    /// Every encountered directory entry counts toward the caller's bound.
    pub fn discover_artifacts(&self, limits: CaptureLimits) -> Result<Vec<String>, CaptureError> {
        self.check_lock()?;
        let mut paths = BTreeSet::new();
        let mut count = 0usize;
        for namespace in ["objects", "manifests", "journals", "private"] {
            let parent =
                protected_directory(&self.store.root, Path::new(namespace), self.store.uid)?;
            for entry in
                Dir::read_from(super::super::transaction::readable(&parent)?).map_err(open_error)?
            {
                let entry = entry.map_err(open_error)?;
                let bytes = entry.file_name().to_bytes();
                if matches!(bytes, b"." | b"..") {
                    continue;
                }
                count = count.checked_add(1).ok_or(CaptureError::Limits)?;
                if count > limits.max_paths {
                    return Err(CaptureError::Limits);
                }
                let Ok(name) = std::str::from_utf8(bytes) else {
                    continue;
                };
                let prefix = format!("{namespace}/{name}");
                if prefix.len() > limits.max_path_bytes {
                    return Err(CaptureError::Limits);
                }
                if matches!(namespace, "objects" | "private") {
                    if Uuid::parse_str(name)
                        .ok()
                        .is_none_or(|id| id.to_string() != name)
                    {
                        continue;
                    }
                    let child = protected_directory(
                        &parent,
                        Path::new(OsStr::from_bytes(bytes)),
                        self.store.uid,
                    )?;
                    for row in Dir::read_from(super::super::transaction::readable(&child)?)
                        .map_err(open_error)?
                    {
                        let row = row.map_err(open_error)?;
                        let bytes = row.file_name().to_bytes();
                        if matches!(bytes, b"." | b"..") {
                            continue;
                        }
                        count = count.checked_add(1).ok_or(CaptureError::Limits)?;
                        if count > limits.max_paths {
                            return Err(CaptureError::Limits);
                        }
                        let Ok(leaf) = std::str::from_utf8(bytes) else {
                            continue;
                        };
                        let path = format!("{prefix}/{leaf}");
                        if path.len() > limits.max_path_bytes {
                            return Err(CaptureError::Limits);
                        }
                        if ArtifactPath::parse(&path).is_some() {
                            paths.insert(path);
                        }
                    }
                    check_inode(
                        &child,
                        &protected_directory(&parent, Path::new(name), self.store.uid)?,
                    )?;
                } else if ArtifactPath::parse(&prefix).is_some() {
                    paths.insert(prefix);
                }
            }
            check_inode(
                &parent,
                &protected_directory(&self.store.root, Path::new(namespace), self.store.uid)?,
            )?;
        }
        self.check_lock()?;
        Ok(paths.into_iter().collect())
    }

    /// Capture a canonical artifact without trusting its content or selecting it.
    pub fn capture_artifact(
        &self,
        path: ArtifactPath,
        limits: ArtifactLimits,
    ) -> Result<CapturedArtifact, CaptureError> {
        if path.target.relative_len() > limits.max_path_bytes {
            return Err(CaptureError::Limits);
        }
        self.check_lock()?;
        let parent = path.target.open_parent(self.store)?;
        let (file, metadata) = file_handle(&parent, Path::new(&path.target.leaf), self.store.uid)?;
        let captured = read_pinned_regular(&file, &metadata, limits.max_file_bytes)?;
        self.confirm_binding(&path.target, &parent, &captured.version)?;
        let sha256 = artifact_sha256(&captured.bytes);
        Ok(CapturedArtifact {
            artifact: ImmutableArtifact {
                public_object: path.role == ArtifactRole::PublicObject,
                bytes: captured.bytes,
                sha256,
            },
            path,
        })
    }

    /// Check an exact immutable-copy destination without creating or overwriting anything.
    pub fn check_artifact_copy(
        &self,
        path: &str,
        bytes: &[u8],
        limits: ArtifactLimits,
    ) -> Result<(), InstallError> {
        let path = ArtifactPath::parse(path).ok_or(InstallError::InvalidTarget)?;
        match self.capture_artifact(path, limits) {
            Ok(existing) if existing.bytes() == bytes => Ok(()),
            Ok(_) => Err(InstallError::Conflict),
            Err(CaptureError::Missing) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Install only public objects/manifests/journals; sealed copies use their authenticated type.
    pub fn install_public_copy(
        &self,
        path: &str,
        bytes: &[u8],
        limits: ArtifactLimits,
    ) -> Result<(), InstallError> {
        let path = ArtifactPath::parse(path).ok_or(InstallError::InvalidTarget)?;
        if path.role == ArtifactRole::Private {
            return Err(InstallError::InvalidTarget);
        }
        if path.role == ArtifactRole::PublicObject {
            self.prepare_object_parent(
                Uuid::parse_str(path.target.resource.as_ref().expect("public shard"))
                    .map_err(|_| InstallError::InvalidTarget)?,
            )?;
        }
        self.install_immutable(&path.target, bytes, limits)?;
        Ok(())
    }

    /// Remove only an exact canonical artifact under a same-directory writer lifetime.
    /// The selecting owner rechecks roots immediately before unlink. Missing leaves are
    /// synchronized exact retries; protected parent directories are never removed.
    pub fn remove_artifact_checked(
        &self,
        lease: &WriterLease,
        path: &str,
        expected: &[u8; 32],
        limits: ArtifactLimits,
        recheck: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        self.check_writer_lease(lease)?;
        let path = ArtifactPath::parse(path).ok_or(InstallError::InvalidTarget)?;
        if path.target.relative_len() > limits.max_path_bytes {
            return Err(InstallError::Limits);
        }
        let parent = path.target.open_parent(self.store)?;
        let parent = super::super::transaction::readable(&parent)?;
        let file = match file_handle(&parent, Path::new(&path.target.leaf), self.store.uid) {
            Ok((file, meta)) => {
                let captured = read_pinned_regular(&file, &meta, limits.max_file_bytes)?;
                if artifact_sha256(&captured.bytes) != *expected {
                    return Err(InstallError::Conflict);
                }
                Some(captured.version)
            }
            Err(CaptureError::Missing) => None,
            Err(error) => return Err(error.into()),
        };
        recheck()?;
        self.check_writer_lease(lease)?;
        self.check_artifact_parent(&path.target, &parent)?;
        if let Some(version) = file {
            self.confirm_binding(&path.target, &parent, &version)?;
            fs::unlinkat(&parent, path.target.leaf.as_str(), AtFlags::empty())
                .map_err(|_| InstallError::Io)?;
        }
        fs::fsync(&parent).map_err(|_| InstallError::DurabilityUnknown)?;
        self.check_artifact_parent(&path.target, &parent)
            .map_err(|_| InstallError::DurabilityUnknown)
    }
}
