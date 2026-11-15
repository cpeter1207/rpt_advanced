use super::default_command;

#[test]
fn unknown_command_default_is_empty() {
    assert_eq!(default_command("unknown command"), "");
}

#[test]
fn dtmf_defaults_inherit_and_invalid_values_warn() {
    use super::{ConfigDocument, NodeId, ResolvedDtmfRegenerationSettings, ResolvedNodeSettings};
    let node = NodeId::new("1000").unwrap();
    let resolve = |text: &str| {
        ResolvedNodeSettings::resolve(&ConfigDocument::parse(text).unwrap(), &node).unwrap()
    };
    let defaults = resolve("[1000]\n");
    assert_eq!(
        defaults.value.dtmf_regeneration,
        ResolvedDtmfRegenerationSettings {
            enabled: false,
            duration_ms: 200,
            gap_ms: 50,
            level_dbfs: -12,
        }
    );
    assert_eq!(defaults.value.link_commands["link_command_pad_test"], "82");
    let general = "[general]\ndtmf_regeneration_enabled=yes\ndtmf_regeneration_duration_ms=1\ndtmf_regeneration_gap_ms=1000\ndtmf_regeneration_level_dbfs=-60\n";
    let inherited = resolve(&format!("{general}[1000]\n"));
    assert!(inherited.warnings.is_empty());
    assert_eq!(
        inherited.value.dtmf_regeneration,
        ResolvedDtmfRegenerationSettings {
            enabled: true,
            duration_ms: 1,
            gap_ms: 1000,
            level_dbfs: -60,
        }
    );
    for (key, value) in [
        ("enabled", "maybe"),
        ("duration_ms", "0"),
        ("duration_ms", "1001"),
        ("gap_ms", "0"),
        ("gap_ms", "1.5"),
        ("level_dbfs", "-61"),
        ("level_dbfs", "1"),
    ] {
        let result = resolve(&format!(
            "{general}[1000]\ndtmf_regeneration_{key}={value}\n"
        ));
        assert_eq!(result.warnings.len(), 1, "{key} {value}");
        assert_eq!(
            result.value.dtmf_regeneration,
            inherited.value.dtmf_regeneration
        );
    }
    let overridden = resolve(&format!(
        "{general}[1000]\ndtmf_regeneration_enabled=no\ndtmf_regeneration_duration_ms=1000\ndtmf_regeneration_gap_ms=1\ndtmf_regeneration_level_dbfs=0\nlink_command_pad_test=83\n"
    ));
    assert!(overridden.warnings.is_empty());
    assert_eq!(
        overridden.value.dtmf_regeneration,
        ResolvedDtmfRegenerationSettings {
            enabled: false,
            duration_ms: 1000,
            gap_ms: 1,
            level_dbfs: 0,
        }
    );
    assert_eq!(
        overridden
            .value
            .command_map()
            .unwrap()
            .parse("83")
            .unwrap()
            .action,
        crate::command::LinkAction::PadTest
    );
}

#[test]
fn radio_phone_prefixes_are_explicit_and_preserve_existing_defaults() {
    use super::{ConfigDocument, NodeId, ResolvedNodeSettings};
    let node = NodeId::new("1000").unwrap();
    let resolve = |text: &str| {
        ResolvedNodeSettings::resolve(&ConfigDocument::parse(text).unwrap(), &node).unwrap()
    };
    let configured = resolve(
        "[general]\nlink_command_phone_dial=6\nlink_command_phone_hangup=0\nphone_outgoing_account=provider\n[1000]\n",
    );
    assert!(
        configured.warnings.is_empty(),
        "explicit phone actions and account must be recognized"
    );
    assert_eq!(
        configured
            .value
            .phone_outgoing_account
            .as_ref()
            .unwrap()
            .as_str(),
        "provider"
    );
    for (value, expected, warnings) in [
        ("provider/other", Some("provider"), 1),
        ("other", Some("other"), 0),
        ("", None, 0),
    ] {
        let inherited = resolve(&format!(
            "[general]\nphone_outgoing_account=provider\n[1000]\nphone_outgoing_account={value}\n",
        ));
        assert_eq!(
            inherited
                .value
                .phone_outgoing_account
                .as_ref()
                .map(|id| id.as_str()),
            expected
        );
        assert_eq!(inherited.warnings.len(), warnings);
    }
    let fallback =
        resolve("[general]\nlink_command_phone_dial=6\n[1000]\nlink_command_phone_dial=bad*\n");
    assert_eq!(fallback.warnings.len(), 1);
    assert_eq!(fallback.value.link_commands["link_command_phone_dial"], "6");
    let map = configured.value.command_map().unwrap();
    let dial = map.parse("600123").expect("configured phone dial");
    assert_eq!(dial.action, crate::command::LinkAction::PhoneDial);
    assert_eq!(dial.node, "00123");
    assert_eq!(
        map.parse("0").unwrap().action,
        crate::command::LinkAction::PhoneHangup
    );
    assert!(map.with_macro_triggers(vec!["6".into()]).is_err());
    assert!(map.with_macro_triggers(vec!["0".into()]).is_err());
    assert_eq!(map.parse("0").expect("configured phone hangup").node, "");
    assert_eq!(map.parse("32000").unwrap().node, "2000");
    let defaults = resolve("[1000]\n");
    assert_eq!(
        defaults
            .value
            .link_commands
            .get("link_command_phone_dial")
            .map(String::as_str),
        Some("")
    );
    assert_eq!(
        defaults
            .value
            .link_commands
            .get("link_command_phone_hangup")
            .map(String::as_str),
        Some("")
    );
    assert!(
        defaults
            .value
            .command_map()
            .unwrap()
            .parse("600123")
            .is_none()
    );
    assert!(defaults.value.phone_outgoing_account.is_none());
    assert!(
        defaults
            .value
            .command_map()
            .unwrap()
            .with_macro_triggers(vec!["6".into(), "0".into()])
            .is_ok()
    );
}
