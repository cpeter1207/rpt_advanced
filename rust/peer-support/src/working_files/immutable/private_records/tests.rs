//! Exercise sealed private records against real protected store fixtures.

use super::*;
use crate::working_files::{
    CaptureError,
    tests::{Fixture, directory, mode, write},
};
use ring::digest;
use rustix::{
    fs::{CWD, Mode, mkfifoat},
    process::geteuid,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
};

const STORE_UUID: &str = "a1111111-1111-4111-8111-111111111111";
const RECORD_UUID: &str = "b2222222-2222-4222-8222-222222222222";
const OTHER_UUID: &str = "c3333333-3333-4333-8333-333333333333";
const KEY_ID: &str = "private-test-key-id";
const ARTIFACT: ArtifactLimits = ArtifactLimits {
    max_path_bytes: 128,
    max_file_bytes: 1024,
};
const SEALED: SealedLimits = SealedLimits {
    max_plaintext_bytes: 64,
    max_document_bytes: 1024,
};
const PAYLOAD: &[u8] = b"private-test-plaintext\0\xff";

fn scope() -> RecordScope {
    RecordScope::new(STORE_UUID, "B2222222-2222-4222-8222-222222222222").unwrap()
}

fn key() -> SecretKey {
    SecretKey::for_test(KeyId::new(KEY_ID).unwrap(), [0xa5; 32])
}

fn prepared(
    plaintext: &[u8],
    limits: SealedLimits,
) -> Result<PreparedPrivateRecord, PrivateRecordError> {
    PreparedPrivateRecord::seal(
        &key(),
        &scope(),
        &SecretBytes::new(plaintext.to_vec()),
        limits,
    )
}

fn protected_fixture() -> Fixture {
    let fixture = Fixture::new();
    directory(&fixture.root.join("private"));
    directory(&fixture.root.join("private").join(RECORD_UUID));
    fixture
}

fn leaf(fixture: &Fixture, identity: RecordIdentity) -> PathBuf {
    fixture
        .root
        .join("private")
        .join(identity.scope().record_uuid().to_string())
        .join(format!("{}.sealed.json", identity.object_version()))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap()
}

fn load_test_key(selected: &KeyId) -> Result<SecretKey, KeyLoadError> {
    assert_eq!(selected.as_str(), KEY_ID);
    Ok(key())
}

fn no_key_lookup(_: &KeyId) -> Result<SecretKey, KeyLoadError> {
    panic!("the stored artifact must be rejected before key lookup")
}

