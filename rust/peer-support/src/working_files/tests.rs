//! Exercise guarded capture against real protected Linux fixtures.

use super::*;
use rustix::{
    fs::{AtFlags, CWD, Mode, mkfifoat},
    process::{Uid, geteuid},
};
use std::{
    env, fs,
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
        net::UnixListener,
    },
    panic::catch_unwind,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 8,
    max_path_bytes: 64,
    max_file_bytes: 128,
    max_total_bytes: 512,
};

/// Temporary parent and provisioned store, removed on unwind as well.
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let parent = env::temp_dir().join(format!(
            "rptadv-working-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        directory(&parent);
        let root = parent.join("store");
        directory(&root);
        directory(&root.join("working"));
        write(&root.join("edit.lock"), b"existing lock");
        Self { parent, root }
    }

    fn open(&self) -> WorkingFiles {
        WorkingFiles::open(&self.root).unwrap()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join("working").join(relative)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.parent).expect("remove isolated working-files fixture");
    }
}

fn directory(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    mode(path, 0o700);
}

fn mode(path: &Path, permissions: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(permissions)).unwrap();
}

fn write(path: &Path, bytes: &[u8]) {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
    mode(path, 0o600);
}

fn required(relative: &str) -> CapturePath {
    CapturePath::new(relative, FileRequirement::Required).unwrap()
}

fn optional(relative: &str) -> CapturePath {
    CapturePath::new(relative, FileRequirement::Optional).unwrap()
}

#[test]
fn captures_exact_owned_bytes_with_value_free_debug() {
    let fixture = Fixture::new();
    let relative = "selected-path-value";
    let payload = b"opaque-file-value\0\xff\r\n";
    let file = fixture.path(relative);
    write(&file, payload);
    let files = fixture.open();
    let path = required(relative);
    let guard = files.try_edit().unwrap();
    let snapshot = guard.capture(&[path], LIMITS).unwrap();
    assert_eq!(snapshot.file(Path::new(relative)), Some(payload.as_slice()));
    assert_eq!(snapshot.file(Path::new("absent")), None);
    assert_eq!(snapshot.file(Path::new("./selected-path-value")), None);
    guard.verify_current(&snapshot).unwrap();
    let debug = format!(
        "{files:?} {guard:?} {:?} {snapshot:?} {:?}",
        required(relative),
        snapshot.fingerprint()
    );
    assert!(!debug.contains(relative));
    assert!(!debug.contains("opaque-file-value"));
    assert!(!debug.contains(fixture.root.to_str().unwrap()));
    assert!(!debug.contains(&format!("{:?}", snapshot.fingerprint().as_bytes())));
    drop(guard);

    write(&file, b"changed on disk");
    assert_eq!(snapshot.file(Path::new(relative)), Some(payload.as_slice()));
    assert_eq!(
        fs::read(fixture.root.join("edit.lock")).unwrap(),
        b"existing lock"
    );
}

#[test]
fn fingerprints_include_only_sorted_selected_presence_and_bytes() {
    let left = Fixture::new();
    let right = Fixture::new();
    for fixture in [&left, &right] {
        write(&fixture.path("a"), b"one");
        write(&fixture.path("b"), b"two");
    }
    write(&right.path("unselected"), b"extra bytes");
    let left_files = left.open();
    let right_files = right.open();
    let left_guard = left_files.try_edit().unwrap();
    let right_guard = right_files.try_edit().unwrap();
    let selected = [required("a"), optional("b"), optional("c")];
    let fingerprint = |guard: &EditGuard<'_>, paths: &[CapturePath]| {
        guard.capture(paths, LIMITS).unwrap().fingerprint()
    };
    let original = fingerprint(&left_guard, &selected);
    assert_eq!(
        original,
        fingerprint(&right_guard, &[optional("c"), optional("b"), required("a")])
    );
    assert_ne!(
        original,
        fingerprint(&left_guard, &[required("a"), optional("b")])
    );
    assert_ne!(
        fingerprint(&left_guard, &[]),
        fingerprint(&left_guard, &[optional("c")])
    );
    for (operation, expected_same) in [
        ("edit", false),
        ("restore", true),
        ("add empty", false),
        ("remove added", true),
        ("rename", false),
        ("restore name", true),
        ("remove existing", false),
    ] {
        match operation {
            "edit" => write(&left.path("a"), b"won"),
            "restore" => write(&left.path("a"), b"one"),
            "add empty" => write(&left.path("c"), b""),
            "remove added" => fs::remove_file(left.path("c")).unwrap(),
            "rename" => fs::rename(left.path("b"), left.path("c")).unwrap(),
            "restore name" => fs::rename(left.path("c"), left.path("b")).unwrap(),
            "remove existing" => fs::remove_file(left.path("b")).unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(
            fingerprint(&left_guard, &selected) == original,
            expected_same,
            "{operation}"
        );
    }
}

