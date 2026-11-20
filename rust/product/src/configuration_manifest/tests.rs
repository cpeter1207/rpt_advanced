//! Exercise public manifest selection using artifacts installed under the real edit lock.

use super::*;
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{CandidateProblem, ResourceBinding, ResourceId, ResourceKind, ResourceScope},
    manifest::ObjectReference,
};
use rptadv_peer_support::working_files::{
    WorkingFiles,
    immutable::{ArtifactLimits, ImmutableTarget},
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const PORT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const RADIO: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ECHO: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const SITE_BODY: &str = r#"{
  "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "kind": "metadata",
  "configuration": {"name": "site-name", "inherit_from": null, "callsign": "N0SITE"}
}
"#;
const PORT_BODY: &str = r#"{"id":"BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB",
    "kind":"metadata","key":" /../North:é #;\t ",
    "configuration":{"name":"port-name","inherit_from":"site","enabled":42}}"#;
const ECHO_BODY: &str = r#"  {"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd", "kind":"echolink",
    "configuration":{"description":"opaque-body"}}  "#;
const PORT_KEY: &str = " /../North:é #;\t ";
const SELECTED: &str = " selected-révision ";
const OTHER: &str = "other-manifest";
const REVISION: &str = "logical révision";
const VERSION: &str = " vöbject ";
const INSTALL_LIMITS: ArtifactLimits = ArtifactLimits {
    max_path_bytes: 128,
    max_file_bytes: 4096,
};
const LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 16,
    max_path_bytes: INSTALL_LIMITS.max_path_bytes,
    max_file_bytes: INSTALL_LIMITS.max_file_bytes,
    max_total_bytes: 8192,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rptadv-public-manifest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        directory(&root);
        let fixture = Self(root);
        for relative in ["manifests", "objects", "working"] {
            directory(&fixture.0.join(relative));
        }
        for id in [SITE, PORT, ECHO] {
            directory(&fixture.0.join("objects").join(id));
        }
        for relative in ["edit.lock", "state.json", "working/metadata.json"] {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(fixture.0.join(relative))
                .unwrap();
        }
        fs::write(fixture.0.join("state.json"), b"invalid competing selection").unwrap();
        fs::write(
            fixture.0.join("working/metadata.json"),
            b"invalid working body",
        )
        .unwrap();
        fixture
    }

    fn open(&self) -> WorkingFiles {
        WorkingFiles::open(&self.0).unwrap()
    }

    fn object(&self, id: &str) -> PathBuf {
        self.0.join(object_path(id))
    }

    fn manifest(&self, id: &str) -> PathBuf {
        self.0.join(format!("manifests/{id}.json"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn directory(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn object_path(id: &str) -> String {
    format!("objects/{id}/{VERSION}.json")
}

fn uuid(input: &str) -> ResourceId {
    input.parse().unwrap()
}

fn port_binding(id: &str, kind: ResourceKind) -> ResourceBinding {
    ResourceBinding {
        id: uuid(id),
        scope: ResourceScope::Port(uuid(PORT)),
        kind,
    }
}

fn install(guard: &EditGuard<'_>, target: &ImmutableTarget, bytes: &[u8]) -> ContentHash {
    let installed = guard
        .install_immutable(target, bytes, INSTALL_LIMITS)
        .unwrap();
    ContentHash::from_bytes(*installed.sha256())
}

fn install_fixture(guard: &EditGuard<'_>, echo_body: &str) -> (Vec<u8>, ContentHash) {
    let site = uuid(SITE);
    let object_rows = [
        (
            ResourceBinding {
                id: site,
                scope: ResourceScope::Site(site),
                kind: ResourceKind::Metadata,
            },
            SITE_BODY,
        ),
        (port_binding(PORT, ResourceKind::Metadata), PORT_BODY),
        (port_binding(ECHO, ResourceKind::Echolink), echo_body),
    ];
    let rows = object_rows
        .into_iter()
        .map(|(binding, body)| {
            let target = ImmutableTarget::public_object(&binding.id.to_string(), VERSION).unwrap();
            let sha256 = install(guard, &target, body.as_bytes());
            (
                binding,
                Some(ObjectReference::new(VERSION.parse().unwrap(), sha256)),
            )
        })
        .chain([(port_binding(RADIO, ResourceKind::Radio), None)]);
    let manifest = PublicManifest::new(
        SELECTED.parse().unwrap(),
        REVISION.parse().unwrap(),
        site,
        &[(uuid(PORT), PortKey::new(PORT_KEY))],
        rows,
    )
    .unwrap();
    let mut wire = manifest.encode();
    assert_eq!(wire.pop(), Some(b'}'));
    wire.extend_from_slice(b",\"future\":\"manifest-note\"}\n");
    let digest = install(guard, &ImmutableTarget::manifest(SELECTED).unwrap(), &wire);
    (wire, digest)
}

fn load(
    guard: &EditGuard<'_>,
    digest: &ContentHash,
    limits: CaptureLimits,
) -> Result<LoadedPublicManifest, PublicLoadError> {
    load_public_manifest(
        guard,
        &SELECTED.parse().unwrap(),
        &REVISION.parse().unwrap(),
        digest,
        limits,
    )
}

#[test]
fn installed_graph_reopens_its_exact_bytes_and_assembles_after_the_guard() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let (loaded, exact) = {
        let guard = store.try_edit().unwrap();
        let (bytes, digest) = install_fixture(&guard, ECHO_BODY);
        install(
            &guard,
            &ImmutableTarget::manifest(OTHER).unwrap(),
            b"competing artifact",
        );
        (load(&guard, &digest, LIMITS).unwrap(), bytes)
    };
    assert_eq!(loaded.manifest_bytes(), exact);
    assert_eq!(loaded.manifest_warnings(), [ResourceWarning::UnknownField]);
    assert_eq!(
        loaded
            .manifest()
            .registry()
            .port_id(&PortKey::new(PORT_KEY)),
        Some(uuid(PORT)),
    );
    let candidate = loaded.assemble_candidate().unwrap();
    let metadata = candidate.port_metadata(uuid(PORT)).unwrap();
    assert_eq!(metadata.local().name, "port-name");
    assert_eq!(
        metadata.effective(candidate.site_metadata()).callsign,
        "N0SITE"
    );
    assert!(metadata.effective(candidate.site_metadata()).enabled);
    assert_eq!(
        candidate.warnings()[0].warning,
        ResourceWarning::InvalidValue("enabled")
    );
    let radio = candidate
        .staged(ResourceScope::Port(uuid(PORT)), ResourceKind::Radio)
        .unwrap();
    assert_eq!(radio.binding().id, uuid(RADIO));
    assert!(radio.is_empty());
    assert_eq!(
        candidate.port_protocols(uuid(PORT)).collect::<Vec<_>>(),
        [port_binding(ECHO, ResourceKind::Echolink)]
    );
    let debug = format!("{loaded:?}");
    for value in [
        SELECTED,
        REVISION,
        ECHO,
        PORT_KEY,
        "opaque-body",
        "manifest-note",
    ] {
        assert!(!debug.contains(value));
    }
    assert_eq!(fs::read(fixture.manifest(SELECTED)).unwrap(), exact);
    assert_eq!(
        fs::read(fixture.object(ECHO)).unwrap(),
        ECHO_BODY.as_bytes()
    );
}

#[test]
fn rejects_wrong_manifest_digest_id_or_revision_before_reading_children() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let (wire, digest) = install_fixture(&guard, ECHO_BODY);
    install(&guard, &ImmutableTarget::manifest(OTHER).unwrap(), &wire);
    fs::remove_file(fixture.object(ECHO)).unwrap();

    let bad_hash = ContentHash::from_bytes([0; 32]);
    assert_eq!(
        load(&guard, &bad_hash, LIMITS).unwrap_err(),
        PublicLoadError::Read(ReadError::DigestMismatch)
    );
    assert_eq!(
        load_public_manifest(
            &guard,
            &OTHER.parse().unwrap(),
            &REVISION.parse().unwrap(),
            &digest,
            LIMITS
        )
        .unwrap_err(),
        PublicLoadError::ManifestIdMismatch,
    );
    assert_eq!(
        load_public_manifest(
            &guard,
            &SELECTED.parse().unwrap(),
            &"different".parse().unwrap(),
            &digest,
            LIMITS
        )
        .unwrap_err(),
        PublicLoadError::RevisionMismatch,
    );
}

#[test]
fn refuses_any_corrupted_missing_or_unsafe_referenced_object() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let (_, digest) = install_fixture(&guard, ECHO_BODY);
    let file = fixture.object(ECHO);
    let corrupted = ECHO_BODY.replace("opaque-body", "secret-body");
    assert_eq!(corrupted.len(), ECHO_BODY.len());
    fs::write(&file, &corrupted).unwrap();
    let error = load(&guard, &digest, LIMITS).unwrap_err();
    assert_eq!(error, PublicLoadError::Read(ReadError::DigestMismatch));
    assert!(!format!("{error:?}").contains(ECHO));
    assert_eq!(fs::read(&file).unwrap(), corrupted.as_bytes());

    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        load(&guard, &digest, LIMITS).unwrap_err(),
        PublicLoadError::Read(ReadError::Store(CaptureError::UnsafeFile)),
    );
    fs::remove_file(&file).unwrap();
    assert_eq!(
        load(&guard, &digest, LIMITS).unwrap_err(),
        PublicLoadError::Read(ReadError::Store(CaptureError::Missing)),
    );
    assert!(!file.exists());
}