#[test]
fn round_trips_binary_and_empty_plaintext_in_protected_private_files() {
    let fixture = protected_fixture();
    for parent in ["objects", "manifests"] {
        directory(&fixture.root.join(parent));
    }
    let sentinels = [
        ("working/input", PAYLOAD),
        ("objects/public.json", b"existing public object"),
        ("manifests/public.json", b"existing public manifest"),
        ("state.json", b"existing state"),
        ("edit.lock", b"existing lock"),
    ];
    for (path, bytes) in sentinels {
        // The fixture already provides the edit lock.
        if path != "edit.lock" {
            write(&fixture.root.join(path), bytes);
        }
    }

    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    for plaintext in [PAYLOAD, b""] {
        let prepared = prepared(plaintext, SEALED).unwrap();
        let identity = prepared.identity();
        let receipt = guard.install_private_record(&prepared, ARTIFACT).unwrap();
        assert_eq!(receipt.identity(), identity);
        assert_eq!(receipt.outcome(), InstallOutcome::Created);
        assert_eq!(identity.scope(), &scope());

        let path = fixture
            .root
            .join("private")
            .join(RECORD_UUID)
            .join(format!("{}.sealed.json", identity.object_version()));
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.uid(), geteuid().as_raw());
        assert_eq!(metadata.nlink(), 1);
        assert_ne!(
            metadata.ino(),
            fs::metadata(fixture.root.join("working/input"))
                .unwrap()
                .ino()
        );
        let document = fs::read(path).unwrap();
        assert_eq!(receipt.sha256(), &sha256(&document));
        assert!(!document.windows(PAYLOAD.len()).any(|part| part == PAYLOAD));
        let opened = guard
            .open_private_record_with(identity, receipt.sha256(), ARTIFACT, SEALED, load_test_key)
            .unwrap();
        assert_eq!(opened.identity(), identity);
        assert_eq!(opened.plaintext().expose_bytes(), plaintext);
        assert!(opened.warnings().is_empty());
        assert_eq!(
            format!("{prepared:?} {receipt:?} {opened:?}"),
            "PreparedPrivateRecord(..) InstalledPrivateRecord(..) OpenedPrivateRecord(..)",
        );
    }

    for (path, bytes) in sentinels {
        assert_eq!(fs::read(fixture.root.join(path)).unwrap(), bytes);
    }
    assert_eq!(
        fs::read_dir(fixture.root.join("private").join(RECORD_UUID))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn retries_reuse_one_preparation_across_edit_guards() {
    let fixture = protected_fixture();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let second = PreparedPrivateRecord::seal(
        &key(),
        &scope(),
        &SecretBytes::new(PAYLOAD.to_vec()),
        SEALED,
    )
    .unwrap();
    assert_ne!(second.identity(), prepared.identity());

    let files = fixture.open();
    let path = leaf(&fixture, prepared.identity());
    let guard = files.try_edit().unwrap();
    let created = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    let original = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let reused = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    assert_eq!(reused.outcome(), InstallOutcome::Reused);
    assert_eq!(reused.sha256(), created.sha256());
    drop(guard);
    let reused = files
        .try_edit()
        .unwrap()
        .install_private_record(&prepared, ARTIFACT)
        .unwrap();
    assert_eq!(reused.outcome(), InstallOutcome::Reused);
    assert_eq!(reused.identity(), created.identity());
    assert_eq!(reused.sha256(), created.sha256());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
}

#[test]
fn preserves_conflicting_bytes_and_rejects_unprotected_layouts() {
    let fixture = Fixture::new();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let identity = prepared.identity();
    let path = leaf(&fixture, identity);
    let parent = path.parent().unwrap();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();

    let check_rejected = |error| {
        assert_eq!(
            guard
                .install_private_record(&prepared, ARTIFACT)
                .unwrap_err(),
            PrivateRecordError::Install(InstallError::Store(error)),
        );
        assert_eq!(
            guard
                .open_private_record_with(identity, &[0; 32], ARTIFACT, SEALED, no_key_lookup)
                .unwrap_err(),
            PrivateRecordError::Read(ReadError::Store(error)),
        );
    };

    check_rejected(CaptureError::Missing);
    let namespace = fixture.root.join("private");
    assert!(!namespace.exists());
    directory(&namespace);
    check_rejected(CaptureError::Missing);
    assert!(!parent.exists());
    symlink(fixture.root.join("working"), parent).unwrap();
    check_rejected(CaptureError::UnsafeFile);
    fs::remove_file(parent).unwrap();
    directory(parent);

    write(&path, b"existing different envelope");
    mode(&path, 0o640);
    check_rejected(CaptureError::UnsafeFile);
    mode(&path, 0o600);
    assert_eq!(
        guard
            .install_private_record(&prepared, ARTIFACT)
            .unwrap_err(),
        PrivateRecordError::Install(InstallError::Conflict),
    );
    assert_eq!(fs::read(&path).unwrap(), b"existing different envelope");
    fs::remove_file(&path).unwrap();
    mkfifoat(CWD, &path, Mode::RUSR | Mode::WUSR).unwrap();
    mode(&path, 0o600);
    check_rejected(CaptureError::UnsafeFile);
}

#[test]
fn enforces_limits_and_checks_digest_and_envelope_before_loading_key() {
    let fixture = protected_fixture();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let identity = prepared.identity();
    let path = leaf(&fixture, identity);
    let initial = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    let document = fs::read(&path).unwrap();
    let artifact_exact = ArtifactLimits {
        max_path_bytes: 93,
        max_file_bytes: document.len() as u64,
    };
    let sealed_exact = SealedLimits {
        max_plaintext_bytes: PAYLOAD.len(),
        max_document_bytes: document.len(),
    };

    let retry = guard
        .install_private_record(&prepared, artifact_exact)
        .unwrap();
    assert_eq!(retry.outcome(), InstallOutcome::Reused);
    assert_eq!(retry.sha256(), initial.sha256());
    guard
        .open_private_record_with(
            identity,
            retry.sha256(),
            artifact_exact,
            sealed_exact,
            load_test_key,
        )
        .unwrap();
    assert!(
        PreparedPrivateRecord::seal(
            &key(),
            &scope(),
            &SecretBytes::new(PAYLOAD.to_vec()),
            sealed_exact,
        )
        .is_ok()
    );

    for limits in [
        ArtifactLimits {
            max_path_bytes: 92,
            ..artifact_exact
        },
        ArtifactLimits {
            max_file_bytes: artifact_exact.max_file_bytes - 1,
            ..artifact_exact
        },
    ] {
        assert_eq!(
            guard.install_private_record(&prepared, limits).unwrap_err(),
            PrivateRecordError::Install(InstallError::Limits),
        );
        assert_eq!(
            guard
                .open_private_record_with(identity, retry.sha256(), limits, SEALED, no_key_lookup)
                .unwrap_err(),
            PrivateRecordError::Read(ReadError::Store(CaptureError::Limits)),
        );
    }

    let secret = SecretBytes::new(PAYLOAD.to_vec());
    for (limits, read_error) in [
        (
            SealedLimits {
                max_document_bytes: sealed_exact.max_document_bytes - 1,
                ..sealed_exact
            },
            PrivateRecordError::Read(ReadError::Store(CaptureError::Limits)),
        ),
        (
            SealedLimits {
                max_plaintext_bytes: sealed_exact.max_plaintext_bytes - 1,
                ..sealed_exact
            },
            PrivateRecordError::Sealed(SealedError::SizeLimit),
        ),
    ] {
        assert_eq!(
            PreparedPrivateRecord::seal(&key(), &scope(), &secret, limits).unwrap_err(),
            PrivateRecordError::Sealed(SealedError::SizeLimit),
        );
        assert_eq!(
            guard
                .open_private_record_with(identity, retry.sha256(), ARTIFACT, limits, no_key_lookup)
                .unwrap_err(),
            read_error,
        );
    }

    let mut bogus_digest = *retry.sha256();
    bogus_digest[0] ^= 1;
    assert_eq!(
        guard
            .open_private_record_with(identity, &bogus_digest, ARTIFACT, SEALED, no_key_lookup)
            .unwrap_err(),
        PrivateRecordError::Read(ReadError::DigestMismatch),
    );

    write(&path, b"{\"invalid-secret-member\"");
    let malformed = fs::read(&path).unwrap();
    let error = guard
        .open_private_record_with(
            identity,
            &sha256(&malformed),
            ARTIFACT,
            SEALED,
            no_key_lookup,
        )
        .unwrap_err();
    assert_eq!(error, PrivateRecordError::Sealed(SealedError::InvalidJson));
    assert!(!format!("{error:?} {error}").contains("invalid-secret-member"));
    assert_eq!(fs::read(path).unwrap(), malformed);
}

#[test]
fn fails_closed_on_key_errors_and_substituted_identity() {
    let fixture = protected_fixture();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let identity = prepared.identity();
    let receipt = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    let path = leaf(&fixture, identity);
    let document = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();

    assert_eq!(
        guard
            .open_private_record_with(identity, receipt.sha256(), ARTIFACT, SEALED, |selected| {
                assert_eq!(selected.as_str(), KEY_ID);
                Err(KeyLoadError::Missing)
            })
            .unwrap_err(),
        PrivateRecordError::Key(KeyLoadError::Missing),
    );
    let auth_error = PrivateRecordError::Sealed(SealedError::AuthenticationFailed);
    assert_eq!(
        guard
            .open_private_record_with(identity, receipt.sha256(), ARTIFACT, SEALED, |selected| {
                Ok(SecretKey::for_test(selected.clone(), [0x5a; 32]))
            })
            .unwrap_err(),
        auth_error,
    );

    let version = identity.object_version().to_string();
    directory(&fixture.root.join("private").join(OTHER_UUID));
    for wrong_identity in [
        RecordIdentity::new(RecordScope::new(OTHER_UUID, RECORD_UUID).unwrap(), &version).unwrap(),
        RecordIdentity::new(RecordScope::new(STORE_UUID, OTHER_UUID).unwrap(), &version).unwrap(),
        RecordIdentity::new(scope(), "00000000-0000-0000-0000-000000000000").unwrap(),
    ] {
        let copy = leaf(&fixture, wrong_identity);
        if copy != path {
            write(&copy, &document);
        }
        assert_eq!(
            guard
                .open_private_record_with(
                    wrong_identity,
                    receipt.sha256(),
                    ARTIFACT,
                    SEALED,
                    load_test_key
                )
                .unwrap_err(),
            auth_error,
        );
        assert_eq!(fs::read(copy).unwrap(), document);
    }
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::read(path).unwrap(), document);
}

