//! Ciphertext, separate delivered key IDs, exact retry and publication faults.
use super::*;
use crate::configuration_transaction::maintenance::{Maintenance, MaintenanceError};

const TEST: &str = "configuration_transaction::tests::private::maintenance::retained_rotation_and_backup_use_real_keys_and_atomic_selectors";
#[test]
fn retained_rotation_and_backup_use_real_keys_and_atomic_selectors() {
    if std::env::var_os("TASK43_KEYS")
        .is_none_or(|dir| std::env::var_os("CREDENTIALS_DIRECTORY") != Some(dir))
    {
        let (f, _) = Fixture::new();
        for (id, value) in [("example", 9), ("alternate", 3)] {
            f.put(&format!("keys/rpt-advanced.aead.{id}"), &[value; 32]);
            fs::set_permissions(
                f.0.join(format!("keys/rpt-advanced.aead.{id}")),
                fs::Permissions::from_mode(0o400),
            )
            .unwrap();
        }
        let dir = f.0.join("keys");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("TASK43_KEYS", &dir)
            .env("CREDENTIALS_DIRECTORY", &dir)
            .status()
            .unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.success());
        return;
    }
    let old = KeyId::new("example").unwrap();
    let new = KeyId::new("alternate").unwrap();
    let missing = KeyId::new("missing").unwrap();
    {
        let (_fixture, store) = Fixture::new();
        let guard = store.try_edit().unwrap();
        first(&Transaction::new(&guard, LIMITS));
        let proof = store.provisioning_writer_lease().unwrap();
        let maintenance = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
        let before = maintenance.capture().unwrap();
        let bytes = guard.read_state(10000).unwrap();
        assert_eq!(
            maintenance.retain(proof, before.digest(), &[], &missing),
            Err(MaintenanceError::SecretsUnavailable)
        );
        assert_eq!(guard.read_state(10000).unwrap(), bytes);
        maintenance
            .retain(proof, before.digest(), &[], &old)
            .unwrap();
    }
    for pending in [false, true] {
        for boundary in [
            None,
            Some(Boundary::PrivateArtifacts),
            Some(Boundary::Manifest),
            Some(Boundary::Pointer),
            Some(Boundary::FinalSync),
        ] {
            let (f, store) = Fixture::new();
            {
                let g = store.try_edit().unwrap();
                let tx = Transaction::new(&g, LIMITS).with_write_key(&old);
                commit_account(
                    &tx,
                    "first",
                    Some((INSTANCE, 123)),
                    &[write(
                        Some(json!(ACCOUNT_SECRET)),
                        INSTANCE.parse().unwrap(),
                        CredentialSlot::AllStarLink,
                    )],
                )
                .unwrap();
                if !pending {
                    activate(&tx);
                }
            }
            drop(store);
            let store = WorkingFiles::open(&f.0).unwrap();
            let lease = store.try_writer_lease().unwrap();
            let g = store.try_edit().unwrap();
            let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
            if pending {
                let interrupted = m.capture().unwrap();
                assert_eq!(m.plan_gc(&lease).unwrap().paths().count(), 0);
                assert_eq!(
                    m.prepare_rotation(&lease, &old, &new).err(),
                    Some(MaintenanceError::Conflict)
                );
                assert_eq!(m.capture().unwrap().digest(), interrupted.digest());
                for (path, bytes) in interrupted.artifacts() {
                    assert_eq!(fs::read(f.0.join(path)).unwrap(), bytes);
                }
                Transaction::new(&g, LIMITS)
                    .with_write_key(&old)
                    .reconcile()
                    .unwrap();
            }
            let before = m.capture().unwrap();
            let before_hash = before.digest();
            let raw_state = g.read_state(10000).unwrap();
            assert_eq!(
                m.retain(
                    &lease,
                    before_hash,
                    &before.state().retained_recovery,
                    &missing
                ),
                Err(MaintenanceError::SecretsUnavailable)
            );
            assert_eq!(
                m.retain(&lease, before_hash, &before.state().retained_recovery, &new),
                Err(MaintenanceError::SecretsUnavailable)
            );
            assert_eq!(g.read_state(10000).unwrap(), raw_state);
            m.retain(&lease, before_hash, &before.state().retained_recovery, &old)
                .unwrap();

            assert_eq!(
                m.prepare_rotation(&lease, &new, &old).err(),
                Some(MaintenanceError::SecretsUnavailable)
            );
            assert_eq!(
                m.prepare_rotation(&lease, &old, &missing).err(),
                Some(MaintenanceError::SecretsUnavailable)
            );
            assert_eq!(m.capture().unwrap().digest(), before_hash);
            let plan = m.prepare_rotation(&lease, &old, &new).unwrap();
            if !pending && boundary.is_none() {
                let working_path = f.0.join("working/site/metadata.json");
                let working_bytes = fs::read(&working_path).unwrap();
                fs::write(&working_path, b"unresolved external edit").unwrap();
                assert_eq!(
                    m.prepare_rotation(&lease, &old, &new).err(),
                    Some(MaintenanceError::ExternalEdit)
                );
                assert_eq!(m.rotate(&lease, &plan), Err(MaintenanceError::ExternalEdit));
                assert_eq!(hash(&g.read_state(10000).unwrap().unwrap()), before_hash);
                fs::write(&working_path, working_bytes).unwrap();
                let path =
                    std::path::PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY").unwrap())
                        .join("rpt-advanced.aead.example");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&path, [4; 32]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
                assert_eq!(
                    m.rotate(&lease, &plan),
                    Err(MaintenanceError::SecretsUnavailable)
                );
                assert_eq!(hash(&g.read_state(10000).unwrap().unwrap()), before_hash);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&path, [9; 32]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
                let path = path.with_file_name("rpt-advanced.aead.alternate");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&path, [8; 32]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
                assert_eq!(
                    m.rotate(&lease, &plan),
                    Err(MaintenanceError::SecretsUnavailable)
                );
                assert_eq!(hash(&g.read_state(10000).unwrap().unwrap()), before_hash);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&path, [3; 32]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
            }

            if let Some(point) = boundary {
                let fault = |at| {
                    if at == point {
                        Err(TransactionError::StorageFault)
                    } else {
                        Ok(())
                    }
                };
                assert!(
                    Maintenance::new(&g, STORE.parse().unwrap(), LIMITS)
                        .with_checkpoint(&fault)
                        .rotate(&lease, &plan)
                        .is_err()
                );
                let actual = hash(&g.read_state(10000).unwrap().unwrap());
                if matches!(point, Boundary::FinalSync | Boundary::Pointer) {
                    assert_ne!(actual, before_hash);
                } else {
                    assert_eq!(actual, before_hash);
                }
                for (path, bytes) in before.artifacts() {
                    assert_eq!(fs::read(f.0.join(path)).unwrap(), bytes);
                }
            }
            m.rotate(&lease, &plan).unwrap();
            let after = m.capture().unwrap();
            assert_eq!(
                after.state().desired.revision,
                before.state().desired.revision
            );
            assert_ne!(after.state().desired, before.state().desired);
            assert_eq!(after.state().activation, before.state().activation);
            assert!(after.state().journal.is_none());
            assert!(
                after
                    .state()
                    .retained_recovery
                    .contains(&before.state().desired)
            );
            assert!(
                after
                    .required_keys()
                    .iter()
                    .any(|k| k.as_str() == "example")
            );
            assert!(
                after
                    .required_keys()
                    .iter()
                    .any(|k| k.as_str() == "alternate")
            );
            let count = after.artifacts().count();
            m.rotate(&lease, &plan).unwrap();
            assert_eq!(m.capture().unwrap().artifacts().count(), count);
            let tx = Transaction::new(&g, LIMITS).with_write_key(&new);
            let selected = tx.inspect().unwrap();
            assert_eq!(
                selected
                    .private
                    .credentials()
                    .unwrap()
                    .get(INSTANCE.parse().unwrap(), CredentialSlot::AllStarLink)
                    .unwrap()
                    .expose_bytes(),
                ACCOUNT_SECRET.as_bytes()
            );
            if pending {
                tx.reconcile().unwrap();
            }
            for (path, bytes) in before.artifacts() {
                assert_eq!(fs::read(f.0.join(path)).unwrap(), bytes);
            }
            let (backup, bstore) = Fixture::new();
            let bg = bstore.try_edit().unwrap();
            let proof = bstore.provisioning_writer_lease().unwrap();
            after.write_to(&bg, proof).unwrap();
            let restored = Maintenance::new(&bg, STORE.parse().unwrap(), LIMITS)
                .capture()
                .unwrap();
            for (path, bytes) in after.artifacts() {
                assert_eq!(fs::read(backup.0.join(path)).unwrap(), bytes);
                assert!(restored.artifacts().any(|(p, b)| p == path && b == bytes));
                assert!(!String::from_utf8_lossy(bytes).contains(ACCOUNT_SECRET));
            }
            assert!(!backup.0.join("keys").exists());
            assert_eq!(restored.state(), after.state());
            if !pending && boundary.is_none() {
                let (target, tstore) = Fixture::new();
                let tg = tstore.try_edit().unwrap();
                let proof = tstore.provisioning_writer_lease().unwrap();
                let tm = Maintenance::new(&tg, STORE.parse().unwrap(), LIMITS);
                let restore = tm
                    .prepare_restore(
                        proof,
                        m.capture().unwrap(),
                        None,
                        digest(&Transaction::new(&tg, LIMITS).inventory().unwrap()),
                        &new,
                    )
                    .unwrap();
                let fail = |point| {
                    if point == Boundary::Working {
                        Err(TransactionError::StorageFault)
                    } else {
                        Ok(())
                    }
                };
                assert!(
                    Maintenance::new(&tg, STORE.parse().unwrap(), LIMITS)
                        .with_checkpoint(&fail)
                        .restore(proof, &restore)
                        .is_err()
                );
                let selected = tm.restore(proof, &restore).unwrap();
                assert_eq!(selected.activation, Activation::Pending);
                assert!(selected.journal.is_none());
                assert_ne!(selected.desired.revision, after.state().desired.revision);
                assert!(selected.retained_recovery.contains(&after.state().desired));
                tm.restore(proof, &restore).unwrap();
                let tx = Transaction::new(&tg, LIMITS).with_write_key(&new);
                assert_eq!(
                    tx.inspect()
                        .unwrap()
                        .private
                        .credentials()
                        .unwrap()
                        .get(INSTANCE.parse().unwrap(), CredentialSlot::AllStarLink)
                        .unwrap()
                        .expose_bytes(),
                    ACCOUNT_SECRET.as_bytes()
                );
                assert!(!target.0.join("keys").exists());
                let dir =
                    std::path::PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY").unwrap());
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
                fs::rename(
                    dir.join("rpt-advanced.aead.example"),
                    dir.join("held-example"),
                )
                .unwrap();
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
                assert_eq!(
                    Transaction::new(&g, LIMITS)
                        .with_write_key(&new)
                        .inspect()
                        .unwrap()
                        .private
                        .state(),
                    SecretsState::Ready
                );
                assert_eq!(
                    m.capture().err(),
                    Some(MaintenanceError::SecretsUnavailable)
                );
                let tx = Transaction::new(&g, LIMITS).with_write_key(&new);
                let retained = tx.inspect().unwrap().state.retained_recovery;
                commit_account(&tx, "without-old-key", Some((INSTANCE, 123)), &[]).unwrap();
                activate(&tx);
                assert_eq!(tx.inspect().unwrap().state.retained_recovery, retained);
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
                fs::rename(
                    dir.join("held-example"),
                    dir.join("rpt-advanced.aead.example"),
                )
                .unwrap();
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
            }
        }
    }
}

