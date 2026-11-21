//! Behavior of owned resource decoding and scope inheritance.

use super::resources::{
    JsonResourceError, PortKey, PortMetadata, ResourceWarning, ScopeParent, SiteMetadata,
    decode_object, inherit_mailboxes, inherit_section,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[test]
fn json_object_decoding_owns_all_value_kinds() {
    let input = br#"{"s":"escaped \u002f","a":[true,false,null,-2,0,1.5,{"n":2}]}"#.to_vec();
    let object = decode_object(&input).unwrap();
    drop(input);

    assert_eq!(
        Value::Object(object),
        json!({"s":"escaped /","a":[true,false,null,-2,0,1.5,{"n":2}]})
    );
}

#[test]
fn duplicate_object_keys_fail_even_in_unknown_nested_arrays() {
    for input in [
        r#"{"secret-key-marker":1,"secret-key-marker":2}"#,
        r#"{"scope":{"secret-key-marker":1,"secret-key-mark\u0065r":2}}"#,
        r#"{"unknown":[{"secret-key-marker":null,"secret-key-marker":false}]}"#,
    ] {
        let error = decode_object(input.as_bytes()).unwrap_err();
        assert!(matches!(error, JsonResourceError::InvalidJson { .. }));
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(!diagnostic.contains("secret-key-marker"));
        }
    }
}

#[test]
fn invalid_json_and_nonobject_roots_are_rejected() {
    for input in [
        b"".as_slice(),
        b"{\"secret-value-marker\":",
        b"{} {}",
        b"{\"s\":\"\xff\"}",
    ] {
        let error = decode_object(input).unwrap_err();
        assert!(matches!(error, JsonResourceError::InvalidJson { .. }));
        assert!(!error.to_string().contains("secret-value-marker"));
    }
    for input in [
        b"null".as_slice(),
        b"[]",
        b"true",
        b"123",
        br#""secret-value-marker""#,
    ] {
        assert_eq!(
            decode_object(input).unwrap_err(),
            JsonResourceError::ExpectedObject
        );
    }
}

#[test]
fn scopes_require_an_explicit_local_name_and_parent() {
    for input in [
        br#"{"inherit_from":null}"#.as_slice(),
        br#"{"name":"","inherit_from":null}"#,
        br#"{"name":false,"inherit_from":null}"#,
    ] {
        assert_eq!(
            SiteMetadata::decode(input).unwrap_err(),
            JsonResourceError::InvalidName
        );
    }
    for input in [
        br#"{"name":"Site"}"#.as_slice(),
        br#"{"name":"Site","inherit_from":"secret-value-marker"}"#,
    ] {
        let error = SiteMetadata::decode(input).unwrap_err();
        assert_eq!(
            error,
            JsonResourceError::InvalidParent(ScopeParent::BuiltIn)
        );
        assert!(!error.to_string().contains("secret-value-marker"));
    }
    for input in [
        br#"{"name":"Port"}"#.as_slice(),
        br#"{"name":"Port","inherit_from":null}"#,
        br#"{"name":"Port","inherit_from":"other"}"#,
    ] {
        assert_eq!(
            PortMetadata::decode(input).unwrap_err(),
            JsonResourceError::InvalidParent(ScopeParent::Site),
        );
    }
}

#[test]
fn common_settings_inherit_under_a_scope_local_name_and_parent() {
    let site = SiteMetadata::decode(br#"{"name":"Station","inherit_from":null,"enabled":false,"callsign":"N0SITE","language":"fr-ca"}"#)
        .unwrap().value;
    let port = PortMetadata::decode(
        br#"{"name":"  Local repeater #1 ;  ","inherit_from":"site","callsign":""}"#,
    )
    .unwrap()
    .value;

    assert_eq!(site.local().inherit_from, ScopeParent::BuiltIn);
    assert_eq!(port.local().enabled, None);
    assert_eq!(port.local().language, None);
    let effective = port.effective(&site);
    assert_eq!(effective.name, "  Local repeater #1 ;  ");
    assert_eq!(effective.inherit_from, ScopeParent::Site);
    assert!(!effective.enabled);
    assert_eq!(effective.callsign, "");
    assert_eq!(effective.language, "fr-CA");

    let updated_site = SiteMetadata::decode(
        br#"{"name":"Station","inherit_from":null,"enabled":true,"language":"en-GB"}"#,
    )
    .unwrap()
    .value;
    let updated = port.effective(&updated_site);
    assert!(updated.enabled);
    assert_eq!(updated.language, "en-GB");
}

#[test]
fn invalid_common_values_and_unknown_fields_warn_without_echoing_input() {
    let site = SiteMetadata::decode(
        br#"{
        "name":"Station","inherit_from":null,"enabled":"secret-value-marker",
        "callsign":"1234567890123456789012345678901234567890123456789012345678901234",
        "language":"<secret-value-marker>","secret-key-marker":{"password":"secret-value-marker"}
    }"#,
    )
    .unwrap();
    assert_eq!(site.warnings.len(), 4);
    assert!(site.warnings.contains(&ResourceWarning::UnknownField));
    for field in ["enabled", "callsign", "language"] {
        assert!(
            site.warnings
                .contains(&ResourceWarning::InvalidValue(field))
        );
    }
    for warning in &site.warnings {
        for diagnostic in [warning.to_string(), format!("{warning:?}")] {
            assert!(!diagnostic.contains("secret-value-marker"));
            assert!(!diagnostic.contains("secret-key-marker"));
        }
    }
    let defaults = site.value.effective();
    assert!(defaults.enabled);
    assert_eq!(defaults.callsign, "");
    assert_eq!(defaults.language, "en-US");

    let site = SiteMetadata::decode(br#"{"name":"Station","inherit_from":null,"enabled":false,"callsign":"N0SITE","language":"es"}"#)
        .unwrap().value;
    let port = PortMetadata::decode(
        br#"{
        "name":"Port","inherit_from":"site","enabled":null,"callsign":false,
        "language":["secret-value-marker"],"id":"injected id","key":"injected key"
    }"#,
    )
    .unwrap();
    assert_eq!(port.warnings.len(), 5);
    assert_eq!(
        port.warnings
            .iter()
            .filter(|warning| **warning == ResourceWarning::UnknownField)
            .count(),
        2
    );
    let effective = port.value.effective(&site);
    assert!(!effective.enabled);
    assert_eq!(effective.callsign, "N0SITE");
    assert_eq!(effective.language, "es");
}

