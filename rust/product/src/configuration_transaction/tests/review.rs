//! Regression cases from the first accepted-state review.
use super::*;
use rpt_advanced_core::config::resources::{PortKey, candidate::ResourceRegistry};
const NEXT: &str = "41000000-0000-4000-8000-000000000001";
const RESTORE: &str = "41000000-0000-4000-8000-000000000002";
const PORT1: &str = "42000000-0000-4000-8000-000000000001";
const PORT2: &str = "42000000-0000-4000-8000-000000000002";
const DEFAULT: &str = "43000000-0000-4000-8000-000000000001";

fn renamed(m: &PublicManifest, id: &str, revision: &str) -> PublicManifest {
    let mut v: serde_json::Value = serde_json::from_slice(&m.encode()).unwrap();
    v["id"] = id.into();
    v["revision"] = revision.into();
    PublicManifest::decode(v.to_string().as_bytes())
        .unwrap()
        .value
}
fn graph(
    revision: &str,
    ports: &[(&str, &str)],
    slot: Option<(ResourceKind, bool)>,
) -> (PublicManifest, Vec<(ResourceId, Vec<u8>)>) {
    let site: ResourceId = SITE.parse().unwrap();
    let (_, site_bytes) = prepared("base");
    let mut objects = Vec::new();
    let mut rows = Vec::new();
    let mut add = |b: ResourceBinding, bytes: Option<Vec<u8>>| {
        let r = bytes
            .as_ref()
            .map(|bytes| ObjectReference::new(revision.parse().unwrap(), hash(bytes)));
        if let Some(bytes) = bytes {
            objects.push((b.id, bytes));
        }
        rows.push((b, r));
    };
    add(
        ResourceBinding {
            id: site,
            scope: ResourceScope::Site(site),
            kind: ResourceKind::Metadata,
        },
        Some(site_bytes),
    );
    let mut keys = Vec::new();
    for &(id, key) in ports {
        let id = id.parse().unwrap();
        keys.push((id, PortKey::new(key)));
        let b = ResourceBinding {
            id,
            scope: ResourceScope::Port(id),
            kind: ResourceKind::Metadata,
        };
        add(b,Some(serde_json::json!({"id":id.to_string(),"kind":"metadata","key":key,"configuration":{"name":key,"inherit_from":"site"}}).to_string().into_bytes()));
    }
    if let Some((kind, present)) = slot {
        add(
            ResourceBinding {
                id: DEFAULT.parse().unwrap(),
                scope: ResourceScope::Site(site),
                kind,
            },
            present.then(|| {
                serde_json::json!({"id":DEFAULT,"kind":kind.as_str(),"configuration":{}})
                    .to_string()
                    .into_bytes()
            }),
        );
    }
    (
        PublicManifest::new(
            revision.parse().unwrap(),
            revision.parse().unwrap(),
            site,
            &keys,
            rows,
        )
        .unwrap(),
        objects,
    )
}
fn install(
    tx: &Transaction<'_, '_>,
    m: &PublicManifest,
    objects: &[(ResourceId, Vec<u8>)],
    admission: Admission,
    jid: &str,
) -> Result<StatePointer, TransactionError> {
    let borrowed: Vec<_> = objects.iter().map(|(id, b)| (*id, b.as_slice())).collect();
    tx.commit(
        admission,
        &tx.inventory().unwrap(),
        m,
        &borrowed,
        jid.parse().unwrap(),
    )
}
fn initial() -> Admission {
    Admission::Provision {
        store: STORE.parse().unwrap(),
    }
}
fn confirmed(tx: &Transaction<'_, '_>) {
    tx.reconcile().unwrap();
    tx.record_activation(&tx.inspect().unwrap(), Activation::Confirmed)
        .unwrap();
}

