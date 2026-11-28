//! End-to-end encrypted selection and omission across real edit-guard commits.

pub(super) mod maintenance;
mod review;
use super::*;
use crate::configuration_private::{CredentialEdit, SecretsState};
use rpt_advanced_core::{
    config::resources::profiles::credentials::CredentialSlot,
    management::patch::{PatchLimits, PatchPolicy, stage_patch},
};
use serde_json::{Value, json};

const INSTANCE: &str = "afcb12fa-16c3-42a6-ab2d-072ba822a29a";
const RECREATED: &str = "7802fb22-c643-473a-a0fe-c3b06861bb6d";
const ACCOUNT_SECRET: &str = "synthetic\"account\n\t\u{1F600}";
const TEST_NAME: &str = "configuration_transaction::tests::private::account_state_is_atomic_and_missing_key_leaves_public_management_readable";

fn write(value: Option<Value>, id: ResourceId, slot: CredentialSlot) -> CredentialEdit {
    let operation = match value {
        Some(v) => json!({"op":"replace","path":"/write","value":v}),
        None => json!({"op":"remove","path":"/write"}),
    };
    let patch = stage_patch(
        &json!({}),
        &serde_json::to_vec(&json!([operation])).unwrap(),
        &PatchPolicy {
            writable: &[""],
            server_owned: &[],
            secret_slots: &["/write"],
        },
        PatchLimits {
            request_bytes: 2048,
            operations: 1,
            pointer_bytes: 64,
            pointer_tokens: 4,
            number_bytes: 32,
            stage_bytes: 2048,
            stage_depth: 8,
        },
    )
    .unwrap();
    CredentialEdit {
        resource: id,
        slot,
        change: patch.private_edits.into_iter().next().unwrap().change,
    }
}

fn commit_account(
    tx: &Transaction<'_, '_>,
    revision: &str,
    protocol: Option<(&str, u32)>,
    edits: &[CredentialEdit],
) -> Result<(StatePointer, usize), TransactionError> {
    commit_account_body(
        tx,
        revision,
        protocol.map(|(id, node)| (id, json!({"node_number":node}))),
        edits,
    )
}

