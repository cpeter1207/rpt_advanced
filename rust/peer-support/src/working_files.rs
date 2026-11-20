//! Bounded, read-only capture of explicitly selected working files on Linux.
//!
//! Coordinated editors share the preprovisioned lock. Rechecking observed file
//! versions and bytes cannot eliminate the final race with an editor that
//! ignores it.

use ring::digest;
use rustix::{
    fd::AsFd,
    fs::{self, FlockOperation, Mode, OFlags},
    io::Errno,
    process::geteuid,
};
use std::{
    fmt,
    fs::{File, Metadata},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

const EDIT_LOCK: &str = "edit.lock";
const WORKING: &str = "working";
const PATH_FLAGS: OFlags = OFlags::PATH.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// Open descriptor for a trusted, service-owned configuration store.
pub struct WorkingFiles {
    root: File,
    uid: u32,
}

/// Exclusive cooperative edit lock released when the guard drops.
pub struct EditGuard<'a> {
    store: &'a WorkingFiles,
    lock: File,
}

/// Presence policy for a selected working file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRequirement {
    /// The selected file must exist.
    Required,
    /// A missing file contributes an explicit absence marker.
    Optional,
}

/// Canonical relative path selected by a trusted caller.
pub struct CapturePath {
    relative: PathBuf,
    requirement: FileRequirement,
}

/// Caller-supplied bounds applied independently to each read pass.
#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    /// Maximum number of requested paths, including absent optional paths.
    pub max_paths: usize,
    /// Maximum byte length of a requested relative path.
    pub max_path_bytes: usize,
    /// Maximum bytes read from one working file.
    pub max_file_bytes: u64,
    /// Maximum total file bytes in the selection.
    pub max_total_bytes: u64,
}

/// Owned bytes and fingerprint for the captured selection.
pub struct WorkingSnapshot {
    entries: Vec<Entry>,
    limits: CaptureLimits,
    fingerprint: Fingerprint,
}

/// Content digest for the selected paths, including explicit absence.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Fingerprint([u8; 32]);

impl fmt::Debug for WorkingFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkingFiles(..)")
    }
}

impl fmt::Debug for EditGuard<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EditGuard(..)")
    }
}

impl fmt::Debug for CapturePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapturePath(..)")
    }
}

impl fmt::Debug for WorkingSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkingSnapshot(..)")
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fingerprint(..)")
    }
}

impl Fingerprint {
    /// Access the version-framed SHA-256 digest as bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Value-free failure from protected working-file operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureError {
    /// Another guard holds the edit lock.
    Busy,
    /// The lock or selected working data changed during capture or recheck.
    Changed,
    /// The selection exceeded a caller-supplied capture limit.
    Limits,
    /// A selected path is noncanonical or appears more than once.
    UnsafePath,
    /// An opened path does not match the protected store layout.
    UnsafeFile,
    /// A required directory, lock, or working file is absent.
    Missing,
    /// The filesystem operation failed.
    Io,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "working-file lock is busy",
            Self::Changed => "working files or edit lock changed",
            Self::Limits => "working-file capture limit exceeded",
            Self::UnsafePath => "invalid working-file selection",
            Self::UnsafeFile => "unsafe working-file layout",
            Self::Missing => "required working-file entry is missing",
            Self::Io => "working-file I/O failed",
        })
    }
}

impl std::error::Error for CaptureError {}

impl WorkingFiles {
    /// Open a trusted configuration root without following any symlink.
    pub fn open(store_root: &Path) -> Result<Self, CaptureError> {
        if store_root.as_os_str().is_empty() {
            return Err(CaptureError::UnsafePath);
        }
        let start = if store_root.is_absolute() { "/" } else { "." };
        let mut root = directory_handle(fs::CWD, Path::new(start))?;
        for part in store_root.components() {
            match part {
                Component::Normal(name) => root = directory_handle(&root, Path::new(name))?,
                Component::ParentDir => root = directory_handle(&root, Path::new(".."))?,
                _ => {}
            }
        }
        let uid = geteuid().as_raw();
        checked_metadata(&root, uid, 0o700)?;
        Ok(Self { root, uid })
    }

    /// Lock the existing regular edit file exclusively without waiting.
    pub fn try_edit(&self) -> Result<EditGuard<'_>, CaptureError> {
        checked_metadata(&self.root, self.uid, 0o700)?;
        let (handle, metadata) = file_handle(&self.root, Path::new(EDIT_LOCK), self.uid)?;
        let lock = reopen_regular(&handle, &FileVersion::from(&metadata))?;
        fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == Errno::WOULDBLOCK {
                CaptureError::Busy
            } else {
                CaptureError::Io
            }
        })?;
        let guard = EditGuard { store: self, lock };
        guard.check_lock()?;
        Ok(guard)
    }

    /// Open working/ from the anchored store for a new read pass.
    fn open_working(&self) -> Result<File, CaptureError> {
        protected_directory(&self.root, Path::new(WORKING), self.uid)
    }
}

