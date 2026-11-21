//! Regression coverage for the encrypted graph adoption review.
use super::*;
use rpt_advanced_core::config::resources::manifest::PrivateReference;
use rptadv_peer_support::{
    sealed_records::{RecordIdentity, RecordScope, SealedLimits, SecretBytes, load_systemd_key},
    working_files::immutable::private_records::PreparedPrivateRecord,
};

fn rejected_public_object(declared_kind: &str, declared_id: &str, config: Value) {
    let (fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let tx = Transaction::new(&guard, LIMITS);
    let site: ResourceId = SITE.parse().unwrap();
    let (meta, bytes) = prepared("public");
    let id: ResourceId = INSTANCE.parse().unwrap();
    let binding = ResourceBinding {
        id,
        scope: ResourceScope::Site(site),
        kind: ResourceKind::Allstarlink,
    };
    let supplied =
        json!({"id":declared_id,"kind":declared_kind,"configuration":config}).to_string();
    let manifest = PublicManifest::new(
        "public".parse().unwrap(),
        "public".parse().unwrap(),
        site,
        &[],
        [
            (
                meta.registry().by_id(site).unwrap(),
                meta.reference(site).cloned(),
            ),
            (
                binding,
                Some(ObjectReference::new(
                    "public".parse().unwrap(),
                    hash(supplied.as_bytes()),
                )),
            ),
        ],
    )
    .unwrap();
    let result = tx.commit(
        Admission::Provision {
            store: STORE.parse().unwrap(),
        },
        &tx.inventory().unwrap(),
        &manifest,
        &[(site, &bytes), (id, supplied.as_bytes())],
        JOURNAL.parse().unwrap(),
    );
    assert_eq!(result.unwrap_err(), TransactionError::InvalidCandidate);
    assert_eq!(
        fs::read_dir(fixture.0.join("objects")).unwrap().count(),
        0,
        "a rejected batch installed public artifacts"
    );
    assert!(!fixture.0.join("state.json").exists());
}

#[test]
fn review_container_rejected_before_any_public_install() {
    for config in [
        json!({"credentials":"fixture"}),
        json!({"credentials":[{"password":"fixture"}]}),
        json!({"proxy":{"credentials":["fixture"]}}),
    ] {
        rejected_public_object("allstarlink", INSTANCE, config);
    }
}

#[test]
fn review_declared_identity_checked_before_any_public_install() {
    rejected_public_object("phone-patch", INSTANCE, json!({"credentials":"fixture"}));
    rejected_public_object("allstarlink", RECREATED, json!({}));
}

/// Raw authoring warnings are value-free. Only the normalized profile enters immutable storage.
pub(super) fn tolerant_authoring(tx: &Transaction<'_, '_>) {
    use rpt_advanced_core::config::resources::{
        ResourceWarning,
        candidate::{CapturedResource, ModularCandidate},
    };
    let selected = tx.inspect().unwrap();
    let manifest = selected.desired.manifest();
    let id: ResourceId = INSTANCE.parse().unwrap();
    let authored=json!({"id":INSTANCE,"kind":"allstarlink","configuration":{"node_number":123,"credentials":[{"password":"fixture"}],"proxy":{"credentials":"fixture"}}}).to_string();
    let captures: Vec<_> = selected
        .desired
        .verified_objects()
        .map(|(key, body)| CapturedResource {
            binding: manifest.registry().by_id(key).unwrap(),
            json: if key == id { authored.as_bytes() } else { body },
        })
        .collect();
    let candidate = ModularCandidate::assemble(manifest.registry(), &captures).unwrap();
    let profiles = candidate.resolve_protocol_profiles().unwrap();
    assert_eq!(
        profiles
            .warnings()
            .iter()
            .filter(|w| w.resource.id == id && w.warning == ResourceWarning::UnknownField)
            .count(),
        2
    );
    let public = Value::Object(profiles.local(id).unwrap().configuration.clone());
    assert_eq!(public, json!({"node_number":123}));
    commit_account_body(tx, "normalized", Some((INSTANCE, public)), &[]).unwrap();
    let accepted = tx.inspect().unwrap();
    assert_eq!(accepted.desired.manifest().private(), manifest.private());
    assert!(
        accepted
            .private
            .credentials()
            .unwrap()
            .get(id, CredentialSlot::AllStarLink)
            .is_some()
    );
    activate(tx);
}

/// Materialize a retained fixture whose authenticated leaf used a different delivered key.
pub(super) fn mixed_key_selection(
    tx: &Transaction<'_, '_>,
    fixture: &Fixture,
    selected_key: &KeyId,
    other_key: &KeyId,
) {
    let selected = tx.inspect().unwrap();
    let store = selected.state.store;
    let root = selected.desired.manifest().private().unwrap();
    let limits = SealedLimits {
        max_plaintext_bytes: 10000,
        max_document_bytes: 10000,
    };
    let scope =
        |r: PrivateReference| RecordScope::new(&store.to_string(), &r.record.to_string()).unwrap();
    let open = |r: PrivateReference| {
        tx.guard
            .open_private_record(
                RecordIdentity::new(scope(r), &r.version.to_string()).unwrap(),
                r.sha256.as_bytes(),
                tx.artifact_limits(),
                limits,
            )
            .unwrap()
    };
    let reseal = |r: PrivateReference, bytes: &SecretBytes, key: &KeyId| {
        let record =
            PreparedPrivateRecord::seal(&load_systemd_key(key).unwrap(), &scope(r), bytes, limits)
                .unwrap();
        let stored = tx
            .guard
            .install_private_record(&record, tx.artifact_limits())
            .unwrap();
        PrivateReference {
            record: r.record,
            version: stored
                .identity()
                .object_version()
                .to_string()
                .parse()
                .unwrap(),
            sha256: ContentHash::from_bytes(*stored.sha256()),
        }
    };
    let mut index: Value = serde_json::from_slice(open(root).plaintext().expose_bytes()).unwrap();
    let reference = &mut index["records"][0][1];
    let leaf = PrivateReference::decode(reference).unwrap();
    *reference = reseal(leaf, open(leaf).plaintext(), other_key).encode();
    let changed = reseal(
        root,
        &SecretBytes::new(serde_json::to_vec(&index).unwrap()),
        selected_key,
    );
    let manifest_path = fixture.0.join("manifests/first.json");
    let state_path = fixture.0.join("state.json");
    let original_manifest = fs::read(&manifest_path).unwrap();
    let original_state = fs::read(&state_path).unwrap();
    let manifest = selected
        .desired
        .manifest()
        .encode_with_private(Some(changed));
    let mut state = selected.state;
    state.desired.sha256 = hash(&manifest);
    state.last_known_good = Some(state.desired.clone());
    fs::write(&manifest_path, manifest).unwrap();
    fs::write(&state_path, state.encode().unwrap()).unwrap();
    let mixed = tx.inspect().unwrap();
    assert_eq!(mixed.status, StartupStatus::SecretsUnavailable);
    assert!(mixed.desired.assemble_candidate().is_ok());
    assert!(
        mixed
            .private
            .configured(INSTANCE.parse().unwrap(), CredentialSlot::AllStarLink)
            .is_none()
    );
    assert_eq!(tx.reconcile(), Err(TransactionError::SecretsUnavailable));
    assert_eq!(
        tx.record_activation(&mixed, Activation::Confirmed)
            .unwrap_err()
            .error,
        TransactionError::SecretsUnavailable
    );
    assert_eq!(
        commit_account(tx, "mixed", Some((INSTANCE, 123)), &[]).unwrap_err(),
        TransactionError::SecretsUnavailable
    );
    fs::write(manifest_path, original_manifest).unwrap();
    fs::write(state_path, original_state).unwrap();
}