fn commit_account_body(
    tx: &Transaction<'_, '_>,
    revision: &str,
    protocol: Option<(&str, Value)>,
    edits: &[CredentialEdit],
) -> Result<(StatePointer, usize), TransactionError> {
    let site: ResourceId = SITE.parse().unwrap();
    let metadata = ResourceBinding {
        id: site,
        scope: ResourceScope::Site(site),
        kind: ResourceKind::Metadata,
    };
    let mut data = vec![(metadata, json!({"name":"test","inherit_from":null}))];
    if let Some((id, body)) = protocol {
        data.push((
            ResourceBinding {
                id: id.parse().unwrap(),
                scope: metadata.scope,
                kind: ResourceKind::Allstarlink,
            },
            body,
        ));
    }
    let docs = data
        .into_iter()
        .map(|(b, config)| {
            (
                b,
                serde_json::to_vec(
                    &json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":config}),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let rows = docs.iter().map(|(binding, bytes)| {
        let digest = hash(bytes);
        let version = digest
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .parse()
            .unwrap();
        (*binding, Some(ObjectReference::new(version, digest)))
    });
    let manifest = PublicManifest::new(
        revision.parse().unwrap(),
        revision.parse().unwrap(),
        site,
        &[],
        rows,
    )
    .unwrap();
    let admission = if revision == "first" {
        Admission::Provision {
            store: STORE.parse().unwrap(),
        }
    } else {
        Admission::Save {
            state: tx.inspect()?.digest,
        }
    };
    let source = tx.inventory()?;
    tx.commit_credentials(
        admission,
        &source,
        &manifest,
        &docs
            .iter()
            .map(|(b, bytes)| (b.id, bytes.as_slice()))
            .collect::<Vec<_>>(),
        rptadv_peer_support::working_files::transaction::new_identity()
            .unwrap()
            .to_string()
            .parse()
            .unwrap(),
        edits,
    )
}

fn check_private_fields() {
    use crate::configuration_private::public_envelope;
    for (kind, body) in [
        (
            ResourceKind::Allstarlink,
            json!({"credentials":{"password":"x"}}),
        ),
        (
            ResourceKind::Echolink,
            json!({"proxy":{"credentials":{"password":"x"}}}),
        ),
        (
            ResourceKind::EcholinkProxy,
            json!({"credentials":{"password_configured":false}}),
        ),
        (
            ResourceKind::PhonePatch,
            json!({"credentials":{"password":""}}),
        ),
        (ResourceKind::Command, json!({"admin":{"unlock_hash":"x"}})),
        (ResourceKind::Mailbox, json!({"pin_hash":"x"})),
    ] {
        let id = SITE.parse().unwrap();
        let binding = ResourceBinding {
            id,
            scope: ResourceScope::Site(id),
            kind,
        };
        assert!(!public_envelope(
            binding,
            &serde_json::to_vec(&json!({"id":SITE,"kind":kind.as_str(),"configuration":body}))
                .unwrap()
        ));
    }
}

fn check_inheritance_and_hashes(tx: &Transaction<'_, '_>, key: &KeyId) {
    use crate::configuration_private::{Credentials, PrivateStore};
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::ResourceRegistry,
        profiles::credentials::{CredentialOwner, CredentialOwners},
    };
    const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";
    let id = |n: u64| {
        format!("21271e86-b1d8-490a-b576-{n:012}")
            .parse::<ResourceId>()
            .unwrap()
    };
    let site = ResourceScope::Site(id(1));
    let port = ResourceScope::Port(id(2));
    let binding = |n, scope, kind| ResourceBinding {
        id: id(n),
        scope,
        kind,
    };
    let store = PrivateStore {
        guard: tx.guard,
        store: STORE.parse().unwrap(),
        limits: LIMITS,
        write_key: Some(key),
    };
    let owners = |shadow: bool| {
        let mut map = CredentialOwners::new();
        for (n, scope, kind, slot, affinity) in [
            (
                3,
                site,
                ResourceKind::Command,
                CredentialSlot::Unlock,
                Value::Null,
            ),
            (
                4,
                port,
                ResourceKind::Command,
                CredentialSlot::Unlock,
                Value::Null,
            ),
            (
                5,
                site,
                ResourceKind::Mailbox,
                CredentialSlot::Pin,
                json!(["001"]),
            ),
            (
                6,
                port,
                ResourceKind::Mailbox,
                CredentialSlot::Pin,
                json!(["001"]),
            ),
            (
                7,
                site,
                ResourceKind::PhonePatch,
                CredentialSlot::Phone,
                json!(["sip", "test", 5060, "u"]),
            ),
            (
                8,
                port,
                ResourceKind::PhonePatch,
                CredentialSlot::Phone,
                json!(["sip", "test", 5060, "u"]),
            ),
        ] {
            if n != 6 || shadow {
                map.insert(
                    (id(n), slot),
                    CredentialOwner {
                        binding: binding(n, scope, kind),
                        parent: None,
                        affinity: Some(affinity),
                    },
                );
            }
        }
        map
    };
    let prepare = |old: &Credentials, shadow: bool, edits: &[CredentialEdit]| {
        store.prepare(old, &owners(shadow), edits)
    };
    let select = |root, shadow| {
        store
            .select(root, Ok(owners(shadow)))
            .into_credentials()
            .unwrap()
    };
    let edits = [
        write(
            Some(json!(PHC.replace("m=19456", "m=8192"))),
            id(3),
            CredentialSlot::Unlock,
        ),
        write(Some(json!(PHC)), id(5), CredentialSlot::Pin),
        write(Some(json!("site-phone")), id(7), CredentialSlot::Phone),
    ];
    let (base, _) = prepare(&Credentials::default(), false, &edits).unwrap();
    let base = select(base, false);
    assert_eq!(base.configured(id(4), CredentialSlot::Unlock), Some(true));
    assert_eq!(base.configured(id(8), CredentialSlot::Phone), Some(true));
    // Both live accounts select one authenticated allocation in this generation.
    let retained_phone = base.retain(id(8), CredentialSlot::Phone).unwrap();
    let site_phone = base.retain(id(7), CredentialSlot::Phone).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &retained_phone.bytes,
        &site_phone.bytes
    ));
    let after_retirement = std::sync::Arc::downgrade(&retained_phone.bytes);
    assert!(site_phone.same_version(&retained_phone));
    let reopened = select(base.reference(), false);
    let independently_opened = reopened.retain(id(8), CredentialSlot::Phone).unwrap();
    assert!(retained_phone.same_version(&independently_opened));
    assert!(!std::sync::Arc::ptr_eq(
        &retained_phone.bytes,
        &independently_opened.bytes
    ));
    drop(independently_opened);
    drop(reopened);
    drop(site_phone);
    // New local mailbox cannot borrow the site PIN. A retained PHC is required.
    assert!(prepare(&base, true, &[]).is_err());
    assert!(
        prepare(
            &base,
            true,
            &[write(Some(json!("invalid")), id(6), CredentialSlot::Pin)]
        )
        .is_err()
    );
    let edits = [
        write(Some(json!(PHC)), id(6), CredentialSlot::Pin),
        write(Some(json!("")), id(4), CredentialSlot::Unlock),
        write(None, id(8), CredentialSlot::Phone),
    ];
    let (shadow, _) = prepare(&base, true, &edits).unwrap();
    let shadow = select(shadow, true);
    assert_eq!(
        shadow.configured(id(4), CredentialSlot::Unlock),
        Some(false)
    );
    assert_eq!(shadow.configured(id(8), CredentialSlot::Phone), Some(false));
    assert_eq!(shadow.configured(id(6), CredentialSlot::Pin), Some(true));
    assert!(shadow.retain(id(8), CredentialSlot::Phone).is_none());
    assert!(shadow.retain(id(4), CredentialSlot::Unlock).is_none());
    drop(base);
    assert_eq!(retained_phone.bytes.expose_bytes(), b"site-phone");
    drop(retained_phone);
    assert!(after_retirement.upgrade().is_none());
    assert!(prepare(&shadow, true, &[write(None, id(6), CredentialSlot::Pin)]).is_err());
    let (retained, warnings) = prepare(
        &shadow,
        true,
        &[write(Some(json!("invalid")), id(6), CredentialSlot::Pin)],
    )
    .unwrap();
    assert_eq!(warnings, 1);
    assert_eq!(retained, shadow.reference());
    let (unshadowed, _) = prepare(&shadow, false, &[]).unwrap();
    let recovered = select(unshadowed, false);
    assert_eq!(recovered.configured(id(6), CredentialSlot::Pin), None);
    assert_eq!(recovered.configured(id(5), CredentialSlot::Pin), Some(true));
    let number = PortKey::new("001");
    let site_member = (binding(5, site, ResourceKind::Mailbox), number.clone());
    let local = (binding(6, port, ResourceKind::Mailbox), number.clone());
    for (members, wanted) in [
        (vec![site_member.clone(), local], id(6)),
        (vec![site_member], id(5)),
    ] {
        let registry =
            ResourceRegistry::with_members(id(1), &[(id(2), PortKey::new("port"))], &[], &members)
                .unwrap();
        assert_eq!(
            registry.effective_mailbox(port, &number).unwrap().id,
            wanted
        );
    }
}

