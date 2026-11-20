use super::*;
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde_json::{Value, json};

const STORE: &str = "11111111-1111-4111-8111-111111111111";
const CREDENTIAL: &str = "22222222-2222-4222-8222-222222222222";
const VERSION: &str = "33333333-3333-4333-8333-333333333333";
const OTHER_UUID: &str = "44444444-4444-4444-8444-444444444444";
const KEY_ID: &str = "fixture-key-1";
const OTHER_KEY_ID: &str = "fixture-key-2";
const LIMITS: SealedLimits = SealedLimits {
    max_plaintext_bytes: 128,
    max_document_bytes: 512,
};
const VECTOR_DOCUMENT: &[u8] = br#"{"format_version":1,"key_id":"fixture-key-1","nonce":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYX","ciphertext":"lUwe64dIkA4Q4g9i5OCdUHD5aWG6HFA7LMxmamJPkLE16McQNp10ZFaTJ1VAM8Z66qRDfeBeVim0rSoBMKWBvHluLuffw7nlzzmkzvHoKLALC9amBOWQaBo"}"#;
const VECTOR_AAD: &[u8] = b"\x72\x70\x74\x2d\x61\x64\x76\x61\x6e\x63\x65\x64\x2e\x70\x72\x69\x76\x61\x74\x65\x2d\x72\x65\x63\x6f\x72\x64\x00\x00\x00\x00\x01\x00\x00\x00\x01\x11\x11\x11\x11\x11\x11\x41\x11\x81\x11\x11\x11\x11\x11\x11\x11\x22\x22\x22\x22\x22\x22\x42\x22\x82\x22\x22\x22\x22\x22\x22\x22\x33\x33\x33\x33\x33\x33\x43\x33\x83\x33\x33\x33\x33\x33\x33\x33\x0d\x66\x69\x78\x74\x75\x72\x65\x2d\x6b\x65\x79\x2d\x31";

fn scope() -> RecordScope {
    RecordScope::new(STORE, CREDENTIAL).unwrap()
}

fn identity() -> RecordIdentity {
    RecordIdentity::new(scope(), VERSION).unwrap()
}

fn key_with_id(value: &str) -> SecretKey {
    SecretKey::from_material(
        KeyId::new(value).unwrap(),
        Zeroizing::new(std::array::from_fn(|i| i as u8)),
    )
}

fn key() -> SecretKey {
    key_with_id(KEY_ID)
}

fn fixture_plaintext() -> SecretBytes {
    SecretBytes::new((0u16..73).map(|i| ((i * 131 + 11) % 256) as u8).collect())
}

fn vector_fields() -> Value {
    serde_json::from_slice(VECTOR_DOCUMENT).unwrap()
}

fn decode_fields(value: &Value) -> Result<SealedResolution, SealedError> {
    SealedRecord::decode(identity(), &serde_json::to_vec(value).unwrap(), LIMITS)
}

#[test]
fn key_ids_match_the_fixed_ascii_grammar() {
    let longest = "a".repeat(64);
    for value in ["a", "0", "_", "-", "aZ_0-9", &longest] {
        assert_eq!(KeyId::new(value).unwrap().as_str(), value);
    }

    let longer = "a".repeat(65);
    for value in ["", ".", "../key", "a/b", "a b", "a\u{ff10}", "a\0", &longer] {
        assert_eq!(KeyId::new(value).unwrap_err(), InvalidKeyId);
    }
}

#[test]
fn record_identity_checks_each_uuid_input() {
    assert_eq!(
        RecordScope::new("invalid-store", CREDENTIAL).unwrap_err(),
        SealedError::InvalidIdentity
    );
    assert_eq!(
        RecordScope::new(STORE, "invalid-record").unwrap_err(),
        SealedError::InvalidIdentity
    );
    assert_eq!(
        RecordIdentity::new(scope(), "invalid-version").unwrap_err(),
        SealedError::InvalidIdentity
    );
    let identity = identity();
    assert_eq!(identity.scope(), &scope());
    assert_eq!(identity.object_version().to_string(), VERSION);
}