#[test]
fn independent_opens_are_busy_until_drop_including_unwind() {
    let fixture = Fixture::new();
    let first = fixture.open();
    let second = fixture.open();
    let guard = first.try_edit().unwrap();
    assert_eq!(first.try_edit().unwrap_err(), CaptureError::Busy);
    assert_eq!(second.try_edit().unwrap_err(), CaptureError::Busy);
    drop(guard);
    drop(second.try_edit().unwrap());

    let result = catch_unwind(|| {
        let _guard = first.try_edit().unwrap();
        assert_eq!(second.try_edit().unwrap_err(), CaptureError::Busy);
        std::panic::panic_any(42_u8);
    });
    assert_eq!(result.unwrap_err().downcast_ref::<u8>(), Some(&42));
    drop(second.try_edit().unwrap());
}

#[test]
fn lock_must_be_preprovisioned_and_held_inode_remain_current() {
    let fixture = Fixture::new();
    let lock = fixture.root.join("edit.lock");
    let files = fixture.open();
    fs::remove_file(&lock).unwrap();
    assert_eq!(files.try_edit().unwrap_err(), CaptureError::Missing);
    assert!(!lock.exists());
    let other = fixture.root.join("actual-lock");
    write(&other, b"original");
    symlink(&other, &lock).unwrap();
    assert_eq!(files.try_edit().unwrap_err(), CaptureError::UnsafeFile);
    fs::remove_file(&lock).unwrap();
    write(&lock, b"original");
    mode(&lock, 0o640);
    assert_eq!(files.try_edit().unwrap_err(), CaptureError::UnsafeFile);
    mode(&lock, 0o600);

    let guard = files.try_edit().unwrap();
    let snapshot = guard.capture(&[], LIMITS).unwrap();
    fs::rename(&lock, &other).unwrap();
    write(&lock, b"replacement");
    assert_eq!(
        guard.capture(&[], LIMITS).unwrap_err(),
        CaptureError::Changed
    );
    assert_eq!(guard.verify_current(&snapshot), Err(CaptureError::Changed));
    assert_eq!(fs::read(&lock).unwrap(), b"replacement");
    assert_eq!(fs::read(&other).unwrap(), b"original");
}

#[test]
fn root_and_working_directories_must_be_protected_and_symlink_free() {
    let fixture = Fixture::new();
    mode(&fixture.parent, 0o755);
    let files = fixture.open();
    let link = fixture.parent.join("store-alias");
    symlink(&fixture.root, &link).unwrap();
    assert_eq!(
        WorkingFiles::open(&link).unwrap_err(),
        CaptureError::UnsafeFile
    );
    let link = fixture.parent.join("ancestor-alias");
    symlink(&fixture.parent, &link).unwrap();
    assert_eq!(
        WorkingFiles::open(&link.join("store")).unwrap_err(),
        CaptureError::UnsafeFile
    );
    mode(&fixture.root, 0o750);
    assert_eq!(
        WorkingFiles::open(&fixture.root).unwrap_err(),
        CaptureError::UnsafeFile
    );
    mode(&fixture.root, 0o700);

    let guard = files.try_edit().unwrap();
    let working = fixture.root.join("working");
    mode(&working, 0o750);
    assert_eq!(
        guard.capture(&[], LIMITS).unwrap_err(),
        CaptureError::UnsafeFile
    );
    fs::remove_dir(&working).unwrap();
    assert_eq!(
        guard.capture(&[], LIMITS).unwrap_err(),
        CaptureError::Missing
    );
    let other = fixture.root.join("other-working");
    directory(&other);
    symlink(&other, &working).unwrap();
    assert_eq!(
        guard.capture(&[], LIMITS).unwrap_err(),
        CaptureError::UnsafeFile
    );
}

