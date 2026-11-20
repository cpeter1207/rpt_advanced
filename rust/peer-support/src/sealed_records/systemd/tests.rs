//! Exercise the systemd lookup boundary with isolated synthetic files.

use super::*;
use rustix::{
    fs::{AtFlags, CWD, Mode, chownat, mkfifoat},
    process::{Uid, geteuid},
};
use std::{
    env, fs,
    io::Cursor,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const MATERIAL: &[u8; 32] = b"\x00\xff0123456789abcdefghijklmnopqrs\n";
const BASENAME: &str = "rpt-advanced.aead.fixture-key_1";

struct Fixture {
    root: PathBuf,
    credentials: PathBuf,
    leaf: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = env::temp_dir().join(format!(
            "rsk-{:x}-{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let credentials = root.join("credentials");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&credentials)
            .unwrap();
        let leaf = credentials.join(BASENAME);
        Self {
            root,
            credentials,
            leaf,
        }
    }

    fn load(&self) -> Result<SecretKey, KeyLoadError> {
        load_key(Some(&self.credentials), &key_id())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.credentials).is_ok_and(|metadata| metadata.is_dir()) {
            mode(&self.credentials, 0o700);
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn mode(path: &Path, permissions: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(permissions)).unwrap();
}

fn write(path: &Path, bytes: &[u8]) {
    use std::io::Write;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn key_id() -> KeyId {
    KeyId::new("fixture-key_1").unwrap()
}

#[test]
fn missing_directory_or_selected_credential_stays_missing() {
    let fixture = Fixture::new();
    let unselected = fixture.credentials.join("rpt-advanced.aead.another-key");
    write(&unselected, MATERIAL);
    let absent_directory = fixture.root.join("absent");
    assert_eq!(
        load_key(None, &key_id()).unwrap_err(),
        KeyLoadError::Missing
    );
    assert_eq!(
        load_key(Some(&absent_directory), &key_id()).unwrap_err(),
        KeyLoadError::Missing
    );
    assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Missing);
    assert_eq!(fs::read_dir(&fixture.credentials).unwrap().count(), 1);
    assert!(fs::read(&unselected).unwrap().as_slice() == MATERIAL);
}

#[test]
fn rejects_invalid_directory_paths() {
    let fixture = Fixture::new();
    let parent_traversal = fixture.credentials.join("../credentials");
    let invalid_bytes = fixture.root.join("bad\0path");
    for directory in [
        Path::new(""),
        Path::new("relative"),
        &parent_traversal,
        &invalid_bytes,
    ] {
        assert_eq!(
            load_key(Some(directory), &key_id()).unwrap_err(),
            KeyLoadError::Invalid
        );
    }
}

#[test]
fn loads_raw_bytes_from_owner_only_and_acl_mask_modes() {
    let fixture = Fixture::new();
    write(&fixture.leaf, MATERIAL);
    for (directory_mode, leaf_mode) in [(0o500, 0o400), (0o550, 0o440), (0o750, 0o640)] {
        mode(&fixture.credentials, directory_mode);
        mode(&fixture.leaf, leaf_mode);
        let key = fixture.load().unwrap();
        assert_eq!(key.key_id(), &key_id());
        assert!(&*key.material == MATERIAL);
        assert_eq!(
            fs::metadata(&fixture.credentials)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            directory_mode
        );
        assert_eq!(
            fs::metadata(&fixture.leaf).unwrap().permissions().mode() & 0o7777,
            leaf_mode
        );
    }
}

#[test]
fn rejects_short_oversized_and_appended_newline_credentials() {
    let fixture = Fixture::new();
    let mut appended_newline = MATERIAL.to_vec();
    appended_newline.push(b'\n');
    for material in [
        &MATERIAL[..31],
        &[0_u8; 33],
        &[0_u8; 4096],
        &appended_newline,
    ] {
        write(&fixture.leaf, material);
        assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Invalid);
        assert!(fs::read(&fixture.leaf).unwrap().as_slice() == material);
        fs::remove_file(&fixture.leaf).unwrap();
    }
}

#[test]
fn probes_only_one_extra_input_byte() {
    let mut input = Cursor::new([0_u8; 64]);
    assert_eq!(read_material(&mut input).err(), Some(KeyLoadError::Invalid));
    assert_eq!(input.position(), 33);
}

#[test]
fn rejects_unsafe_directory_and_file_modes() {
    let fixture = Fixture::new();
    write(&fixture.leaf, MATERIAL);
    for (path, safe_mode, unsafe_modes) in [
        (
            &fixture.credentials,
            0o700,
            [0o300, 0o400, 0o701, 0o720, 0o1700],
        ),
        (&fixture.leaf, 0o600, [0o200, 0o700, 0o601, 0o620, 0o4600]),
    ] {
        for unsafe_mode in unsafe_modes {
            mode(path, unsafe_mode);
            assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Invalid);
        }
        mode(path, safe_mode);
    }
}

#[test]
fn rejects_foreign_owned_directory_and_file() {
    if !geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    write(&fixture.leaf, MATERIAL);
    let change_owner = |path: &Path, uid| {
        chownat(CWD, path, Some(uid), None, AtFlags::empty()).unwrap();
    };
    for path in [&fixture.credentials, &fixture.leaf] {
        change_owner(path, Uid::from_raw(65_534));
        assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Invalid);
        change_owner(path, geteuid());
    }
}