#[test]
fn independent_libsodium_vector_matches_aad_combined_tag_and_wire_shape() {
    let identity = identity();
    let key = key();
    let plaintext = fixture_plaintext();
    assert!(
        associated_data(&identity, key.key_id()) == VECTOR_AAD,
        "AAD differs from the independent format fixture"
    );
    let record = seal_with_identity(
        &key,
        identity,
        std::array::from_fn(|i| i as u8),
        &plaintext,
        LIMITS,
    )
    .unwrap();
    assert_eq!(record.identity(), &identity);
    assert_eq!(record.key_id(), key.key_id());
    let document = record.encode(LIMITS).unwrap();
    assert!(
        document == VECTOR_DOCUMENT,
        "wire bytes differ from the independent fixture"
    );

    let resolved = SealedRecord::decode(identity, VECTOR_DOCUMENT, LIMITS).unwrap();
    assert!(resolved.warnings.is_empty());
    assert_eq!(resolved.record.identity(), &identity);
    assert_eq!(resolved.record.key_id(), key.key_id());
    let opened = resolved.record.open(&key, LIMITS).unwrap();
    assert!(opened.expose_bytes() == plaintext.expose_bytes());
}

#[test]
fn seal_roundtrips_empty_and_binary_with_fresh_nonce_and_version() {
    let key = key();
    for bytes in [&[][..], &[0, 255, 4, 128][..]] {
        let plaintext = SecretBytes::new(bytes.to_vec());
        let first = seal(&key, &scope(), &plaintext, LIMITS).unwrap();
        let second = seal(&key, &scope(), &plaintext, LIMITS).unwrap();
        assert_ne!(
            first.identity().object_version(),
            second.identity().object_version()
        );
        assert_eq!(first.identity().object_version().get_version_num(), 4);
        assert_eq!(second.identity().object_version().get_version_num(), 4);
        assert!(first.nonce != second.nonce);

        let document = first.encode(LIMITS).unwrap();
        let record = SealedRecord::decode(*first.identity(), &document, LIMITS)
            .unwrap()
            .record;
        for _ in 0..2 {
            let opened = record.open(&key, LIMITS).unwrap();
            assert!(opened.expose_bytes() == bytes, "opened bytes differ");
        }
    }
}

#[test]
fn changed_trusted_identity_fails_authentication() {
    let key = key();
    for (store, credential, version) in [
        (OTHER_UUID, CREDENTIAL, VERSION),
        (STORE, OTHER_UUID, VERSION),
        (STORE, CREDENTIAL, OTHER_UUID),
    ] {
        let substituted =
            RecordIdentity::new(RecordScope::new(store, credential).unwrap(), version).unwrap();
        let record = SealedRecord::decode(substituted, VECTOR_DOCUMENT, LIMITS)
            .unwrap()
            .record;
        assert_eq!(
            record.open(&key, LIMITS).unwrap_err(),
            SealedError::AuthenticationFailed
        );
    }
}

#[test]
fn wrong_material_or_key_id_fails_authentication() {
    let record = decode_fields(&vector_fields()).unwrap().record;
    let wrong_key = SecretKey::from_material(KeyId::new(KEY_ID).unwrap(), Zeroizing::new([0; 32]));
    let alias = key_with_id(OTHER_KEY_ID);
    for key in [&wrong_key, &alias] {
        assert_eq!(
            record.open(key, LIMITS).unwrap_err(),
            SealedError::AuthenticationFailed
        );
    }

    let mut routed_to_alias = vector_fields();
    routed_to_alias["key_id"] = json!(OTHER_KEY_ID);
    let record = decode_fields(&routed_to_alias).unwrap().record;
    assert_eq!(
        record.open(&alias, LIMITS).unwrap_err(),
        SealedError::AuthenticationFailed
    );
}

#[test]
fn modified_nonce_body_or_tag_fails_authentication() {
    let key = key();
    for (field, index) in [("nonce", 0), ("ciphertext", 0), ("ciphertext", 88)] {
        let mut document = vector_fields();
        let mut field_bytes = STANDARD_NO_PAD
            .decode(document[field].as_str().unwrap())
            .unwrap();
        field_bytes[index] ^= 1;
        document[field] = json!(STANDARD_NO_PAD.encode(field_bytes));
        let record = decode_fields(&document).unwrap().record;
        assert_eq!(
            record.open(&key, LIMITS).unwrap_err(),
            SealedError::AuthenticationFailed
        );
    }
}