#[test]
fn warns_without_values_and_retains_exact_stored_document() {
    let fixture = protected_fixture();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let identity = prepared.identity();
    let receipt = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    let path = leaf(&fixture, identity);
    let mut document = fs::read(&path).unwrap();
    assert_eq!(document.pop(), Some(b'}'));
    document.extend_from_slice(br#","unknown-test-member":"unknown-test-value"}"#);
    write(&path, &document);

    assert_eq!(
        guard
            .open_private_record_with(identity, receipt.sha256(), ARTIFACT, SEALED, no_key_lookup)
            .unwrap_err(),
        PrivateRecordError::Read(ReadError::DigestMismatch),
    );
    let opened = guard
        .open_private_record_with(
            identity,
            &sha256(&document),
            ARTIFACT,
            SEALED,
            load_test_key,
        )
        .unwrap();
    assert_eq!(opened.plaintext().expose_bytes(), PAYLOAD);
    assert_eq!(opened.warnings(), &[SealedWarning::UnknownField]);
    let warning = opened.warnings()[0];
    let diagnostic = format!("{opened:?} {warning:?} {warning}");
    for value in [
        "unknown-test-member",
        "unknown-test-value",
        KEY_ID,
        "private-test-plaintext",
    ] {
        assert!(!diagnostic.contains(value));
    }
    assert_eq!(fs::read(path).unwrap(), document);
}

#[test]
fn uncertain_durability_preserves_envelope_for_exact_retry() {
    let fixture = protected_fixture();
    let files = fixture.open();
    let guard = files.try_edit().unwrap();
    let prepared = prepared(PAYLOAD, SEALED).unwrap();
    let error = guard
        .install_private_record_with(&prepared, ARTIFACT, |boundary| {
            if boundary == InstallBoundary::BeforeParentSync {
                return Err(InstallError::Io);
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        error,
        PrivateRecordError::Install(InstallError::DurabilityUnknown)
    );

    let path = leaf(&fixture, prepared.identity());
    let document = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let receipt = guard.install_private_record(&prepared, ARTIFACT).unwrap();
    assert_eq!(receipt.outcome(), InstallOutcome::Reused);
    assert_eq!(receipt.sha256(), &sha256(&document));
    assert_eq!(fs::read(&path).unwrap(), document);
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
}