#[test]
fn rejects_supplied_directory_intermediate_and_leaf_symlinks() {
    let fixture = Fixture::new();
    let link = fixture.root.join("link");
    symlink(&fixture.credentials, &link).unwrap();
    assert_eq!(
        load_key(Some(&link), &key_id()).unwrap_err(),
        KeyLoadError::Invalid
    );
    fs::remove_file(&link).unwrap();
    let external_root = fixture.root.join("real");
    fs::create_dir(&external_root).unwrap();
    let external_credentials = external_root.join("credentials");
    fs::create_dir(&external_credentials).unwrap();
    mode(&external_credentials, 0o700);
    write(&external_credentials.join(BASENAME), MATERIAL);
    symlink(&external_root, &link).unwrap();
    assert_eq!(
        load_key(Some(&link.join("credentials")), &key_id()).unwrap_err(),
        KeyLoadError::Invalid
    );

    assert!(!fixture.leaf.exists());
    symlink(external_credentials.join(BASENAME), &fixture.leaf).unwrap();
    assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Invalid);
    fs::remove_file(&fixture.leaf).unwrap();
    symlink(fixture.root.join("does-not-exist"), &fixture.leaf).unwrap();
    assert_eq!(fixture.load().unwrap_err(), KeyLoadError::Invalid);
}

#[test]
fn rejects_fifo_and_directory_before_reading_material() {
    let fixture = Fixture::new();
    let load = || {
        load_key_with(Some(&fixture.credentials), &key_id(), |_| {
            panic!("nonregular entry reached the credential reader")
        })
    };
    mkfifoat(CWD, &fixture.leaf, Mode::RUSR).unwrap();
    assert_eq!(load().unwrap_err(), KeyLoadError::Invalid);
    fs::remove_file(&fixture.leaf).unwrap();

    fs::create_dir(&fixture.leaf).unwrap();
    assert_eq!(load().unwrap_err(), KeyLoadError::Invalid);
}

#[test]
fn rejects_a_replaced_leaf_or_credentials_directory_after_reading() {
    for replace_directory in [false, true] {
        let fixture = Fixture::new();
        write(&fixture.leaf, MATERIAL);
        let displaced = fixture.root.join("displaced");
        let replacement = fixture.root.join("replacement");
        let original_version = if replace_directory {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&replacement)
                .unwrap();
            // Link before loading so the directory swap preserves the leaf's version.
            fs::hard_link(&fixture.leaf, replacement.join(BASENAME)).unwrap();
            Some(FileVersion::from(&fs::metadata(&fixture.leaf).unwrap()))
        } else {
            None
        };

        let loaded = load_key_with(Some(&fixture.credentials), &key_id(), |file| {
            let material = read_material(file)?;
            if replace_directory {
                fs::rename(&fixture.credentials, &displaced).unwrap();
                fs::rename(&replacement, &fixture.credentials).unwrap();
            } else {
                fs::rename(&fixture.leaf, &displaced).unwrap();
                write(&fixture.leaf, MATERIAL);
            }
            Ok(material)
        });
        assert_eq!(loaded.unwrap_err(), KeyLoadError::Invalid);
        let original = if replace_directory {
            displaced.join(BASENAME)
        } else {
            displaced
        };
        for path in [&original, &fixture.leaf] {
            assert!(fs::read(path).unwrap().as_slice() == MATERIAL);
            if let Some(expected) = &original_version {
                assert!(&FileVersion::from(&fs::metadata(path).unwrap()) == expected);
            }
        }
    }
}
