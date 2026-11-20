//! Candidate assembly and registry behavior from captured documents.

use super::*;
use crate::config::resources::ScopeParent;
use serde_json::json;

const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const FIRST: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SECOND: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const SECTION: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const OTHER_SECTION: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const FIRST_KEY: &str = " /../Radio:é #;\t ";
const SECOND_KEY: &str = " /../radio:é #;\t ";

const SITE_JSON: &[u8] = br#"{
    "id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","kind":"metadata",
    "configuration":{"name":"Station","inherit_from":null,"enabled":false,
                     "callsign":"N0SITE","language":"fr-ca"}
}"#;
const FIRST_JSON: &str = r#"{
    "id":"BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB", "kind":"metadata",
    "key":" /../Radio:é #;\t ",
    "configuration":{"name":"  Main repeater #1 ;  ","inherit_from":"site","callsign":""}
}"#;
const SECOND_JSON: &[u8] = br#"{
    "id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","kind":"metadata",
    "configuration":{"name":"Backup","inherit_from":"site","enabled":true}
}"#;

fn id(value: &str) -> ResourceId {
    value.parse().unwrap()
}

fn site() -> ResourceScope {
    ResourceScope::Site(id(SITE))
}

fn first() -> ResourceScope {
    ResourceScope::Port(id(FIRST))
}

fn binding(id: &str, scope: ResourceScope, kind: ResourceKind) -> ResourceBinding {
    ResourceBinding {
        id: self::id(id),
        scope,
        kind,
    }
}

fn capture(binding: ResourceBinding, json: &[u8]) -> CapturedResource<'_> {
    CapturedResource { binding, json }
}

fn metadata(site_json: &[u8]) -> [CapturedResource<'_>; 3] {
    [
        capture(binding(SITE, site(), ResourceKind::Metadata), site_json),
        capture(
            binding(FIRST, first(), ResourceKind::Metadata),
            FIRST_JSON.as_bytes(),
        ),
        capture(
            binding(
                SECOND,
                ResourceScope::Port(id(SECOND)),
                ResourceKind::Metadata,
            ),
            SECOND_JSON,
        ),
    ]
}

fn registry(resources: &[ResourceBinding]) -> ResourceRegistry {
    ResourceRegistry::new(
        id(SITE),
        &[
            (id(FIRST), PortKey::new(FIRST_KEY)),
            (id(SECOND), PortKey::new(SECOND_KEY)),
        ],
        resources,
    )
    .unwrap()
}

#[test]
fn exact_port_identities_survive_changes_to_inherited_metadata() {
    let registry = registry(&[]);
    assert_eq!(registry.port_id(&PortKey::new(FIRST_KEY)), Some(id(FIRST)));
    assert_eq!(
        registry.port_id(&PortKey::new(SECOND_KEY)),
        Some(id(SECOND))
    );
    assert_eq!(registry.port_id(&PortKey::new(FIRST_KEY.trim())), None);
    assert_eq!(
        id(FIRST).to_string(),
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    );
    assert_eq!(
        registry
            .binding(site(), ResourceKind::Metadata)
            .map(|binding| binding.id),
        Some(id(SITE))
    );

    let original = ModularCandidate::assemble(&registry, &metadata(SITE_JSON)).unwrap();
    let first_port = original.port_metadata(id(FIRST)).unwrap();
    let second_port = original.port_metadata(id(SECOND)).unwrap();
    assert_eq!(original.site_metadata().local().name, "Station");
    assert_eq!(first_port.local().name, "  Main repeater #1 ;  ");
    assert_eq!(first_port.local().inherit_from, ScopeParent::Site);
    assert_eq!(first_port.local().enabled, None);
    let effective = first_port.effective(original.site_metadata());
    assert!(!effective.enabled);
    assert_eq!(effective.callsign, "");
    assert_eq!(effective.language, "fr-CA");
    let effective = second_port.effective(original.site_metadata());
    assert_eq!(effective.name, "Backup");
    assert!(effective.enabled);
    assert_eq!(effective.callsign, "N0SITE");

    let updated = ModularCandidate::assemble(
        &registry,
        &metadata(
            br#"{
            "id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","kind":"metadata",
            "configuration":{"name":"Updated site","inherit_from":null,"callsign":"N1NEW",
                             "language":"en-gb"}
        }"#,
        ),
    )
    .unwrap();
    let effective = updated
        .port_metadata(id(FIRST))
        .unwrap()
        .effective(updated.site_metadata());
    assert_eq!(effective.name, "  Main repeater #1 ;  ");
    assert!(effective.enabled);
    assert_eq!(effective.callsign, "");
    assert_eq!(effective.language, "en-GB");
    assert_eq!(
        updated
            .port_metadata(id(SECOND))
            .unwrap()
            .effective(updated.site_metadata())
            .callsign,
        "N1NEW"
    );
    assert_eq!(registry.port_id(&PortKey::new(FIRST_KEY)), Some(id(FIRST)));
}

