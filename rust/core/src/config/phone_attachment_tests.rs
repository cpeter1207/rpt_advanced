use super::*;
use crate::config::{PhoneAccountSettings, Schema};

fn document(account: &str, target: &str, general: &str) -> ConfigDocument {
    ConfigDocument::parse(&format!(
        "[general]\n{general}\n[site,west]\n{target}\n[phone_account provider]\n\
         local_node=site,west\nenabled=yes\nserver=pbx:4569\nusername=fixture\n\
         secrets_file=unused.secret\n{account}"
    ))
    .unwrap()
}

#[test]
fn phone_attachment_policy_configuration_requires_both_exact_lists() {
    let account = PhoneAccountId::new("provider").unwrap();
    let node = NodeId::new("site,west").unwrap();
    for (outbound, inbound, general, expected) in [
        ("", "", "", false),
        (
            "attach_local_nodes=\n",
            "phone_allow_accounts=\n",
            "",
            false,
        ),
        ("attach_local_nodes=site,west\n", "", "", false),
        ("", "phone_allow_accounts=provider\n", "", false),
        (
            "attach_local_nodes=site,west\n",
            "phone_allow_accounts=provider\n",
            "",
            true,
        ),
        (
            "attach_local_nodes=other\tsite,west\n",
            "phone_allow_accounts=other provider\n",
            "",
            true,
        ),
        (
            "attach_local_nodes=other,site,west\n",
            "phone_allow_accounts=provider\n",
            "",
            false,
        ),
        (
            "attach_local_nodes=site,west\n",
            "phone_allow_accounts=Provider\n",
            "",
            false,
        ),
        (
            "attach_local_nodes=*\n",
            "phone_allow_accounts=provider\n",
            "",
            false,
        ),
        (
            "attach_local_nodes=site,west\n",
            "",
            "phone_allow_accounts=provider\n",
            false,
        ),
        (
            "attach_local_nodes=site,west [invalid]\n",
            "phone_allow_accounts=provider\n",
            "",
            false,
        ),
        (
            "attach_local_nodes=site,west\n",
            "phone_allow_accounts=provider bad!\n",
            "",
            false,
        ),
    ] {
        let document = document(outbound, inbound, general);
        assert_eq!(
            document.phone_local_attachment_allowed(&account, node.as_str()),
            expected
        );
    }
}

#[test]
fn phone_attachment_policy_schema_warns_and_keeps_transport_records_unchanged() {
    let plain = document("", "", "");
    let account = PhoneAccountId::new("provider").unwrap();
    let baseline = PhoneAccountSettings::resolve(&plain, &account).value;
    for (outbound, inbound, warnings) in [
        (
            "attach_local_nodes=site,west\n",
            "phone_allow_accounts=provider\n",
            0,
        ),
        ("attach_local_nodes=\n", "phone_allow_accounts=\n", 0),
        (
            "attach_local_nodes=site,west [bad]\n",
            "phone_allow_accounts=provider bad!\n",
            2,
        ),
    ] {
        let document = document(outbound, inbound, "");
        assert_eq!(
            PhoneAccountSettings::resolve(&document, &account).value,
            baseline
        );
        let resolved = Schema::validate(&document).unwrap();
        assert_eq!(resolved.warnings.len(), warnings);
        for warning in resolved.warnings {
            assert_eq!(warning.message, "invalid option value");
            assert!(warning.fallback.is_empty(), "invalid lists use empty deny");
        }
    }
    let document = document(
        "attach_local_nodes=site,west\n",
        "",
        "phone_allow_accounts=provider\n",
    );
    let warnings = Schema::validate(&document).unwrap().warnings;
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unknown option ignored");
}

#[test]
fn phone_attachment_policy_canonical_identifier_bounds_and_literal_star() {
    for name in ["*", "site,west", &"x".repeat(63)] {
        let document = ConfigDocument::parse(&format!(
            "[{name}]\nphone_allow_accounts=provider\n[phone_account provider]\n\
             local_node={name}\nserver=pbx:4569\nusername=fixture\nsecrets_file=unused\n\
             attach_local_nodes={name}\n"
        ))
        .unwrap();
        assert!(Schema::validate(&document).unwrap().warnings.is_empty());
        assert!(
            document
                .phone_local_attachment_allowed(&PhoneAccountId::new("provider").unwrap(), name)
        );
    }
    assert!(node_list_valid(""));
    assert!(account_list_valid(""));
    assert!(!node_list_valid(&"x".repeat(64)));
    assert!(account_list_valid(&"x".repeat(64)));
    assert!(!account_list_valid(&"x".repeat(65)));
    assert!(!account_list_valid("*"));
}
