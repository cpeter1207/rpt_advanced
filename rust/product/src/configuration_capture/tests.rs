//! Protected working files exercised through the product capture boundary.

use super::*;
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{CandidateProblem, ResourceBinding, ResourceId, ResourceKind, ResourceScope},
};
use rptadv_peer_support::working_files::WorkingFiles;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const PORT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const PORT_KEY: &str = " /../Radio:é #;\t ";
const PORT_METADATA: &str = "ports/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb/metadata.json";
const PORT_ECHO: &str = "ports/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb/protocols/echolink.json";
const SITE_BODY: &str = r#"{
    "id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","kind":"metadata",
    "configuration":{"name":"Station","inherit_from":null,"callsign":"N0SITE"}
}"#;
const PORT_BODY: &str = r#"{
    "id":"BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB","kind":"metadata",
    "key":" /../Radio:é #;\t ",
    "configuration":{"name":"  ../North / port #1  ","inherit_from":"site"}
}"#;
const LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 32,
    max_path_bytes: 160,
    max_file_bytes: 2048,
    max_total_bytes: 8192,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rptadv-registered-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Self(root);
        Self::write(&fixture.0.join("edit.lock"), "");
        fixture.put("site/metadata.json", SITE_BODY);
        fixture.put(PORT_METADATA, PORT_BODY);
        fixture
    }

    fn write(path: &Path, body: &str) {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
    }

    fn file(&self, relative: &str) -> PathBuf {
        self.0.join("working").join(relative)
    }

    fn put(&self, relative: &str, body: &str) {
        let path = self.file(relative);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        Self::write(&path, body);
    }

    fn open(&self) -> WorkingFiles {
        WorkingFiles::open(&self.0).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn id(value: &str) -> ResourceId {
    value.parse().unwrap()
}

fn registry(bindings: &[ResourceBinding]) -> ResourceRegistry {
    ResourceRegistry::new(
        id(SITE),
        &[(
            id("BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB"),
            PortKey::new(PORT_KEY),
        )],
        bindings,
    )
    .unwrap()
}

#[test]
fn registered_paths_preserve_sparse_defaults_and_echolink_only_inventory() {
    let fixture = Fixture::new();
    let site = ResourceScope::Site(id(SITE));
    let port = ResourceScope::Port(id(PORT));
    let ordinary = [
        (ResourceKind::Radio, "site/radio.json"),
        (ResourceKind::RxAudio, "site/rx/audio.json"),
        (ResourceKind::RxSignaling, "site/rx/signaling.json"),
        (ResourceKind::TxAudio, "site/tx/audio.json"),
        (ResourceKind::TxSignaling, "site/tx/signaling.json"),
        (ResourceKind::Interface, "site/interface.json"),
        (ResourceKind::PeerAudio, "site/peer-audio.json"),
        (ResourceKind::Control, "site/control.json"),
        (ResourceKind::Memories, "site/memories.json"),
        (ResourceKind::ScanLists, "site/scan-lists.json"),
    ];
    let mut bindings = Vec::new();
    for (index, &(kind, relative)) in ordinary.iter().enumerate() {
        let resource = id(&format!("11111111-1111-4111-8111-{index:012}"));
        bindings.push(ResourceBinding {
            id: resource,
            scope: site,
            kind,
        });
        fixture.put(
            relative,
            &format!(
                r#"{{"id":"{resource}","kind":"{}","configuration":{{"authored":true}}}}"#,
                kind.as_str(),
            ),
        );
    }

    let site_default = ResourceBinding {
        id: id("22222222-2222-4222-8222-222222222222"),
        scope: site,
        kind: ResourceKind::Allstarlink,
    };
    let port_allstar = ResourceBinding {
        id: id("33333333-3333-4333-8333-333333333333"),
        scope: port,
        kind: ResourceKind::Allstarlink,
    };
    let port_echo = ResourceBinding {
        id: id("44444444-4444-4444-8444-444444444444"),
        scope: port,
        kind: ResourceKind::Echolink,
    };
    let port_empty = ResourceBinding {
        id: id("55555555-5555-4555-8555-555555555555"),
        scope: port,
        kind: ResourceKind::Control,
    };
    bindings.extend([site_default, port_allstar, port_echo, port_empty]);
    fixture.put(
        "site/protocol-defaults/allstarlink.json",
        r#"{"id":"22222222-2222-4222-8222-222222222222","kind":"allstarlink","configuration":{}}"#,
    );
    fixture.put(
        PORT_ECHO,
        r#"{"id":"44444444-4444-4444-8444-444444444444","kind":"echolink","configuration":{"authored":"body-marker"}}"#,
    );

    let registry = registry(&bindings);
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let registered = capture_registered(&guard, &registry, LIMITS).unwrap();
    let candidate = registered.candidate();
    let metadata = candidate.port_metadata(id(PORT)).unwrap();
    assert_eq!(metadata.local().name, "  ../North / port #1  ");
    assert_eq!(
        metadata.effective(candidate.site_metadata()).callsign,
        "N0SITE"
    );
    assert_eq!(registry.port_id(&PortKey::new(PORT_KEY)), Some(id(PORT)));
    for (kind, _) in ordinary {
        assert!(
            !candidate.staged(site, kind).unwrap().is_empty(),
            "{kind:?}"
        );
    }
    let empty = candidate.staged(port, ResourceKind::Control).unwrap();
    assert_eq!(empty.binding(), port_empty);
    assert!(empty.is_empty());
    assert!(
        candidate
            .staged(site, ResourceKind::Allstarlink)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        candidate.port_protocols(id(PORT)).collect::<Vec<_>>(),
        [port_echo]
    );
    let debug = format!("{registered:?}");
    for value in ["body-marker", "North / port", PORT, PORT_KEY] {
        assert!(!debug.contains(value), "{debug}");
    }
}

#[test]
fn registered_capture_rejects_reassigned_identity_without_mutating_bytes() {
    const RX_PATH: &str = "ports/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb/rx/audio.json";
    const TX_PATH: &str = "ports/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb/tx/audio.json";
    let fixture = Fixture::new();
    let port = ResourceScope::Port(id(PORT));
    let rx = ResourceBinding {
        id: id("11111111-1111-4111-8111-111111111111"),
        scope: port,
        kind: ResourceKind::RxAudio,
    };
    let tx = ResourceBinding {
        id: id("22222222-2222-4222-8222-222222222222"),
        scope: port,
        kind: ResourceKind::TxAudio,
    };
    let registry = registry(&[rx, tx]);
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    for (field, rx_id, rx_kind, tx_id, tx_kind) in [
        ("id", tx.id, "rx-audio", rx.id, "tx-audio"),
        ("kind", rx.id, "tx-audio", tx.id, "rx-audio"),
    ] {
        let body = |id, kind| {
            format!(
                r#"{{"id":"{id}","kind":"{kind}","configuration":{{"authored":"value-marker"}}}}"#,
            )
        };
        let rx_body = body(rx_id, rx_kind);
        let tx_body = body(tx_id, tx_kind);
        fixture.put(RX_PATH, &rx_body);
        fixture.put(TX_PATH, &tx_body);

        let error = capture_registered(&guard, &registry, LIMITS).unwrap_err();
        assert_eq!(
            error,
            RegisteredCaptureError::Candidate(CandidateError {
                resource: rx,
                problem: CandidateProblem::InvalidEnvelope(field),
            })
        );
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            for value in [
                PORT,
                "11111111-1111-4111-8111-111111111111",
                "value-marker",
                RX_PATH,
            ] {
                assert!(!diagnostic.contains(value), "{diagnostic}");
            }
        }
        assert_eq!(fs::read(fixture.file(RX_PATH)).unwrap(), rx_body.as_bytes());
        assert_eq!(fs::read(fixture.file(TX_PATH)).unwrap(), tx_body.as_bytes());
    }
}

