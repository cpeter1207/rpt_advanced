//! Writer-only provisioning and compare-and-publish operations under the stable edit lock.
use super::immutable::{
    ArtifactLimits, ImmutableTarget, InstallError, ReadError, Scratch, artifact_sha256,
};
use super::*;
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::{AtFlags, Dir, RenameFlags};
use std::{io::Write, sync::atomic::Ordering};
use uuid::{Builder, Uuid};

/// Allocate a fresh version-4 identity for a newly created resource.
pub fn new_identity() -> Result<Uuid, CaptureError> {
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CaptureError::Io)?;
    Ok(Builder::from_random_bytes(bytes).into_uuid())
}

const DIRECTORY: Mode = Mode::RWXU;
const FILE: Mode = Mode::RUSR.union(Mode::WUSR);

/// Open an ordinary descriptor for syncing or enumerating a pinned directory.
pub(super) fn readable(dir: &File) -> Result<File, CaptureError> {
    fs::openat(
        dir,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(open_error)
}

fn sync_directory(dir: &File) -> Result<(), CaptureError> {
    readable(dir)?.sync_all().map_err(|_| CaptureError::Io)
}

/// Synchronize newly created directories, including their link in the parent.
/// Rechecking existing parents also makes retries after uncertain mkdir durable.
fn ensure_dir(parent: &File, name: &Path, uid: u32) -> Result<File, CaptureError> {
    match fs::mkdirat(parent, name, DIRECTORY) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(err) => return Err(open_error(err)),
    }
    let child = protected_directory(parent, name, uid)?;
    sync_directory(&child)?;
    sync_directory(parent)?;
    Ok(child)
}

const NAMESPACES: [&str; 5] = ["objects", "manifests", "journals", "private", WORKING];

fn marker(id: Uuid) -> String {
    format!(".bootstrap-{id}")
}

impl WorkingFiles {
    /// Initialize an empty store or explicitly resume its matching, unfinished UUID.
    pub fn provision(path: &Path, id: Uuid) -> Result<Self, CaptureError> {
        Self::provision_with(path, id, || Ok(()))
    }
    fn provision_with(
        path: &Path,
        id: Uuid,
        mut durable_component: impl FnMut() -> Result<(), CaptureError>,
    ) -> Result<Self, CaptureError> {
        let mut store = Self::open(path)?;
        let root = readable(&store.root)?;
        fs::flock(&root, FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| CaptureError::Busy)?;
        let marker = marker(id);
        let mut marked = false;
        let mut empty = true;
        for entry in Dir::read_from(&root).map_err(open_error)? {
            let entry = entry.map_err(open_error)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| CaptureError::UnsafeFile)?;
            if matches!(name, "." | "..") {
                continue;
            }
            empty = false;
            let path = Path::new(name);
            if name == marker {
                let dir = protected_directory(&root, path, store.uid)?;
                for entry in Dir::read_from(readable(&dir)?).map_err(open_error)? {
                    if !matches!(
                        entry.map_err(open_error)?.file_name().to_bytes(),
                        b"." | b".."
                    ) {
                        return Err(CaptureError::UnsafeFile);
                    }
                }
                marked = true;
            } else if NAMESPACES.contains(&name) {
                protected_directory(&root, path, store.uid)?;
            } else if name == EDIT_LOCK
                || name.strip_prefix(".immutable-").is_some_and(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
                })
            {
                file_handle(&root, path, store.uid)?;
            } else {
                return Err(CaptureError::UnsafeFile);
            }
        }
        if !marked && !empty {
            return Err(CaptureError::UnsafeFile);
        }
        ensure_dir(&root, Path::new(&marker), store.uid)?;
        durable_component()?;
        match fs::openat(
            &root,
            EDIT_LOCK,
            OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::RDWR,
            FILE,
        ) {
            Ok(fd) => {
                fs::fchmod(&fd, FILE).map_err(open_error)?;
            }
            Err(Errno::EXIST) => {}
            Err(e) => return Err(open_error(e)),
        }
        let guard = store.try_edit()?;
        guard.lock.sync_all().map_err(|_| CaptureError::Io)?;
        sync_directory(&root)?;
        for name in NAMESPACES {
            guard.check_lock()?;
            ensure_dir(&root, Path::new(name), store.uid)?;
            durable_component()?;
        }
        drop(guard);
        store.bootstrap = Some(id);
        Ok(store)
    }
}

