//! Durable installation of prepared public objects and manifests on Linux.
//!
//! Writers coordinate through the held edit lock. Revalidation can detect
//! changes that are observed before it finishes, with a final race against
//! writers that ignore the lock.

use super::{
    CaptureError, EditGuard, FileVersion, PATH_FLAGS, WorkingFiles, check_inode, check_version,
    file_handle, on_recheck, open_error, protected_directory, reopen_regular, valid_component,
};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use rustix::{
    fs::{self, AtFlags, Mode, OFlags, RenameFlags},
    io::Errno,
};
use std::{
    fmt,
    fs::{File, Metadata},
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

/// Validated location for one public object or manifest.
pub struct ImmutableTarget {
    namespace: &'static str,
    resource: Option<String>,
    leaf: String,
}

impl ImmutableTarget {
    /// Select a version under the canonical resource UUID.
    pub fn public_object(resource_uuid: &str, version: &str) -> Result<Self, InstallError> {
        let uuid = Uuid::parse_str(resource_uuid).map_err(|_| InstallError::InvalidTarget)?;
        Self::new("objects", Some(uuid.to_string()), version)
    }

    /// Select a manifest by its server-assigned identifier.
    pub fn manifest(manifest_id: &str) -> Result<Self, InstallError> {
        Self::new("manifests", None, manifest_id)
    }

    /// Append the fixed extension to a single validated component.
    fn new(
        namespace: &'static str,
        resource: Option<String>,
        id: &str,
    ) -> Result<Self, InstallError> {
        if !valid_component(id.as_bytes()) {
            return Err(InstallError::InvalidTarget);
        }
        Ok(Self {
            namespace,
            resource,
            leaf: format!("{id}.json"),
        })
    }

    /// Length of the actual store-relative filesystem path in bytes.
    fn relative_len(&self) -> usize {
        self.namespace.len()
            + 1
            + self.resource.as_ref().map_or(0, |uuid| uuid.len() + 1)
            + self.leaf.len()
    }

    /// Reopen all protected parent components from the store descriptor.
    fn open_parent(&self, store: &WorkingFiles) -> Result<File, CaptureError> {
        let namespace = protected_directory(&store.root, Path::new(self.namespace), store.uid)?;
        match &self.resource {
            Some(uuid) => protected_directory(&namespace, Path::new(uuid), store.uid),
            None => Ok(namespace),
        }
    }
}

impl fmt::Debug for ImmutableTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImmutableTarget(..)")
    }
}

/// Caller-supplied bounds for one immutable installation.
#[derive(Clone, Copy, Debug)]
pub struct InstallLimits {
    /// Maximum length of the actual store-relative path in bytes.
    pub max_path_bytes: usize,
    /// Maximum size of the supplied artifact in bytes.
    pub max_file_bytes: u64,
}

/// Receipt for a synchronized immutable artifact.
pub struct InstalledArtifact {
    outcome: InstallOutcome,
    sha256: [u8; 32],
}

impl InstalledArtifact {
    /// Report whether installation created the artifact or reused exact bytes.
    pub fn outcome(&self) -> InstallOutcome {
        self.outcome
    }

    /// Return the SHA-256 digest of the installed bytes.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

impl fmt::Debug for InstalledArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InstalledArtifact(..)")
    }
}

/// How the synchronized immutable artifact was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    /// A newly synchronized file was published.
    Created,
    /// The exact existing bytes were verified and synchronized.
    Reused,
}

/// Content-free failure from immutable installation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// The resource UUID or artifact identifier is invalid.
    InvalidTarget,
    /// The actual relative path or supplied payload exceeds its limit.
    Limits,
    /// The protected path or edit lock could not be validated.
    Store(CaptureError),
    /// A protected file at the target contains different bytes.
    Conflict,
    /// The final name may exist, but successful durability is unconfirmed.
    DurabilityUnknown,
    /// An installation I/O operation failed before committing.
    Io,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTarget => "invalid immutable artifact target",
            Self::Limits => "immutable artifact limit exceeded",
            Self::Store(_) => "protected immutable artifact location is unavailable",
            Self::Conflict => "immutable artifact already exists with different bytes",
            Self::DurabilityUnknown => "immutable artifact durability is unconfirmed",
            Self::Io => "immutable artifact installation failed",
        })
    }
}

impl std::error::Error for InstallError {}

impl From<CaptureError> for InstallError {
    fn from(error: CaptureError) -> Self {
        Self::Store(error)
    }
}

/// Per-call commit boundaries used by the real-filesystem tests.
#[derive(Clone, Copy, Eq, PartialEq)]
enum InstallBoundary {
    BeforePublish,
    BeforeParentSync,
}