#[test]
fn optional_enoent_is_absence_but_symlinks_special_files_and_bad_modes_fail() {
    let fixture = Fixture::new();
    directory(&fixture.path("private-dir"));
    write(&fixture.path("private-dir/source"), b"untouched");
    directory(&fixture.path("dir-leaf"));
    symlink(fixture.path("private-dir"), fixture.path("dir-link")).unwrap();
    symlink(fixture.path("missing"), fixture.path("dangling")).unwrap();
    symlink(
        fixture.path("private-dir/source"),
        fixture.path("file-link"),
    )
    .unwrap();
    mkfifoat(CWD, fixture.path("fifo"), Mode::RUSR | Mode::WUSR).unwrap();
    let _socket = UnixListener::bind(fixture.path("socket")).unwrap();
    write(&fixture.path("mode-leaf"), b"protected value");
    mode(&fixture.path("mode-leaf"), 0o1600);
    directory(&fixture.path("mode-dir"));
    mode(&fixture.path("mode-dir"), 0o750);

    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    for relative in [
        "dir-leaf",
        "dir-link/source",
        "dangling/child",
        "dangling",
        "file-link",
        "fifo",
        "socket",
        "mode-leaf",
        "mode-dir/missing",
        "private-dir/source/child",
    ] {
        let error = guard.capture(&[optional(relative)], LIMITS).unwrap_err();
        assert_eq!(error, CaptureError::UnsafeFile, "{relative}");
        assert!(!format!("{error:?} {error}").contains(relative));
    }

    for missing in ["missing", "missing-dir/leaf"] {
        let snapshot = guard.capture(&[optional(missing)], LIMITS).unwrap();
        assert_eq!(snapshot.file(Path::new(missing)), None);
        assert_eq!(
            guard.capture(&[required(missing)], LIMITS).unwrap_err(),
            CaptureError::Missing
        );
    }
    assert_eq!(
        fs::read(fixture.path("private-dir/source")).unwrap(),
        b"untouched"
    );
}

#[test]
fn root_lock_directories_and_files_require_the_effective_service_owner() {
    // The constrained container can provision a wrong owner. Non-root native
    // runs exercise the same layout through the other tests.
    if !geteuid().is_root() {
        return;
    }
    let fixture = Fixture::new();
    let files = fixture.open();
    directory(&fixture.path("child"));
    write(&fixture.path("child/leaf"), b"foreign file");
    let change_owner = |path: &Path, uid| {
        rustix::fs::chownat(CWD, path, Some(uid), None, AtFlags::empty()).unwrap();
    };
    let uid = Uid::from_raw(65_534);
    let lock = fixture.root.join("edit.lock");

    change_owner(&fixture.root, uid);
    assert_eq!(
        WorkingFiles::open(&fixture.root).unwrap_err(),
        CaptureError::UnsafeFile
    );
    change_owner(&fixture.root, geteuid());
    change_owner(&lock, uid);
    assert_eq!(files.try_edit().unwrap_err(), CaptureError::UnsafeFile);
    change_owner(&lock, geteuid());

    let guard = files.try_edit().unwrap();
    for entry in ["child/leaf", "child"] {
        let path = fixture.path(entry);
        change_owner(&path, uid);
        assert_eq!(
            guard
                .capture(&[optional("child/leaf")], LIMITS)
                .unwrap_err(),
            CaptureError::UnsafeFile
        );
        change_owner(&path, geteuid());
    }
}

