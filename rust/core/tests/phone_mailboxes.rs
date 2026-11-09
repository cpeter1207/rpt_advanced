//! Mailbox credentials are private configuration, never an administrative grant.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use rpt_advanced_core::config::{ConfigDocument, Schema, resolve_phone_mailboxes};
use std::fmt::Write;

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
    let document = ConfigDocument::parse(&format!(
        "[1000]\n[mailbox 0012]\nfirst_name=Chris\ncallsign=KG0BP\npin_hash={}\n",
        hash()
    ))
    .unwrap();
    assert_eq!(document.nodes(), ["1000"]);
    assert!(Schema::validate(&document).unwrap().warnings.is_empty());
}

#[test]
fn invalid_mailbox_hash_is_diagnosed_without_echoing_the_secret() {
    let document = ConfigDocument::parse(
        "[1000]\n[mailbox 0012]\nfirst_name=Chris\ncallsign=KG0BP\npin_hash=PRIVATE_PIN_1234\n",
    )
    .unwrap();
    let result = Schema::validate(&document).unwrap();
    assert!(!result.warnings.is_empty());
    assert!(!format!("{:?}", result.warnings).contains("PRIVATE_PIN_1234"));
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.key == "pin_hash")
    );
}

#[test]
fn mailbox_document_debug_never_discloses_an_entered_or_stored_secret() {
    let document =
        ConfigDocument::parse("[mailbox 12]\npin=PRIVATE_PIN_1234\npin_hash=PRIVATE_HASH\n")
            .unwrap();
    let debug = format!("{document:?}");
    assert!(!debug.contains("PRIVATE_PIN_1234"));
    assert!(!debug.contains("PRIVATE_HASH"));
    let ordinary = ConfigDocument::parse("[1000]\nradio_channel=fixture\n").unwrap();
    assert!(format!("{ordinary:?}").contains("fixture"));
}

#[test]
fn salted_record_verifies_only_bounded_digits_and_preserves_profile() {
    let digest = hash();
    let document = ConfigDocument::parse(&format!(
        "[mailbox 0012]\nfirst_name=Chris\ncallsign=KG0BP\npin_hash={digest}\n"
    ))
    .unwrap();
    let result = resolve_phone_mailboxes(&document);
    assert!(result.warnings.is_empty());
    let mailbox = &result.value[0];
    assert_eq!(mailbox.id.as_str(), "0012");
    assert_eq!(
        (&*mailbox.first_name, &*mailbox.callsign),
        ("Chris", "KG0BP")
    );
    assert!(!format!("{mailbox:?}").contains(&digest));
    assert!(mailbox.verify_pin("1234"));
    for input in [
        "",
        "123",
        "9999",
        "1234#",
        "12345678901234567890123456789012345",
    ] {
        assert!(!mailbox.verify_pin(input));
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
        let mut fields = [
            ("first_name", "Chris".to_owned()),
            ("callsign", "KG0BP".into()),
            ("pin_hash", digest.clone()),
        ];
        fields.iter_mut().find(|(name, _)| *name == key).unwrap().1 = invalid.clone();
        let mut body = String::new();
        for (key, value) in fields {
            writeln!(&mut body, "{key}={value}").unwrap();
        }
        let document = ConfigDocument::parse(&format!("[mailbox 0012]\n{body}")).unwrap();
        assert!(resolve_phone_mailboxes(&document).value.is_empty());
        let warnings = Schema::validate(&document).unwrap().warnings;
        assert!(!warnings.is_empty());
        if key == "pin_hash" {
            assert!(!format!("{warnings:?}").contains(&invalid));
        }
    }
    for section in [
        "mailbox invalid",
        "mailbox 123456789012345678901234567890123",
        "mailbox 42",
    ] {
        let document = ConfigDocument::parse(&format!("[{section}]\npin=UNSAFE1234\n")).unwrap();
        let warnings = Schema::validate(&document).unwrap().warnings;
        assert!(resolve_phone_mailboxes(&document).value.is_empty());
        assert!(!format!("{warnings:?}").contains("UNSAFE1234"));
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
    let document = ConfigDocument::parse(&format!(
        "[mailbox 12]\nfirst_name=Chris\ncallsign=KG0BP\npin_hash={short}\n"
    ))
    .unwrap();
    assert!(resolve_phone_mailboxes(&document).value.is_empty());
    assert!(!Schema::validate(&document).unwrap().warnings.is_empty());
}
