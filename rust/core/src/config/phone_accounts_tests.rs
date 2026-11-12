use super::*;

fn account(extra: &str) -> ConfigDocument {
    ConfigDocument::parse(&format!("[station]\n[phone_account trunk]\nlocal_node=station\nserver=pbx.example:4569\nusername=private-user\nsecrets_file=/private/provider.secret\n{extra}")).unwrap()
}

#[test]
fn phone_account_schema_recognizes_global_records() {
    let document = account("");
    assert!(Schema::validate(&document).unwrap().warnings.is_empty());
}

#[test]
fn phone_account_defaults_and_explicit_settings() {
    let defaults = resolve_phone_accounts(&account(""));
    assert!(defaults.warnings.is_empty());
    let settings = &defaults.value[0];
    assert_eq!(settings.id.as_str(), "trunk");
    assert_eq!(settings.local_node.as_str(), "station");
    assert!(!settings.enabled);
    assert!(!settings.register);
    assert_eq!(settings.protocol, PhoneAccountProtocol::Iax);
    assert_eq!(settings.registration_interval_s, 300);
    assert_eq!(settings.authentication_timeout_ms, 30_000);
    let explicit = resolve_phone_accounts(&account(
        "enabled=yes\nprotocol=sip\nregister=yes\nregistration_interval_s=65535\nauthentication_timeout_ms=120000",
    ));
    let settings = &explicit.value[0];
    assert!(settings.enabled && settings.register);
    assert_eq!(settings.protocol, PhoneAccountProtocol::Sip);
    assert_eq!(settings.registration_interval_s, 65535);
    assert_eq!(settings.authentication_timeout_ms, 120_000);
    assert_eq!(settings.server, "pbx.example:4569");
    assert_eq!(settings.username, "private-user");
    assert_eq!(settings.secrets_file, "/private/provider.secret");
}

#[test]
fn phone_account_invalid_optional_values_use_safe_defaults() {
    for (key, values, fallback) in [
        ("enabled", vec!["", "true", "secret-value"], "no"),
        ("register", vec!["", "true"], "no"),
        ("protocol", vec!["", "http"], "iax"),
        (
            "registration_interval_s",
            vec!["0", "65536", "-1", "1.5"],
            "300",
        ),
        (
            "authentication_timeout_ms",
            vec!["0", "120001", "unknown"],
            "30000",
        ),
    ] {
        for value in values {
            let document = account(&format!("{key}={value}"));
            let resolved = resolve_phone_accounts(&document);
            assert_eq!(resolved.value, resolve_phone_accounts(&account("")).value);
            assert_eq!(resolved.warnings.len(), 1, "{key}={value}");
            assert_eq!(resolved.warnings[0].fallback, fallback);
            assert!(!resolved.warnings[0].value.contains("secret-value"));
            assert_eq!(
                Schema::validate(&document).unwrap().warnings,
                resolved.warnings
            );
        }
    }
}