#[test]
fn missing_ordinary_documents_keep_their_bound_empty_overrides() {
    let rx = binding(SECTION, first(), ResourceKind::RxAudio);
    let site_control = binding(OTHER_SECTION, site(), ResourceKind::Control);
    let registry = registry(&[rx, site_control]);
    let captures = metadata(SITE_JSON);
    let missing = ModularCandidate::assemble(&registry, &captures).unwrap();
    let missing_rx = missing.staged(first(), ResourceKind::RxAudio).unwrap();
    assert_eq!(missing_rx.binding(), rx);
    assert!(missing_rx.is_empty());
    let missing_control = missing.staged(site(), ResourceKind::Control).unwrap();
    assert_eq!(missing_control.binding(), site_control);
    assert!(missing_control.is_empty());

    let input = br#"{
        "id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","kind":"rx-audio",
        "configuration":{"deemphasis":{"enabled":true}}
    }"#
    .to_vec();
    let edited = {
        let mut captures = captures.to_vec();
        captures.push(capture(rx, &input));
        ModularCandidate::assemble(&registry, &captures).unwrap()
    };
    drop(input);
    let staged = edited.staged(first(), ResourceKind::RxAudio).unwrap();
    assert_eq!(staged.binding(), rx);
    assert!(!staged.is_empty());
    assert_eq!(staged.fields["deemphasis"], json!({"enabled": true}));

    let removed = ModularCandidate::assemble(&registry, &captures).unwrap();
    assert_eq!(
        removed
            .staged(first(), ResourceKind::RxAudio)
            .unwrap()
            .binding(),
        rx
    );
    assert!(
        removed
            .staged(first(), ResourceKind::RxAudio)
            .unwrap()
            .is_empty()
    );
    assert!(removed.staged(first(), ResourceKind::TxAudio).is_none());
}

