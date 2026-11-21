use super::*;
use serde_json::json;

const LIMITS: PatchLimits = PatchLimits {
    request_bytes: 4096,
    operations: 16,
    pointer_bytes: 128,
    pointer_tokens: 16,
    number_bytes: 128,
    stage_bytes: 8192,
    stage_depth: 24,
};

const PUBLIC: PatchPolicy<'_> = PatchPolicy {
    writable: &[""],
    server_owned: &[],
    secret_slots: &[],
};

const ACCOUNT: PatchPolicy<'_> = PatchPolicy {
    writable: &["/configuration"],
    server_owned: &["/_links", "/meta", "/configuration/id"],
    secret_slots: &["/configuration/password", "/configuration/proxy/password"],
};

fn stage(base: &Value, body: &str) -> Result<StagedPatch, PatchError> {
    stage_patch(base, body.as_bytes(), &PUBLIC, LIMITS)
}

fn account(base: &Value, body: &str) -> Result<StagedPatch, PatchError> {
    stage_patch(base, body.as_bytes(), &ACCOUNT, LIMITS)
}

fn error(kind: PatchErrorKind, operation: usize) -> PatchError {
    PatchError {
        kind,
        operation: Some(operation),
    }
}

#[test]
fn applies_all_operations_in_local_order_with_rfc_escapes_and_array_indices() {
    let base = json!({"a/b": {"~1": [1, 2, 3]}, "stale": true});
    let patched = stage(
        &base,
        r#"[
        {"op":"add","path":"/a~1b/~01/-","value":4,"from":true},
        {"op":"copy","from":"/a~1b/~01/1","path":"/copied","value":{"extra":true}},
        {"op":"move","from":"/a~1b/~01/0","path":"/a~1b/~01/2"},
        {"op":"test","path":"/a~1b/~01","value":[2,3,1,4]},
        {"op":"replace","path":"/a~1b/~01/0","value":"new","ignored":null},
        {"op":"remove","path":"/stale","value":125}
    ]"#,
    )
    .unwrap();
    assert_eq!(
        patched.authoring,
        json!({"a/b": {"~1": ["new", 3, 1, 4]}, "copied": 2})
    );
    assert_eq!(base, json!({"a/b": {"~1": [1, 2, 3]}, "stale": true}));
}

#[test]
fn allows_root_operations_and_object_keys_that_resemble_array_indices() {
    let patched = stage(
        &json!({"old": 1}),
        r#"[
        {"op":"replace","path":"","value":{"01":1,"-":2,"":3}},
        {"op":"test","path":"","value":{"":3,"-":2,"01":1}},
        {"op":"move","from":"/01","path":"/010"},
        {"op":"copy","from":"/-","path":"/-"},
        {"op":"copy","from":"","path":"/snapshot"},
        {"op":"add","path":"","value":[0,1]},
        {"op":"move","from":"/0","path":""}
    ]"#,
    )
    .unwrap();
    assert_eq!(patched.authoring, 0);
    assert_eq!(
        stage(&json!({"": 7}), r#"[{"op":"test","path":"/","value":7}]"#)
            .unwrap()
            .authoring,
        json!({"": 7})
    );
}

#[test]
fn rejects_missing_local_parents_invalid_arrays_and_moves_into_descendants_atomically() {
    let base = json!({"configuration": {"local": [1,2], "a": {}}});
    for operation in [
        r#"{"op":"replace","path":"/configuration/inherited/0","value":0}"#,
        r#"{"op":"add","path":"/configuration/absent/key","value":0}"#,
        r#"{"op":"add","path":"/configuration/local/01","value":0}"#,
        r#"{"op":"add","path":"/configuration/local/4","value":0}"#,
        r#"{"op":"remove","path":"/configuration/local/-"}"#,
        r#"{"op":"move","from":"/configuration/a","path":"/configuration/a/child"}"#,
        r#"{"op":"move","from":"/configuration/absent","path":"/configuration/local/0"}"#,
    ] {
        let body = format!(
            r#"[{{"op":"replace","path":"/configuration/local/0","value":9}},{operation}]"#
        );
        assert_eq!(
            stage(&base, &body).unwrap_err(),
            error(PatchErrorKind::Path, 1)
        );
        assert_eq!(base["configuration"]["local"], json!([1, 2]));
    }
}

