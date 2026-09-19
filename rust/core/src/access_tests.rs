use super::AccessPolicy;

#[test]
fn parsed_entries_preserve_exact_identity_and_errors_are_operator_readable() {
    let policy = AccessPolicy::new(" 0001, 2 ", "3, 4").unwrap();
    assert_eq!(policy.allow_entries(), ["0001", "2"]);
    assert_eq!(policy.deny_entries(), ["3", "4"]);
    assert_eq!(
        AccessPolicy::new("?", "").unwrap_err().to_string(),
        "invalid node access list"
    );
}

#[test]
fn validates_decimal_lists_with_spaces_and_rejects_empty_entries() {
    let valid = ["", " \t", "524950", " 524950 , 508422\t", "1,2,3", "0001"];
    let invalid = [",", "1,", "1,,2", "1, ", "1 2", "*", "-1", "1.0", "1\n"];

    for list in valid {
        assert!(
            AccessPolicy::list_valid(list),
            "expected valid list {list:?}"
        );
    }
    for list in invalid {
        assert!(
            !AccessPolicy::list_valid(list),
            "expected invalid list {list:?}"
        );
    }
}

#[test]
fn enforces_verification_deny_precedence_and_exact_tokens() {
    let cases = [
        (false, false, false, false, false),
        (true, false, false, false, true),
        (true, true, false, false, false),
        (true, false, true, false, false),
        (true, false, true, true, true),
        (false, true, true, true, false),
    ];
    for (verified, denied, restricted, listed, expected) in cases {
        let allow = if restricted {
            if listed { "508422, 524950" } else { "508422" }
        } else {
            ""
        };
        let deny = if denied { "524950" } else { "508422" };
        let policy = AccessPolicy::new(allow, deny).unwrap();
        assert_eq!(policy.allows("524950", verified), expected);
    }
    assert!(AccessPolicy::new(" \t", "").unwrap().allows("524950", true));
    assert!(
        !AccessPolicy::new("5249500,52495, 508422 ", "")
            .unwrap()
            .allows("524950", true)
    );
    assert!(
        AccessPolicy::new(" 524950 ", "508422, 52495 ")
            .unwrap()
            .allows("524950", true)
    );
}