#[test]
fn phone_account_invalid_required_values_are_unavailable() {
    for (key, values) in [
        ("local_node", vec!["", "absent", "bad node"]),
        (
            "server",
            vec![
                "",
                "pbx",
                "pbx:0",
                "pbx:65536",
                "pbx:service",
                "pbx:+1",
                "https://pbx:4569",
                "user:password@pbx:4569",
                "pbx:4569/path",
                "pbx:4569?secret",
                "bad host:4569",
                "999.1.2.3:4569",
            ],
        ),
        ("username", vec!["", "bad user"]),
        ("secrets_file", vec!["", "https://secret"]),
    ] {
        for value in values {
            let resolved = resolve_phone_accounts(&account(&format!("{key}={value}")));
            assert!(resolved.value.is_empty(), "{key}={value}");
            assert!(!resolved.warnings.is_empty());
        }
    }
    for key in ["local_node", "server", "username", "secrets_file"] {
        let source = String::from(
            "[station]\n[phone_account trunk]\nlocal_node=station\nserver=pbx:4569\nusername=user\nsecrets_file=/secret",
        );
        let source = source
            .lines()
            .filter(|line| !line.starts_with(&format!("{key}=")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            resolve_phone_accounts(&ConfigDocument::parse(&source).unwrap())
                .value
                .is_empty()
        );
    }
}

#[test]
fn phone_account_addresses_and_lower_bounds() {
    let relative = resolve_phone_accounts(&account("secrets_file=private/provider.secret"));
    assert_eq!(relative.value[0].secrets_file, "private/provider.secret");
    for server in [
        "127.0.0.1:1",
        "[::1]:65535",
        "localhost:5060",
        "sip.example.:5060",
    ] {
        let document = account(&format!(
            "server={server}\nregistration_interval_s=1\nauthentication_timeout_ms=1"
        ));
        let resolved = resolve_phone_accounts(&document);
        assert!(resolved.warnings.is_empty(), "{server}");
        assert_eq!(resolved.value[0].server, server);
        assert_eq!(resolved.value[0].registration_interval_s, 1);
        assert_eq!(resolved.value[0].authentication_timeout_ms, 1);
    }
}

#[test]
fn phone_account_identity_namespace_and_redaction() {
    let document = account("password=do-not-print\nprotocol=do-not-print");
    let resolved = resolve_phone_accounts(&document);
    assert_eq!(resolved.warnings.len(), 2);
    let debug = format!("{document:?} {resolved:?}");
    for private in ["private-user", "/private/provider.secret", "do-not-print"] {
        assert!(!debug.contains(private), "{private}");
    }
    for label in ["bad@label".to_owned(), "x".repeat(65)] {
        let text = format!("[station]\n[phone_account {label}]");
        let document = ConfigDocument::parse(&text).unwrap();
        assert!(resolve_phone_accounts(&document).value.is_empty());
        assert!(!Schema::validate(&document).unwrap().warnings.is_empty());
    }
    let document =
        ConfigDocument::parse("[station]\n[phone_account trunk]\n[mailbox 001]").unwrap();
    assert_eq!(document.nodes(), vec!["station"]);
    assert_eq!(
        document.named_sections("phone_account", None),
        vec!["phone_account trunk"]
    );
}

#[test]
fn phone_account_records_do_not_inherit_node_or_general_options() {
    let document = ConfigDocument::parse("[general]\nenabled=yes\nregister=yes\n[station]\n[phone_account empty]\n[phone_account complete]\nlocal_node=station\nserver=pbx:4569\nusername=user\nsecrets_file=/secret").unwrap();
    let resolved = resolve_phone_accounts(&document);
    assert_eq!(resolved.value.len(), 1);
    assert_eq!(resolved.value[0].id.as_str(), "complete");
    assert!(!resolved.value[0].enabled && !resolved.value[0].register);
    let label = "a".repeat(64);
    let document = ConfigDocument::parse(&format!("[station]\n[phone_account {label}]\nlocal_node=station\nserver=pbx:4569\nusername=user\nsecrets_file=/secret")).unwrap();
    assert!(Schema::validate(&document).unwrap().warnings.is_empty());
    assert_eq!(
        resolve_phone_accounts(&document).value[0].id.as_str(),
        label
    );
}

#[test]
fn single_phone_account_resolution_uses_canonical_defaults_and_validation() {
    let id = crate::link::PhoneAccountId::new("trunk").unwrap();
    for extra in [
        "",
        "enabled=yes",
        "enabled=invalid",
        "protocol=sip",
        "protocol=invalid",
        "local_node=missing",
        "server=invalid",
        "secrets_file=https://invalid.example",
    ] {
        let document = account(extra);
        let all = resolve_phone_accounts(&document);
        let single = PhoneAccountSettings::resolve(&document, &id);
        assert_eq!(single.value.as_ref(), all.value.first(), "{extra}");
        assert_eq!(single.warnings, all.warnings, "{extra}");
    }
    let missing = crate::link::PhoneAccountId::new("absent").unwrap();
    assert!(
        PhoneAccountSettings::resolve(&account(""), &missing)
            .value
            .is_none()
    );
}
