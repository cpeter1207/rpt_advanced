//! Mailbox credentials are private configuration, never an administrative grant.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use rpt_advanced_core::config::{
    self,
    resources::{
        PortKey,
        candidate::{ResourceKind, ResourceScope},
    },
    valid_mailbox_pin_hash, verify_mailbox_pin,
};
use serde_json::json;
#[path = "support/capture.rs"]
mod capture;
const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn identity(
    key: &str,
    fields: serde_json::Value,
) -> Result<
    config::resources::profiles::mailbox::ScopedMailboxes,
    config::resources::profiles::mailbox::MailboxError,
> {
    let fixture = capture::Fixture::new(&[], &[(SITE, ResourceKind::Mailbox, key, fields)]);
    fixture
        .candidate()
        .mailbox_profiles(ResourceScope::Site(fixture.registry.site_id()))
}

fn hash() -> String {
    Argon2::default()
        .hash_password(
            b"1234",
            &SaltString::encode_b64(b"sixteen-byte-salt").unwrap(),
        )
        .unwrap()
        .to_string()
}

#[test]
fn mailbox_configuration_is_recognized_without_a_public_node() {
    let accounts = identity("0012", json!({"first_name":"Chris","callsign":"KG0BP"})).unwrap();
    assert!(accounts.warnings.is_empty());
    assert_eq!(
        accounts.effective[&PortKey::new("0012")].number.as_str(),
        "0012"
    );
}

#[test]
fn malformed_private_input_is_not_echoed_by_mailbox_diagnostics() {
    let malformed = "PRIVATE_PIN_1234";
    assert!(!valid_mailbox_pin_hash(malformed));
    let error = identity(
        "0012",
        json!({"first_name":"","callsign":"KG0BP","pin":malformed}),
    )
    .err()
    .unwrap();
    assert!(!format!("{error:?}").contains(malformed));
}
#[test]
fn salted_record_verifies_only_bounded_digits_and_preserves_profile() {
    let digest = hash();
    let result = identity("0012", json!({"first_name":"Chris","callsign":"KG0BP"})).unwrap();
    let mailbox = &result.effective[&PortKey::new("0012")];
    assert_eq!(mailbox.number.as_str(), "0012");
    assert_eq!(
        (&*mailbox.first_name, &*mailbox.callsign),
        ("Chris", "KG0BP")
    );
    assert!(!format!("{mailbox:?}").contains(&digest));
    assert!(verify_mailbox_pin(&digest, "1234"));
    for input in [
        "",
        "123",
        "9999",
        "1234#",
        "12345678901234567890123456789012345",
    ] {
        assert!(!verify_mailbox_pin(&digest, input));
    }
}

#[test]
fn incomplete_or_weak_mailboxes_remain_unavailable_without_secret_diagnostics() {
    let digest = hash();
    for (key, invalid) in [
        ("pin_hash", digest.replace("argon2id", "argon2i")),
        ("pin_hash", digest.replace("m=19456", "m=8192")),
        ("pin_hash", digest.replace("m=19456", "m=4294967295")),
        ("pin_hash", digest.replace("t=2", "t=1")),
        ("pin_hash", digest.replace("t=2", "t=100")),
        ("pin_hash", digest.replace("v=19", "v=16")),
        ("pin_hash", "x".repeat(257)),
        ("pin_hash", digest.replace("p=1", "p=5")),
        (
            "pin_hash",
            digest.replace(
                "JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw",
                "dG9vLXNob3J0",
            ),
        ),
        (
            "pin_hash",
            digest.replace("c2l4dGVlbi1ieXRlLXNhbHQ", "c2hvcnQ"),
        ),
        ("first_name", String::new()),
        ("first_name", "x".repeat(65)),
        ("first_name", "Chr\tis".into()),
        ("callsign", String::new()),
        ("callsign", "A".repeat(17)),
        ("callsign", "INVALID CALLSIGN".into()),
    ] {
        if key == "pin_hash" {
            assert!(!valid_mailbox_pin_hash(&invalid));
        } else {
            let mut fields = json!({"first_name":"Chris","callsign":"KG0BP"});
            fields[key] = json!(invalid);
            assert!(identity("0012", fields).is_err());
        }
    }
    for number in ["invalid", "123456789012345678901234567890123", "42"] {
        let error = identity(number, json!({"pin":"UNSAFE1234"})).err().unwrap();
        assert!(!format!("{error:?}").contains("UNSAFE1234"));
    }
}

#[test]
fn a_parseable_phc_with_short_digest_is_not_an_available_mailbox() {
    let digest = hash();
    let short = format!(
        "{}$c2l4dGVlbi1ieXRlLXNhbHQ",
        digest.rsplit_once('$').unwrap().0
    );
    assert!(argon2::password_hash::PasswordHash::new(&short).is_ok());
    assert!(!valid_mailbox_pin_hash(&short));
}