fn activate(tx: &Transaction<'_, '_>) {
    tx.reconcile().unwrap();
    tx.record_activation(&tx.inspect().unwrap(), Activation::Confirmed)
        .unwrap();
}

#[test]
fn account_state_is_atomic_and_missing_key_leaves_public_management_readable() {
    // Scope the systemd environment to this fresh test process, without mutating other tests.
    if std::env::var_os("RPTADV_TEST_PRIVATE_CHILD")
        .is_none_or(|expected| std::env::var_os("CREDENTIALS_DIRECTORY") != Some(expected))
    {
        let (fixture, _store) = Fixture::new();
        fixture.put("keys/rpt-advanced.aead.example", &[9; 32]);
        fixture.put("keys/rpt-advanced.aead.alternate", &[3; 32]);
        let dir = fixture.0.join("keys");
        fs::set_permissions(
            dir.join("rpt-advanced.aead.alternate"),
            fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        fs::set_permissions(
            dir.join("rpt-advanced.aead.example"),
            fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env("RPTADV_TEST_PRIVATE_CHILD", &dir)
            .env("CREDENTIALS_DIRECTORY", &dir)
            .status()
            .unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.success());
        return;
    }
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let key = KeyId::new("example").unwrap();
    let tx = Transaction::new(&guard, LIMITS).with_write_key(&key);
    let id = INSTANCE.parse().unwrap();
    let slot = CredentialSlot::AllStarLink;
    let account = Some((INSTANCE, 123));
    check_inheritance_and_hashes(&tx, &key);
    check_private_fields();
    let token = rptadv_json::decode_borrowed(br#""synthetic\"account\n\t\uD83D\uDE00""#).unwrap();
    let first_edit = CredentialEdit {
        resource: id,
        slot,
        change: rpt_advanced_core::management::patch::PrivateChange::Set(
            rpt_advanced_core::management::patch::PrivateValue::from_token(&token),
        ),
    };
    let (_, warnings) = commit_account(&tx, "first", account, &[first_edit]).unwrap();
    assert_eq!(warnings, 0);
    let selected = tx.inspect().unwrap();
    assert_eq!(selected.private.configured(id, slot), Some(true));
    let reference = selected.desired.manifest().private().unwrap();
    let disk = fs::read(fixture.0.join("manifests/first.json")).unwrap();
    assert!(!String::from_utf8_lossy(&disk).contains(ACCOUNT_SECRET));
    activate(&tx);
    let alternate = KeyId::new("alternate").unwrap();
    review::mixed_key_selection(&tx, &fixture, &key, &alternate);
    review::tolerant_authoring(&tx);
    let wrong = Transaction::new(&guard, LIMITS).with_write_key(&alternate);
    assert_eq!(
        wrong.inspect().unwrap().private.state(),
        SecretsState::Locked
    );
    assert_eq!(
        commit_account(&wrong, "wrong", account, &[]).unwrap_err(),
        TransactionError::SecretsUnavailable
    );
    let fault = |point| {
        if point == Boundary::PrivateArtifacts {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    let mut interrupted = Transaction::new(&guard, LIMITS).with_write_key(&key);
    interrupted.boundary = Some(&fault);
    assert_eq!(
        commit_account(
            &interrupted,
            "interrupted",
            account,
            &[write(Some(json!("replacement")), id, slot)]
        )
        .unwrap_err(),
        TransactionError::StorageFault
    );
    assert_eq!(
        tx.inspect().unwrap().desired.manifest().private(),
        Some(reference)
    );
    let encrypted_path = fixture.0.join(format!(
        "private/{}/{}.sealed.json",
        reference.record, reference.version
    ));
    let original = fs::read(&encrypted_path).unwrap();
    assert!(!String::from_utf8_lossy(&original).contains(ACCOUNT_SECRET));
    fs::write(&encrypted_path, b"invalid private index").unwrap();
    let broken = tx.inspect().unwrap();
    assert_eq!(broken.private.state(), SecretsState::Error);
    assert!(broken.desired.assemble_candidate().is_ok());
    assert_eq!(tx.reconcile(), Err(TransactionError::SecretsUnavailable));
    fs::write(&encrypted_path, original).unwrap();
    let (_, warnings) = commit_account(
        &tx,
        "unrelated",
        account,
        &[write(Some(json!(42)), id, slot)],
    )
    .unwrap();
    assert_eq!(warnings, 1);
    let selected = tx.inspect().unwrap();
    assert_eq!(selected.desired.manifest().private(), Some(reference));
    assert_eq!(
        selected
            .private
            .credentials()
            .unwrap()
            .get(id, slot)
            .unwrap()
            .expose_bytes(),
        ACCOUNT_SECRET.as_bytes()
    );
    activate(&tx);
    commit_account(&tx, "changed", Some((INSTANCE, 124)), &[]).unwrap();
    assert_eq!(
        tx.inspect().unwrap().private.configured(id, slot),
        Some(false)
    );
    let pending = tx.inspect().unwrap();
    let retained = pending.last_known_good.as_ref().unwrap().manifest();
    let mut restored: Value = serde_json::from_slice(&retained.encode()).unwrap();
    restored["id"] = json!("restored");
    restored["revision"] = json!("restored");
    let restore = |wire: &Value| {
        let manifest = PublicManifest::decode(&serde_json::to_vec(wire).unwrap())
            .unwrap()
            .value;
        let snapshot = tx.inventory().unwrap();
        tx.commit(
            Admission::Restore {
                state: pending.digest,
                working: digest(&snapshot),
                retained: pending.state.last_known_good.clone().unwrap(),
            },
            &snapshot,
            &manifest,
            &[],
            rptadv_peer_support::working_files::transaction::new_identity()
                .unwrap()
                .to_string()
                .parse()
                .unwrap(),
        )
    };
    let mut substituted = restored.clone();
    substituted["private"] = pending.desired.manifest().private().unwrap().encode();
    assert_eq!(
        restore(&substituted).unwrap_err(),
        TransactionError::InvalidCandidate
    );
    restore(&restored).unwrap();
    assert_eq!(
        tx.inspect().unwrap().private.configured(id, slot),
        Some(true)
    );
    activate(&tx);
    commit_account(&tx, "changed-again", Some((INSTANCE, 124)), &[]).unwrap();
    activate(&tx);
    commit_account(&tx, "reverted", account, &[]).unwrap();
    assert_eq!(
        tx.inspect().unwrap().private.configured(id, slot),
        Some(false)
    );
    activate(&tx);
    commit_account(
        &tx,
        "reset",
        account,
        &[write(Some(json!(ACCOUNT_SECRET)), id, slot)],
    )
    .unwrap();
    activate(&tx);
    commit_account(&tx, "removed", account, &[write(None, id, slot)]).unwrap();
    assert_eq!(
        tx.inspect().unwrap().private.configured(id, slot),
        Some(false)
    );
    activate(&tx);
    commit_account(
        &tx,
        "reset-again",
        account,
        &[write(Some(json!(ACCOUNT_SECRET)), id, slot)],
    )
    .unwrap();
    activate(&tx);
    // A missing key blocks replay, save, and activation while public objects remain accessible.
    let dir = std::path::PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY").unwrap());
    let path = dir.join("rpt-advanced.aead.example");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&path, dir.join("saved")).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let locked = tx.inspect().unwrap();
    assert_eq!(locked.status, StartupStatus::SecretsUnavailable);
    assert_eq!(locked.private.state(), SecretsState::Locked);
    assert_eq!(locked.private.configured(id, slot), None);
    assert!(locked.desired.assemble_candidate().is_ok());
    assert_eq!(tx.reconcile(), Err(TransactionError::SecretsUnavailable));
    assert_eq!(
        commit_account(&tx, "blocked", None, &[]).unwrap_err(),
        TransactionError::SecretsUnavailable
    );
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(dir.join("saved"), path).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    commit_account(&tx, "deleted", None, &[]).unwrap();
    activate(&tx);
    commit_account(&tx, "recreated", Some((RECREATED, 123)), &[]).unwrap();
    let selected = tx.inspect().unwrap();
    assert_eq!(
        selected
            .private
            .configured(RECREATED.parse().unwrap(), slot),
        Some(false)
    );
    assert!(selected.desired.manifest().private().is_none());
    activate(&tx);
    let missing = KeyId::new("not-delivered").unwrap();
    let locked = Transaction::new(&guard, LIMITS).with_write_key(&missing);
    let selected = locked.inspect().unwrap();
    assert_eq!(selected.private.state(), SecretsState::Locked);
    assert!(selected.desired.assemble_candidate().is_ok());
    assert_eq!(
        commit_account(&locked, "blocked-public", None, &[]).unwrap_err(),
        TransactionError::SecretsUnavailable
    );
}

#[test]
fn import_stage_distinguishes_wrong_admin_shape_from_reserved_private_members() {
    use crate::configuration_private::{public_envelope, public_input_envelope};
    let id = SITE.parse().unwrap();
    let b = ResourceBinding {
        id,
        scope: ResourceScope::Site(id),
        kind: ResourceKind::Command,
    };
    let encode = |admin| {
        serde_json::to_vec(
            &serde_json::json!({"id":SITE,"kind":"command","configuration":{"admin":admin}}),
        )
        .unwrap()
    };
    for value in [
        serde_json::json!(null),
        serde_json::json!(false),
        serde_json::json!([1, 2]),
    ] {
        let raw = encode(value);
        assert!(public_input_envelope(b, &raw));
        assert!(!public_envelope(b, &raw));
    }
    for value in [
        serde_json::json!({"unlock_hash":"forbidden"}),
        serde_json::json!([{"lock_hash":"forbidden"}]),
    ] {
        let raw = encode(value);
        assert!(!public_input_envelope(b, &raw));
        assert!(!public_envelope(b, &raw));
    }
}