impl EditGuard<'_> {
    /// Install trusted prepared bytes and synchronize the resulting artifact.
    ///
    /// A durability-unknown result permits an exact-byte retry. This operation
    /// does not select accepted configuration.
    pub fn install_immutable(
        &self,
        target: &ImmutableTarget,
        bytes: &[u8],
        limits: InstallLimits,
    ) -> Result<InstalledArtifact, InstallError> {
        self.install(target, bytes, limits, |_| Ok(()))
    }

    /// Install through an anchored parent and share the exact-retry path.
    fn install(
        &self,
        target: &ImmutableTarget,
        bytes: &[u8],
        limits: InstallLimits,
        boundary: impl FnMut(InstallBoundary) -> Result<(), InstallError>,
    ) -> Result<InstalledArtifact, InstallError> {
        if target.relative_len() > limits.max_path_bytes
            || bytes.len() as u64 > limits.max_file_bytes
        {
            return Err(InstallError::Limits);
        }
        self.check_lock()?;
        let parent = target.open_parent(self.store)?;
        let parent = File::from(
            fs::openat(
                &parent,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| InstallError::Io)?,
        );
        let leaf = Path::new(&target.leaf);
        let outcome = match file_handle(&parent, leaf, self.store.uid) {
            Ok(existing) => self.reuse(target, &parent, existing, bytes)?,
            Err(CaptureError::Missing) => self.publish(target, &parent, bytes, boundary)?,
            Err(error) => return Err(error.into()),
        };
        let mut sha256 = [0; 32];
        sha256.copy_from_slice(digest::digest(&digest::SHA256, bytes).as_ref());
        Ok(InstalledArtifact { outcome, sha256 })
    }

    /// Stage and publish without replacing any existing final entry.
    fn publish(
        &self,
        target: &ImmutableTarget,
        parent: &File,
        bytes: &[u8],
        mut boundary: impl FnMut(InstallBoundary) -> Result<(), InstallError>,
    ) -> Result<InstallOutcome, InstallError> {
        let mut scratch = Scratch::create(parent)?;
        scratch
            .file
            .write_all(bytes)
            .map_err(|_| InstallError::Io)?;
        scratch.file.sync_all().map_err(|_| InstallError::Io)?;
        boundary(InstallBoundary::BeforePublish)?;
        scratch.check_name().map_err(on_recheck)?;
        self.check_install_parent(target, parent)?;
        let leaf = Path::new(&target.leaf);
        match fs::renameat_with(
            parent,
            scratch.name.as_str(),
            parent,
            leaf,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => scratch.published = true,
            Err(Errno::EXIST) => {
                drop(scratch);
                let existing = file_handle(parent, leaf, self.store.uid)?;
                return self.reuse(target, parent, existing, bytes);
            }
            Err(_) => return Err(InstallError::Io),
        }

        // Rename may change ctime. Recheck against the published version.
        let metadata = scratch
            .file
            .metadata()
            .map_err(|_| InstallError::DurabilityUnknown)?;
        boundary(InstallBoundary::BeforeParentSync).map_err(|_| InstallError::DurabilityUnknown)?;
        self.sync_and_confirm(target, parent, &FileVersion::from(&metadata))?;
        Ok(InstallOutcome::Created)
    }

    /// Compare a pinned regular file in bounded chunks and synchronize an exact retry.
    fn reuse(
        &self,
        target: &ImmutableTarget,
        parent: &File,
        (handle, metadata): (File, Metadata),
        bytes: &[u8],
    ) -> Result<InstallOutcome, InstallError> {
        if metadata.len() != bytes.len() as u64 {
            return Err(InstallError::Conflict);
        }
        let version = FileVersion::from(&metadata);
        let mut file = reopen_regular(&handle, &version)?;
        let mut buffer = [0; 8192];
        for expected in bytes.chunks(buffer.len()) {
            let actual = &mut buffer[..expected.len()];
            file.read_exact(actual).map_err(|_| InstallError::Io)?;
            if actual != expected {
                return Err(InstallError::Conflict);
            }
        }
        check_version(&file, &version)?;
        self.check_install_parent(target, parent)?;
        file.sync_all()
            .map_err(|_| InstallError::DurabilityUnknown)?;
        self.sync_and_confirm(target, parent, &version)?;
        Ok(InstallOutcome::Reused)
    }

    /// Verify that the pinned parent and held lock are still current.
    fn check_install_parent(
        &self,
        target: &ImmutableTarget,
        parent: &File,
    ) -> Result<(), CaptureError> {
        let current = target.open_parent(self.store).map_err(on_recheck)?;
        check_inode(parent, &current)?;
        self.check_lock()
    }

    /// Sync the changed directory and confirm the current final binding.
    fn sync_and_confirm(
        &self,
        target: &ImmutableTarget,
        parent: &File,
        version: &FileVersion,
    ) -> Result<(), InstallError> {
        fs::fsync(parent).map_err(|_| InstallError::DurabilityUnknown)?;
        let (current, _) = file_handle(parent, Path::new(&target.leaf), self.store.uid)
            .map_err(|_| InstallError::DurabilityUnknown)?;
        check_version(&current, version).map_err(|_| InstallError::DurabilityUnknown)?;
        self.check_install_parent(target, parent)
            .map_err(|_| InstallError::DurabilityUnknown)
    }
}

/// Exclusively created private entry, cleaned on abort until rename consumes it.
struct Scratch<'a> {
    parent: &'a File,
    name: String,
    file: File,
    published: bool,
}

impl<'a> Scratch<'a> {
    /// Create an unpredictable staging name and enforce exact mode on its fd.
    fn create(parent: &'a File) -> Result<Self, InstallError> {
        let mut entropy = [0; 16];
        SystemRandom::new()
            .fill(&mut entropy)
            .map_err(|_| InstallError::Io)?;
        let name = format!(".immutable-{:032x}", u128::from_be_bytes(entropy));
        let permissions = Mode::RUSR | Mode::WUSR;
        let file = File::from(
            fs::openat(
                parent,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                permissions,
            )
            .map_err(|_| InstallError::Io)?,
        );
        let scratch = Self {
            parent,
            name,
            file,
            published: false,
        };
        fs::fchmod(&scratch.file, permissions).map_err(|_| InstallError::Io)?;
        Ok(scratch)
    }

    /// Confirm that the scratch basename still identifies our created inode.
    fn check_name(&self) -> Result<(), CaptureError> {
        let current = File::from(
            fs::openat(self.parent, self.name.as_str(), PATH_FLAGS, Mode::empty())
                .map_err(open_error)?,
        );
        check_inode(&self.file, &current)
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        if !self.published && self.check_name().is_ok() {
            let _ = fs::unlinkat(self.parent, self.name.as_str(), AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests;