#[test]
fn unknown_members_warn_without_exposing_or_reemitting_data() {
    let mut document = vector_fields();
    document["unknown-member-sentinel"] = json!("unknown-value-sentinel");
    document["$serde_json::private::Number"] = json!("1");
    let resolution = decode_fields(&document).unwrap();
    assert_eq!(resolution.warnings, vec![SealedWarning::UnknownField; 2]);
    assert!(!format!("{resolution:?}").contains("sentinel"));
    assert!(
        resolution.record.encode(LIMITS).unwrap() == VECTOR_DOCUMENT,
        "ignored JSON fields leaked into encoded result"
    );
}

#[test]
fn invalid_json_and_duplicate_members_cannot_default() {
    let missing_brace = &VECTOR_DOCUMENT[..VECTOR_DOCUMENT.len() - 1];
    let duplicate = format!(
        "{},\"\\u006bey_id\":\"other\"}}",
        std::str::from_utf8(missing_brace).unwrap()
    );
    let nested_duplicate = format!(
        "{},\"unknown\":{{\"key\":1,\"key\":2}}}}",
        std::str::from_utf8(missing_brace).unwrap()
    );
    for document in [
        missing_brace,
        duplicate.as_bytes(),
        nested_duplicate.as_bytes(),
    ] {
        assert_eq!(
            SealedRecord::decode(identity(), document, LIMITS).unwrap_err(),
            SealedError::InvalidJson
        );
    }
    assert_eq!(
        SealedRecord::decode(identity(), b"[]", LIMITS).unwrap_err(),
        SealedError::InvalidEnvelope
    );
}

#[test]
fn malformed_required_fields_and_unknown_format_fail_closed() {
    for field in ["format_version", "key_id", "nonce", "ciphertext"] {
        let mut document = vector_fields();
        document.as_object_mut().unwrap().remove(field);
        assert_eq!(
            decode_fields(&document).unwrap_err(),
            SealedError::InvalidEnvelope
        );
    }

    for (field, value) in [
        ("format_version", json!("1")),
        ("format_version", json!(1.0)),
        (
            "format_version",
            json!({"$serde_json::private::Number":"1"}),
        ),
        ("key_id", json!("../unsafe")),
        ("key_id", json!({"id": KEY_ID})),
        ("nonce", json!(0)),
        ("ciphertext", json!(null)),
        ("nonce", json!(STANDARD_NO_PAD.encode([0; 23]))),
        ("nonce", json!(STANDARD_NO_PAD.encode([0; 25]))),
        ("ciphertext", json!(STANDARD_NO_PAD.encode([0; 15]))),
        ("ciphertext", json!("")),
    ] {
        let mut document = vector_fields();
        document[field] = value;
        assert_eq!(
            decode_fields(&document).unwrap_err(),
            SealedError::InvalidEnvelope
        );
    }
    let mut document = vector_fields();
    document["format_version"] = json!(2);
    assert_eq!(
        decode_fields(&document).unwrap_err(),
        SealedError::UnsupportedFormat
    );
}

#[test]
fn both_base64_fields_require_the_canonical_unpadded_standard_spelling() {
    for field in ["nonce", "ciphertext"] {
        let mut document = vector_fields();
        let valid = document[field].as_str().unwrap().to_owned();
        for bad in [
            format!("{valid}="),
            format!("_{rest}", rest = &valid[1..]),
            format!(" {valid}"),
        ] {
            document[field] = json!(bad);
            assert_eq!(
                decode_fields(&document).unwrap_err(),
                SealedError::InvalidEnvelope
            );
        }
    }
    let mut document = vector_fields();
    // Sixteen zero bytes have a final A. B would decode to the same bytes if trailing bits were ignored.
    document["ciphertext"] = json!("AAAAAAAAAAAAAAAAAAAAAB");
    assert_eq!(
        decode_fields(&document).unwrap_err(),
        SealedError::InvalidEnvelope
    );
}