#[test]
fn review_pointer_barrier_precedes_any_working_write_and_clean_acknowledgment() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let mut tx = Transaction::new(&g, LIMITS);
    let (m, o) = graph("initial", &[], None);
    let desired = |p| {
        if p == Boundary::DesiredSync {
            Err(TransactionError::DurabilityUnknown)
        } else {
            Ok(())
        }
    };
    tx.boundary = Some(&desired);
    assert_eq!(
        install(&tx, &m, &o, initial(), JOURNAL),
        Err(TransactionError::DurabilityUnknown)
    );
    let fail = |p| {
        if p == Boundary::PointerBarrier {
            Err(TransactionError::DurabilityUnknown)
        } else {
            Ok(())
        }
    };
    tx.boundary = Some(&fail);
    assert_eq!(tx.reconcile(), Err(TransactionError::DurabilityUnknown));
    assert!(!fx.0.join("working/site/metadata.json").exists());
    let clean = |p| {
        if p == Boundary::FinalSync {
            Err(TransactionError::DurabilityUnknown)
        } else {
            Ok(())
        }
    };
    tx.boundary = Some(&clean);
    assert_eq!(tx.reconcile(), Err(TransactionError::DurabilityUnknown));
    tx.boundary = Some(&fail);
    assert_eq!(tx.reconcile(), Err(TransactionError::DurabilityUnknown));
}
#[test]
fn review_resume_initial_selection_after_journal_or_partial_layout() {
    let (fx, store) = Fixture::new();
    let (m, o) = graph("initial", &[], None);
    {
        let g = store.try_edit().unwrap();
        let mut tx = Transaction::new(&g, LIMITS);
        let fail = |p| {
            if p == Boundary::Journal {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        tx.boundary = Some(&fail);
        assert_eq!(
            install(&tx, &m, &o, initial(), JOURNAL),
            Err(TransactionError::StorageFault)
        );
    }
    drop(store);
    let resumed = WorkingFiles::provision(&fx.0, STORE.parse().unwrap()).unwrap();
    let g = resumed.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    install(&tx, &m, &o, initial(), JOURNAL).unwrap();
    confirmed(&tx);
    drop(g);
    drop(resumed);
    fs::remove_file(fx.0.join("state.json")).unwrap();
    assert!(WorkingFiles::provision(&fx.0, STORE.parse().unwrap()).is_err());
}
#[test]
fn review_restore_supersedes_interrupted_journal_only_with_observed_working_bytes() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    first(&tx);
    let old = tx.inspect().unwrap();
    let restore = renamed(old.desired.manifest(), "restore", "restore");
    let (second, bytes) = prepared("second");
    install(
        &tx,
        &second,
        &[(SITE.parse().unwrap(), bytes)],
        Admission::Save { state: old.digest },
        NEXT,
    )
    .unwrap();
    let previous = tx.inspect().unwrap();
    let stale = digest(&tx.inventory().unwrap());
    fx.put("working/site/metadata.json", b"third digest");
    assert_eq!(tx.reconcile(), Err(TransactionError::ExternalEdit));
    let admission = |working| Admission::Restore {
        state: previous.digest,
        working,
        retained: old.state.desired.clone(),
    };
    assert!(install(&tx, &restore, &[], admission(stale), RESTORE).is_err());
    assert_eq!(tx.inspect().unwrap().digest, previous.digest);
    install(
        &tx,
        &restore,
        &[],
        admission(digest(&tx.inventory().unwrap())),
        RESTORE,
    )
    .unwrap();
    assert_eq!(
        fs::read(fx.0.join("working/site/metadata.json")).unwrap(),
        b"third digest"
    );
    assert!(fx.0.join(format!("journals/{NEXT}.json")).exists());
    tx.reconcile().unwrap();
    assert_eq!(
        fs::read(fx.0.join("working/site/metadata.json")).unwrap(),
        prepared("first").1
    );
}
fn keys(reg: &ResourceRegistry) -> Vec<&str> {
    reg.port_keys().map(|(k, _)| k.as_str()).collect()
}
#[test]
fn review_port_order_survives_wire_and_reload_appends_in_token_path_order() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    let (m, o) = graph("initial", &[(PORT2, "z"), (PORT1, "a")], None);
    install(&tx, &m, &o, initial(), JOURNAL).unwrap();
    confirmed(&tx);
    let selected = tx.inspect().unwrap();
    let reg = selected.desired.manifest().registry();
    assert_eq!(keys(reg), ["z", "a"]);
    for (token, key) in [("new-z", "!"), ("new-a", "0")] {
        fx.put(&format!("working/ports/{token}/metadata.json"),serde_json::json!({"kind":"metadata","key":key,"configuration":{"name":key,"inherit_from":"site"}}).to_string().as_bytes());
    }
    let draft = import::import_working(&tx.inventory().unwrap(), reg).unwrap();
    assert_eq!(keys(draft.registry()), ["z", "a", "0", "!"]);
}
#[test]
fn review_identical_edited_bytes_and_external_deletion_need_durable_baseline() {
    for (present, point) in [
        (true, Boundary::WorkingFileSync),
        (false, Boundary::WorkingDirSync),
    ] {
        let (fx, store) = Fixture::new();
        let g = store.try_edit().unwrap();
        let mut tx = Transaction::new(&g, LIMITS);
        let (m, o) = graph("initial", &[], Some((ResourceKind::Radio, true)));
        install(&tx, &m, &o, initial(), JOURNAL).unwrap();
        confirmed(&tx);
        let old = tx.inspect().unwrap();
        let (dest, bytes) = prepared("edited");
        fx.put("working/site/metadata.json", &bytes);
        fs::remove_file(fx.0.join("working/site/radio.json")).unwrap();
        let observed = digest(&tx.inventory().unwrap());
        install(
            &tx,
            &dest,
            &[(SITE.parse().unwrap(), bytes)],
            Admission::Reload {
                state: old.digest,
                working: observed,
            },
            NEXT,
        )
        .unwrap();
        let fault = |p| {
            if p == point {
                Err(TransactionError::DurabilityUnknown)
            } else {
                Ok(())
            }
        };
        tx.boundary = Some(&fault);
        assert_eq!(
            tx.reconcile(),
            Err(TransactionError::DurabilityUnknown),
            "present={present}"
        );
        assert!(tx.inspect().unwrap().state.journal.is_some());
        tx.boundary = None;
        tx.reconcile().unwrap();
    }
}
#[test]
fn review_retired_port_directories_free_space_for_the_next_port() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(
        &g,
        CaptureLimits {
            max_paths: 5,
            ..LIMITS
        },
    );
    let (m, o) = graph("initial", &[(PORT1, "first")], None);
    install(&tx, &m, &o, initial(), JOURNAL).unwrap();
    confirmed(&tx);
    for (id, key, rev, j) in [
        (PORT2, "two", "second", NEXT),
        (PORT1, "one", "third", RESTORE),
    ] {
        let prior = tx.inspect().unwrap();
        let (m, o) = graph(rev, &[(id, key)], None);
        install(
            &tx,
            &m,
            &o,
            Admission::Save {
                state: prior.digest,
            },
            j,
        )
        .unwrap();
        confirmed(&tx);
        assert_eq!(fs::read_dir(fx.0.join("working/ports")).unwrap().count(), 1);
    }
}
#[test]
fn review_unpopulated_site_protocol_default_retains_bound_id() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    let (m, o) = graph("initial", &[], Some((ResourceKind::Echolink, false)));
    install(&tx, &m, &o, initial(), JOURNAL).unwrap();
    confirmed(&tx);
    let selected = tx.inspect().unwrap();
    let old = selected.desired.manifest().registry();
    let draft = import::import_working(&tx.inventory().unwrap(), old).unwrap();
    assert!(draft.registry().by_id(DEFAULT.parse().unwrap()).is_some());
    fx.put(
        "working/site/protocol-defaults/echolink.json",
        serde_json::json!({"id":DEFAULT,"kind":"echolink","configuration":{}})
            .to_string()
            .as_bytes(),
    );
    assert!(
        import::import_working(&tx.inventory().unwrap(), draft.registry())
            .unwrap()
            .envelope(DEFAULT.parse().unwrap())
            .is_some()
    );
}
#[test]
fn review_unrepresentable_confirmation_rejected_before_first_selection() {
    let (_fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let tx = Transaction::new(&g, LIMITS);
    let (m, o) = graph("base", &[], None);
    let m = renamed(&m, "x", &"x".repeat(3500));
    assert_eq!(
        install(&tx, &m, &o, initial(), JOURNAL),
        Err(TransactionError::InvalidCandidate)
    );
    assert_eq!(g.read_state(10000).unwrap(), None);
}
#[test]
fn review_post_promotion_recovery_capacity_before_first_selection() {
    let (m, objects) = graph("base", &[], None);
    let manifest = renamed(&m, "x", &"x".repeat(3171));
    // With the explicit empty retained_recovery array, confirmation takes 10,023 bytes.
    // A later recovery_required retains the promoted LKG and takes eight more bytes.
    for limit in [10_023, 10_030, 10_031] {
        let (_fixture, store) = Fixture::new();
        let guard = store.try_edit().unwrap();
        let tx = Transaction::new(
            &guard,
            CaptureLimits {
                max_file_bytes: limit,
                ..LIMITS
            },
        );
        let result = install(&tx, &manifest, &objects, initial(), JOURNAL);
        if limit < 10_031 {
            assert_eq!(result, Err(TransactionError::InvalidCandidate));
            assert!(guard.read_state(limit).unwrap().is_none());
        } else {
            result.unwrap();
            confirmed(&tx);
            assert_eq!(guard.read_state(limit).unwrap().unwrap().len(), 10_023);
            tx.record_activation(&tx.inspect().unwrap(), Activation::RecoveryRequired)
                .unwrap();
            assert_eq!(guard.read_state(limit).unwrap().unwrap().len(), 10_031);
            let selected = tx.inspect().unwrap();
            assert_eq!(
                selected.state.last_known_good.as_ref(),
                Some(&selected.state.desired)
            );
            assert_eq!(selected.state.activation, Activation::RecoveryRequired);
        }
    }
}

#[test]
fn review_omitted_orphan_requires_its_directory_sync_before_selection() {
    let (fx, store) = Fixture::new();
    let g = store.try_edit().unwrap();
    let mut tx = Transaction::new(&g, LIMITS);
    first(&tx);
    let old = tx.inspect().unwrap();
    let (dest, bytes) = prepared("orphan");
    fx.put(&format!("objects/{SITE}/orphan.json"), &bytes);
    let fault = |p| {
        if p == Boundary::OrphanSync {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    tx.boundary = Some(&fault);
    assert_eq!(
        install(&tx, &dest, &[], Admission::Save { state: old.digest }, NEXT),
        Err(TransactionError::StorageFault)
    );
    assert_eq!(tx.inspect().unwrap().digest, old.digest);
}
