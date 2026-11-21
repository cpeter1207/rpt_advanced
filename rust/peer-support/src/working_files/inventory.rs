//! Bounded discovery under a caller-owned versioned catalog.
use super::*;
use rustix::fs::Dir;
use std::{collections::BTreeMap, ffi::OsStr};

/// Allowed role of the next catalog component.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum CatalogEntry {
    /// Protected catalog directory.
    Directory,
    /// Protected regular resource file.
    File,
}

impl EditGuard<'_> {
    /// Enumerate every allowed file twice around its pinned read. An unknown
    /// entry, incorrect type, or changed inventory cannot yield a clean tree.
    pub fn inventory(
        &self,
        limits: CaptureLimits,
        catalog: impl Fn(&Path) -> Option<CatalogEntry>,
    ) -> Result<WorkingSnapshot, CaptureError> {
        let discover = || {
            self.check_lock()?;
            let root = self.store.open_working()?;
            let mut entries = Inventory::default();
            self.walk(&root, Path::new(""), limits, &catalog, &mut entries)?;
            entries.directories.sort_unstable();
            entries
                .paths
                .sort_unstable_by(|a, b| a.relative().cmp(b.relative()));
            check_inode(&root, &self.store.open_working()?)?;
            Ok::<_, CaptureError>(entries)
        };
        let first = discover()?;
        let mut snapshot = self.capture(&first.paths, limits)?;
        let current = discover()?;
        if first.directories != current.directories
            || !first
                .paths
                .iter()
                .map(CapturePath::relative)
                .eq(current.paths.iter().map(CapturePath::relative))
        {
            return Err(CaptureError::Changed);
        }
        self.verify_current(&snapshot)?;
        snapshot.directories = Some(first.directories);
        Ok(snapshot)
    }
    /// Synchronize every edited regular inode and directory entry in the captured baseline.
    /// The caller rechecks the complete inventory before recording a clean pointer.
    pub fn sync_inventory(
        &self,
        snapshot: &WorkingSnapshot,
        mut before_sync: impl FnMut(CatalogEntry) -> Result<(), immutable::InstallError>,
    ) -> Result<(), immutable::InstallError> {
        use immutable::InstallError;
        self.verify_current(snapshot)?;
        let working = self.store.open_working()?;
        for entry in &snapshot.entries {
            let data = entry.data.as_ref().ok_or(CaptureError::Changed)?;
            let (handle, _) = selected_handle(&working, entry.path.relative(), self.store.uid)?;
            let file = reopen_regular(&handle, &data.version)?;
            before_sync(CatalogEntry::File)?;
            file.sync_all()
                .map_err(|_| InstallError::DurabilityUnknown)?;
            check_version(&file, &data.version)?;
        }
        for path in snapshot
            .directories()
            .ok_or(CaptureError::UnsafePath)?
            .iter()
            .rev()
            .map(PathBuf::as_path)
            .chain([Path::new("")])
        {
            let open = || -> Result<File, CaptureError> {
                let mut current = self.store.open_working()?;
                for part in path.components() {
                    current =
                        protected_directory(&current, Path::new(part.as_os_str()), self.store.uid)?;
                }
                Ok(current)
            };
            let dir = open()?;
            self.check_lock()?;
            before_sync(CatalogEntry::Directory)?;
            transaction::readable(&dir)?
                .sync_all()
                .map_err(|_| InstallError::DurabilityUnknown)?;
            check_inode(&dir, &open()?)?;
        }
        check_inode(&working, &self.store.open_working()?)?;
        self.verify_current(snapshot)?;
        Ok(())
    }

    fn walk(
        &self,
        dir: &File,
        prefix: &Path,
        limits: CaptureLimits,
        catalog: &impl Fn(&Path) -> Option<CatalogEntry>,
        entries: &mut Inventory,
    ) -> Result<(), CaptureError> {
        let readable = transaction::readable(dir)?;
        for entry in Dir::read_from(&readable).map_err(open_error)? {
            let entry = entry.map_err(open_error)?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if entries.paths.len() + entries.directories.len() >= limits.max_paths {
                return Err(CaptureError::Limits);
            }
            let name = Path::new(OsStr::from_bytes(name));
            let path = prefix.join(name);
            if path_bytes(&path).len() > limits.max_path_bytes {
                return Err(CaptureError::Limits);
            }
            match catalog(&path).ok_or(CaptureError::UnsafePath)? {
                CatalogEntry::Directory => {
                    let child = protected_directory(dir, name, self.store.uid)?;
                    entries.directories.push(path.clone());
                    self.walk(&child, &path, limits, catalog, entries)?;
                    check_inode(&child, &protected_directory(dir, name, self.store.uid)?)?;
                }
                CatalogEntry::File => entries
                    .paths
                    .push(CapturePath::new(path, FileRequirement::Required)?),
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Inventory {
    paths: Vec<CapturePath>,
    directories: Vec<PathBuf>,
}

impl WorkingSnapshot {
    /// Directories counted by full discovery. A registered selection has no directory inventory.
    pub fn directories(&self) -> Option<&[PathBuf]> {
        self.directories.as_deref()
    }

    /// Iterate only present files in canonical byte-path order.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.entries.iter().filter_map(|entry| {
            Some((entry.path.relative(), entry.data.as_ref()?.bytes.as_slice()))
        })
    }
}

impl Fingerprint {
    /// Exact v1 digest of a complete present-file inventory. Missing files do
    /// not contribute a row; a present empty file contributes its content hash.
    pub fn full(hashes: &BTreeMap<String, [u8; 32]>) -> Self {
        fingerprint_rows(
            hashes.len(),
            hashes
                .iter()
                .map(|(path, hash)| (path.as_bytes(), Some(*hash))),
        )
    }
}