#[test]
fn rejects_malformed_envelopes_and_duplicate_keys_at_every_depth() {
    for request in [
        "{}",
        "[null]",
        r#"[{"path":"/x"}]"#,
        r#"[{"op":"merge","path":"/x"}]"#,
        r#"[{"op":"remove","path":null}]"#,
        r#"[{"op":"remove","path":"/x","path":"/y"}]"#,
        r#"[{"op":"add","path":"/x"}]"#,
        r#"[{"op":"copy","path":"/x"}]"#,
        r#"[{"op":"move","path":"/x","from":"/~2"}]"#,
        r##"[{"op":"remove","path":"#/x"}]"##,
        r#"[{"op":"remove","path":"x"}]"#,
        r#"[{"op":"remove","path":"/x~"}]"#,
        r#"[{"op":"add","path":"/x","value":{"a":0,"\u0061":1}}]"#,
        r#"[{"op":"remove","path":"/x","ignored":{"a":[{"x":0,"x":1}]}}]"#,
        r#"[{"op":"add","path":"/configuration/password","value":{"x":0,"x":1}}]"#,
        "[",
        "[] []",
    ] {
        assert_eq!(
            account(&json!({"configuration":{"proxy":{},"x":0}}), request)
                .unwrap_err()
                .kind,
            PatchErrorKind::Syntax,
            "{request}"
        );
    }
}

#[test]
fn validates_strings_in_private_and_ignored_members() {
    let base = json!({"configuration": {"proxy": {}}});
    let staged = account(
        &base,
        r#"[
            {"op":"replace","path":"/configuration/password","value":"\uD83D\uDE00",
             "ignored":{"literal":"\\uD83D"}}
        ]"#,
    )
    .unwrap();
    assert_eq!(staged.authoring, base);
    let [edit] = staged.private_edits.as_slice() else {
        panic!("expected one private edit");
    };
    let PrivateChange::Set(value) = &edit.change else {
        panic!("expected private set");
    };
    assert_eq!(value.as_json(), br#""\uD83D\uDE00""#);

    for request in [
        r#"[{"op":"replace","path":"/configuration/password","value":"synthetic-marker\uD83D"}]"#,
        r#"[{"op":"add","path":"/configuration/x","value":0,"ignored":["synthetic-marker\uDE00"]}]"#,
    ] {
        let failure = account(&base, request).unwrap_err();
        assert_eq!(failure.kind, PatchErrorKind::Syntax);
        assert!(!format!("{failure:?} {failure}").contains("synthetic-marker"));
    }
}

