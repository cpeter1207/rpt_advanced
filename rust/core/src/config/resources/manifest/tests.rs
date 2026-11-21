use super::*;
use crate::config::resources::{
    ResourceWarning,
    candidate::{CandidateProblem, ResourceKind, ResourceScope},
};
use serde_json::{Value, json};

const SITE: &str = "00aa0000-0000-4000-8000-000000000001";
const PORT: &str = "00aa0000-0000-4000-8000-000000000002";
const RADIO: &str = "00aa0000-0000-4000-8000-000000000003";
const DEFAULT: &str = "00aa0000-0000-4000-8000-000000000004";
const ECHO: &str = "00aa0000-0000-4000-8000-000000000005";
const EXTRA: &str = "00aa0000-0000-4000-8000-000000000006";
const PORT_KEY: &str = " /~\\Köst \t\0";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn row(kind: &str, id: &str, owner: &str) -> Value {
    json!({
        "kind": kind,
        "owner": owner,
        "version": "版本-v",
        "object": format!("objects/{id}/版本-v.json"),
        "sha256": SHA256,
    })
}

fn fixture() -> Value {
    json!({
        "format_version": 1,
        "id": " Manifest @ ",
        "revision": "révision ~1",
        "site_uuid": SITE,
        "port_keys": { PORT_KEY: PORT },
        "resources": {
            SITE: row("metadata", SITE, SITE),
            PORT: row("metadata", PORT, PORT),
            RADIO: { "kind": "radio", "owner": PORT },
            DEFAULT: { "kind": "allstarlink", "owner": SITE },
            ECHO: row("echolink", ECHO, PORT),
        },
    })
}

#[test]
fn encrypted_index_reference_round_trips_and_participates_in_restore_equivalence() {
    let mut wire = fixture();
    let empty = PublicManifest::decode(&bytes(&wire)).unwrap().value;
    let reference = json!({"record":EXTRA,"version":RADIO,"sha256":SHA256});
    wire["private"] = reference.clone();
    let graph = PublicManifest::decode(&bytes(&wire)).unwrap().value;
    assert_eq!(graph.private().unwrap().record.to_string(), EXTRA);
    assert!(!empty.same_configuration(&graph));
    assert_eq!(
        serde_json::from_slice::<Value>(&graph.encode()).unwrap()["private"],
        reference
    );
    wire["private"]["unknown"] = json!("ignored");
    assert_eq!(
        PublicManifest::decode(&bytes(&wire)).unwrap().warnings,
        vec![ResourceWarning::UnknownField]
    );
    wire["private"]["version"] = json!(RADIO.replace('-', ""));
    assert!(PublicManifest::decode(&bytes(&wire)).is_err());
}

#[test]
fn round_trip_preserves_exact_keys_empty_slots_and_explicit_protocol_presence() {
    let expected = fixture();
    let mut wire = expected.clone();
    wire["site_uuid"] = json!(SITE.to_uppercase());
    wire["port_keys"][PORT_KEY] = json!(PORT.replace('-', ""));
    let site = wire["resources"]
        .as_object_mut()
        .unwrap()
        .remove(SITE)
        .unwrap();
    wire["resources"][SITE.to_uppercase()] = site;
    wire["resources"][ECHO]["owner"] = json!(PORT.to_uppercase());

    let ResourceResolution { value, warnings } = PublicManifest::decode(&bytes(&wire)).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(value.id().as_str(), " Manifest @ ");
    assert_eq!(value.revision().as_str(), "révision ~1");
    let port = PORT.parse().unwrap();
    let scope = ResourceScope::Port(port);
    assert_eq!(
        value.registry().port_id(&PortKey::new(PORT_KEY)),
        Some(port)
    );
    assert_eq!(
        value.registry().port_id(&PortKey::new(PORT_KEY.trim())),
        None
    );
    assert_eq!(
        value
            .registry()
            .binding(scope, ResourceKind::Radio)
            .unwrap()
            .id,
        RADIO.parse().unwrap()
    );
    assert!(value.reference(RADIO.parse().unwrap()).is_none());
    assert!(value.reference(DEFAULT.parse().unwrap()).is_none());
    assert!(
        value
            .registry()
            .binding(scope, ResourceKind::Allstarlink)
            .is_none()
    );
    let object = value.reference(ECHO.parse().unwrap()).unwrap();
    assert_eq!(object.version().as_str(), "版本-v");
    assert_eq!(
        object.sha256().as_bytes()[..8],
        [1, 35, 69, 103, 137, 171, 205, 239]
    );

    let encoded = value.encode();
    assert_eq!(encoded, bytes(&expected));
    assert_eq!(
        PublicManifest::decode(&encoded).unwrap().value.encode(),
        encoded
    );
}