#[test]
fn guarded_recheck_detects_changes_to_exact_selected_bytes() {
    let fixture = Fixture::new();
    let registry = registry(&[]);
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let original = capture_registered(&guard, &registry, LIMITS).unwrap();
    fixture.put(PORT_ECHO, "outside the selection");
    original.verify_current(&guard).unwrap();

    let whitespace_edit = format!("{PORT_BODY}\n");
    fixture.put(PORT_METADATA, &whitespace_edit);
    assert_eq!(original.verify_current(&guard), Err(CaptureError::Changed));
    assert_eq!(
        fs::read(fixture.file(PORT_METADATA)).unwrap(),
        whitespace_edit.as_bytes()
    );

    let updated = capture_registered(&guard, &registry, LIMITS).unwrap();
    assert_ne!(updated.fingerprint(), original.fingerprint());
    assert_eq!(
        original
            .candidate()
            .port_metadata(id(PORT))
            .unwrap()
            .local(),
        updated.candidate().port_metadata(id(PORT)).unwrap().local(),
    );
    updated.verify_current(&guard).unwrap();
}

#[test]
fn registered_metadata_is_required_and_optional_paths_count_toward_limits() {
    let fixture = Fixture::new();
    let registry = registry(&[ResourceBinding {
        id: id("11111111-1111-4111-8111-111111111111"),
        scope: ResourceScope::Port(id(PORT)),
        kind: ResourceKind::Radio,
    }]);
    let store = fixture.open();
    let guard = store.try_edit().unwrap();
    let only_metadata = CaptureLimits {
        max_paths: 2,
        ..LIMITS
    };
    assert_eq!(
        capture_registered(&guard, &registry, only_metadata).unwrap_err(),
        RegisteredCaptureError::Capture(CaptureError::Limits)
    );
    fs::remove_file(fixture.file(PORT_METADATA)).unwrap();
    assert_eq!(
        capture_registered(&guard, &registry, LIMITS).unwrap_err(),
        RegisteredCaptureError::Capture(CaptureError::Missing)
    );
}
