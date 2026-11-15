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
    assert_eq!(settings.inter_digit_timeout_ms, 10_000);
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
    let long_host = [
        "x".repeat(63),
        "x".repeat(63),
        "x".repeat(63),
        "x".repeat(62),
    ]
    .join(".");
    for (key, values) in [
        ("local_node", vec!["", "absent", "bad node"]),
        (
            "server",
            vec![
                "",
                "pbx",
                ":4569",
                &format!("{long_host}:4569"),
                "pbx..example:4569",
                &format!("{}:4569", "x".repeat(64)),
                "-pbx:4569",
                "pbx-:4569",
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
        (
            "username",
            vec!["", "bad user", "bad\u{7}user", &"x".repeat(256)],
        ),
        (
            "secrets_file",
            vec!["", "https://secret", &"x".repeat(4097)],
        ),
    ] {
        for value in values {
            let document = account(&format!("{key}={value}"));
            let resolved = resolve_phone_accounts(&document);
            assert!(resolved.value.is_empty(), "{key}={value}");
            assert!(!resolved.warnings.is_empty());
            assert!(
                resolved
                    .warnings
                    .iter()
                    .all(|warning| warning.fallback == "account unavailable")
            );
            assert_eq!(
                Schema::validate(&document).unwrap().warnings,
                resolved.warnings
            );
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
        "127.0.0.1.:1",
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
fn phone_account_maximum_field_lengths_are_accepted() {
    let host = [
        "x".repeat(63),
        "x".repeat(63),
        "x".repeat(63),
        "x".repeat(61),
    ]
    .join(".");
    for (key, value) in [
        ("server", format!("{host}:4569")),
        ("username", "x".repeat(255)),
        ("secrets_file", "x".repeat(4096)),
    ] {
        let document = account(&format!("{key}={value}"));
        let resolved = resolve_phone_accounts(&document);
        assert!(resolved.warnings.is_empty(), "{key}");
        assert_eq!(resolved.value.len(), 1, "{key}");
        let settings = &resolved.value[0];
        let actual = match key {
            "server" => &settings.server,
            "username" => &settings.username,
            _ => &settings.secrets_file,
        };
        assert_eq!(actual, &value, "{key}");
        assert!(Schema::validate(&document).unwrap().warnings.is_empty());
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

#[test]
fn phone_interdigit_option_is_recognized_without_changing_authentication_policy() {
    let resolved = resolve_phone_accounts(&account("inter_digit_timeout_ms=15000"));
    assert!(resolved.warnings.is_empty());
    assert_eq!(resolved.value[0].authentication_timeout_ms, 30_000);
}

#[test]
fn phone_interdigit_config_bounds_and_warning_defaults() {
    for (value, expected, warns) in [
        ("1", 1, false),
        ("15000", 15000, false),
        ("60000", 60000, false),
        ("0", 10000, true),
        ("60001", 10000, true),
        ("-1", 10000, true),
        ("1.5", 10000, true),
        ("", 10000, true),
    ] {
        let resolved = resolve_phone_accounts(&account(&format!("inter_digit_timeout_ms={value}")));
        assert_eq!(resolved.value[0].inter_digit_timeout_ms, expected);
        assert_eq!(!resolved.warnings.is_empty(), warns);
        assert_eq!(resolved.value[0].authentication_timeout_ms, 30000);
    }
}

#[test]
fn phone_speech_options_are_validated_per_exact_account() {
    let valid = account(
        "speech_preprocessing_enabled=yes\nspeech_bandpass_enabled=no\nspeech_bandpass_low_hz=250\nspeech_bandpass_high_hz=3200\nspeech_rnnoise_enabled=yes\nspeech_threshold=0.75\nspeech_onset_frames=4294967295\nspeech_preroll_ms=0\nspeech_hold_ms=4294967295\nspeech_reserve_ms=160",
    );
    assert!(Schema::validate(&valid).unwrap().warnings.is_empty());
    let defaults = resolve_phone_accounts(&account("")).value;
    assert_eq!(
        resolve_phone_accounts(&valid).value,
        defaults,
        "speech tuning must not change account transport identity"
    );
}

#[test]
fn phone_speech_invalid_preset_values_warn_with_the_effective_fallback() {
    for (key, value, fallback) in [
        ("speech_preprocessing_enabled", "invalid", "no"),
        ("speech_bandpass_enabled", "invalid", "yes"),
        ("speech_rnnoise_enabled", "invalid", "yes"),
        ("speech_threshold", "NaN", "0.75"),
        ("speech_onset_frames", "0", "2"),
        ("speech_bandpass_low_hz", "0", "300"),
        ("speech_bandpass_high_hz", "24000", "3000"),
        ("speech_preroll_ms", "301", "160"),
        ("speech_reserve_ms", "85", "160"),
        ("speech_reserve_ms", "281", "160"),
        ("speech_hold_ms", "-1", "1000"),
    ] {
        let document = account(&format!("speech_preprocessing_enabled=yes\n{key}={value}"));
        let warnings = Schema::validate(&document).unwrap().warnings;
        assert_eq!(warnings.len(), 1, "{key}");
        assert_eq!(warnings[0].fallback, fallback, "{key}");
    }
}

#[test]
fn phone_speech_preset_overrides_and_timings_are_independent() {
    let id = crate::link::PhoneAccountId::new("trunk").unwrap();
    let plain = PhoneSpeechSettings::resolve(&account(""), &id);
    assert_eq!(plain, PhoneSpeechSettings::default());
    let selected = PhoneSpeechSettings::resolve(&account("speech_preprocessing_enabled=yes"), &id);
    assert!(
        selected.preprocessing_enabled && selected.bandpass_enabled && selected.rnnoise_enabled
    );
    assert_eq!((selected.threshold, selected.onset_frames), (0.75, 2));
    assert_eq!(
        (selected.bandpass_low_hz, selected.bandpass_high_hz),
        (300.0, 3000.0)
    );
    assert_eq!(
        (selected.preroll_ms, selected.hold_ms, selected.reserve_ms),
        (160, 1000, 160)
    );
    for enabled in ["yes", "no"] {
        let document = account(&format!(
            "speech_preprocessing_enabled={enabled}\nspeech_threshold=0.3\nspeech_onset_frames=4294967295\nspeech_preroll_ms=0\nspeech_hold_ms=0\nspeech_reserve_ms=86\nspeech_bandpass_enabled=no\nspeech_rnnoise_enabled=no\nspeech_bandpass_low_hz=200\nspeech_bandpass_high_hz=3500"
        ));
        let explicit = PhoneSpeechSettings::resolve(&document, &id);
        assert_eq!((explicit.threshold, explicit.onset_frames), (0.3, u32::MAX));
        assert_eq!(
            (explicit.preroll_ms, explicit.hold_ms, explicit.reserve_ms),
            (0, 0, 86)
        );
        assert!(!explicit.bandpass_enabled && !explicit.rnnoise_enabled);
        assert_eq!(
            (explicit.bandpass_low_hz, explicit.bandpass_high_hz),
            (200.0, 3500.0)
        );
    }
    let largest = PhoneSpeechSettings::resolve(
        &account(
            "speech_threshold=1\nspeech_preroll_ms=300\nspeech_reserve_ms=280\nspeech_hold_ms=4294967295",
        ),
        &id,
    );
    assert_eq!(
        (
            largest.threshold,
            largest.preroll_ms,
            largest.reserve_ms,
            largest.hold_ms
        ),
        (1.0, 300, 280, u32::MAX)
    );
}

#[test]
fn phone_speech_cutoff_order_warns_and_restores_a_valid_pair() {
    let id = crate::link::PhoneAccountId::new("trunk").unwrap();
    for options in [
        "speech_bandpass_low_hz=4000",
        "speech_bandpass_high_hz=200",
        "speech_bandpass_low_hz=1500\nspeech_bandpass_high_hz=1500",
        "speech_bandpass_low_hz=2000\nspeech_bandpass_high_hz=1000",
    ] {
        let document = account(options);
        let settings = PhoneSpeechSettings::resolve(&document, &id);
        assert_eq!(
            (settings.bandpass_low_hz, settings.bandpass_high_hz),
            (300.0, 3000.0)
        );
        let warnings = Schema::validate(&document).unwrap().warnings;
        assert_eq!(warnings.len(), options.lines().count());
        assert!(
            warnings
                .iter()
                .all(|warning| warning.message.contains("lower cutoff"))
        );
    }
}

#[test]
fn phone_speech_invalid_values_never_escape_the_exact_account_defaults() {
    let id = crate::link::PhoneAccountId::new("trunk").unwrap();
    for (key, values) in [
        ("speech_threshold", vec!["NaN", "inf", "-0.1", "1.1", ""]),
        (
            "speech_onset_frames",
            vec!["0", "-1", "1.5", "4294967296", ""],
        ),
        ("speech_preroll_ms", vec!["-1", "301", "1.5", ""]),
        ("speech_reserve_ms", vec!["0", "85", "281", ""]),
        ("speech_hold_ms", vec!["-1", "4294967296", "1.5", ""]),
        (
            "speech_bandpass_low_hz",
            vec!["0", "NaN", "inf", "-1", "24000", ""],
        ),
        (
            "speech_bandpass_high_hz",
            vec!["0", "NaN", "inf", "-1", "24000", ""],
        ),
    ] {
        for value in values {
            let document = account(&format!("{key}={value}"));
            assert_eq!(
                PhoneSpeechSettings::resolve(&document, &id),
                PhoneSpeechSettings::default(),
                "{key}={value}"
            );
            assert_eq!(
                Schema::validate(&document).unwrap().warnings.len(),
                1,
                "{key}={value}"
            );
        }
    }
    let document = ConfigDocument::parse("[general]\nspeech_preprocessing_enabled=yes\n[station]\nspeech_threshold=0.9\n[phone_account other]\nspeech_preprocessing_enabled=yes\n[phone_account trunk]\nlocal_node=station\nserver=pbx:4569\nusername=user\nsecrets_file=/secret").unwrap();
    assert_eq!(
        PhoneSpeechSettings::resolve(&document, &id),
        PhoneSpeechSettings::default()
    );
}