#[test]
fn unsupported_numeric_values_warn_and_keep_valid_siblings() {
    for (extra, warning) in [
        (
            r#""enabled":1e400"#,
            ResourceWarning::InvalidValue("enabled"),
        ),
        (
            r#""future":[{"limit":1e400}]"#,
            ResourceWarning::UnknownField,
        ),
    ] {
        let input =
            format!(r#"{{"name":"Station","inherit_from":null,"callsign":"N0CALL",{extra}}}"#);
        let site = SiteMetadata::decode(input.as_bytes()).unwrap();

        assert_eq!(site.warnings, [warning]);
        let effective = site.value.effective();
        assert!(effective.enabled);
        assert_eq!(effective.callsign, "N0CALL");
    }
}

#[test]
fn arbitrary_number_tokens_do_not_reserve_an_object_member_name() {
    let object =
        decode_object(br#"{"number":1e400,"nested":{"$serde_json::private::Number":"1e400"}}"#)
            .unwrap();

    assert!(object["number"].is_number());
    assert!(object["number"].as_f64().is_none());
    assert_eq!(
        object["nested"],
        json!({"$serde_json::private::Number":"1e400"})
    );
}

#[test]
fn callsign_limit_counts_unicode_characters() {
    let callsign = "N0ÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉÉ";
    let input = format!(r#"{{"name":"Station","inherit_from":null,"callsign":"{callsign}"}}"#);
    let site = SiteMetadata::decode(input.as_bytes()).unwrap();

    assert!(site.warnings.is_empty());
    assert_eq!(site.value.effective().callsign, callsign);
}

#[test]
fn local_keys_preserve_exact_text_independently_of_network_node_numbers() {
    for text in [
        "",
        " /..~/[port] \u{e9} \t ",
        "000012",
        "Repeater",
        "repeater",
    ] {
        assert_eq!(PortKey::new(text).as_str(), text);
    }
    assert_ne!(PortKey::new("Repeater"), PortKey::new("repeater"));
    assert_ne!(PortKey::new("12"), PortKey::new("000012"));
}

#[test]
fn section_overrides_merge_objects_and_replace_arrays_or_scalars() {
    let inherited = decode_object(
        br#"{
        "alpha":{"level":2,"timing":{"hold":40,"rise":5},"order":["A","B"],"extra":{"a":1}},
        "beta":{"level":3},
        "gamma":true
    }"#,
    )
    .unwrap();
    let local = decode_object(
        br#"{
        "alpha":{"timing":{"hold":25},"order":["C"],"extra":{}},
        "beta":[],
        "gamma":{"new":false},
        "delta":null
    }"#,
    )
    .unwrap();

    assert_eq!(
        Value::Object(inherit_section(&inherited, &local)),
        json!({
            "alpha":{"level":2,"timing":{"hold":25,"rise":5},"order":["C"],"extra":{"a":1}},
            "beta":[],
            "gamma":{"new":false},
            "delta":null
        })
    );

    let mut reduced = local;
    reduced
        .get_mut("alpha")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("order");
    assert_eq!(
        inherit_section(&inherited, &reduced)["alpha"]["order"],
        json!(["A", "B"])
    );
    assert_eq!(
        inherit_section(&inherited, &decode_object(b"{}").unwrap()),
        inherited
    );
}

#[test]
fn local_mailbox_shadows_complete_pair_without_blending_profile_and_hash() {
    let site = BTreeMap::from([
        ("0012".to_owned(), ("site profile", "site hash")),
        ("0042".to_owned(), ("unchanged profile", "unchanged hash")),
    ]);
    let port = BTreeMap::from([("0012".to_owned(), ("port profile", "port hash"))]);

    assert_eq!(
        inherit_mailboxes(&site, &port),
        BTreeMap::from([
            ("0012".to_owned(), ("port profile", "port hash")),
            ("0042".to_owned(), ("unchanged profile", "unchanged hash")),
        ])
    );
    assert_eq!(inherit_mailboxes(&site, &BTreeMap::new()), site);
}