#[test]
fn structural_fields_cannot_fall_back() {
    let cases = [
        (
            &["format_version"][..],
            json!(2),
            ManifestError::UnsupportedFormat,
        ),
        (
            &["format_version"][..],
            json!(1.0),
            ManifestError::UnsupportedFormat,
        ),
        (
            &["format_version"][..],
            json!({"$serde_json::private::Number": "1"}),
            ManifestError::UnsupportedFormat,
        ),
        (
            &["id"][..],
            json!("../secret"),
            ManifestError::InvalidField("id"),
        ),
        (
            &["revision"][..],
            json!(7),
            ManifestError::InvalidField("revision"),
        ),
        (
            &["site_uuid"][..],
            json!("secret"),
            ManifestError::InvalidField("site_uuid"),
        ),
        (
            &["port_keys"][..],
            json!([]),
            ManifestError::InvalidField("port_keys"),
        ),
        (
            &["port_keys", PORT_KEY][..],
            json!("secret"),
            ManifestError::InvalidField("port_keys"),
        ),
        (
            &["resources"][..],
            Value::Null,
            ManifestError::InvalidField("resources"),
        ),
        (
            &["resources", ECHO][..],
            json!([]),
            ManifestError::InvalidField("resources"),
        ),
        (
            &["resources", ECHO, "kind"][..],
            json!("bogus-secret"),
            ManifestError::InvalidField("kind"),
        ),
        (
            &["resources", ECHO, "owner"][..],
            json!("secret"),
            ManifestError::InvalidField("owner"),
        ),
        (
            &["resources", ECHO, "version"][..],
            Value::Null,
            ManifestError::InvalidField("version"),
        ),
        (
            &["resources", ECHO, "object"][..],
            json!(format!("objects/{ECHO}/different.json")),
            ManifestError::InvalidField("object"),
        ),
        (
            &["resources", ECHO, "object"][..],
            json!(format!("objects/{}/版本-v.json", ECHO.to_uppercase())),
            ManifestError::InvalidField("object"),
        ),
        (
            &["resources", ECHO, "sha256"][..],
            json!(SHA256.to_uppercase()),
            ManifestError::InvalidField("sha256"),
        ),
        (
            &["resources", ECHO, "sha256"][..],
            json!("too-short"),
            ManifestError::InvalidField("sha256"),
        ),
        (
            &["resources", ECHO, "sha256"][..],
            json!(format!("{}g", &SHA256[..63])),
            ManifestError::InvalidField("sha256"),
        ),
        (
            &["resources", RADIO, "sha256"][..],
            Value::Null,
            ManifestError::InvalidField("reference"),
        ),
        (&["private"][..], Value::Null, ManifestError::InvalidPrivate),
        (
            &["private"][..],
            json!({ "secret": "secret" }),
            ManifestError::InvalidPrivate,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(path, replacement, _)| {
            let mut wire = fixture();
            let mut leaf = &mut wire;
            for key in *path {
                leaf = &mut leaf[*key];
            }
            *leaf = replacement.clone();
            PublicManifest::decode(&bytes(&wire)).unwrap_err()
        })
        .collect();
    let expected: Vec<_> = cases.into_iter().map(|(_, _, error)| error).collect();
    assert_eq!(actual, expected);
}

#[test]
fn graph_identity_and_required_references_are_checked() {
    let decode = |wire: Value| PublicManifest::decode(&bytes(&wire)).unwrap_err();
    let problem = |error| match error {
        ManifestError::Registry(error) => error.problem,
        other => panic!("{other:?}"),
    };

    let mut wire = fixture();
    wire["port_keys"]["second"] = json!(PORT.to_uppercase());
    assert_eq!(problem(decode(wire)), CandidateProblem::DuplicateId);

    let mut wire = fixture();
    wire["resources"][RADIO.to_uppercase()] = wire["resources"][RADIO].clone();
    assert_eq!(decode(wire), ManifestError::DuplicateResourceId);

    let mut wire = fixture();
    wire["resources"][EXTRA] = wire["resources"][RADIO].clone();
    assert_eq!(problem(decode(wire)), CandidateProblem::DuplicateSlot);

    let mut wire = fixture();
    wire["resources"][RADIO]["owner"] = json!(EXTRA);
    assert_eq!(problem(decode(wire)), CandidateProblem::InvalidOwner);

    let mut wire = fixture();
    wire["resources"][PORT] = row("metadata", PORT, SITE);
    assert_eq!(decode(wire), ManifestError::InvalidMetadata);

    for id in [SITE, PORT] {
        let mut wire = fixture();
        wire["resources"].as_object_mut().unwrap().remove(id);
        assert_eq!(decode(wire), ManifestError::InvalidMetadata);
    }

    for (id, kind, owner) in [
        (SITE, "metadata", SITE),
        (PORT, "metadata", PORT),
        (ECHO, "echolink", PORT),
    ] {
        let mut wire = fixture();
        wire["resources"][id] = json!({ "kind": kind, "owner": owner });
        assert_eq!(decode(wire), ManifestError::MissingReference);
    }
}

#[test]
fn builder_requires_the_same_registry_and_reference_invariants() {
    let site = SITE.parse().unwrap();
    let port = PORT.parse().unwrap();
    let site_binding = ResourceBinding {
        id: site,
        scope: ResourceScope::Site(site),
        kind: ResourceKind::Metadata,
    };
    let port_binding = ResourceBinding {
        id: port,
        scope: ResourceScope::Port(port),
        kind: ResourceKind::Metadata,
    };
    let object = ObjectReference::new("版本-v".parse().unwrap(), SHA256.parse().unwrap());
    let build = |ports: &[_], rows| {
        PublicManifest::new(
            " Manifest @ ".parse().unwrap(),
            "révision ~1".parse().unwrap(),
            site,
            ports,
            rows,
        )
    };
    let port_key = PortKey::new("");
    let ports = [(port, port_key.clone())];
    let built = build(
        &ports,
        vec![
            (site_binding, Some(object.clone())),
            (port_binding, Some(object.clone())),
        ],
    )
    .unwrap();
    let decoded = PublicManifest::decode(&built.encode()).unwrap().value;
    assert_eq!(decoded.registry().port_id(&port_key), Some(port));

    let mut expected = fixture();
    expected["port_keys"] = json!({ "": PORT });
    for id in [RADIO, DEFAULT, ECHO] {
        expected["resources"].as_object_mut().unwrap().remove(id);
    }
    assert_eq!(built.encode(), bytes(&expected));

    assert_eq!(
        build(&ports, vec![(site_binding, Some(object.clone()))]).unwrap_err(),
        ManifestError::InvalidMetadata
    );
    let error = build(
        &[(port, port_key.clone()), (EXTRA.parse().unwrap(), port_key)],
        vec![],
    )
    .unwrap_err();
    assert!(
        matches!(error, ManifestError::Registry(error) if error.problem == CandidateProblem::DuplicatePortKey)
    );
    assert_eq!(
        build(&[], vec![(site_binding, None)]).unwrap_err(),
        ManifestError::MissingReference
    );

    expected["port_keys"] = json!({});
    expected["resources"].as_object_mut().unwrap().remove(PORT);
    let site_only = build(&[], vec![(site_binding, Some(object))]).unwrap();
    assert_eq!(site_only.encode(), bytes(&expected));
}

#[test]
fn parser_retains_value_free_warnings_and_rejects_duplicate_json_members() {
    let mut wire = fixture();
    let marker = "sensitive marker";
    wire[marker] = json!({ "body": marker });
    wire["resources"][ECHO][marker] = json!(marker);
    let ResourceResolution { value, warnings } = PublicManifest::decode(&bytes(&wire)).unwrap();
    assert_eq!(warnings, [ResourceWarning::UnknownField; 2]);
    assert!(!format!("{value:?} {warnings:?}").contains(marker));
    assert_eq!(value.encode(), bytes(&fixture()));

    let duplicate = bytes(&wire);
    let mut duplicate = String::from_utf8(duplicate).unwrap();
    duplicate.insert_str(1, r#""extra":{"sensitive marker":1,"sensitive marker":2},"#);
    let error = PublicManifest::decode(duplicate.as_bytes()).unwrap_err();
    assert!(matches!(
        error,
        ManifestError::Decode(JsonResourceError::InvalidJson { .. })
    ));
    assert!(!format!("{error:?}").contains(marker));
    assert!(!format!("{:?}", value.reference(ECHO.parse().unwrap()).unwrap()).contains(SHA256));
}

#[test]
fn typed_identifiers_match_posix_component_rules() {
    for invalid in ["", ".", "..", "a/b", "a\0b"] {
        assert_eq!(
            invalid.parse::<ManifestId>(),
            Err(ManifestError::InvalidField("id"))
        );
        assert_eq!(
            invalid.parse::<LogicalRevision>(),
            Err(ManifestError::InvalidField("revision"))
        );
        assert_eq!(
            invalid.parse::<ObjectVersion>(),
            Err(ManifestError::InvalidField("version"))
        );
    }
    let allowed = " arbitrary \\字 \n";
    assert_eq!(allowed.parse::<ManifestId>().unwrap().as_str(), allowed);
    assert_eq!(
        allowed.parse::<LogicalRevision>().unwrap().as_str(),
        allowed
    );
    assert_eq!(allowed.parse::<ObjectVersion>().unwrap().as_str(), allowed);
}