pub(in crate::configuration_transaction::tests) fn fix1_keys(test: &str) -> bool {
    if std::env::var_os("TASK43_FIX1_KEYS")
        .is_some_and(|dir| std::env::var_os("CREDENTIALS_DIRECTORY") == Some(dir))
    {
        return false;
    }
    let (f, _) = Fixture::new();
    for (id, byte) in [("example", 9), ("alternate", 3)] {
        f.put(&format!("keys/rpt-advanced.aead.{id}"), &[byte; 32]);
        fs::set_permissions(
            f.0.join(format!("keys/rpt-advanced.aead.{id}")),
            fs::Permissions::from_mode(0o400),
        )
        .unwrap();
    }
    let dir = f.0.join("keys");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("TASK43_FIX1_KEYS", &dir)
        .env("CREDENTIALS_DIRECTORY", &dir)
        .status()
        .unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.success(), "fix1 credential child failed: {test}");
    true
}

fn fix1_public(revision: &str) -> (Fixture, WorkingFiles) {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS);
        let (manifest, bytes) = prepared(revision);
        tx.commit(
            Admission::Provision {
                store: STORE.parse().unwrap(),
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
        activate(&tx);
    }
    (fixture, store)
}
fn fix1_capture(store: &WorkingFiles) -> crate::configuration_transaction::maintenance::Backup {
    let guard = store.try_edit().unwrap();
    Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
        .capture()
        .unwrap()
}
fn fix1_size(backup: &crate::configuration_transaction::maintenance::Backup) -> u64 {
    backup.state().encode().unwrap().len() as u64
        + backup
            .artifacts()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum::<u64>()
}
fn fix1_private() -> (Fixture, WorkingFiles) {
    let (fixture, store) = Fixture::new();
    {
        let guard = store.try_edit().unwrap();
        let key = KeyId::new("example").unwrap();
        let tx = Transaction::new(&guard, LIMITS).with_write_key(&key);
        commit_account(
            &tx,
            "first",
            Some((INSTANCE, 123)),
            &[write(
                Some(json!(ACCOUNT_SECRET)),
                INSTANCE.parse().unwrap(),
                CredentialSlot::AllStarLink,
            )],
        )
        .unwrap();
        activate(&tx);
    }
    (fixture, store)
}

