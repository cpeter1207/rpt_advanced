//! Exercise installation and durability boundaries against real protected files.

use super::*;
use crate::working_files::{
    WorkingFiles,
    tests::{Fixture, directory, mode, write},
};
use ring::digest;
use rustix::{
    fs::{AtFlags, CWD, Mode, chownat, mkfifoat},
    process::{Uid, geteuid},
};
use std::{
    fs,
    os::unix::{
        fs::{MetadataExt, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

const UUID: &str = "abcd1234-1234-4abc-8abc-12345678abcd";
const UPPER_UUID: &str = "ABCD1234-1234-4ABC-8ABC-12345678ABCD";
const LIMITS: InstallLimits = InstallLimits {
    max_path_bytes: 128,
    max_file_bytes: 128,
};
const PAYLOAD: &[u8] = b"opaque-artifact\0\xff";

fn manifests(fixture: &Fixture) -> PathBuf {
    let path = fixture.root.join("manifests");
    directory(&path);
    path
}

fn objects(fixture: &Fixture) -> PathBuf {
    let path = fixture.root.join("objects");
    directory(&path);
    let resource = path.join(UUID);
    directory(&resource);
    resource
}

fn target() -> ImmutableTarget {
    ImmutableTarget::manifest("a").unwrap()
}

fn names(parent: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn installs_both_targets_with_exact_bytes_and_opaque_receipts() {
    let fixture = Fixture::new();
    let objects = objects(&fixture);
    let manifests = manifests(&fixture);
    directory(&fixture.root.join("private"));
    write(&fixture.root.join("private/input"), b"private fixture");
    write(&fixture.root.join("state.json"), b"pointer fixture");
    let working = fixture.root.join("working/input");
    write(&working, PAYLOAD);
    let files = fixture.open();
    let guard = files.try_edit().unwrap();

    for (target, parent, id) in [
        (
            ImmutableTarget::public_object(UPPER_UUID, " v\u{e9}rsion\\1 ").unwrap(),
            objects,
            " v\u{e9}rsion\\1 ",
        ),
        (
            ImmutableTarget::manifest("artifact-value").unwrap(),
            manifests,
            "artifact-value",
        ),
    ] {
        let receipt = guard.install_immutable(&target, PAYLOAD, LIMITS).unwrap();
        assert_eq!(receipt.outcome(), InstallOutcome::Created);
        assert_eq!(
            receipt.sha256().as_slice(),
            digest::digest(&digest::SHA256, PAYLOAD).as_ref()
        );
        assert_eq!(
            format!("{target:?} {receipt:?}"),
            "ImmutableTarget(..) InstalledArtifact(..)"
        );

        let leaf = format!("{id}.json");
        let final_path = parent.join(&leaf);
        let metadata = fs::metadata(&final_path).unwrap();
        assert_eq!(fs::read(final_path).unwrap(), PAYLOAD);
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.uid(), geteuid().as_raw());
        assert_eq!(metadata.nlink(), 1);
        assert_ne!(metadata.ino(), fs::metadata(&working).unwrap().ino());
        assert_eq!(names(&parent), [leaf]);
    }

    assert_eq!(
        names(&fixture.root),
        [
            "edit.lock",
            "manifests",
            "objects",
            "private",
            "state.json",
            "working"
        ]
    );
    assert_eq!(fs::read(working).unwrap(), PAYLOAD);
    assert_eq!(
        fs::read(fixture.root.join("edit.lock")).unwrap(),
        b"existing lock"
    );
    assert_eq!(
        fs::read(fixture.root.join("private/input")).unwrap(),
        b"private fixture"
    );
    assert_eq!(
        fs::read(fixture.root.join("state.json")).unwrap(),
        b"pointer fixture"
    );
}

#[test]
fn reuses_exact_bytes_and_preserves_different_files_and_missing_parents() {
    let fixture = Fixture::new();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let target = target();
    let object = ImmutableTarget::public_object(UUID, "version").unwrap();
    let missing = InstallError::Store(CaptureError::Missing);
    assert_eq!(
        guard
            .install_immutable(&target, b"old", LIMITS)
            .unwrap_err(),
        missing
    );
    assert_eq!(
        guard
            .install_immutable(&object, b"old", LIMITS)
            .unwrap_err(),
        missing
    );
    assert!(!fixture.root.join("manifests").exists());
    let objects = fixture.root.join("objects");
    assert!(!objects.exists());
    directory(&objects);
    assert_eq!(
        guard
            .install_immutable(&object, b"old", LIMITS)
            .unwrap_err(),
        missing
    );
    assert!(names(&objects).is_empty());

    let parent = manifests(&fixture);
    let path = parent.join("a.json");
    let created = guard.install_immutable(&target, b"old", LIMITS).unwrap();
    let original_inode = fs::metadata(&path).unwrap().ino();
    let reused = guard.install_immutable(&target, b"old", LIMITS).unwrap();
    assert_eq!(reused.outcome(), InstallOutcome::Reused);
    assert_eq!(reused.sha256(), created.sha256());
    for different in [b"new".as_slice(), b"next"] {
        assert_eq!(
            guard
                .install_immutable(&target, different, LIMITS)
                .unwrap_err(),
            InstallError::Conflict,
        );
    }
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert_eq!(fs::metadata(&path).unwrap().ino(), original_inode);
    assert_eq!(names(&parent), ["a.json"]);
}

#[test]
fn rejects_invalid_ids_and_protected_layout_violations() {
    for invalid in ["", ".", "..", "/path-value", "path/value", "path\0value"] {
        for result in [
            ImmutableTarget::manifest(invalid),
            ImmutableTarget::public_object(UUID, invalid),
        ] {
            assert_eq!(result.unwrap_err(), InstallError::InvalidTarget);
        }
    }
    let error = ImmutableTarget::public_object("uuid-input-value", "version").unwrap_err();
    assert_eq!(error, InstallError::InvalidTarget);
    assert!(!format!("{error:?} {error}").contains("uuid-input-value"));

    let fixture = Fixture::new();
    let parent = manifests(&fixture);
    let object_parent = objects(&fixture);
    let object = ImmutableTarget::public_object(UUID, "version").unwrap();
    let root_link = fixture.root.join("root-link");
    symlink(&fixture.root, &root_link).unwrap();
    assert_eq!(
        WorkingFiles::open(&root_link).unwrap_err(),
        CaptureError::UnsafeFile
    );
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let unsafe_file = InstallError::Store(CaptureError::UnsafeFile);
    let target = target();
    let leaf = parent.join("a.json");
    let check = |target: &ImmutableTarget| {
        assert_eq!(
            guard
                .install_immutable(target, PAYLOAD, LIMITS)
                .unwrap_err(),
            unsafe_file
        );
    };

    for path in [fixture.root.join("objects"), object_parent.clone()] {
        mode(&path, 0o750);
        check(&object);
        mode(&path, 0o700);
    }
    let alternate = fixture.root.join("actual-object-parent");
    fs::rename(&object_parent, &alternate).unwrap();
    symlink(&alternate, &object_parent).unwrap();
    check(&object);
    assert!(names(&alternate).is_empty());

    let source = fixture.root.join("working/source");
    write(&source, b"immutable test source");
    symlink(&source, &leaf).unwrap();
    check(&target);
    assert_eq!(fs::read(&source).unwrap(), b"immutable test source");
    fs::remove_file(&leaf).unwrap();

    directory(&leaf);
    mode(&leaf, 0o600);
    check(&target);
    mode(&leaf, 0o700);
    fs::remove_dir(&leaf).unwrap();

    mkfifoat(CWD, &leaf, Mode::RUSR | Mode::WUSR).unwrap();
    mode(&leaf, 0o600);
    check(&target);
    fs::remove_file(&leaf).unwrap();

    let _socket = UnixListener::bind(&leaf).unwrap();
    mode(&leaf, 0o600);
    check(&target);
    fs::remove_file(&leaf).unwrap();

    write(&leaf, b"protected file");
    mode(&leaf, 0o640);
    check(&target);
    mode(&leaf, 0o600);

    // Provision foreign ownership where the container permits it.
    if geteuid().is_root() {
        for path in [&parent, &leaf] {
            chownat(
                CWD,
                path,
                Some(Uid::from_raw(65_534)),
                None,
                AtFlags::empty(),
            )
            .unwrap();
            check(&target);
            chownat(CWD, path, Some(geteuid()), None, AtFlags::empty()).unwrap();
        }
    }

    mode(&fixture.root, 0o750);
    assert_eq!(
        guard
            .install_immutable(&target, PAYLOAD, LIMITS)
            .unwrap_err(),
        InstallError::Store(CaptureError::Changed),
    );
    mode(&fixture.root, 0o700);
    let lock = fixture.root.join("edit.lock");
    fs::rename(&lock, fixture.root.join("old-lock")).unwrap();
    write(&lock, b"replacement lock");
    assert_eq!(
        guard
            .install_immutable(&target, PAYLOAD, LIMITS)
            .unwrap_err(),
        InstallError::Store(CaptureError::Changed),
    );
    assert_eq!(fs::read(&leaf).unwrap(), b"protected file");
    assert_eq!(names(&parent), ["a.json"]);
}

#[test]
fn enforces_relative_path_and_byte_bounds_before_staging() {
    let fixture = Fixture::new();
    let parent = manifests(&fixture);
    let object_parent = objects(&fixture);
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let exact = InstallLimits {
        max_path_bytes: "manifests/\u{e9}.json".len(),
        max_file_bytes: 4,
    };
    let target = ImmutableTarget::manifest("\u{e9}").unwrap();
    let short_path = InstallLimits {
        max_path_bytes: exact.max_path_bytes - 1,
        ..exact
    };
    for (limits, bytes) in [(short_path, b"".as_slice()), (exact, b"12345")] {
        assert_eq!(
            guard.install_immutable(&target, bytes, limits).unwrap_err(),
            InstallError::Limits
        );
    }
    assert!(names(&parent).is_empty());
    assert_eq!(
        guard
            .install_immutable(&target, b"1234", exact)
            .unwrap()
            .outcome(),
        InstallOutcome::Created
    );

    let empty = ImmutableTarget::public_object(UPPER_UUID, "version").unwrap();
    let object_limits = InstallLimits {
        max_path_bytes: format!("objects/{UUID}/version.json").len(),
        max_file_bytes: 0,
    };
    assert_eq!(
        guard
            .install_immutable(
                &empty,
                b"",
                InstallLimits {
                    max_path_bytes: object_limits.max_path_bytes - 1,
                    ..object_limits
                },
            )
            .unwrap_err(),
        InstallError::Limits,
    );
    assert!(names(&object_parent).is_empty());
    guard.install_immutable(&empty, b"", object_limits).unwrap();
    assert!(
        fs::read(object_parent.join("version.json"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        guard
            .install_immutable(&empty, b"", object_limits)
            .unwrap()
            .outcome(),
        InstallOutcome::Reused,
    );
}

#[test]
fn cleans_only_owned_scratch_on_both_commit_boundary_failures() {
    let fixture = Fixture::new();
    let parent = manifests(&fixture);
    let final_path = parent.join("a.json");
    let unrelated = ".immutable-other";
    write(&parent.join(unrelated), b"another attempt");
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let target = target();
    let before = guard.install(&target, PAYLOAD, LIMITS, |boundary| {
        assert!(boundary == InstallBoundary::BeforePublish);
        let staged: Vec<_> = names(&parent)
            .into_iter()
            .filter(|entry| entry != unrelated)
            .collect();
        assert_eq!(staged.len(), 1);
        let scratch = parent.join(&staged[0]);
        assert_eq!(fs::read(&scratch).unwrap(), PAYLOAD);
        assert_eq!(fs::metadata(&scratch).unwrap().mode() & 0o7777, 0o600);
        Err(InstallError::Io)
    });
    assert_eq!(before.unwrap_err(), InstallError::Io);
    assert_eq!(names(&parent), [unrelated]);

    let after = guard.install(&target, PAYLOAD, LIMITS, |boundary| {
        if boundary == InstallBoundary::BeforeParentSync {
            assert_eq!(fs::read(&final_path).unwrap(), PAYLOAD);
            return Err(InstallError::Io);
        }
        Ok(())
    });
    assert_eq!(after.unwrap_err(), InstallError::DurabilityUnknown);
    assert_eq!(fs::read(&final_path).unwrap(), PAYLOAD);
    assert_eq!(
        guard
            .install_immutable(&target, PAYLOAD, LIMITS)
            .unwrap()
            .outcome(),
        InstallOutcome::Reused
    );
    assert_eq!(names(&parent), [unrelated, "a.json"]);
    assert_eq!(
        fs::read(parent.join(unrelated)).unwrap(),
        b"another attempt"
    );
}

#[test]
fn does_not_replace_entries_that_appear_after_staging() {
    for (existing, abort, expected) in [
        (b"old", false, Ok(InstallOutcome::Reused)),
        (b"new", false, Err(InstallError::Conflict)),
        (b"new", true, Err(InstallError::Io)),
    ] {
        let fixture = Fixture::new();
        let parent = manifests(&fixture);
        let path = parent.join("a.json");
        let files = fixture.open();
        let guard = files.try_edit().unwrap();
        let result = guard.install(&target(), b"old", LIMITS, |boundary| {
            assert!(boundary == InstallBoundary::BeforePublish);
            write(&path, existing);
            if abort {
                return Err(InstallError::Io);
            }
            Ok(())
        });
        assert_eq!(result.map(|receipt| receipt.outcome()), expected);
        assert_eq!(fs::read(&path).unwrap(), existing);
        assert_eq!(names(&parent), ["a.json"]);
    }
}

#[test]
fn revalidates_parent_and_lock_at_publication_boundaries() {
    for (replaced, step, expected) in [
        (
            "manifests",
            InstallBoundary::BeforePublish,
            InstallError::Store(CaptureError::Changed),
        ),
        (
            "edit.lock",
            InstallBoundary::BeforePublish,
            InstallError::Store(CaptureError::Changed),
        ),
        (
            "manifests",
            InstallBoundary::BeforeParentSync,
            InstallError::DurabilityUnknown,
        ),
        (
            "edit.lock",
            InstallBoundary::BeforeParentSync,
            InstallError::DurabilityUnknown,
        ),
    ] {
        let fixture = Fixture::new();
        let parent = manifests(&fixture);
        let moved = fixture.root.join("moved");
        let files = fixture.open();
        let guard = files.try_edit().unwrap();
        let result = guard.install(&target(), PAYLOAD, LIMITS, |boundary| {
            if boundary == step {
                let original = fixture.root.join(replaced);
                fs::rename(&original, &moved).unwrap();
                if replaced == "manifests" {
                    directory(&original);
                } else {
                    write(&original, b"new lock");
                }
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err(), expected);
        let original_parent = if replaced == "manifests" {
            assert!(names(&parent).is_empty());
            &moved
        } else {
            &parent
        };
        if step == InstallBoundary::BeforePublish {
            assert!(names(original_parent).is_empty());
        } else {
            assert_eq!(names(original_parent), ["a.json"]);
            assert_eq!(fs::read(original_parent.join("a.json")).unwrap(), PAYLOAD);
        }
    }
}