impl EditGuard<'_> {
    /// Identity granted by explicit provisioning and still backed by its durable marker.
    pub fn bootstrap_identity(&self) -> Result<Option<Uuid>, CaptureError> {
        let Some(id) = self.store.bootstrap else {
            return Ok(None);
        };
        self.check_lock()?;
        match protected_directory(&self.store.root, Path::new(&marker(id)), self.store.uid) {
            Ok(_) => Ok(Some(id)),
            Err(CaptureError::Missing) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn retire_marker(&self, id: Uuid) -> Result<(), CaptureError> {
        let name = marker(id);
        let path = Path::new(&name);
        match protected_directory(&self.store.root, path, self.store.uid) {
            Ok(dir) => {
                self.check_lock()?;
                check_inode(
                    &dir,
                    &protected_directory(&self.store.root, path, self.store.uid)?,
                )?;
                fs::unlinkat(&self.store.root, path, AtFlags::REMOVEDIR).map_err(open_error)?;
                sync_directory(&self.store.root)
            }
            Err(CaptureError::Missing) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Establish durability of the exact visible selector before replay or a clean acknowledgment.
    pub fn confirm_state(
        &self,
        expected: &[u8; 32],
        store: Uuid,
        limit: u64,
        before_root_sync: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        self.check_lock()?;
        let path = Path::new("state.json");
        let (file, meta) = file_handle(&self.store.root, path, self.store.uid)?;
        let captured = read_pinned_regular(&file, &meta, limit)?;
        if artifact_sha256(&captured.bytes) != *expected {
            return Err(InstallError::Conflict);
        }
        reopen_regular(&file, &captured.version)?
            .sync_all()
            .map_err(|_| InstallError::DurabilityUnknown)?;
        before_root_sync().map_err(|_| InstallError::DurabilityUnknown)?;
        sync_directory(&self.store.root).map_err(|_| InstallError::DurabilityUnknown)?;
        let (current, _) = file_handle(&self.store.root, path, self.store.uid)?;
        check_version(&current, &captured.version)?;
        self.check_lock()?;
        self.retire_marker(store)
            .map_err(|_| InstallError::DurabilityUnknown)
    }

    /// Bookkeeping failure on this open store inhibits subsequent mutations across edit guards.
    pub fn bookkeeping_fault(&self) -> bool {
        self.store.bookkeeping_fault.load(Ordering::Acquire)
    }

    /// Record whether the owner's last durable activation bookkeeping attempt failed.
    pub fn set_bookkeeping_fault(&self, failed: bool) {
        self.store
            .bookkeeping_fault
            .store(failed, Ordering::Release);
    }

    /// Durably prepare one canonical public object shard under the held guard.
    pub fn prepare_object_parent(&self, id: Uuid) -> Result<(), CaptureError> {
        self.prepare_shard("objects", id)
    }

    /// Durably prepare the canonical private shard only for a sealed record from this store.
    pub fn prepare_private_parent(
        &self,
        store: Uuid,
        prepared: &super::immutable::private_records::PreparedPrivateRecord,
    ) -> Result<(), CaptureError> {
        let identity = prepared.identity();
        if identity.scope().store_uuid() != store {
            return Err(CaptureError::UnsafePath);
        }
        self.prepare_shard("private", identity.scope().record_uuid())
    }

    fn prepare_shard(&self, name: &str, id: Uuid) -> Result<(), CaptureError> {
        self.check_lock()?;
        let namespace = protected_directory(&self.store.root, Path::new(name), self.store.uid)?;
        ensure_dir(&namespace, Path::new(&id.to_string()), self.store.uid)?;
        check_inode(
            &namespace,
            &protected_directory(&self.store.root, Path::new(name), self.store.uid)?,
        )?;
        self.check_lock()
    }

    /// Install the immutable bounded journal for a typed transaction UUID.
    pub fn install_journal(
        &self,
        id: Uuid,
        bytes: &[u8],
        limits: ArtifactLimits,
    ) -> Result<[u8; 32], InstallError> {
        Ok(*self
            .install_immutable(&ImmutableTarget::journal(id)?, bytes, limits)?
            .sha256())
    }

    /// Read only the journal selected by identity and digest.
    pub fn read_journal(
        &self,
        id: Uuid,
        hash: &[u8; 32],
        limits: ArtifactLimits,
    ) -> Result<Vec<u8>, ReadError> {
        let target = ImmutableTarget::journal(id).map_err(|_| CaptureError::UnsafePath)?;
        Ok(self.read_immutable(&target, hash, limits)?.bytes().to_vec())
    }

    /// Bounded read of a protected current pointer. Only ENOENT means absent.
    pub fn read_state(&self, limit: u64) -> Result<Option<Vec<u8>>, CaptureError> {
        self.check_lock()?;
        let bytes = self.read_leaf(&self.store.root, Path::new("state.json"), limit)?;
        self.check_lock()?;
        Ok(bytes)
    }

    fn read_leaf(
        &self,
        parent: &File,
        name: &Path,
        limit: u64,
    ) -> Result<Option<Vec<u8>>, CaptureError> {
        let (handle, metadata) = match file_handle(parent, name, self.store.uid) {
            Err(CaptureError::Missing) => return Ok(None),
            result => result?,
        };
        let data = read_pinned_regular(&handle, &metadata, limit)?;
        let (current, _) = file_handle(parent, name, self.store.uid).map_err(on_recheck)?;
        check_version(&current, &data.version)?;
        Ok(Some(data.bytes))
    }

    /// CAS state.json and synchronize its directory. A rename followed by a
    /// sync or validation error is always reported as durability unknown.
    pub fn replace_state(
        &self,
        expected: Option<&[u8; 32]>,
        bytes: &[u8],
        limit: u64,
    ) -> Result<(), InstallError> {
        self.replace_state_checked(expected, bytes, limit, || Ok(()), || Ok(()))
    }

    /// Stage the new selector before the product's final inventory recheck.
    pub fn replace_state_checked(
        &self,
        expected: Option<&[u8; 32]>,
        bytes: &[u8],
        limit: u64,
        recheck: impl FnOnce() -> Result<(), InstallError>,
        after_rename: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        if expected.is_none() && self.bootstrap_identity()?.is_none() {
            return Err(InstallError::Conflict);
        }
        if bytes.len() as u64 > limit {
            return Err(InstallError::Limits);
        }
        let root = readable(&self.store.root)?;
        let mut stage = stage(&root, bytes)?;
        recheck()?;
        stage.check_name()?;
        let current = self.read_state(limit)?;
        if current.as_deref().map(artifact_sha256).as_ref() != expected {
            return Err(InstallError::Conflict);
        }
        publish(
            &mut stage,
            &root,
            &root,
            Path::new("state.json"),
            expected.is_none(),
        )?;
        after_rename().map_err(|_| InstallError::DurabilityUnknown)?;
        root.sync_all()
            .map_err(|_| InstallError::DurabilityUnknown)?;
        self.check_lock()
            .map_err(|_| InstallError::DurabilityUnknown)?;
        let current = self
            .read_state(limit)
            .map_err(|_| InstallError::DurabilityUnknown)?;
        if current.as_deref() != Some(bytes) {
            return Err(InstallError::DurabilityUnknown);
        }
        if let Some(id) = self.store.bootstrap {
            self.retire_marker(id)
                .map_err(|_| InstallError::DurabilityUnknown)?;
        }
        Ok(())
    }

    /// Resolve only validated canonical path components; readers never mkdir.
    fn working_parent(&self, path: &CapturePath, create: bool) -> Result<File, CaptureError> {
        let mut parent = self.store.open_working()?;
        for component in path
            .relative()
            .parent()
            .ok_or(CaptureError::UnsafePath)?
            .components()
        {
            let name = Path::new(component.as_os_str());
            parent = if create {
                ensure_dir(&parent, name, self.store.uid)?
            } else {
                protected_directory(&parent, name, self.store.uid)?
            };
        }
        Ok(parent)
    }

    /// Retry one recorded, empty retired port directory. Never remove its contents.
    pub fn prune_directory(&self, current: &CapturePath) -> Result<(), InstallError> {
        self.check_lock()?;
        let parent = match self.working_parent(current, false) {
            Ok(dir) => dir,
            Err(CaptureError::Missing) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let name = current
            .relative()
            .file_name()
            .ok_or(InstallError::InvalidTarget)?
            .as_ref();
        match protected_directory(&parent, name, self.store.uid) {
            Ok(opened) => {
                check_inode(&parent, &self.working_parent(current, false)?)?;
                check_inode(
                    &opened,
                    &protected_directory(&parent, name, self.store.uid)?,
                )?;
                self.check_lock()?;
                fs::unlinkat(&parent, name, AtFlags::REMOVEDIR).map_err(|e| {
                    if e == Errno::NOTEMPTY {
                        InstallError::Conflict
                    } else {
                        open_error(e).into()
                    }
                })?;
            }
            Err(CaptureError::Missing) => {}
            Err(e) => return Err(e.into()),
        }
        sync_directory(&parent).map_err(|_| InstallError::DurabilityUnknown)
    }

    /// Publish verified immutable bytes or a removal only over their recorded
    /// source. Already-target retries sync the parent again.
    pub fn reconcile_file(
        &self,
        path: &CapturePath,
        source: Option<&[u8; 32]>,
        target: Option<&super::immutable::ImmutableArtifact>,
        limit: u64,
    ) -> Result<(), InstallError> {
        self.check_lock()?;
        if target.is_some_and(|t| !t.public_object) {
            return Err(InstallError::InvalidTarget);
        }
        let parent = match self.working_parent(path, target.is_some()) {
            Err(CaptureError::Missing) if target.is_none() => return Ok(()),
            result => result?,
        };
        let leaf = path
            .relative()
            .file_name()
            .ok_or(InstallError::InvalidTarget)?
            .as_ref();
        let root = readable(&self.store.root)?;
        let current_hash = || -> Result<Option<[u8; 32]>, CaptureError> {
            let current = self.read_leaf(&parent, leaf, limit)?;
            check_inode(&parent, &self.working_parent(path, false)?)?;
            self.check_lock()?;
            Ok(current.as_deref().map(artifact_sha256))
        };
        let target_hash = target.map(|artifact| artifact.sha256());
        let current = current_hash()?;
        if current.as_ref() != target_hash {
            if current.as_ref() != source {
                return Err(InstallError::Conflict);
            }
            // Stage outside working/ so a crash never imports a temp file.
            let mut scratch = target
                .map(|artifact| stage(&root, artifact.bytes()))
                .transpose()?;
            if current_hash()?.as_ref() != source {
                return Err(InstallError::Conflict);
            }
            if let Some(scratch) = &mut scratch {
                scratch.check_name()?;
                publish(scratch, &root, &parent, leaf, source.is_none())?;
            } else {
                fs::unlinkat(&parent, leaf, AtFlags::empty()).map_err(|_| InstallError::Io)?;
            }
        } else if target.is_some() {
            let (handle, metadata) = file_handle(&parent, leaf, self.store.uid)?;
            let version = FileVersion::from(&metadata);
            reopen_regular(&handle, &version)?
                .sync_all()
                .map_err(|_| InstallError::DurabilityUnknown)?;
            check_version(&handle, &version).map_err(|_| InstallError::DurabilityUnknown)?;
        }
        sync_directory(&parent).map_err(|_| InstallError::DurabilityUnknown)?;
        if current_hash()
            .map_err(|_| InstallError::DurabilityUnknown)?
            .as_ref()
            != target_hash
        {
            return Err(InstallError::DurabilityUnknown);
        }
        Ok(())
    }
}

fn stage<'a>(parent: &'a File, bytes: &[u8]) -> Result<Scratch<'a>, InstallError> {
    let mut scratch = Scratch::create(parent)?;
    scratch
        .file
        .write_all(bytes)
        .map_err(|_| InstallError::Io)?;
    scratch.file.sync_all().map_err(|_| InstallError::Io)?;
    Ok(scratch)
}

fn publish(
    scratch: &mut Scratch<'_>,
    from: &File,
    parent: &File,
    leaf: &Path,
    missing: bool,
) -> Result<(), InstallError> {
    let flags = if missing {
        RenameFlags::NOREPLACE
    } else {
        RenameFlags::empty()
    };
    fs::renameat_with(from, scratch.name.as_str(), parent, leaf, flags).map_err(|err| {
        if err == Errno::EXIST {
            InstallError::Conflict
        } else {
            InstallError::Io
        }
    })?;
    scratch.published = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Fixture, directory};
    use super::*;
    #[test]
    fn interrupted_layout_requires_the_original_marker_and_service_owned_parents() {
        for stop in [1, 3] {
            let f = Fixture::new();
            let path = f.root.join("bootstrap-test");
            directory(&path);
            let id = new_identity().unwrap();
            let mut n = 0;
            assert!(
                WorkingFiles::provision_with(&path, id, || {
                    n += 1;
                    if n == stop {
                        Err(CaptureError::Io)
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            assert!(WorkingFiles::provision(&path, new_identity().unwrap()).is_err());
            let resumed = WorkingFiles::provision(&path, id).unwrap();
            let guard = resumed.try_edit().unwrap();
            assert_eq!(guard.bootstrap_identity().unwrap(), Some(id));
            guard.replace_state(None, b"selected", 64).unwrap();
            assert_eq!(guard.bootstrap_identity().unwrap(), None);
            std::fs::remove_file(path.join("state.json")).unwrap();
            drop(guard);
            drop(resumed);
            assert!(WorkingFiles::provision(&path, id).is_err());
        }
    }
    #[test]
    fn rename_before_failed_sync_is_read_back_as_possibly_committed() {
        let fixture = Fixture::new();
        let path = fixture.root.join("initial");
        directory(&path);
        let store = WorkingFiles::provision(&path, Uuid::nil()).unwrap();
        let guard = store.try_edit().unwrap();
        assert_eq!(
            guard.replace_state_checked(None, b"selected", 64, || Ok(()), || Err(InstallError::Io)),
            Err(InstallError::DurabilityUnknown)
        );
        assert_eq!(guard.read_state(64).unwrap().unwrap(), b"selected");
        assert_eq!(
            guard.replace_state(None, b"other", 64),
            Err(InstallError::Conflict)
        );
        guard
            .replace_state(Some(&artifact_sha256(b"selected")), b"selected", 64)
            .unwrap();
    }
}