#[test]
fn tests_numbers_exactly_without_coercing_objects_strings_or_array_order() {
    let original = rptadv_json::decode_value(br#"{
        "nested":[1,90123456789012345678901234567890,-0.00,1e99999999999999999999,10e-99999999999999999998],
        "token":{"$serde_json::private::Number":"42"}
    }"#).unwrap();
    let positive = r#"[
        {"op":"test","path":"/nested","value":[1e0,901234567890123456789012345678900e-1,0,10e99999999999999999998,1e-99999999999999999997]},
        {"op":"test","path":"/nested/0","value":1.0},
        {"op":"test","path":"/token","value":{"$serde_json::private::Number":"42"}},
        {"op":"add","path":"/literal","value":{"$serde_json::private::RawValue":"1"}}
    ]"#;
    let out = stage(&original, positive).unwrap();
    assert_eq!(
        out.authoring["literal"],
        json!({"$serde_json::private::RawValue":"1"})
    );

    for (path, value) in [
        ("/nested/0", r#""1""#),
        ("/nested/0", "true"),
        ("/nested/1", "90123456789012345678901234567891"),
        ("/nested/2", "-1e-99999999999999999999"),
        ("/nested/3", "1e99999999999999999998"),
        ("/nested", "[0,1]"),
        ("/token", "42"),
        ("/token", r#"{"$serde_json::private::Number":"42","x":0}"#),
    ] {
        let body = format!(r#"[{{"op":"test","path":"{path}","value":{value}}}]"#);
        assert_eq!(
            stage(&original, &body).unwrap_err(),
            error(PatchErrorKind::TestFailed, 0)
        );
    }
    assert_eq!(
        stage(
            &json!(["a", "b"]),
            r#"[{"op":"test","path":"","value":["b","a"]}]"#
        )
        .unwrap_err(),
        error(PatchErrorKind::TestFailed, 0)
    );
}

#[test]
fn scopes_paths_and_checks_server_owned_fields_inside_replacements() {
    let base = json!({"configuration": {"link": false, "proxy": {}}});
    for op in [
        r#"{"op":"test","path":"/_links","value":null}"#,
        r#"{"op":"test","path":"/configuration/id","value":null}"#,
        r#"{"op":"add","path":"/configuration/id","value":7}"#,
        r#"{"op":"replace","path":"","value":{"configuration":{},"meta":null}}"#,
        r#"{"op":"add","path":"/configuration","value":{"id":null}}"#,
        r#"{"op":"replace","path":"","value":{"rogue":null}}"#,
        r#"{"op":"add","path":"/configuration2","value":true}"#,
        r#"{"op":"copy","from":"/configuration/link","path":"/meta"}"#,
        r#"{"op":"move","from":"/configuration/id","path":"/configuration/link"}"#,
    ] {
        assert_eq!(
            account(&base, &format!("[{op}]")).unwrap_err(),
            error(PatchErrorKind::Forbidden, 0),
            "{op}"
        );
    }
    let staged = account(
        &base,
        r#"[{"op":"replace","path":"","value":{"configuration":{"enabled":true}}}]"#,
    )
    .unwrap();
    assert_eq!(staged.authoring, json!({"configuration":{"enabled":true}}));
    assert!(staged.private_edits.is_empty());
}

#[test]
fn slots_are_neutral_sequential_and_absent_from_returned_authoring() {
    let base = json!({"configuration":{"proxy":{},"enabled":true}});
    let stage = account(
        &base,
        r#"[
        {"op":"replace","path":"/configuration/password","value":"synthetic-before"},
        {"op":"remove","path":"/configuration/password"},
        {"op":"add","path":"/configuration/password","value":"synthetic-after"}
    ]"#,
    )
    .unwrap();
    assert_eq!(stage.authoring, base);
    assert!(!format!("{stage:?}").contains("synthetic-"));
    let [edit] = stage.private_edits.as_slice() else {
        panic!("expected one final explicit edit")
    };
    assert_eq!(edit.slot, 0);
    let PrivateChange::Set(value) = &edit.change else {
        panic!("expected final set")
    };
    assert_eq!(value.as_json(), br#""synthetic-after""#);

    let removed = account(
        &base,
        r#"[{"op":"remove","path":"/configuration/proxy/password"}]"#,
    )
    .unwrap();
    assert_eq!(removed.authoring, base);
    assert_eq!(removed.private_edits[0].slot, 1);
    assert!(matches!(
        removed.private_edits[0].change,
        PrivateChange::Remove
    ));
    for second in [
        r#"{"op":"replace","path":"/configuration/password","value":"synthetic-absent"}"#,
        r#"{"op":"remove","path":"/configuration/password"}"#,
    ] {
        assert_eq!(
            account(
                &base,
                &format!(r#"[{{"op":"remove","path":"/configuration/password"}},{second}]"#)
            )
            .unwrap_err(),
            error(PatchErrorKind::Path, 1)
        );
    }
    assert!(account(&base, "[]").unwrap().private_edits.is_empty());
    let invalid = json!({"configuration":{"proxy":{},"password":"synthetic-stored"}});
    let failure = account(&invalid, "[]").unwrap_err();
    assert_eq!(failure.kind, PatchErrorKind::Authoring);
    assert!(!format!("{failure:?}").contains("synthetic"));
}

#[test]
fn all_secret_intersecting_reads_and_indirect_destinations_are_forbidden() {
    let base = json!({"configuration":{"proxy":{},"public":{"x":0},"password_lookalike":0}});
    for operation in [
        r#"{"op":"test","path":"/configuration/password","value":"synthetic-guess"}"#,
        r#"{"op":"test","path":"/configuration/password/0","value":"synthetic-guess"}"#,
        r#"{"op":"test","path":"/configuration/proxy","value":{}}"#,
        r#"{"op":"test","path":"","value":{}}"#,
        r#"{"op":"copy","from":"/configuration/password","path":"/configuration/public"}"#,
        r#"{"op":"move","from":"/configuration/proxy","path":"/configuration/public"}"#,
        r#"{"op":"copy","from":"/configuration/public","path":"/configuration/password"}"#,
        r#"{"op":"move","from":"/configuration/public","path":"/configuration/proxy"}"#,
        r#"{"op":"add","path":"/configuration/password/x","value":"synthetic-guess"}"#,
    ] {
        let first = account(&base, &format!("[{operation}]")).unwrap_err();
        assert_eq!(first, error(PatchErrorKind::Forbidden, 0));
        assert!(!format!("{first:?}").contains("synthetic"));
        let after_removal =
            format!(r#"[{{"op":"remove","path":"/configuration/password"}},{operation}]"#);
        assert_eq!(
            account(&base, &after_removal).unwrap_err(),
            error(PatchErrorKind::Forbidden, 1)
        );
    }
    assert!(
        account(
            &base,
            r#"[{"op":"test","path":"/configuration/password_lookalike","value":0}]"#
        )
        .is_ok()
    );
}

#[test]
fn explicit_secrets_in_parent_replacements_are_extracted_but_omission_preserves() {
    let base = json!({"configuration":{"proxy":{"enabled":true},"enabled":true}});
    let patched = account(&base, r#"[
        {"op":"replace","path":"","value":{"configuration":{"password":{"unvalidated":"synthetic-raw"},"proxy":{"password":""}}}},
        {"op":"replace","path":"/configuration/proxy","value":{"enabled":false}},
        {"op":"add","path":"/configuration/source","value":"new"},
        {"op":"test","path":"/configuration/source","value":"new"}
    ]"#).unwrap();
    assert_eq!(
        patched.authoring,
        json!({"configuration":{"proxy":{"enabled":false},"source":"new"}})
    );
    assert!(!format!("{patched:?}").contains("synthetic-raw"));
    let [first, second] = patched.private_edits.as_slice() else {
        panic!("expected both explicit credential edits")
    };
    let (PrivateChange::Set(first), PrivateChange::Set(second)) = (&first.change, &second.change)
    else {
        panic!("expected set edits")
    };
    assert_eq!(first.as_json(), br#"{"unvalidated":"synthetic-raw"}"#);
    assert_eq!(second.as_json(), br#""""#);
    let omitted = account(&base, r#"[{"op":"remove","path":"/configuration/proxy"}]"#).unwrap();
    assert!(omitted.private_edits.is_empty());
    assert_eq!(omitted.authoring, json!({"configuration":{"enabled":true}}));
}

#[test]
fn enforces_request_operation_pointer_numeric_and_intermediate_stage_limits() {
    let base = json!({"x":[1]});
    for (body, limits, operation) in [
        (
            "[]",
            PatchLimits {
                request_bytes: 1,
                ..LIMITS
            },
            None,
        ),
        (
            r#"[{"op":"remove","path":"/x"}]"#,
            PatchLimits {
                operations: 0,
                ..LIMITS
            },
            None,
        ),
        (
            r#"[{"op":"test","path":"/x","value":[1]}]"#,
            PatchLimits {
                pointer_bytes: 1,
                ..LIMITS
            },
            Some(0),
        ),
        (
            r#"[{"op":"copy","from":"/x/0","path":"/y"}]"#,
            PatchLimits {
                pointer_tokens: 1,
                ..LIMITS
            },
            Some(0),
        ),
        (
            r#"[{"op":"test","path":"/x/0","value":1.0}]"#,
            PatchLimits {
                number_bytes: 2,
                ..LIMITS
            },
            Some(0),
        ),
        (
            r#"[{"op":"copy","from":"/x","path":"/y"},{"op":"remove","path":"/y"}]"#,
            PatchLimits {
                stage_bytes: 14,
                ..LIMITS
            },
            Some(0),
        ),
        (
            r#"[{"op":"copy","from":"","path":"/x/-"}]"#,
            PatchLimits {
                stage_depth: 3,
                ..LIMITS
            },
            Some(0),
        ),
    ] {
        let result = stage_patch(&base, body.as_bytes(), &PUBLIC, limits).unwrap_err();
        assert_eq!(
            result,
            PatchError {
                kind: PatchErrorKind::Limit,
                operation
            },
            "{body}"
        );
    }
}

#[test]
fn rejects_runtime_error_after_private_edits_without_exposing_request_values() {
    let base = json!({"configuration":{"proxy":{},"public":true}});
    let fail = account(
        &base,
        r#"[
        {"op":"replace","path":"/configuration/password","value":"synthetic-secret"},
        {"op":"test","path":"/configuration/public","value":false}
    ]"#,
    )
    .unwrap_err();
    assert_eq!(fail, error(PatchErrorKind::TestFailed, 1));
    assert!(!format!("{fail:?}").contains("synthetic-secret"));
    let fail = stage(
        &base,
        r#"[{"op":"remove","path":"/synthetic-sensitive-path"}]"#,
    )
    .unwrap_err();
    assert!(!format!("{fail:?}").contains("synthetic-sensitive-path"));
    assert_eq!(base, json!({"configuration":{"proxy":{},"public":true}}));
}