#[test]
fn fix1_fresh_gc_requires_durable_forgotten_selector_before_unlink() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_fresh_gc_requires_durable_forgotten_selector_before_unlink",
    ) {
        return;
    }
    let (f, store) = fix1_public("first");
    let key = KeyId::new("example").unwrap();
    let protected;
    let before_forget;
    {
        let guard = store.try_edit().unwrap();
        let proof = store.provisioning_writer_lease().unwrap();
        let old =
            crate::configuration_transaction::tests::maintenance::fix1_orphan(&guard, "history");
        let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
        m.retain(proof, m.capture().unwrap().digest(), &[old], &key)
            .unwrap();
        let selected = m.capture().unwrap();
        before_forget = selected.digest();
        protected = selected
            .artifacts()
            .map(|(p, b)| (p.to_owned(), b.to_vec()))
            .collect::<Vec<_>>();
        let fail = |point| {
            if point == Boundary::FinalSync {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
                .with_checkpoint(&fail)
                .retain(proof, selected.digest(), &[], &key),
            Err(MaintenanceError::DurabilityUnknown)
        );
        assert!(m.capture().unwrap().state().retained_recovery.is_empty());
    }
    drop(store);
    let store = WorkingFiles::open(&f.0).unwrap();
    let proof = store.try_writer_lease().unwrap();
    let guard = store.try_edit().unwrap();
    let m = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS);
    let reserved = m.plan_gc(&proof).unwrap();
    assert_eq!(
        m.collect(&proof, &reserved),
        Err(MaintenanceError::Conflict)
    );
    m.retain(&proof, before_forget, &[], &key).unwrap();
    let plan = m.plan_gc(&proof).unwrap();
    assert!(plan.paths().any(|p| p == "manifests/history.json"));
    let fail = |point| {
        if point == Boundary::PointerBarrier {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
            .with_checkpoint(&fail)
            .collect(&proof, &plan),
        Err(MaintenanceError::DurabilityUnknown),
        "GC unlinked before confirming durability of the visible selector"
    );
    for (path, bytes) in protected {
        assert_eq!(fs::read(f.0.join(path)).unwrap(), bytes);
    }
}

#[test]
fn fix1_unknown_authenticated_index_refuses_lossy_rotation() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_unknown_authenticated_index_refuses_lossy_rotation",
    ) {
        return;
    }
    use rpt_advanced_core::config::resources::manifest::PrivateReference;
    use rptadv_peer_support::{
        sealed_records::{
            RecordIdentity, RecordScope, SealedLimits, SecretBytes, load_systemd_key,
        },
        working_files::immutable::private_records::PreparedPrivateRecord,
    };
    let (f, store) = fix1_private();
    let old = KeyId::new("example").unwrap();
    let new = KeyId::new("alternate").unwrap();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    let selected = tx.inspect().unwrap();
    let reference = selected.desired.manifest().private().unwrap();
    let scope = RecordScope::new(STORE, &reference.record.to_string()).unwrap();
    let sealed = SealedLimits {
        max_plaintext_bytes: 10000,
        max_document_bytes: 10000,
    };
    let opened = g
        .open_private_record(
            RecordIdentity::new(scope, &reference.version.to_string()).unwrap(),
            reference.sha256.as_bytes(),
            tx.artifact_limits(),
            sealed,
        )
        .unwrap();
    let mut index: Value = serde_json::from_slice(opened.plaintext().expose_bytes()).unwrap();
    index["future_private_references"] = json!([index["records"][0][1].clone()]);
    let record = PreparedPrivateRecord::seal(
        &load_systemd_key(&old).unwrap(),
        opened.identity().scope(),
        &SecretBytes::new(serde_json::to_vec(&index).unwrap()),
        sealed,
    )
    .unwrap();
    let stored = g
        .install_private_record(&record, tx.artifact_limits())
        .unwrap();
    let replacement = PrivateReference {
        record: reference.record,
        version: stored
            .identity()
            .object_version()
            .to_string()
            .parse()
            .unwrap(),
        sha256: ContentHash::from_bytes(*stored.sha256()),
    };
    let bytes = selected
        .desired
        .manifest()
        .encode_with_private(Some(replacement));
    let mut state = selected.state;
    state.desired.sha256 = hash(&bytes);
    state.last_known_good = Some(state.desired.clone());
    f.put("manifests/first.json", &bytes);
    f.put("state.json", &state.encode().unwrap());
    assert!(tx.inspect().unwrap().private.warning_count() > 0);
    let original = g.read_state(10000).unwrap();
    let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
    assert!(
        m.prepare_rotation(store.provisioning_writer_lease().unwrap(), &old, &new)
            .is_err(),
        "rotation accepted an authenticated unknown private-index selector"
    );
    assert_eq!(g.read_state(10000).unwrap(), original);
    assert_eq!(fs::read(f.0.join("manifests/first.json")).unwrap(), bytes);
}

#[test]
fn fix1_restore_plan_cannot_cross_trusted_store_uuid() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_restore_plan_cannot_cross_trusted_store_uuid",
    ) {
        return;
    }
    let (_s, source) = fix1_public("first");
    let (_a, a) = Fixture::new();
    let ag = a.try_edit().unwrap();
    let key = KeyId::new("example").unwrap();
    let plan = Maintenance::new(&ag, STORE.parse().unwrap(), LIMITS)
        .prepare_restore(
            a.provisioning_writer_lease().unwrap(),
            fix1_capture(&source),
            None,
            digest(&Transaction::new(&ag, LIMITS).inventory().unwrap()),
            &key,
        )
        .unwrap();
    let id = "31271e86-b1d8-490a-b576-008d69fa0499";
    let path = std::env::temp_dir().join(format!(
        "rptadv-fix1-other-{}",
        rptadv_peer_support::working_files::transaction::new_identity().unwrap()
    ));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let b = Fixture(path);
    let store = WorkingFiles::provision(&b.0, id.parse().unwrap()).unwrap();
    let g = store.try_edit().unwrap();
    assert!(
        Maintenance::new(&g, id.parse().unwrap(), LIMITS)
            .restore(store.provisioning_writer_lease().unwrap(), &plan)
            .is_err(),
        "restore accepted a plan prepared for a different trusted store UUID"
    );
    assert!(g.read_state(10000).unwrap().is_none());
    assert!(g.discover_artifacts(LIMITS).unwrap().is_empty());
}

#[test]
fn fix1_restore_rechecks_generated_path_conflicts_before_any_copy() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_restore_rechecks_generated_path_conflicts_before_any_copy",
    ) {
        return;
    }
    let (_s, source) = fix1_public("first");
    let key = KeyId::new("example").unwrap();
    for index in 0..2 {
        let (f, store) = Fixture::new();
        let g = store.try_edit().unwrap();
        let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
        let proof = store.provisioning_writer_lease().unwrap();
        let plan = m
            .prepare_restore(
                proof,
                fix1_capture(&source),
                None,
                digest(&Transaction::new(&g, LIMITS).inventory().unwrap()),
                &key,
            )
            .unwrap();
        let path = plan.generated_paths()[index].to_owned();
        f.put(&path, b"post-prepare immutable conflict");
        let inventory = g.discover_artifacts(LIMITS).unwrap();
        assert_eq!(
            m.restore(proof, &plan).err(),
            Some(MaintenanceError::Conflict)
        );
        assert_eq!(
            g.discover_artifacts(LIMITS).unwrap(),
            inventory,
            "restore copied backup bytes before rechecking its generated manifest/journal"
        );
        assert!(g.read_state(10000).unwrap().is_none());
        assert_eq!(
            fs::read(f.0.join(path)).unwrap(),
            b"post-prepare immutable conflict"
        );
    }
}

