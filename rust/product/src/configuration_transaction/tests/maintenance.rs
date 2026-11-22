//! Actual guarded files: retention, backup bytes, offline proof and stale plans.
use super::*;
use crate::configuration_transaction::maintenance::{Maintenance, MaintenanceError};
use rptadv_peer_support::working_files::immutable::ImmutableTarget;

fn offline(
    fixture: &Fixture,
) -> (
    WorkingFiles,
    rptadv_peer_support::working_files::WriterLease,
) {
    let store = WorkingFiles::open(&fixture.0).unwrap();
    let lease = store.try_writer_lease().unwrap();
    (store, lease)
}
#[test]
fn verified_backup_preserves_exact_graph_without_writer_downtime() {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        first(&Transaction::new(&guard, LIMITS));
    }
    drop(store);
    let (store, writer) = offline(&fixture);
    let guard = store.try_edit().unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    let backup = m.capture().unwrap();
    assert_eq!(backup.state().desired.manifest.as_str(), "first");
    assert_eq!(backup.artifacts().count(), 2);
    for (path, bytes) in backup.artifacts() {
        assert_eq!(fs::read(fixture.0.join(path)).unwrap(), bytes);
    }
    assert!(backup.required_keys().is_empty());
    assert_eq!(store.try_writer_lease().unwrap_err(), CaptureError::Busy);
    drop(writer);
}
#[test]
fn gc_deletes_only_registered_unreachable_artifacts_and_rechecks_selector() {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        first(&Transaction::new(&guard, LIMITS));
        let (old, bytes) = prepared("orphan");
        guard.prepare_object_parent(SITE.parse().unwrap()).unwrap();
        guard
            .install_immutable(
                &ImmutableTarget::public_object(SITE, "orphan").unwrap(),
                &bytes,
                ArtifactLimits {
                    max_file_bytes: 10000,
                    max_path_bytes: 256,
                },
            )
            .unwrap();
        guard
            .install_immutable(
                &ImmutableTarget::manifest("orphan").unwrap(),
                &old.encode(),
                ArtifactLimits {
                    max_file_bytes: 10000,
                    max_path_bytes: 256,
                },
            )
            .unwrap();
    }
    drop(store);
    fixture.put("objects/unregistered.txt", b"third-party");
    fixture.put("manifests/.editor-backup", b"third-party");
    let (store, lease) = offline(&fixture);
    let guard = store.try_edit().unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    let plan = m.plan_gc(&lease).unwrap();
    assert!(plan.paths().any(|p| p == "manifests/orphan.json"));
    assert!(plan.paths().any(|p| p.ends_with("/orphan.json")));
    let tx = Transaction::new(&guard, LIMITS);
    tx.record_activation(&tx.inspect().unwrap(), Activation::Failed)
        .unwrap();
    assert_eq!(m.collect(&lease, &plan), Err(MaintenanceError::Conflict));
    assert!(fixture.0.join("manifests/orphan.json").exists());
    let plan = m.plan_gc(&lease).unwrap();
    m.collect(&lease, &plan).unwrap();
    assert!(!fixture.0.join("manifests/orphan.json").exists());
    assert!(fixture.0.join("manifests/first.json").exists());
    assert_eq!(
        fs::read(fixture.0.join("objects/unregistered.txt")).unwrap(),
        b"third-party"
    );
    assert!(fixture.0.join("manifests/.editor-backup").exists());
}
#[test]
fn corrupt_history_unknown_graph_and_wrong_root_proof_preserve_bytes() {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        first(&Transaction::new(&guard, LIMITS));
    }
    drop(store);
    let (other, other_store) = Fixture::new();
    drop(other_store);
    let (_, wrong) = offline(&other);
    let (store, lease) = offline(&fixture);
    let guard = store.try_edit().unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    assert_eq!(m.plan_gc(&wrong).err(), Some(MaintenanceError::Conflict));
    fixture.put("manifests/corrupt.json", b"{}");
    assert_eq!(
        m.plan_gc(&lease).err(),
        Some(MaintenanceError::InvalidGraph)
    );
    assert_eq!(
        fs::read(fixture.0.join("manifests/corrupt.json")).unwrap(),
        b"{}"
    );
    assert_eq!(
        Maintenance::new(&guard, SITE.parse().unwrap(), LIMITS)
            .capture()
            .err(),
        Some(MaintenanceError::InvalidGraph)
    );
}