#[test]
fn protocol_presence_requires_an_explicit_capture_for_that_port_and_kind() {
    let definitions = [
        (
            "11111111-1111-4111-8111-111111111111",
            site(),
            ResourceKind::Allstarlink,
            "allstarlink",
        ),
        (
            "22222222-2222-4222-8222-222222222222",
            site(),
            ResourceKind::Echolink,
            "echolink",
        ),
        (
            "33333333-3333-4333-8333-333333333333",
            site(),
            ResourceKind::PhonePatch,
            "phone-patch",
        ),
        (
            "44444444-4444-4444-8444-444444444444",
            first(),
            ResourceKind::Echolink,
            "echolink",
        ),
        (
            "55555555-5555-4555-8555-555555555555",
            first(),
            ResourceKind::Allstarlink,
            "allstarlink",
        ),
        (
            "66666666-6666-4666-8666-666666666666",
            first(),
            ResourceKind::PhonePatch,
            "phone-patch",
        ),
        (
            "77777777-7777-4777-8777-777777777777",
            first(),
            ResourceKind::PeerAudio,
            "peer-audio",
        ),
    ];
    let bindings = definitions.map(|(id, scope, kind, _)| binding(id, scope, kind));
    let records = definitions
        .map(|(id, _, _, kind)| format!(r#"{{"id":"{id}","kind":"{kind}","configuration":{{}}}}"#));
    let registry = registry(&bindings);
    let mut captures = metadata(SITE_JSON).to_vec();
    captures.extend(
        bindings
            .into_iter()
            .zip(&records)
            .take(3)
            .map(|(binding, record)| capture(binding, record.as_bytes())),
    );
    let absent = ModularCandidate::assemble(&registry, &captures).unwrap();
    assert!(absent.staged(site(), ResourceKind::Echolink).is_some());
    assert_eq!(absent.port_protocols(id(FIRST)).count(), 0);
    assert_eq!(absent.port_protocols(id(SECOND)).count(), 0);
    assert!(absent.staged(first(), ResourceKind::Echolink).is_none());
    assert!(
        absent
            .staged(first(), ResourceKind::PeerAudio)
            .unwrap()
            .is_empty()
    );

    captures.push(capture(bindings[3], records[3].as_bytes()));
    let echo_only = ModularCandidate::assemble(&registry, &captures).unwrap();
    assert!(
        !echo_only
            .port_metadata(id(FIRST))
            .unwrap()
            .effective(echo_only.site_metadata())
            .enabled
    );
    assert_eq!(
        echo_only.port_protocols(id(FIRST)).collect::<Vec<_>>(),
        [bindings[3]]
    );
    assert_eq!(echo_only.port_protocols(id(SECOND)).count(), 0);

    for index in 4..bindings.len() {
        captures.push(capture(bindings[index], records[index].as_bytes()));
    }
    let all = ModularCandidate::assemble(&registry, &captures).unwrap();
    let mut instance_ids: Vec<_> = all
        .port_protocols(id(FIRST))
        .map(|binding| binding.id)
        .collect();
    instance_ids.sort_unstable();
    assert_eq!(
        instance_ids,
        [
            id("44444444-4444-4444-8444-444444444444"),
            id("55555555-5555-4555-8555-555555555555"),
            id("66666666-6666-4666-8666-666666666666"),
        ]
    );
}

#[test]
fn registry_rejects_conflicting_identity_slots_and_owner_roles() {
    let radio = binding(SECTION, first(), ResourceKind::Radio);
    let cases = [
        (
            vec![(id(SITE), PortKey::new(FIRST_KEY))],
            vec![],
            CandidateProblem::DuplicateId,
        ),
        (
            vec![
                (id(FIRST), PortKey::new(FIRST_KEY)),
                (id(SECOND), PortKey::new(FIRST_KEY)),
            ],
            vec![],
            CandidateProblem::DuplicatePortKey,
        ),
        (
            vec![
                (id(FIRST), PortKey::new(FIRST_KEY)),
                (id(FIRST), PortKey::new(SECOND_KEY)),
            ],
            vec![],
            CandidateProblem::DuplicateId,
        ),
        (
            vec![(id(FIRST), PortKey::new(FIRST_KEY))],
            vec![radio, binding(SECTION, site(), ResourceKind::Radio)],
            CandidateProblem::DuplicateId,
        ),
        (
            vec![(id(FIRST), PortKey::new(FIRST_KEY))],
            vec![radio, binding(OTHER_SECTION, first(), ResourceKind::Radio)],
            CandidateProblem::DuplicateSlot,
        ),
        (vec![], vec![radio], CandidateProblem::InvalidOwner),
        (
            vec![],
            vec![binding(
                SECTION,
                ResourceScope::Port(id(SITE)),
                ResourceKind::Radio,
            )],
            CandidateProblem::InvalidOwner,
        ),
        (
            vec![(id(FIRST), PortKey::new(FIRST_KEY))],
            vec![binding(
                SECTION,
                ResourceScope::Site(id(FIRST)),
                ResourceKind::Radio,
            )],
            CandidateProblem::InvalidOwner,
        ),
        (
            vec![],
            vec![binding(SECTION, site(), ResourceKind::Metadata)],
            CandidateProblem::DuplicateSlot,
        ),
    ];
    for (ports, resources, expected) in cases {
        let error = ResourceRegistry::new(id(SITE), &ports, &resources).unwrap_err();
        assert_eq!(error.problem, expected);
        assert_redacted(&error);
    }
}

#[test]
fn capture_errors_are_structural_atomic_and_value_free() {
    use CandidateProblem::{Decode, InvalidEnvelope};
    use ResourceKind::{Echolink, Metadata, Radio};

    let radio = binding(SECTION, first(), Radio);
    let registry = registry(&[radio]);
    let valid = metadata(SITE_JSON);
    let valid_radio = br#"{"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd",
                          "kind":"radio","configuration":{}}"#;
    let invalid = [
        (
            radio,
            r#"{"id":"secret-value-marker","kind":"radio","configuration":{}}"#,
            InvalidEnvelope("id"),
        ),
        (
            radio,
            r#"{"id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","kind":"radio","configuration":{}}"#,
            InvalidEnvelope("id"),
        ),
        (
            radio,
            r#"{"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","kind":"secret-value-marker","configuration":{}}"#,
            InvalidEnvelope("kind"),
        ),
        (
            radio,
            r#"{"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","kind":"radio","configuration":"secret-value-marker"}"#,
            InvalidEnvelope("configuration"),
        ),
        (
            radio,
            r#"{"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","kind":"radio","configuration":{},"key":"secret-value-marker"}"#,
            InvalidEnvelope("key"),
        ),
        (
            radio,
            r#"{"secret-key-marker":"secret-value-marker","#,
            Decode(JsonResourceError::InvalidJson {
                line: 1,
                column: 45,
            }),
        ),
        (
            radio,
            r#"{"configuration":{"secret-key-marker":1,"secret-key-mark\u0065r":2}}"#,
            Decode(JsonResourceError::InvalidJson {
                line: 1,
                column: 65,
            }),
        ),
        (radio, "[]", Decode(JsonResourceError::ExpectedObject)),
        (
            binding(SECTION, site(), Radio),
            std::str::from_utf8(valid_radio).unwrap(),
            CandidateProblem::UnexpectedBinding,
        ),
        (
            binding(SECTION, first(), Echolink),
            std::str::from_utf8(valid_radio).unwrap(),
            CandidateProblem::UnexpectedBinding,
        ),
        (
            binding(OTHER_SECTION, first(), Radio),
            std::str::from_utf8(valid_radio).unwrap(),
            CandidateProblem::UnexpectedBinding,
        ),
    ];
    for (binding, json, expected) in invalid {
        let mut input = valid.to_vec();
        input.push(capture(binding, json.as_bytes()));
        let error = ModularCandidate::assemble(&registry, &input).unwrap_err();
        // Decoder line and column are deliberately not a contractual spelling.
        if matches!(expected, Decode(JsonResourceError::InvalidJson { .. })) {
            assert!(matches!(
                error.problem,
                Decode(JsonResourceError::InvalidJson { .. })
            ));
        } else {
            assert_eq!(error.problem, expected);
        }
        assert_redacted(&error);
    }
    for (document, problem) in [
        (
            br#"{"id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","kind":"metadata",
                  "key":"secret-value-marker","configuration":{"name":"Port","inherit_from":"site"}}"#.as_slice(),
            InvalidEnvelope("key"),
        ),
        (
            br#"{"id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","kind":"metadata",
                  "configuration":{"name":"","inherit_from":"site"}}"#.as_slice(),
            Decode(JsonResourceError::InvalidName),
        ),
        (
            br#"{"id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","kind":"metadata",
                  "configuration":{"name":"Port","inherit_from":"secret-value-marker"}}"#.as_slice(),
            Decode(JsonResourceError::InvalidParent(ScopeParent::Site)),
        ),
    ] {
        let input = [
            valid[0],
            capture(binding(FIRST, first(), Metadata), document),
            valid[2],
        ];
        let error = ModularCandidate::assemble(&registry, &input).unwrap_err();
        assert_eq!(error.problem, problem);
        assert_redacted(&error);
    }
    assert_eq!(
        ModularCandidate::assemble(&registry, &valid[..2])
            .unwrap_err()
            .problem,
        CandidateProblem::MissingMetadata
    );
    let mut input = valid.to_vec();
    input.extend([capture(radio, valid_radio), capture(radio, valid_radio)]);
    assert_eq!(
        ModularCandidate::assemble(&registry, &input)
            .unwrap_err()
            .problem,
        CandidateProblem::DuplicateCapture
    );

    input.pop();
    assert!(ModularCandidate::assemble(&registry, &input).is_ok());
    let error = "secret-value-marker".parse::<ResourceId>().unwrap_err();
    assert_redacted(&error);
}