#[test]
fn fix1_restore_preserves_destination_confirmed_lkg_role() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_restore_preserves_destination_confirmed_lkg_role",
    ) {
        return;
    }
    let key = KeyId::new("example").unwrap();
    for absent in [false, true] {
        let (_s, source) = fix1_public("source-one");
        if absent {
            let g = source.try_edit().unwrap();
            let tx = Transaction::new(&g, LIMITS);
            let selected = tx.inspect().unwrap();
            let mut state = selected.state;
            state.last_known_good = None;
            state.activation = Activation::Pending;
            g.replace_state(
                Some(selected.digest.as_bytes()),
                &state.encode().unwrap(),
                10000,
            )
            .unwrap();
        }
        let (_d, dest) = fix1_public("target-one");
        let g = dest.try_edit().unwrap();
        let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
        let previous = m.capture().unwrap();
        let lkg = previous.state().last_known_good.clone();
        let plan = m
            .prepare_restore(
                dest.provisioning_writer_lease().unwrap(),
                fix1_capture(&source),
                Some(previous.digest()),
                digest(&Transaction::new(&g, LIMITS).inventory().unwrap()),
                &key,
            )
            .unwrap();
        let result = m
            .restore(dest.provisioning_writer_lease().unwrap(), &plan)
            .unwrap();
        assert_eq!(
            result.last_known_good, lkg,
            "restore replaced the destination's confirmed LKG"
        );
        assert_eq!(result.activation, Activation::Pending);
    }
}

#[test]
fn fix1_rotation_preflights_full_selected_graph_path_and_byte_union() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_rotation_preflights_full_selected_graph_path_and_byte_union",
    ) {
        return;
    }
    let old = KeyId::new("example").unwrap();
    let new = KeyId::new("alternate").unwrap();
    let (_probe, ps) = fix1_private();
    let union;
    {
        let g = ps.try_edit().unwrap();
        let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
        let lease = ps.provisioning_writer_lease().unwrap();
        let plan = m.prepare_rotation(lease, &old, &new).unwrap();
        m.rotate(lease, &plan).unwrap();
        let b = m.capture().unwrap();
        union = (b.artifacts().count(), fix1_size(&b));
    }
    for paths in [true, false] {
        let (f, store) = fix1_private();
        let g = store.try_edit().unwrap();
        let before = g.read_state(10000).unwrap();
        let limits = if paths {
            CaptureLimits {
                max_paths: union.0 - 1,
                ..LIMITS
            }
        } else {
            CaptureLimits {
                max_total_bytes: union.1 - 1,
                ..LIMITS
            }
        };
        let m = Maintenance::new(&g, STORE.parse().unwrap(), limits);
        m.capture().unwrap(); // The existing selected half is independently admissible.
        let inventory = g.discover_artifacts(LIMITS).unwrap();
        assert_eq!(
            m.prepare_rotation(store.provisioning_writer_lease().unwrap(), &old, &new)
                .err(),
            Some(MaintenanceError::Limits),
            "rotation omitted retained originals from its final union budget"
        );
        assert_eq!(g.read_state(10000).unwrap(), before);
        assert_eq!(g.discover_artifacts(LIMITS).unwrap(), inventory);
        assert!(f.0.join("manifests/first.json").exists());
    }
}

#[test]
fn fix1_restore_preflights_full_selected_graph_path_and_byte_union() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_restore_preflights_full_selected_graph_path_and_byte_union",
    ) {
        return;
    }
    let key = KeyId::new("example").unwrap();
    let (_s, source) = fix1_public("backup-one");
    let (_probe, ps) = fix1_public("target-one");
    let union;
    {
        let g = ps.try_edit().unwrap();
        let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
        let old = m.capture().unwrap();
        let plan = m
            .prepare_restore(
                ps.provisioning_writer_lease().unwrap(),
                fix1_capture(&source),
                Some(old.digest()),
                digest(&Transaction::new(&g, LIMITS).inventory().unwrap()),
                &key,
            )
            .unwrap();
        let fail = |point| {
            if point == Boundary::Pointer {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        assert!(
            Maintenance::new(&g, STORE.parse().unwrap(), LIMITS)
                .with_checkpoint(&fail)
                .restore(ps.provisioning_writer_lease().unwrap(), &plan)
                .is_err()
        );
        let b = m.capture().unwrap();
        union = (b.artifacts().count(), fix1_size(&b));
    }
    for paths in [true, false] {
        let (_f, dest) = fix1_public("target-one");
        let g = dest.try_edit().unwrap();
        let before = g.read_state(10000).unwrap();
        let inventory = g.discover_artifacts(LIMITS).unwrap();
        let limits = if paths {
            CaptureLimits {
                max_paths: union.0 - 1,
                ..LIMITS
            }
        } else {
            CaptureLimits {
                max_total_bytes: union.1 - 1,
                ..LIMITS
            }
        };
        let m = Maintenance::new(&g, STORE.parse().unwrap(), limits);
        let previous = m.capture().unwrap();
        // Both input graphs fit these same limits independently; only their selected union exceeds them.
        let source_guard = source.try_edit().unwrap();
        let backup = Maintenance::new(&source_guard, STORE.parse().unwrap(), limits)
            .capture()
            .unwrap();
        assert_eq!(
            m.prepare_restore(
                dest.provisioning_writer_lease().unwrap(),
                backup,
                Some(previous.digest()),
                digest(&Transaction::new(&g, LIMITS).inventory().unwrap()),
                &key
            )
            .err(),
            Some(MaintenanceError::Limits),
            "restore omitted distinct destination roots from its final union budget"
        );
        assert_eq!(g.read_state(10000).unwrap(), before);
        assert_eq!(g.discover_artifacts(LIMITS).unwrap(), inventory);
    }
}

#[test]
fn fix1_restore_carries_pending_port_directory_retirements() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix1_restore_carries_pending_port_directory_retirements",
    ) {
        return;
    }
    use rpt_advanced_core::config::resources::PortKey;
    let (_s, source) = fix1_public("first");
    let (f, dest) = Fixture::new();
    let g = dest.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    let port: ResourceId = "42000000-0000-4000-8000-000000000001".parse().unwrap();
    let site: ResourceId = SITE.parse().unwrap();
    let (base, site_bytes) = prepared("with-port");
    let port_bytes = json!({"id":port.to_string(),"kind":"metadata","key":"removed",
        "configuration":{"name":"removed","inherit_from":"site"}})
    .to_string()
    .into_bytes();
    let manifest = PublicManifest::new(
        "with-port".parse().unwrap(),
        "with-port".parse().unwrap(),
        site,
        &[(port, PortKey::new("removed"))],
        [
            (
                base.registry().by_id(site).unwrap(),
                base.reference(site).cloned(),
            ),
            (
                ResourceBinding {
                    id: port,
                    scope: ResourceScope::Port(port),
                    kind: ResourceKind::Metadata,
                },
                Some(ObjectReference::new(
                    "with-port".parse().unwrap(),
                    hash(&port_bytes),
                )),
            ),
        ],
    )
    .unwrap();
    tx.commit(
        Admission::Provision {
            store: STORE.parse().unwrap(),
        },
        &tx.inventory().unwrap(),
        &manifest,
        &[(site, &site_bytes), (port, &port_bytes)],
        JOURNAL.parse().unwrap(),
    )
    .unwrap();
    activate(&tx);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(f.0.join(format!("working/ports/{port}/protocols")))
        .unwrap();
    let selected = tx.inspect().unwrap();
    let (without, bytes) = prepared("without-port");
    tx.commit(
        Admission::Save {
            state: selected.digest,
        },
        &tx.inventory().unwrap(),
        &without,
        &[(site, &bytes)],
        rptadv_peer_support::working_files::transaction::new_identity()
            .unwrap()
            .to_string()
            .parse()
            .unwrap(),
    )
    .unwrap();
    let interrupted = tx.inspect().unwrap();
    assert!(
        interrupted
            .journal
            .as_ref()
            .unwrap()
            .prune_directories
            .iter()
            .any(|p| p == &format!("ports/{port}/protocols"))
    );
    let m = Maintenance::new(&g, STORE.parse().unwrap(), LIMITS);
    let key = KeyId::new("example").unwrap();
    let plan = m
        .prepare_restore(
            dest.provisioning_writer_lease().unwrap(),
            fix1_capture(&source),
            Some(interrupted.digest),
            digest(&tx.inventory().unwrap()),
            &key,
        )
        .unwrap();
    m.restore(dest.provisioning_writer_lease().unwrap(), &plan)
        .unwrap();
    assert!(
        !f.0.join(format!("working/ports/{port}")).exists(),
        "superseding restore lost the pending journal's port-directory retirements"
    );
}