#[test]
fn matching_object_hash_does_not_override_envelope_identity() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let alternate = ECHO_BODY.replace(ECHO, PORT);
    let (_, digest) = install_fixture(&guard, &alternate);
    let loaded = load(&guard, &digest, LIMITS).unwrap();
    assert_eq!(
        loaded.assemble_candidate().unwrap_err(),
        CandidateError {
            resource: port_binding(ECHO, ResourceKind::Echolink),
            problem: CandidateProblem::InvalidEnvelope("id"),
        },
    );
    assert_eq!(
        fs::read(fixture.object(ECHO)).unwrap(),
        alternate.as_bytes()
    );
}

#[test]
fn enforces_actual_path_per_file_and_total_bounds_with_empty_slots_counted() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let (wire, digest) = install_fixture(&guard, ECHO_BODY);
    let exact = CaptureLimits {
        max_paths: 5,
        max_path_bytes: object_path(ECHO).len(),
        max_file_bytes: wire.len() as u64,
        max_total_bytes: (wire.len() + SITE_BODY.len() + PORT_BODY.len() + ECHO_BODY.len()) as u64,
    };
    load(&guard, &digest, exact).unwrap();
    let exceeded = PublicLoadError::Read(ReadError::Store(CaptureError::Limits));
    for limits in [
        CaptureLimits {
            max_path_bytes: exact.max_path_bytes - 1,
            ..exact
        },
        CaptureLimits {
            max_file_bytes: exact.max_file_bytes - 1,
            ..exact
        },
        CaptureLimits {
            max_total_bytes: exact.max_total_bytes - 1,
            ..exact
        },
        CaptureLimits {
            max_total_bytes: wire.len() as u64 - 1,
            ..exact
        },
        CaptureLimits {
            max_paths: 0,
            ..exact
        },
    ] {
        assert_eq!(load(&guard, &digest, limits).unwrap_err(), exceeded);
    }
    fs::remove_file(fixture.object(ECHO)).unwrap();
    assert_eq!(
        load(
            &guard,
            &digest,
            CaptureLimits {
                max_paths: exact.max_paths - 1,
                ..exact
            }
        )
        .unwrap_err(),
        exceeded,
    );
}