#[test]
fn exact_document_and_plaintext_limits_apply_at_each_boundary() {
    let key = key();
    let plaintext = fixture_plaintext();
    let exact = SealedLimits {
        max_plaintext_bytes: plaintext.expose_bytes().len(),
        max_document_bytes: VECTOR_DOCUMENT.len(),
    };
    let record = seal(&key, &scope(), &plaintext, exact).unwrap();
    assert_eq!(
        record.encode(exact).unwrap().len(),
        exact.max_document_bytes
    );
    assert!(record.open(&key, exact).is_ok());
    assert!(SealedRecord::decode(identity(), VECTOR_DOCUMENT, exact).is_ok());

    for smaller in [
        SealedLimits {
            max_plaintext_bytes: exact.max_plaintext_bytes - 1,
            ..exact
        },
        SealedLimits {
            max_document_bytes: exact.max_document_bytes - 1,
            ..exact
        },
    ] {
        assert_eq!(
            seal(&key, &scope(), &plaintext, smaller).unwrap_err(),
            SealedError::SizeLimit
        );
        assert_eq!(
            record.open(&key, smaller).unwrap_err(),
            SealedError::SizeLimit
        );
        assert_eq!(record.encode(smaller).unwrap_err(), SealedError::SizeLimit);
        assert_eq!(
            SealedRecord::decode(identity(), VECTOR_DOCUMENT, smaller).unwrap_err(),
            SealedError::SizeLimit
        );
    }
    assert_eq!(
        SealedRecord::decode(
            identity(),
            b"malformed",
            SealedLimits {
                max_document_bytes: 8,
                ..exact
            }
        )
        .unwrap_err(),
        SealedError::SizeLimit
    );
}

#[test]
fn ciphertext_preflight_accepts_exact_tag_and_each_base64_tail_size() {
    let key = key();
    for length in 0..3 {
        let plaintext = SecretBytes::new(vec![0; length]);
        let limits = SealedLimits {
            max_plaintext_bytes: length,
            ..LIMITS
        };
        let record = seal(&key, &scope(), &plaintext, limits).unwrap();
        let document = record.encode(limits).unwrap();
        assert!(SealedRecord::decode(*record.identity(), &document, limits).is_ok());
        if length > 0 {
            let smaller = SealedLimits {
                max_plaintext_bytes: length - 1,
                ..limits
            };
            assert_eq!(
                SealedRecord::decode(*record.identity(), &document, smaller).unwrap_err(),
                SealedError::SizeLimit
            );
        }
    }

    let unbounded = SealedLimits {
        max_plaintext_bytes: usize::MAX,
        max_document_bytes: usize::MAX,
    };
    assert!(SealedRecord::decode(identity(), VECTOR_DOCUMENT, unbounded).is_ok());
}

#[test]
fn secret_and_envelope_debug_and_errors_expose_no_values() {
    let sentinel = "content-sentinel";
    let selected = key_with_id(sentinel);
    let plaintext = SecretBytes::new(sentinel.as_bytes().to_vec());
    let resolved = decode_fields(&vector_fields()).unwrap();
    let invalid = KeyId::new("content-sentinel/").unwrap_err();
    let invalid_envelope =
        SealedRecord::decode(identity(), sentinel.as_bytes(), LIMITS).unwrap_err();
    let error = resolved.record.open(&selected, LIMITS).unwrap_err();
    let debug = format!(
        "{selected:?} {:?} {plaintext:?} {:?} {:?} {:?}",
        selected.key_id(),
        scope(),
        identity(),
        resolved.record
    );
    let expected = concat!(
        "SecretKey(..) KeyId(..) SecretBytes(..) ",
        "RecordScope(..) RecordIdentity(..) SealedRecord(..)"
    );
    assert!(
        debug == expected,
        "Debug rendered data from an opaque value"
    );
    for text in [
        format!("{resolved:?}"),
        format!("{invalid:?} {invalid}"),
        format!("{invalid_envelope:?} {invalid_envelope}"),
        format!("{error:?} {error}"),
    ] {
        assert!(!text.contains(sentinel));
        assert!(!text.contains(KEY_ID));
        assert!(!text.contains(VERSION));
    }
}
