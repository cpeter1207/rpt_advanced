//! Real store crash/retry sequence; accepted and observed runtime state remain separate.
mod private;
mod review;
use super::*;
use rpt_advanced_core::config::resources::{
    candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope},
    manifest::{ObjectReference, PublicManifest},
};
use rptadv_peer_support::working_files::WorkingFiles;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};

const SITE: &str = "48a69011-0563-43e2-b694-4db9b447b35e";
const STORE: &str = "31271e86-b1d8-490a-b576-008d69fa0428";
const JOURNAL: &str = "31271e86-b1d8-490a-b576-008d69fa0429";
const LIMITS: CaptureLimits = CaptureLimits {
    max_paths: 100,
    max_path_bytes: 256,
    max_file_bytes: 10000,
    max_total_bytes: 100000,
};
fn prepared(revision: &str) -> (PublicManifest, Vec<u8>) {
    let id: ResourceId = SITE.parse().unwrap();
    let binding = ResourceBinding {
        id,
        scope: ResourceScope::Site(id),
        kind: ResourceKind::Metadata,
    };
    let bytes = serde_json::json!({"id": SITE, "kind": "metadata", "configuration":{
        "name": revision, "inherit_from": null
    }})
    .to_string()
    .into_bytes();
    let reference = ObjectReference::new(revision.parse().unwrap(), hash(&bytes));
    let manifest = PublicManifest::new(
        revision.parse().unwrap(),
        revision.parse().unwrap(),
        id,
        &[],
        [(binding, Some(reference))],
    )
    .unwrap();
    (manifest, bytes)
}
#[test]
fn interrupted_publication_preserves_third_bytes_until_explicit_recovery() {
    let path = std::env::temp_dir().join(format!("rptadv-storage-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    struct Cleanup<'a>(&'a std::path::Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            fs::remove_dir_all(self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(&path);
    let store = WorkingFiles::provision(&path, STORE.parse().unwrap()).unwrap();
    let guard = store.try_edit().unwrap();
    let transaction = Transaction::new(&guard, LIMITS);
    let empty = transaction.inventory().unwrap();
    let (manifest, bytes) = prepared("r1");
    let store_id = STORE.parse().unwrap();
    let journal_id = JOURNAL.parse().unwrap();
    transaction
        .commit(
            Admission::Provision { store: store_id },
            &empty,
            &manifest,
            &[(SITE.parse().unwrap(), &bytes)],
            journal_id,
        )
        .unwrap();
    assert_eq!(
        transaction.inspect().unwrap().status,
        StartupStatus::Interrupted
    );
    let working = path.join("working/site/metadata.json");
    fs::create_dir_all(working.parent().unwrap()).unwrap();
    fs::set_permissions(working.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&working, b"external editor").unwrap();
    fs::set_permissions(&working, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(transaction.reconcile(), Err(TransactionError::ExternalEdit));
    assert_eq!(fs::read(&working).unwrap(), b"external editor");
    fs::remove_file(&working).unwrap();
    transaction.reconcile().unwrap();
    assert_eq!(fs::read(&working).unwrap(), bytes);
    let loaded = transaction.inspect().unwrap();
    assert_eq!(loaded.status, StartupStatus::Interrupted);
    assert!(loaded.state.last_known_good.is_none());
    transaction
        .record_activation(&loaded, Activation::Confirmed)
        .unwrap();
    let loaded = transaction.inspect().unwrap();
    assert_eq!(loaded.status, StartupStatus::Ready);
    assert_eq!(
        loaded.state.last_known_good.as_ref(),
        Some(&loaded.state.desired)
    );
    fs::write(&working, b"").unwrap();
    assert_eq!(
        transaction.inspect().unwrap().status,
        StartupStatus::ExternalEdit
    );
}

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> (Self, WorkingFiles) {
        let path = std::env::temp_dir().join(format!(
            "rptadv-transaction-{}",
            rptadv_peer_support::working_files::transaction::new_identity().unwrap()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let store = WorkingFiles::provision(&path, STORE.parse().unwrap()).unwrap();
        (Self(path), store)
    }
    fn put(&self, relative: &str, bytes: &[u8]) {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let path = self.0.join(relative);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn first(tx: &Transaction<'_, '_>) {
    let (manifest, bytes) = prepared("first");
    tx.commit(
        Admission::Provision {
            store: STORE.parse().unwrap(),
        },
        &tx.inventory().unwrap(),
        &manifest,
        &[(SITE.parse().unwrap(), &bytes)],
        JOURNAL.parse().unwrap(),
    )
    .unwrap();
    tx.reconcile().unwrap();
    tx.record_activation(&tx.inspect().unwrap(), Activation::Confirmed)
        .unwrap();
}

#[test]
fn restart_after_each_durable_boundary_selects_only_the_committed_graph() {
    for boundary in [
        Boundary::Objects,
        Boundary::Manifest,
        Boundary::Journal,
        Boundary::Pointer,
        Boundary::Working,
        Boundary::Clean,
    ] {
        let (fixture, store) = Fixture::new();
        {
            let guard = store.try_edit().unwrap();
            let mut tx = Transaction::new(&guard, LIMITS);
            first(&tx);
            let selected = tx.inspect().unwrap();
            let fault = |point| {
                if point == boundary {
                    Err(TransactionError::StorageFault)
                } else {
                    Ok(())
                }
            };
            tx.boundary = Some(&fault);
            let (manifest, bytes) = prepared("second");
            let result = tx
                .commit(
                    Admission::Save {
                        state: selected.digest,
                    },
                    &tx.inventory().unwrap(),
                    &manifest,
                    &[(SITE.parse().unwrap(), &bytes)],
                    "3f08c9e7-d5a6-4881-b210-ef9a96d59db2".parse().unwrap(),
                )
                .and_then(|_| tx.reconcile());
            assert_eq!(result, Err(TransactionError::StorageFault), "{boundary:?}");
        }
        let reopened = WorkingFiles::open(&fixture.0).unwrap();
        let guard = reopened.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS);
        let selected = tx.inspect().unwrap();
        let committed = matches!(
            boundary,
            Boundary::Pointer | Boundary::Working | Boundary::Clean
        );
        assert_eq!(
            selected.state.desired.revision.as_str(),
            if committed { "second" } else { "first" }
        );
        assert_eq!(
            selected.state.last_known_good.unwrap().revision.as_str(),
            "first"
        );
        assert_eq!(
            selected.status,
            if committed {
                StartupStatus::Interrupted
            } else {
                StartupStatus::Ready
            }
        );
        tx.reconcile().unwrap();
        assert_eq!(
            fs::read(fixture.0.join("working/site/metadata.json")).unwrap(),
            prepared(if committed { "second" } else { "first" }).1
        );
    }
}

#[test]
fn preserving_dirty_or_corrupt_working_and_selecting_inputs() {
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let mut tx = Transaction::new(&guard, LIMITS);
    first(&tx);
    let old = tx.inspect().unwrap();
    let before = tx.inventory().unwrap();
    let (manifest, bytes) = prepared("second");
    let objects = [(SITE.parse().unwrap(), bytes.as_slice())];
    let journal = "3f08c9e7-d5a6-4881-b210-ef9a96d59db2".parse().unwrap();
    let during_stage = |point| {
        if point == Boundary::Journal {
            fixture.put("working/site/radio.json", b"");
        }
        Ok(())
    };
    tx.boundary = Some(&during_stage);
    assert_eq!(
        tx.commit(
            Admission::Save { state: old.digest },
            &before,
            &manifest,
            &objects,
            journal
        ),
        Err(TransactionError::ExternalEdit)
    );
    tx.boundary = None;
    assert_eq!(tx.inspect().unwrap().digest, old.digest);
    assert_eq!(
        tx.commit(
            Admission::Save { state: old.digest },
            &tx.inventory().unwrap(),
            &manifest,
            &objects,
            journal
        ),
        Err(TransactionError::ExternalEdit)
    );
    assert_eq!(
        tx.commit(
            Admission::Reload {
                state: old.digest,
                working: digest(&before)
            },
            &tx.inventory().unwrap(),
            &manifest,
            &objects,
            journal
        ),
        Err(TransactionError::ExternalEdit)
    );
    fs::remove_file(fixture.0.join("working/site/radio.json")).unwrap();
    tx.commit(
        Admission::Save { state: old.digest },
        &before,
        &manifest,
        &objects,
        journal,
    )
    .unwrap();
    let state_path = fixture.0.join("state.json");
    let journal_path = fixture.0.join(format!("journals/{journal}.json"));
    let original = fs::read(&state_path).unwrap();
    let original_journal = fs::read(&journal_path).unwrap();
    fs::write(&journal_path, b"{}").unwrap();
    assert!(matches!(tx.inspect(), Err(TransactionError::StorageFault)));
    let mut wrong: serde_json::Value = serde_json::from_slice(&original_journal).unwrap();
    wrong["store_uuid"] = serde_json::json!(SITE);
    let altered = wrong.to_string();
    fs::write(&journal_path, &altered).unwrap();
    let mut selector = StatePointer::decode(&original).unwrap().value;
    selector.journal.as_mut().unwrap().sha256 = hash(altered.as_bytes());
    fs::write(&state_path, selector.encode().unwrap()).unwrap();
    assert!(matches!(tx.inspect(), Err(TransactionError::StorageFault)));
    fs::write(&journal_path, &original_journal).unwrap();
    for corrupt in [
        b"".as_slice(),
        br#"{"format_version":8}"#,
        br#"{"format_version":1,"format_version":1}"#,
    ] {
        fs::write(&state_path, corrupt).unwrap();
        assert!(matches!(tx.inspect(), Err(TransactionError::StorageFault)));
        assert_eq!(fs::read(&state_path).unwrap(), corrupt);
    }
    fs::write(&state_path, &original).unwrap();
    fs::remove_file(fixture.0.join("working/site/metadata.json")).unwrap();
    assert_eq!(tx.reconcile(), Err(TransactionError::ExternalEdit));
    let err = tx
        .record_activation(&old, Activation::Confirmed)
        .unwrap_err();
    assert_eq!(err.observed, Activation::Confirmed);
    assert_eq!(err.error, TransactionError::Conflict);
    assert!(guard.bookkeeping_fault());
    assert_eq!(
        Transaction::new(&guard, LIMITS).commit(
            Admission::Save { state: old.digest },
            &before,
            &manifest,
            &objects,
            journal
        ),
        Err(TransactionError::StorageFault)
    );
}

#[test]
fn import_new_tokens_canonicalizes_exact_keys_and_rejects_unassigned_ids() {
    use import::import_working;
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let tx = Transaction::new(&guard, LIMITS);
    first(&tx);
    let accepted = tx.inspect().unwrap();
    let registry = accepted.desired.manifest().registry();
    fixture.put(
        "working/site/command/macros/new-a.json",
        br#"{"kind":"macro","key":"../a/KEY","configuration":{"actions":[]}}"#,
    );
    fixture.put(
        "working/site/command/macros/new-b.json",
        br#"{"kind":"macro","key":"B","configuration":{"actions":[]}}"#,
    );
    fixture.put("working/ports/new-test/metadata.json",br#"{"kind":"metadata","key":"../Port/1","configuration":{"name":"Port","inherit_from":"site"}}"#);
    fixture.put(
        "working/ports/new-test/protocols/echolink.json",
        br#"{"kind":"echolink","configuration":{}}"#,
    );
    fixture.put(
        "working/ports/new-test/radio.json",
        br#"{"kind":"radio","configuration":{}}"#,
    );
    let snapshot = tx.inventory().unwrap();
    let draft = import_working(&snapshot, registry).unwrap();
    let reg = draft.registry();
    assert_eq!(
        reg.members()
            .iter()
            .map(|(_, k)| k.as_str())
            .collect::<Vec<_>>(),
        ["../a/KEY", "B"]
    );
    let version: rpt_advanced_core::config::resources::manifest::ObjectVersion =
        "second".parse().unwrap();
    let rows = reg.bindings().map(|binding| {
        let reference = draft
            .envelope(binding.id)
            .map(|b| ObjectReference::new(version.clone(), hash(b)));
        (binding, reference)
    });
    let keys = reg
        .port_keys()
        .map(|(k, id)| (id, k.clone()))
        .collect::<Vec<_>>();
    let members = reg
        .members()
        .iter()
        .map(|(b, k)| (b.id, k.clone()))
        .collect::<Vec<_>>();
    let manifest = PublicManifest::with_members(
        "second".parse().unwrap(),
        "second".parse().unwrap(),
        reg.site_id(),
        &keys,
        rows,
        &members,
    )
    .unwrap();
    let objects = reg
        .bindings()
        .filter_map(|b| Some((b.id, draft.envelope(b.id)?)))
        .collect::<Vec<_>>();
    tx.commit(
        Admission::Reload {
            state: accepted.digest,
            working: digest(&snapshot),
        },
        &snapshot,
        &manifest,
        &objects,
        "3f08c9e7-d5a6-4881-b210-ef9a96d59db2".parse().unwrap(),
    )
    .unwrap();
    tx.reconcile().unwrap();
    assert!(
        !fixture
            .0
            .join("working/site/command/macros/new-a.json")
            .exists()
    );
    assert!(!fixture.0.join("working/ports/new-test").exists());
    let saved = tx.inspect().unwrap();
    assert_eq!(
        saved.state.last_known_good.unwrap().revision.as_str(),
        "first"
    );
    let reg = saved.desired.manifest().registry();
    let first_id = reg.members()[0].0.id;
    fixture.put(
        "working/site/command/macros/new-c.json",
        br#"{"kind":"macro","key":"C","configuration":{}}"#,
    );
    let removed = reg.members()[1].0;
    fs::remove_file(
        fixture
            .0
            .join("working")
            .join(catalog::relative_path(removed)),
    )
    .unwrap();
    let scope = ResourceScope::Port(reg.port_keys().next().unwrap().1);
    let ordinary = reg.binding(scope, ResourceKind::Radio).unwrap();
    let protocol = reg.binding(scope, ResourceKind::Echolink).unwrap();
    for b in [ordinary, protocol] {
        fs::remove_file(fixture.0.join("working").join(catalog::relative_path(b))).unwrap();
    }
    let draft = import_working(&tx.inventory().unwrap(), reg).unwrap();
    assert_eq!(draft.registry().by_id(ordinary.id), Some(ordinary));
    assert!(draft.envelope(ordinary.id).is_none());
    assert!(draft.registry().by_id(protocol.id).is_none());
    assert_eq!(draft.registry().members()[0].0.id, first_id);
    assert_eq!(draft.registry().members()[1].1.as_str(), "C");
    let proxy = format!(
        "working/ports/{}/protocols/echolink-proxy.json",
        reg.port_keys().next().unwrap().1
    );
    fixture.put(&proxy, br#"{"kind":"echolink-proxy","configuration":{}}"#);
    assert!(import_working(&tx.inventory().unwrap(), reg).is_err());
    fs::remove_file(fixture.0.join(proxy)).unwrap();
    let first_path = fixture
        .0
        .join("working")
        .join(catalog::relative_path(reg.members()[0].0));
    let before = fs::read(&first_path).unwrap();
    let mut altered: serde_json::Value = serde_json::from_slice(&before).unwrap();
    altered["id"] = serde_json::json!(SITE);
    fs::write(&first_path, altered.to_string()).unwrap();
    assert!(import_working(&tx.inventory().unwrap(), reg).is_err());
    fs::write(&first_path, before).unwrap();
    fixture.put(
        "working/site/command/macros/88fd4be5-3118-4465-9098-a11df9860c79.json",
        b"{}",
    );
    assert!(import_working(&tx.inventory().unwrap(), reg).is_err());
}

#[test]
fn selecting_fields_limits_and_unsafe_working_layout_fail_closed() {
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let tx = Transaction::new(&guard, LIMITS);
    let tiny = Transaction::new(
        &guard,
        CaptureLimits {
            max_paths: 2,
            ..LIMITS
        },
    );
    let (manifest, bytes) = prepared("first");
    // The manifest and one metadata row fit. An existing empty unrelated
    // branch plus the target site/ and metadata leaf exceed the inventory cap.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(fixture.0.join("working/ports"))
        .unwrap();
    assert_eq!(
        tiny.commit(
            Admission::Provision {
                store: STORE.parse().unwrap()
            },
            &tiny.inventory().unwrap(),
            &manifest,
            &[(SITE.parse().unwrap(), &bytes)],
            JOURNAL.parse().unwrap()
        ),
        Err(TransactionError::InvalidCandidate)
    );
    assert!(guard.read_state(1024).unwrap().is_none());
    first(&tx);
    let state = tx.inspect().unwrap().state;
    let mut wire: serde_json::Value = serde_json::from_slice(&state.encode().unwrap()).unwrap();
    wire["extra"] = serde_json::json!(0);
    wire["desired"]["extra"] = serde_json::json!(true);
    assert_eq!(
        StatePointer::decode(wire.to_string().as_bytes())
            .unwrap()
            .warnings
            .len(),
        2
    );
    for (pointer, bad) in [
        ("/activation/state", serde_json::json!("other")),
        ("/activation/target_revision", serde_json::json!("wrong")),
        ("/desired/sha256", serde_json::json!("1234")),
        ("/last_known_good", serde_json::Value::Null),
        ("/clean_working_sha256", serde_json::Value::Null),
        ("/store_uuid", serde_json::json!("other")),
    ] {
        let mut trial = wire.clone();
        *trial.pointer_mut(pointer).unwrap() = bad;
        assert!(StatePointer::decode(trial.to_string().as_bytes()).is_err());
    }
    fixture.put("working/unrecognized", b"preserve");
    assert_eq!(tx.inspect().unwrap().status, StartupStatus::ExternalEdit);
    fs::remove_file(fixture.0.join("working/unrecognized")).unwrap();
    std::os::unix::fs::symlink(
        fixture.0.join("state.json"),
        fixture.0.join("working/site/radio.json"),
    )
    .unwrap();
    assert_eq!(tx.inspect().unwrap().status, StartupStatus::StorageFault);
    assert_eq!(tx.reconcile(), Err(TransactionError::StorageFault));
}

mod http;