#[test]
fn fix2_restore_rejects_reference_free_registry_over_receiver_limit() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix2_restore_rejects_reference_free_registry_over_receiver_limit",
    ) {
        return;
    }
    let (_fixture, source) = Fixture::new();
    {
        let guard = source.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS);
        let (base, bytes) = prepared("registry");
        let site = SITE.parse().unwrap();
        let mut rows = vec![(
            base.registry().by_id(site).unwrap(),
            base.reference(site).cloned(),
        )];
        for kind in [
            ResourceKind::Radio,
            ResourceKind::RxAudio,
            ResourceKind::TxAudio,
            ResourceKind::Control,
        ] {
            let id = rptadv_peer_support::working_files::transaction::new_identity()
                .unwrap()
                .to_string()
                .parse()
                .unwrap();
            rows.push((
                ResourceBinding {
                    id,
                    scope: ResourceScope::Site(site),
                    kind,
                },
                None,
            ));
        }
        let manifest =
            PublicManifest::new(base.id().clone(), base.revision().clone(), site, &[], rows)
                .unwrap();
        tx.commit(
            Admission::Provision {
                store: STORE.parse().unwrap(),
            },
            &tx.inventory().unwrap(),
            &manifest,
            &[(site, &bytes)],
            JOURNAL.parse().unwrap(),
        )
        .unwrap();
        activate(&tx);
        assert_eq!(
            tx.inspect()
                .unwrap()
                .desired
                .manifest()
                .registry()
                .bindings()
                .len(),
            5
        );
    }
    let backup = fix1_capture(&source);
    assert_eq!(backup.artifacts().count(), 2);
    let (_empty, dest) = Fixture::new();
    let guard = dest.try_edit().unwrap();
    let limits = CaptureLimits {
        max_paths: 5,
        ..LIMITS
    };
    let receiver = Maintenance::new(&guard, STORE.parse().unwrap(), limits);
    let working = Transaction::new(&guard, limits).inventory().unwrap();
    let prepared = receiver.prepare_restore(
        dest.provisioning_writer_lease().unwrap(),
        backup,
        None,
        digest(&working),
        &KeyId::new("example").unwrap(),
    );
    assert_eq!(
        prepared.err(),
        Some(MaintenanceError::Limits),
        "restore prepared a graph the receiving manifest loader cannot read"
    );
    assert!(guard.discover_artifacts(LIMITS).unwrap().is_empty());
    assert!(guard.read_state(10000).unwrap().is_none());
}

#[test]
fn fix2_restore_rechecks_working_directory_budget_before_apply() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix2_restore_rechecks_working_directory_budget_before_apply",
    ) {
        return;
    }
    let (_source_fixture, source) = fix1_public("working-layout");
    let (fixture, dest) = Fixture::new();
    for suffix in 1..=3 {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(fixture.0.join(format!(
                "working/ports/42000000-0000-4000-8000-00000000000{suffix}"
            )))
            .unwrap();
    }
    let guard = dest.try_edit().unwrap();
    let limits = CaptureLimits {
        max_paths: 5,
        ..LIMITS
    };
    let working = Transaction::new(&guard, limits).inventory().unwrap();
    assert_eq!(working.directories().unwrap().len(), 4);
    let store = STORE.parse().unwrap();
    let lease = dest.provisioning_writer_lease().unwrap();
    let source_guard = source.try_edit().unwrap();
    let backup = Maintenance::new(&source_guard, store, limits)
        .capture()
        .unwrap();
    let plan = Maintenance::new(&guard, store, LIMITS)
        .prepare_restore(
            lease,
            backup,
            None,
            digest(&working),
            &KeyId::new("example").unwrap(),
        )
        .unwrap();
    // The four empty port directories fit now. Restoring site/metadata.json adds a fifth directory and a file.
    let result = Maintenance::new(&guard, store, limits).restore(lease, &plan);
    assert!(
        guard.discover_artifacts(LIMITS).unwrap().is_empty(),
        "restore copied artifacts before admitting the receiving working layout"
    );
    assert!(guard.read_state(10000).unwrap().is_none());
    assert_eq!(result.err(), Some(MaintenanceError::Limits));
}

use crate::configuration_transaction::maintenance::execute_test_request as command;

fn fix3_request(fixture: &Fixture) -> Value {
    json!({"store_root": fixture.0, "store_uuid": STORE})
}