#[test]
fn warnings_keep_valid_metadata_and_payloads_stay_opaque_staging_data() {
    let echo = binding(SECTION, first(), ResourceKind::Echolink);
    let registry = registry(&[echo]);
    let [site, _, other] = metadata(SITE_JSON);
    let captures = [
        site,
        capture(
            binding(FIRST, first(), ResourceKind::Metadata),
            br#"{
                "id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","kind":"metadata",
                "owner":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","scope":"site",
                "secret-key-marker":"secret-value-marker",
                "configuration":{"name":"Port","inherit_from":"site","enabled":"secret-value-marker",
                                 "language":["secret-value-marker"],"callsign":"N1PORT",
                                 "another-secret-key-marker":"secret-value-marker"}
            }"#,
        ),
        other,
        capture(
            echo,
            br#"{
                "id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","kind":"echolink",
                "configuration":{"proxy":{"password":"secret-value-marker"},
                                 "still-opaque":{"enabled":1e400}}
            }"#,
        ),
    ];
    let candidate = ModularCandidate::assemble(&registry, &captures).unwrap();
    let warnings = candidate.warnings();
    assert_eq!(warnings.len(), 6);
    for (warning, count) in [
        (ResourceWarning::UnknownField, 4),
        (ResourceWarning::InvalidValue("enabled"), 1),
        (ResourceWarning::InvalidValue("language"), 1),
    ] {
        assert_eq!(
            warnings
                .iter()
                .filter(|item| item.warning == warning)
                .count(),
            count
        );
    }
    for warning in warnings {
        assert_eq!(warning.resource.id, id(FIRST));
        assert_redacted(warning);
    }
    let effective = candidate
        .port_metadata(id(FIRST))
        .unwrap()
        .effective(candidate.site_metadata());
    assert_eq!(effective.callsign, "N1PORT");
    assert_eq!(effective.language, "fr-CA");
    assert!(!effective.enabled);
    let staged = candidate.staged(first(), ResourceKind::Echolink).unwrap();
    assert_eq!(staged.fields["proxy"]["password"], "secret-value-marker");
    assert!(staged.fields["still-opaque"]["enabled"].is_number());
    assert_debug_redacted(format!("{staged:?}"));
    assert_debug_redacted(format!("{candidate:?}"));
}

fn assert_redacted(diagnostic: &(impl fmt::Debug + fmt::Display)) {
    assert_debug_redacted(format!("{diagnostic:?}"));
    assert_debug_redacted(diagnostic.to_string());
}

fn assert_debug_redacted(diagnostic: String) {
    for supplied in [
        "secret-value-marker",
        "secret-key-marker",
        FIRST_KEY,
        SITE,
        FIRST,
    ] {
        assert!(!diagnostic.contains(supplied), "{diagnostic}");
    }
}
