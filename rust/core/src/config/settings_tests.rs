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