#[test]
fn rejects_lexical_aliases_and_duplicate_paths() {
    for relative in [
        "",
        "/",
        "/x",
        ".",
        "./x",
        "x/.",
        "x/./y",
        "x/../y",
        "../x",
        "x//y",
        "x/",
        "path\0value",
    ] {
        let error = CapturePath::new(relative, FileRequirement::Required).unwrap_err();
        assert_eq!(error, CaptureError::UnsafePath, "{relative:?}");
        assert!(!format!("{error:?} {error}").contains("path\0value"));
    }
    let fixture = Fixture::new();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    assert_eq!(
        guard
            .capture(&[required("same"), optional("same")], LIMITS)
            .unwrap_err(),
        CaptureError::UnsafePath
    );
    let name = std::ffi::OsStr::from_bytes(b"byte-\xff");
    write(&fixture.root.join("working").join(name), b"opaque name");
    let path = CapturePath::new(name, FileRequirement::Required).unwrap();
    let snapshot = guard.capture(&[path], LIMITS).unwrap();
    assert_eq!(
        snapshot.file(Path::new(name)),
        Some(b"opaque name".as_slice())
    );
}

#[test]
fn enforces_all_limits_at_and_past_boundaries() {
    let fixture = Fixture::new();
    write(&fixture.path("a"), b"1234");
    write(&fixture.path("bb"), b"567");
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let selected = [required("a"), required("bb"), optional("ccc")];
    let exact = CaptureLimits {
        max_paths: 3,
        max_path_bytes: 3,
        max_file_bytes: 4,
        max_total_bytes: 7,
    };
    let snapshot = guard.capture(&selected, exact).unwrap();
    guard.verify_current(&snapshot).unwrap();
    for limits in [
        CaptureLimits {
            max_paths: 2,
            ..exact
        },
        CaptureLimits {
            max_path_bytes: 2,
            ..exact
        },
        CaptureLimits {
            max_file_bytes: 3,
            ..exact
        },
        CaptureLimits {
            max_total_bytes: 6,
            ..exact
        },
    ] {
        assert_eq!(
            guard.capture(&selected, limits).unwrap_err(),
            CaptureError::Limits
        );
    }
    let zero_bytes = CaptureLimits {
        max_file_bytes: 0,
        max_total_bytes: 0,
        ..exact
    };
    write(&fixture.path("e"), b"");
    guard.capture(&[required("e")], zero_bytes).unwrap();
    assert_eq!(
        guard.capture(&[required("a")], zero_bytes).unwrap_err(),
        CaptureError::Limits
    );

    fs::rename(fixture.root.join("working"), fixture.root.join("displaced")).unwrap();
    assert_eq!(
        guard
            .capture(
                &selected,
                CaptureLimits {
                    max_paths: 2,
                    ..exact
                }
            )
            .unwrap_err(),
        CaptureError::Limits
    );
    assert_eq!(
        guard
            .capture(
                &selected,
                CaptureLimits {
                    max_path_bytes: 2,
                    ..exact
                }
            )
            .unwrap_err(),
        CaptureError::Limits
    );
}

#[test]
fn verify_current_reopens_selected_files_and_working_directory() {
    let fixture = Fixture::new();
    let file = fixture.path("a");
    write(&file, b"one");
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let selected = [required("a"), optional("b")];
    let snapshot = guard.capture(&selected, LIMITS).unwrap();
    write(&fixture.path("unselected"), b"unselected");
    guard.verify_current(&snapshot).unwrap();

    let replacement = fixture.path("new-a");
    write(&replacement, b"one");
    fs::rename(&replacement, &file).unwrap();
    assert_eq!(guard.verify_current(&snapshot), Err(CaptureError::Changed));
    assert_eq!(fs::read(&file).unwrap(), b"one");

    let snapshot = guard.capture(&selected, LIMITS).unwrap();
    let working = fixture.root.join("working");
    fs::rename(&working, fixture.root.join("working-old")).unwrap();
    directory(&working);
    write(&file, b"two");
    assert_eq!(guard.verify_current(&snapshot), Err(CaptureError::Changed));
    assert_eq!(snapshot.file(Path::new("a")), Some(b"one".as_slice()));
    assert_eq!(fs::read(&file).unwrap(), b"two");

    let snapshot = guard.capture(&selected, LIMITS).unwrap();
    fs::remove_file(&file).unwrap();
    assert_eq!(guard.verify_current(&snapshot), Err(CaptureError::Changed));
    assert!(!file.exists());
}
