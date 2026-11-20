//! Read-only access to the host's systemd-delivered key material.

use std::{
    fmt,
    fs::{File, Metadata},
    io::{ErrorKind, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

use rustix::{fs::CWD, process::geteuid};
use zeroize::Zeroizing;

use crate::working_files::{
    CaptureError, FileVersion, check_inode, check_version, directory_handle, path_handle,
    reopen_regular,
};

use super::{KeyId, SecretKey};

#[cfg(test)]
mod tests;

/// Failure to load the selected systemd credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyLoadError {
    /// The credential directory or file is absent.
    Missing,
    /// The credential path, metadata or material is unsafe or invalid.
    Invalid,
    /// The credential could not be accessed or read.
    Unavailable,
}

impl fmt::Display for KeyLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "systemd credential is missing",
            Self::Invalid => "systemd credential is invalid",
            Self::Unavailable => "systemd credential is unavailable",
        })
    }
}

impl std::error::Error for KeyLoadError {}

/// Loads exactly 32 raw bytes from the selected systemd credential.
///
/// The hosting unit supplies the protected namespace in
/// `CREDENTIALS_DIRECTORY`. The loader uses the fixed basename
/// `rpt-advanced.aead.<key_id>` and leaves the namespace intact.
pub fn load_systemd_key(key_id: &KeyId) -> Result<SecretKey, KeyLoadError> {
    let directory = std::env::var_os("CREDENTIALS_DIRECTORY");
    load_key(directory.as_deref().map(Path::new), key_id)
}

fn load_key(directory: Option<&Path>, key_id: &KeyId) -> Result<SecretKey, KeyLoadError> {
    load_key_with(directory, key_id, read_material)
}

/// Keep the inspected inode pinned through the read and its observed rechecks.
fn load_key_with(
    directory: Option<&Path>,
    key_id: &KeyId,
    read: impl FnOnce(&mut File) -> Result<Zeroizing<[u8; 32]>, KeyLoadError>,
) -> Result<SecretKey, KeyLoadError> {
    let directory = directory.ok_or(KeyLoadError::Missing)?;
    let uid = geteuid().as_raw();
    let parent = open_directory(directory, uid)?;
    let basename = format!("rpt-advanced.aead.{}", key_id.as_str());
    let basename = Path::new(&basename);

    let handle = path_handle(&parent, basename).map_err(capture_error)?;
    let metadata = protected_metadata(&handle, uid, 0o400, 0o640)?;
    if !metadata.is_file() {
        return Err(KeyLoadError::Invalid);
    }
    let expected = FileVersion::from(&metadata);
    let mut file = reopen_regular(&handle, &expected).map_err(capture_error)?;
    let material = read(&mut file)?;

    check_version(&file, &expected).map_err(capture_error)?;
    let current_parent = open_directory(directory, uid)?;
    check_inode(&parent, &current_parent).map_err(capture_error)?;
    let current = path_handle(&current_parent, basename).map_err(capture_error)?;
    check_version(&current, &expected).map_err(capture_error)?;
    Ok(SecretKey::from_material(key_id.clone(), material))
}

/// Traverse from the filesystem root with a no-follow descriptor per component.
fn open_directory(path: &Path, uid: u32) -> Result<File, KeyLoadError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(KeyLoadError::Invalid);
    }

    let mut directory = directory_handle(CWD, Path::new("/")).map_err(capture_error)?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            directory = directory_handle(&directory, Path::new(name)).map_err(capture_error)?;
        }
    }
    protected_metadata(&directory, uid, 0o500, 0o750)?;
    Ok(directory)
}

/// Accept systemd's group-class ACL mask and let the kernel enforce actual access.
fn protected_metadata(
    file: &File,
    uid: u32,
    required: u32,
    allowed: u32,
) -> Result<Metadata, KeyLoadError> {
    let metadata = file.metadata().map_err(|_| KeyLoadError::Unavailable)?;
    let permissions = metadata.mode() & 0o7777;
    let owner = metadata.uid();
    if (owner != 0 && owner != uid)
        || permissions & required != required
        || permissions & !allowed != 0
    {
        return Err(KeyLoadError::Invalid);
    }
    Ok(metadata)
}

fn capture_error(error: CaptureError) -> KeyLoadError {
    match error {
        CaptureError::Missing => KeyLoadError::Missing,
        CaptureError::UnsafeFile | CaptureError::Changed => KeyLoadError::Invalid,
        _ => KeyLoadError::Unavailable,
    }
}

/// Read a fixed key and use a separate zeroizing buffer to verify EOF.
fn read_material(input: &mut impl Read) -> Result<Zeroizing<[u8; 32]>, KeyLoadError> {
    let mut material = Zeroizing::new([0; 32]);
    input.read_exact(&mut material[..]).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            KeyLoadError::Invalid
        } else {
            KeyLoadError::Unavailable
        }
    })?;

    let mut probe = Zeroizing::new([0; 1]);
    loop {
        match input.read(&mut probe[..]) {
            Ok(0) => return Ok(material),
            Ok(_) => return Err(KeyLoadError::Invalid),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(KeyLoadError::Unavailable),
        }
    }
}