#[test]
fn ordinary_save_and_activation_preserve_explicit_recovery_roots() {
    let (_fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let tx = Transaction::new(&guard, LIMITS);
    first(&tx);
    let selected = tx.inspect().unwrap();
    let mut state = selected.state.clone();
    state.retained_recovery = vec![state.desired.clone()];
    guard
        .replace_state(
            Some(selected.digest.as_bytes()),
            &state.encode().unwrap(),
            10000,
        )
        .unwrap();
    let selected = tx.inspect().unwrap();
    let (manifest, bytes) = prepared("second");
    tx.commit(
        Admission::Save {
            state: selected.digest,
        },
        &tx.inventory().unwrap(),
        &manifest,
        &[(SITE.parse().unwrap(), &bytes)],
        rptadv_peer_support::working_files::transaction::new_identity()
            .unwrap()
            .to_string()
            .parse()
            .unwrap(),
    )
    .unwrap();
    tx.reconcile().unwrap();
    tx.record_activation(&tx.inspect().unwrap(), Activation::Confirmed)
        .unwrap();
    assert_eq!(
        tx.inspect().unwrap().state.retained_recovery,
        state.retained_recovery
    );
}

#[test]
fn explicit_history_blocks_gc_until_forget_and_gc_retries_after_unlink_fault() {
    let (f, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        first(&Transaction::new(&guard, LIMITS));
        let (old, bytes) = prepared("history");
        guard
            .install_immutable(
                &ImmutableTarget::public_object(SITE, "history").unwrap(),
                &bytes,
                ArtifactLimits {
                    max_file_bytes: 10000,
                    max_path_bytes: 256,
                },
            )
            .unwrap();
        guard
            .install_immutable(
                &ImmutableTarget::manifest("history").unwrap(),
                &old.encode(),
                ArtifactLimits {
                    max_file_bytes: 10000,
                    max_path_bytes: 256,
                },
            )
            .unwrap();
    }
    drop(store);
    let (store, lease) = offline(&f);
    let guard = store.try_edit().unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    let (old, _) = prepared("history");
    let reference = ManifestRef {
        manifest: old.id().clone(),
        revision: old.revision().clone(),
        sha256: hash(&old.encode()),
    };
    // Seed explicit roots through the keyless low-level fixture. The real-key child
    // separately checks the production retain/forget write-policy boundary.
    let tx = Transaction::new(&guard, LIMITS);
    let selected = tx.inspect().unwrap();
    let mut state = selected.state;
    state.retained_recovery = vec![reference];
    guard
        .replace_state(
            Some(selected.digest.as_bytes()),
            &state.encode().unwrap(),
            10000,
        )
        .unwrap();
    assert!(
        !m.plan_gc(&lease)
            .unwrap()
            .paths()
            .any(|p| p == "manifests/history.json")
    );
    let selected = tx.inspect().unwrap();
    state.retained_recovery.clear();
    guard
        .replace_state(
            Some(selected.digest.as_bytes()),
            &state.encode().unwrap(),
            10000,
        )
        .unwrap();
    let plan = m.plan_gc(&lease).unwrap();
    let fail = |point| {
        if point == Boundary::Garbage {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(
        Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
            .with_checkpoint(&fail)
            .collect(&lease, &plan)
            .is_err()
    );
    m.collect(&lease, &plan).unwrap();
    m.collect(&lease, &plan).unwrap();
    assert!(!f.0.join("manifests/history.json").exists());
    assert!(f.0.join("manifests/first.json").exists());
}
#[test]
fn restore_preflights_conflicts_and_replays_a_fresh_pending_revision() {
    // A public-only restore also needs a delivered write key before immutable preflight.
    if super::private::maintenance::fix1_keys(
        "configuration_transaction::tests::maintenance::restore_preflights_conflicts_and_replays_a_fresh_pending_revision",
    ) {
        return;
    }
    let (_source, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    first(&Transaction::new(&guard, LIMITS));
    let backup = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
        .capture()
        .unwrap();
    let (destination, dstore) = Fixture::new();
    destination.put(
        &format!("objects/{SITE}/first.json"),
        b"conflicting immutable bytes",
    );
    let dguard = dstore.try_edit().unwrap();
    let proof = dstore.provisioning_writer_lease().unwrap();
    let dm = Maintenance::new(&dguard, STORE.parse().unwrap(), LIMITS);
    let working = digest(&Transaction::new(&dguard, LIMITS).inventory().unwrap());
    assert_eq!(
        dm.prepare_restore(
            proof,
            backup,
            None,
            working,
            &KeyId::new("example").unwrap()
        )
        .err(),
        Some(MaintenanceError::Conflict)
    );
    assert!(dguard.read_state(10000).unwrap().is_none());
    assert_eq!(
        fs::read(destination.0.join(format!("objects/{SITE}/first.json"))).unwrap(),
        b"conflicting immutable bytes"
    );
}

#[test]
fn export_public_fixture_for_the_actual_dynamic_client() {
    use std::os::unix::fs::FileTypeExt;
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    first(&Transaction::new(&guard, LIMITS));
    let backup = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
        .capture()
        .unwrap();
    assert_eq!(backup.artifacts().count(), 2);
    assert_eq!(
        fs::read(fixture.0.join("working/site/metadata.json")).unwrap(),
        prepared("first").1
    );
    if let Some(fifo) = std::env::var_os("TASK43_CLIENT_FIXTURE_FIFO") {
        assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo());
        assert!(
            std::process::Command::new("tar")
                .args(["-czf"])
                .arg(&fifo)
                .arg("-C")
                .arg(&fixture.0)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
    }
}

// Fix-round1 registration fixtures use the real guarded immutable owner.
pub(super) fn fix1_orphan(guard: &EditGuard<'_>, revision: &str) -> ManifestRef {
    let (manifest, bytes) = prepared(revision);
    guard.prepare_object_parent(SITE.parse().unwrap()).unwrap();
    for (target, bytes) in [
        (
            ImmutableTarget::public_object(SITE, revision).unwrap(),
            bytes,
        ),
        (
            ImmutableTarget::manifest(revision).unwrap(),
            manifest.encode(),
        ),
    ] {
        guard
            .install_immutable(
                &target,
                &bytes,
                ArtifactLimits {
                    max_file_bytes: LIMITS.max_file_bytes,
                    max_path_bytes: LIMITS.max_path_bytes,
                },
            )
            .unwrap();
    }
    ManifestRef {
        manifest: manifest.id().clone(),
        revision: manifest.revision().clone(),
        sha256: hash(&manifest.encode()),
    }
}

#[test]
fn fix1_unknown_selector_cannot_authorize_gc_of_its_graph() {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS);
        first(&tx);
        let orphan = fix1_orphan(&guard, "future-history");
        // Prove this is a complete, otherwise-unselected registered graph.
        tx.load(&orphan).unwrap();
        let mut state = tx.inspect().unwrap().state;
        state.retained_recovery = vec![orphan];
        let mut raw: serde_json::Value = serde_json::from_slice(&state.encode().unwrap()).unwrap();
        raw["future_recovery_selector"] = raw["retained_recovery"][0].clone();
        raw["retained_recovery"] = serde_json::json!([]);
        fixture.put("state.json", &serde_json::to_vec(&raw).unwrap());
        assert!(!tx.inspect().unwrap().warnings.is_empty());
    }
    drop(store);
    let (store, lease) = offline(&fixture);
    let guard = store.try_edit().unwrap();
    let before = guard.read_state(10000).unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    match m.plan_gc(&lease) {
        Err(MaintenanceError::InvalidGraph) => {}
        Err(error) => panic!("unexpected maintenance refusal: {error:?}"),
        Ok(plan) => {
            m.collect(&lease, &plan).unwrap();
        }
    }
    assert_eq!(guard.read_state(10000).unwrap(), before);
    assert!(
        fixture.0.join("manifests/future-history.json").exists(),
        "unknown selector authorized deleting its registered recovery graph"
    );
    assert_eq!(
        fs::read(
            fixture
                .0
                .join(format!("objects/{SITE}/future-history.json"))
        )
        .unwrap(),
        prepared("future-history").1
    );
}