#[test]
fn fix3_rotation_resumes_exact_sealed_versions_after_owner_is_dropped() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix3_rotation_resumes_exact_sealed_versions_after_owner_is_dropped",
    ) {
        return;
    }
    let (fixture, store) = fix1_private();
    let original = {
        let guard = store.try_edit().unwrap();
        guard.discover_artifacts(LIMITS).unwrap()
    };
    drop(store);
    let mut request = fix3_request(&fixture);
    request["expected_state_sha256"] = command(1, &request, None).unwrap()["state_sha256"].clone();
    request["write_key_id"] = json!("example");
    request["new_key_id"] = json!("alternate");
    let fail = |point| {
        if point == Boundary::PrivateArtifacts {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(command(5, &request, Some(&fail)).is_err());
    let installed: Vec<_> = {
        let owner = WorkingFiles::open(&fixture.0).unwrap();
        let guard = owner.try_edit().unwrap();
        guard
            .discover_artifacts(LIMITS)
            .unwrap()
            .into_iter()
            .filter(|p| p.starts_with("private/") && !original.contains(p))
            .map(|p| {
                let bytes = fs::read(fixture.0.join(&p)).unwrap();
                (p, bytes)
            })
            .collect()
    };
    assert_eq!(installed.len(), 2);
    command(5, &request, None).unwrap();
    let owner = WorkingFiles::open(&fixture.0).unwrap();
    let selected = fix1_capture(&owner);
    for (path, bytes) in installed {
        assert!(
            selected.artifacts().any(|(p, b)| p == path && b == bytes),
            "fresh rotation did not select the exact ciphertext installed by the first request"
        );
    }
}

#[test]
fn fix3_gc_resumes_original_registered_candidates_after_owner_is_dropped() {
    let (fixture, store) = fix1_public("gc-selected");
    let registered_paths = {
        let guard = store.try_edit().unwrap();
        crate::configuration_transaction::tests::maintenance::fix1_orphan(&guard, "gc-orphan");
        let plan = Maintenance::new(&guard, STORE.parse().unwrap(), LIMITS)
            .plan_gc(store.provisioning_writer_lease().unwrap())
            .unwrap();
        plan.paths().map(str::to_owned).collect::<Vec<_>>()
    };
    let orphan_manifest = "manifests/gc-orphan.json";
    let orphan_object = format!("objects/{SITE}/gc-orphan.json");
    assert!(registered_paths.iter().any(|path| path == orphan_manifest));
    assert!(registered_paths.contains(&orphan_object));
    drop(store);
    let mut request = fix3_request(&fixture);
    request["expected_state_sha256"] = command(1, &request, None).unwrap()["state_sha256"].clone();
    let fail = |point| {
        if point == Boundary::Garbage && !fixture.0.join(orphan_manifest).exists() {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(command(2, &request, Some(&fail)).is_err());
    assert!(!fixture.0.join(orphan_manifest).exists());
    let resumed = command(2, &request, None).unwrap();
    assert_eq!(
        resumed["removed"],
        json!(registered_paths.len()),
        "fresh GC lost registration evidence or its original removal count"
    );
    for path in registered_paths {
        assert!(
            !fixture.0.join(path).exists(),
            "GC left a registered candidate"
        );
    }
    assert!(fixture.0.join("manifests/gc-selected.json").exists());
}

#[test]
fn fix3_restore_resumes_the_published_revision_after_owner_is_dropped() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix3_restore_resumes_the_published_revision_after_owner_is_dropped",
    ) {
        return;
    }
    let (source, owner) = fix1_public("restore-source");
    drop(owner);
    let (dest, owner) = Fixture::new();
    drop(owner);
    let mut request = fix3_request(&dest);
    request["other_root"] = json!(source.0);
    request["expected_state_sha256"] = Value::Null;
    request["working_sha256"] = json!("empty");
    request["write_key_id"] = json!("example");
    let fail = |point| {
        if point == Boundary::Pointer {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(command(4, &request, Some(&fail)).is_err());
    let pending = {
        let owner = WorkingFiles::open(&dest.0).unwrap();
        let guard = owner.try_edit().unwrap();
        let state = Transaction::new(&guard, LIMITS).inspect().unwrap().state;
        assert!(state.journal.is_some());
        state.desired
    };
    let resumed = command(4, &request, None);
    assert!(
        resumed.is_ok(),
        "fresh restore rejected its own already-published revision"
    );
    let owner = WorkingFiles::open(&dest.0).unwrap();
    let state = fix1_capture(&owner);
    assert_eq!(state.state().desired, pending);
    assert!(state.state().journal.is_none());
    assert_eq!(
        resumed.unwrap()["revision"],
        json!(pending.revision.as_str())
    );
}

#[test]
fn fix3_retain_resumes_its_own_visible_pin_selection() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix3_retain_resumes_its_own_visible_pin_selection",
    ) {
        return;
    }
    let (fixture, store) = fix1_public("retained");
    let reference = {
        let guard = store.try_edit().unwrap();
        crate::configuration_transaction::tests::maintenance::fix1_orphan(&guard, "pinned")
    };
    drop(store);
    let mut request = fix3_request(&fixture);
    request["expected_state_sha256"] = command(1, &request, None).unwrap()["state_sha256"].clone();
    request["write_key_id"] = json!("example");
    request["retained_recovery"] = json!([{"manifest":reference.manifest.as_str(),"revision":reference.revision.as_str(),
        "sha256":reference.sha256.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>()}]);
    let fail = |point| {
        if point == Boundary::FinalSync {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        command(6, &request, Some(&fail)),
        Err(MaintenanceError::DurabilityUnknown)
    );
    let observed = command(1, &fix3_request(&fixture), None).unwrap()["state_sha256"].clone();
    let resumed = command(6, &request, None);
    assert!(
        resumed.is_ok(),
        "fresh retain rejected its own already-visible pin selection"
    );
    assert_eq!(resumed.unwrap()["state_sha256"], observed);
}

#[test]
fn fix3_reservation_fsync_conflict_and_completion_receipt_survive_reopen() {
    let (f, store) = fix1_public("selected");
    {
        let g = store.try_edit().unwrap();
        crate::configuration_transaction::tests::maintenance::fix1_orphan(&g, "orphan");
    }
    drop(store);
    let mut req = fix3_request(&f);
    req["expected_state_sha256"] = command(1, &req, None).unwrap()["state_sha256"].clone();
    let state = fs::read(f.0.join("state.json")).unwrap();
    let path = f.0.join("manifests/orphan.json");
    let original = fs::read(&path).unwrap();
    let fail = |point| {
        if point == Boundary::IntentRename {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        command(2, &req, Some(&fail)),
        Err(MaintenanceError::DurabilityUnknown)
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(f.0.join("state.json")).unwrap(), state);
    let pending = fs::read(f.0.join("maintenance.json")).unwrap();
    assert_eq!(
        command(1, &req, None).unwrap()["maintenance"]["status"],
        "pending"
    );
    let mut conflict = req.clone();
    conflict["expected_state_sha256"] = json!("0".repeat(64));
    assert_eq!(command(2, &conflict, None), Err(MaintenanceError::Conflict));
    let barrier = |point| {
        if point == Boundary::IntentBarrier {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        command(2, &req, Some(&barrier)),
        Err(MaintenanceError::DurabilityUnknown)
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    let owner = WorkingFiles::open(&f.0).unwrap();
    {
        let guard = owner.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS);
        assert_eq!(
            tx.ordinary_writer_admission(STORE.parse().unwrap()),
            Err(TransactionError::Conflict)
        );
        assert!(tx.ordinary_writer_admission(SITE.parse().unwrap()).is_err());
    }
    drop(owner);
    f.put("manifests/orphan.json", b"{}");
    assert!(command(2, &req, None).is_err());
    assert_eq!(fs::read(f.0.join("maintenance.json")).unwrap(), pending);
    f.put("manifests/orphan.json", &original);
    let lost_ack = |point| {
        if point == Boundary::ReceiptRename {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        command(2, &req, Some(&lost_ack)),
        Err(MaintenanceError::DurabilityUnknown)
    );
    let receipt = fs::read(f.0.join("maintenance.json")).unwrap();
    assert_eq!(
        command(1, &req, None).unwrap()["maintenance"]["status"],
        "completed"
    );
    assert_eq!(command(2, &req, None).unwrap()["removed"], json!(3));
    assert_eq!(fs::read(f.0.join("maintenance.json")).unwrap(), receipt);
    assert!(!path.exists());
}

#[test]
fn fix3_completed_ordinary_and_stale_exact_retry_use_distinct_state_checks() {
    let (f, store) = fix1_public("before");
    drop(store);
    let mut req = fix3_request(&f);
    req["expected_state_sha256"] = command(1, &req, None).unwrap()["state_sha256"].clone();
    command(2, &req, None).unwrap();
    let before = fs::read(f.0.join("maintenance.json")).unwrap();
    let owner = WorkingFiles::open(&f.0)
        .unwrap()
        .with_identity(STORE.parse().unwrap());
    let lease = owner.try_writer_lease().unwrap();
    {
        let g = owner.try_edit().unwrap();
        g.check_writer_lease(&lease).unwrap();
        let tx = Transaction::new(&g, LIMITS);
        tx.ordinary_writer_admission(STORE.parse().unwrap())
            .unwrap();
        let (manifest, bytes) = prepared("later");
        tx.commit(
            Admission::Save {
                state: tx.inspect().unwrap().digest,
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
        tx.ordinary_writer_admission(STORE.parse().unwrap())
            .unwrap();
    }
    drop(lease);
    assert_eq!(command(2, &req, None), Err(MaintenanceError::Conflict));
    assert_eq!(fs::read(f.0.join("maintenance.json")).unwrap(), before);
}

#[test]
fn fix3_encoded_record_cap_and_unknown_metadata_stop_before_effects() {
    let (f, owner) = fix1_public("cap");
    let raw_state = fs::read(f.0.join("state.json")).unwrap();
    {
        let g = owner.try_edit().unwrap();
        crate::configuration_transaction::tests::maintenance::fix1_orphan(&g, "orphan");
        let total = g
            .discover_artifacts(LIMITS)
            .unwrap()
            .iter()
            .map(|p| fs::metadata(f.0.join(p)).unwrap().len())
            .sum::<u64>()
            + raw_state.len() as u64;
        let limits = CaptureLimits {
            max_total_bytes: total,
            ..LIMITS
        };
        let m = Maintenance::new(&g, STORE.parse().unwrap(), limits);
        let lease = owner.provisioning_writer_lease().unwrap();
        let plan = m.plan_gc(lease).unwrap();
        assert_eq!(m.collect(lease, &plan), Err(MaintenanceError::Limits));
        assert!(!f.0.join("maintenance.json").exists());
    }
    drop(owner);
    f.put("maintenance.json", br#"{"version":77,"unsupported":true}"#);
    let mut req = fix3_request(&f);
    let inspect = command(1, &req, None).unwrap();
    assert_eq!(inspect["maintenance"]["status"], "invalid");
    req["expected_state_sha256"] = inspect["state_sha256"].clone();
    assert!(command(2, &req, None).is_err());
    assert_eq!(fs::read(f.0.join("state.json")).unwrap(), raw_state);
    assert!(f.0.join("manifests/orphan.json").exists());
}

#[test]
fn fix3_restore_source_is_required_only_before_original_selector_publication() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix3_restore_source_is_required_only_before_original_selector_publication",
    ) {
        return;
    }
    for boundary in [
        Boundary::IntentBarrier,
        Boundary::Manifest,
        Boundary::Working,
        Boundary::Clean,
    ] {
        let (src, owner) = fix1_public("source");
        drop(owner);
        let (dst, owner) = Fixture::new();
        drop(owner);
        let mut req = fix3_request(&dst);
        req["other_root"] = json!(src.0);
        req["expected_state_sha256"] = Value::Null;
        req["working_sha256"] = json!("empty");
        req["write_key_id"] = json!("example");
        let fault = |p| {
            if p == boundary {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        assert!(command(4, &req, Some(&fault)).is_err(), "{boundary:?}");
        let pending = fs::read(dst.0.join("maintenance.json")).unwrap();
        assert_eq!(
            command(1, &req, None).unwrap()["maintenance"]["status"],
            "pending"
        );
        fs::rename(src.0.join("state.json"), src.0.join("held-state.json")).unwrap();
        if matches!(boundary, Boundary::IntentBarrier | Boundary::Manifest) {
            assert!(command(4, &req, None).is_err());
            assert_eq!(fs::read(dst.0.join("maintenance.json")).unwrap(), pending);
            fs::rename(src.0.join("held-state.json"), src.0.join("state.json")).unwrap();
        }
        let result = command(4, &req, None).unwrap();
        assert_eq!(command(4, &req, None).unwrap(), result);
        assert_eq!(
            command(1, &req, None).unwrap()["maintenance"]["status"],
            "completed"
        );
    }
}

#[test]
fn fix3_rotation_pointer_recovery_reauthenticates_delivered_keys() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix3_rotation_pointer_recovery_reauthenticates_delivered_keys",
    ) {
        return;
    }
    let (f, store) = fix1_private();
    drop(store);
    let mut req = fix3_request(&f);
    req["expected_state_sha256"] = command(1, &req, None).unwrap()["state_sha256"].clone();
    req["write_key_id"] = json!("example");
    req["new_key_id"] = json!("alternate");
    let fail = |p| {
        if p == Boundary::Pointer {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(command(5, &req, Some(&fail)).is_err());
    let raw = fs::read(f.0.join("maintenance.json")).unwrap();
    let decoded = String::from_utf8(raw.clone()).unwrap();
    assert!(!decoded.contains("synthetic"));
    assert!(!raw.windows(32).any(|v| v == [9; 32] || v == [3; 32]));
    let path = std::path::PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY").unwrap())
        .join("rpt-advanced.aead.alternate");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, [8; 32]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(command(5, &req, None).is_err());
    assert_eq!(fs::read(f.0.join("maintenance.json")).unwrap(), raw);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, [3; 32]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    let resumed = command(5, &req, None).unwrap();
    assert_eq!(command(5, &req, None).unwrap(), resumed);
}

fn fix4_restore_request(source: &Fixture, destination: &std::path::Path) -> Value {
    json!({"store_root":destination, "store_uuid":STORE, "other_root":source.0,
        "expected_state_sha256":null, "working_sha256":"empty", "write_key_id":"example"})
}

#[test]
fn fix4_restore_checks_directory_changes_before_resuming_work() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix4_restore_checks_directory_changes_before_resuming_work",
    ) {
        return;
    }
    for present_before in [false, true] {
        let (source, owner) = fix1_public("restore-directories");
        drop(owner);
        let (dest, owner) = Fixture::new();
        drop(owner);
        let directory = dest.0.join("working/ports");
        if present_before {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        }
        let request = fix4_restore_request(&source, &dest.0);
        let fault = |point| {
            if point == Boundary::Pointer {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        assert!(command(4, &request, Some(&fault)).is_err());
        let selector = fs::read(dest.0.join("state.json")).unwrap();
        assert!(
            StatePointer::decode(&selector)
                .unwrap()
                .value
                .journal
                .is_some()
        );
        let reservation = fs::read(dest.0.join("maintenance.json")).unwrap();
        let working = dest.0.join("working/site/metadata.json");
        assert!(!working.exists());
        drop(source);
        if present_before {
            fs::remove_dir(&directory).unwrap();
        } else {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        }
        assert_eq!(
            command(4, &request, None),
            Err(MaintenanceError::ExternalEdit)
        );
        assert!(
            fs::read(dest.0.join("state.json")).unwrap() == selector && !working.exists(),
            "restore changed the selector or working files before admitting the directory inventory"
        );
        assert_eq!(
            fs::read(dest.0.join("maintenance.json")).unwrap(),
            reservation
        );
        if present_before {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        } else {
            fs::remove_dir(&directory).unwrap();
        }
        assert_eq!(command(4, &request, None).unwrap()["status"], "complete");
        assert!(working.is_file());
    }
}

#[test]
fn fix4_typed_gc_caps_record_items_before_reserving() {
    let (fixture, owner) = fix1_public("bounded");
    let guard = owner.try_edit().unwrap();
    let lease = owner.provisioning_writer_lease().unwrap();
    let limits = CaptureLimits {
        max_paths: 10_000,
        max_total_bytes: 8 * 1024 * 1024,
        ..LIMITS
    };
    let initial = guard.discover_artifacts(limits).unwrap().len();
    for index in initial..8193 {
        fixture.put(&format!("objects/{SITE}/spare-{index}.json"), b"");
    }
    assert_eq!(guard.discover_artifacts(limits).unwrap().len(), 8193);
    let maintenance = Maintenance::new(&guard, STORE.parse().unwrap(), limits);
    let plan = maintenance.plan_gc(lease).unwrap();
    let result = maintenance.collect(lease, &plan);
    assert!(
        result == Err(MaintenanceError::Limits) && !fixture.0.join("maintenance.json").exists(),
        "typed GC published a record exceeding the reader's sequence bound: result={result:?}, receipt={}",
        fixture.0.join("maintenance.json").exists()
    );
    assert_eq!(guard.discover_artifacts(limits).unwrap().len(), 8193);
}

#[test]
fn fix4_completed_receipts_check_their_own_request_and_selector() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix4_completed_receipts_check_their_own_request_and_selector",
    ) {
        return;
    }
    let mut observed = Vec::new();
    for operation in [2, 6] {
        let (fixture, owner) = fix1_public("receipt");
        drop(owner);
        let mut request = fix3_request(&fixture);
        let before = command(1, &request, None).unwrap()["state_sha256"].clone();
        request["expected_state_sha256"] = before.clone();
        request["write_key_id"] = json!("example");
        request["retained_recovery"] = json!([]);
        command(operation, &request, None).unwrap();
        assert_eq!(
            command(1, &request, None).unwrap()["maintenance"]["status"],
            "completed"
        );
        let path = fixture.0.join("maintenance.json");
        let raw = fs::read_to_string(&path).unwrap();
        let changed = if operation == 2 {
            raw.replacen(before.as_str().unwrap(), &"0".repeat(64), 1)
        } else {
            let owner = WorkingFiles::open(&fixture.0).unwrap();
            let backup = fix1_capture(&owner);
            let reference = &backup.state().desired;
            let pin = json!({"manifest":reference.manifest.as_str(), "revision":reference.revision.as_str(),
                "sha256":reference.sha256.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>()});
            raw.replacen(r#""pins":[]"#, &format!(r#""pins":[{pin}]"#), 1)
        };
        assert_ne!(raw, changed);
        fixture.put("maintenance.json", changed.as_bytes());
        observed.push(command(1, &request, None).unwrap()["maintenance"]["status"].clone());
    }
    assert_eq!(
        observed,
        [json!("invalid"), json!("invalid")],
        "inspection accepted a completed receipt whose request contradicts its own selector"
    );
}

#[test]
fn fix4_restore_authenticates_source_before_first_provisioning() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::fix4_restore_authenticates_source_before_first_provisioning",
    ) {
        return;
    }
    let (source, owner) = fix1_public("backup");
    drop(owner);
    let (parent, _owner) = Fixture::new();
    let dest = parent.0.join("destination");
    fs::DirBuilder::new().mode(0o700).create(&dest).unwrap();
    let mut request = fix4_restore_request(&source, &dest);
    request["store_uuid"] = json!(SITE);
    assert!(command(4, &request, None).is_err());
    assert_eq!(
        fs::read_dir(&dest).unwrap().count(),
        0,
        "restore provisioned the empty destination before authenticating its source UUID"
    );
    request["store_uuid"] = json!(STORE);
    assert_eq!(command(4, &request, None).unwrap()["status"], "complete");
}

#[test]
fn init_reconciles_the_same_revision_after_the_first_pointer() {
    if fix1_keys(
        "configuration_transaction::tests::private::maintenance::init_reconciles_the_same_revision_after_the_first_pointer",
    ) {
        return;
    }
    let (fixture, owner) = Fixture::new();
    drop(owner);
    let mut request = fix3_request(&fixture);
    request["write_key_id"] = json!("example");
    let published = std::cell::Cell::new(false);
    let stop = |point| {
        if point == Boundary::Pointer {
            published.set(true);
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(command(7, &request, Some(&stop)).is_err());
    assert!(published.get(), "init did not publish its first selector");
    let before = {
        let owner = WorkingFiles::open(&fixture.0).unwrap();
        let guard = owner.try_edit().unwrap();
        let state = Transaction::new(&guard, LIMITS).inspect().unwrap().state;
        assert!(state.journal.is_some());
        state.desired
    };

    let resumed = command(7, &request, None).unwrap();
    assert_eq!(resumed["revision"], before.revision.as_str());
    let owner = WorkingFiles::open(&fixture.0).unwrap();
    let selected = fix1_capture(&owner);
    let state = selected.state();
    assert_eq!(state.desired, before);
    assert!(state.journal.is_none());
    assert_eq!(state.activation, Activation::Pending);
    assert!(state.last_known_good.is_none());
    assert!(state.clean_working.is_some());
}