impl CapturePath {
    /// Validate a byte-exact relative path before retaining it.
    pub fn new(
        relative: impl AsRef<Path>,
        requirement: FileRequirement,
    ) -> Result<Self, CaptureError> {
        let relative = relative.as_ref();
        if path_bytes(relative)
            .split(|byte| *byte == b'/')
            .any(|part| matches!(part, b"" | b"." | b"..") || part.contains(&0))
        {
            return Err(CaptureError::UnsafePath);
        }
        Ok(Self {
            relative: relative.to_path_buf(),
            requirement,
        })
    }

    /// Canonical selected path, relative to the working directory.
    pub fn relative(&self) -> &Path {
        &self.relative
    }
}

impl EditGuard<'_> {
    /// Capture only the selected paths and recheck them before returning bytes.
    pub fn capture(
        &self,
        paths: &[CapturePath],
        limits: CaptureLimits,
    ) -> Result<WorkingSnapshot, CaptureError> {
        if paths.len() > limits.max_paths
            || paths
                .iter()
                .any(|path| path_bytes(path.relative()).len() > limits.max_path_bytes)
        {
            return Err(CaptureError::Limits);
        }
        let mut paths: Vec<_> = paths.iter().collect();
        paths.sort_unstable_by_key(|path| path_bytes(path.relative()));
        if paths
            .windows(2)
            .any(|pair| pair[0].relative() == pair[1].relative())
        {
            return Err(CaptureError::UnsafePath);
        }

        self.check_lock()?;
        let working = self.store.open_working()?;
        let mut remaining = limits.max_total_bytes;
        let entries = paths
            .into_iter()
            .map(|path| {
                let data = read_selected(&working, path, self.store.uid, limits, &mut remaining)?;
                Ok(Entry {
                    path: CapturePath {
                        relative: path.relative.clone(),
                        requirement: path.requirement,
                    },
                    data,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = WorkingSnapshot {
            fingerprint: fingerprint(&entries),
            entries,
            limits,
        };
        self.verify_current(&snapshot)?;
        Ok(snapshot)
    }

    /// Reopen working/ and check the captured selection under a held lock.
    pub fn verify_current(&self, snapshot: &WorkingSnapshot) -> Result<(), CaptureError> {
        self.check_lock()?;
        let working = self.store.open_working().map_err(on_recheck)?;
        let mut remaining = snapshot.limits.max_total_bytes;
        for entry in &snapshot.entries {
            let current = read_selected(
                &working,
                &entry.path,
                self.store.uid,
                snapshot.limits,
                &mut remaining,
            )
            .map_err(on_recheck)?;
            if current != entry.data {
                return Err(CaptureError::Changed);
            }
        }
        let current_working = self.store.open_working().map_err(on_recheck)?;
        check_inode(&working, &current_working)?;
        self.check_lock()
    }

    /// Confirm the preprovisioned path still denotes the held lock.
    fn check_lock(&self) -> Result<(), CaptureError> {
        checked_metadata(&self.store.root, self.store.uid, 0o700).map_err(on_recheck)?;
        let (current, _) = file_handle(&self.store.root, Path::new(EDIT_LOCK), self.store.uid)
            .map_err(on_recheck)?;
        check_inode(&self.lock, &current)
    }
}

impl WorkingSnapshot {
    /// Borrow the exact bytes of a present, selected relative path.
    pub fn file(&self, relative: &Path) -> Option<&[u8]> {
        let index = self
            .entries
            .binary_search_by(|entry| path_bytes(entry.path.relative()).cmp(path_bytes(relative)))
            .ok()?;
        self.entries[index]
            .data
            .as_ref()
            .map(|data| data.bytes.as_slice())
    }

    /// Return the content digest for the captured selection.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }
}

/// Selected path and optional captured file.
struct Entry {
    path: CapturePath,
    data: Option<CapturedFile>,
}

/// File version alongside exact bytes for rechecking.
#[derive(PartialEq, Eq)]
struct CapturedFile {
    version: FileVersion,
    bytes: Vec<u8>,
}

/// Observable file identity and version, excluding read-induced access time.
#[derive(PartialEq, Eq)]
struct FileVersion {
    identity: (u64, u64),
    mode: u32,
    uid: u32,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl From<&Metadata> for FileVersion {
    fn from(metadata: &Metadata) -> Self {
        Self {
            identity: (metadata.dev(), metadata.ino()),
            mode: metadata.mode(),
            uid: metadata.uid(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

/// Raw Linux path bytes preserve exact spelling and non-UTF-8 names.
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

/// Interpret absence only from an actual missing path component.
fn open_error(error: Errno) -> CaptureError {
    match error {
        Errno::NOENT => CaptureError::Missing,
        Errno::LOOP | Errno::NOTDIR => CaptureError::UnsafeFile,
        _ => CaptureError::Io,
    }
}

/// Translate observed invalidation of an already captured selection.
fn on_recheck(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::Missing | CaptureError::UnsafeFile | CaptureError::Limits => {
            CaptureError::Changed
        }
        _ => error,
    }
}

/// Open a directory descriptor without following its basename.
fn directory_handle(parent: impl AsFd, name: &Path) -> Result<File, CaptureError> {
    fs::openat(parent, name, PATH_FLAGS | OFlags::DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(open_error)
}

/// Check service ownership and exact permissions on an open descriptor.
fn checked_metadata(file: &File, uid: u32, permissions: u32) -> Result<Metadata, CaptureError> {
    let metadata = file.metadata().map_err(|_| CaptureError::Io)?;
    if metadata.uid() != uid || metadata.mode() & 0o7777 != permissions {
        return Err(CaptureError::UnsafeFile);
    }
    Ok(metadata)
}

/// Open and check a protected child directory.
fn protected_directory(parent: &File, name: &Path, uid: u32) -> Result<File, CaptureError> {
    let directory = directory_handle(parent, name)?;
    checked_metadata(&directory, uid, 0o700)?;
    Ok(directory)
}

/// Identify a protected regular leaf without opening it for I/O.
fn file_handle(parent: &File, name: &Path, uid: u32) -> Result<(File, Metadata), CaptureError> {
    let file = File::from(fs::openat(parent, name, PATH_FLAGS, Mode::empty()).map_err(open_error)?);
    let metadata = checked_metadata(&file, uid, 0o600)?;
    if !metadata.is_file() {
        return Err(CaptureError::UnsafeFile);
    }
    Ok((file, metadata))
}

/// Reopen the pinned regular inode for reading through Linux procfs.
fn reopen_regular(handle: &File, version: &FileVersion) -> Result<File, CaptureError> {
    let file = File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))
        .map_err(|_| CaptureError::Io)?;
    check_version(&file, version)?;
    Ok(file)
}

/// Check that two open descriptors refer to the same inode.
fn check_inode(original: &File, current: &File) -> Result<(), CaptureError> {
    let original = original.metadata().map_err(|_| CaptureError::Io)?;
    let current = current.metadata().map_err(|_| CaptureError::Io)?;
    if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
        return Err(CaptureError::Changed);
    }
    Ok(())
}

/// Detect an observed inode replacement, metadata change, or concurrent write.
fn check_version(file: &File, expected: &FileVersion) -> Result<(), CaptureError> {
    let current = file.metadata().map_err(|_| CaptureError::Io)?;
    if &FileVersion::from(&current) != expected {
        return Err(CaptureError::Changed);
    }
    Ok(())
}

/// Resolve validated intermediate components and precheck the selected leaf.
fn selected_handle(
    working: &File,
    path: &Path,
    uid: u32,
) -> Result<(File, Metadata), CaptureError> {
    let mut parts = path.components();
    let leaf = parts.next_back().expect("validated path has a leaf");
    let mut current = None;
    for part in parts {
        current = Some(protected_directory(
            current.as_ref().unwrap_or(working),
            Path::new(part.as_os_str()),
            uid,
        )?);
    }
    file_handle(
        current.as_ref().unwrap_or(working),
        Path::new(leaf.as_os_str()),
        uid,
    )
}

/// Capture one path, accounting for actual bytes against the pass-wide budget.
fn read_selected(
    working: &File,
    path: &CapturePath,
    uid: u32,
    limits: CaptureLimits,
    remaining: &mut u64,
) -> Result<Option<CapturedFile>, CaptureError> {
    let (handle, metadata) = match selected_handle(working, path.relative(), uid) {
        Err(CaptureError::Missing) if path.requirement == FileRequirement::Optional => {
            return Ok(None);
        }
        other => other?,
    };
    let limit = limits.max_file_bytes.min(*remaining);
    if metadata.len() > limit {
        return Err(CaptureError::Limits);
    }

    let version = FileVersion::from(&metadata);
    let file = reopen_regular(&handle, &version)?;
    let mut reader = file.take(limit.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::Io)?;
    check_version(reader.get_ref(), &version)?;
    let size = bytes.len() as u64;
    if size > limit {
        return Err(CaptureError::Limits);
    }
    *remaining -= size;
    Ok(Some(CapturedFile { version, bytes }))
}

/// Frame the sorted selection using path lengths, presence tags, and SHA-256.
fn fingerprint(entries: &[Entry]) -> Fingerprint {
    let mut digest = digest::Context::new(&digest::SHA256);
    digest.update(b"rptadv-working-files\0v1\0");
    digest.update(&(entries.len() as u64).to_be_bytes());
    for entry in entries {
        let path = path_bytes(entry.path.relative());
        digest.update(&(path.len() as u64).to_be_bytes());
        digest.update(path);
        match &entry.data {
            Some(data) => {
                digest.update(&[1]);
                digest.update(digest::digest(&digest::SHA256, &data.bytes).as_ref());
            }
            None => digest.update(&[0]),
        }
    }
    let mut value = [0; 32];
    value.copy_from_slice(digest.finish().as_ref());
    Fingerprint(value)
}

#[cfg(test)]
mod tests;
