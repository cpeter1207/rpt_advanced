use super::{CommandMapError, CommandMapping, DtmfCommandMap, LinkAction};

#[test]
fn validated_mappings_and_errors_preserve_operator_diagnostics() {
    let mappings = vec![CommandMapping::new("7", LinkAction::Status)];
    assert_eq!(
        DtmfCommandMap::new(mappings.clone()).unwrap().mappings(),
        mappings
    );
    assert_eq!(
        CommandMapError::InvalidPrefix.to_string(),
        "invalid command prefix"
    );
    assert_eq!(
        CommandMapError::AmbiguousPrefix.to_string(),
        "ambiguous command prefix"
    );
}

fn mappings() -> Vec<CommandMapping> {
    vec![
        ("1", LinkAction::Disconnect),
        ("2", LinkAction::Monitor),
        ("3", LinkAction::Transceive),
        ("4", LinkAction::Command),
        ("70", LinkAction::Status),
        ("806", LinkAction::DisconnectAll),
        ("72", LinkAction::LastKeyed),
        ("75", LinkAction::LocalMonitor),
        ("811", LinkAction::DisconnectPermanent),
        ("812", LinkAction::PermanentMonitor),
        ("813", LinkAction::PermanentTransceive),
        ("73", LinkAction::FullStatus),
        ("816", LinkAction::ReconnectAll),
        ("818", LinkAction::PermanentLocalMonitor),
        ("10", LinkAction::DisconnectNonPermanentAll),
        ("722", LinkAction::Time),
    ]
    .into_iter()
    .map(|(digits, action)| CommandMapping::new(digits, action))
    .collect()
}

#[test]
fn disabled_mappings_are_ignored_and_custom_prefix_bounds_are_exact() {
    let mut collector = DtmfCommandMap::standard().collector();
    collector.feed('*', 0);
    assert!(collector.feed('A', 1).is_none());
    assert!(collector.active());
    for prefix in ["1".repeat(64), "*".into(), "E".into()] {
        assert!(matches!(
            DtmfCommandMap::new(vec![CommandMapping::new(&prefix, LinkAction::Status)]),
            Err(CommandMapError::InvalidPrefix)
        ));
    }
    for mappings in [
        vec![
            CommandMapping::new("", LinkAction::Status),
            CommandMapping::new("3", LinkAction::Transceive),
        ],
        vec![
            CommandMapping::new("3", LinkAction::Transceive),
            CommandMapping::new("", LinkAction::Status),
        ],
    ] {
        let map = DtmfCommandMap::new(mappings).unwrap();
        assert!(map.parse("").is_none());
        assert_eq!(map.parse("32000").unwrap().node, "2000");
        let mut collector = map.collector();
        collector.feed('*', 0);
        collector.feed('3', 0);
        assert!(collector.feed('\0', 2999).is_none());
    }
    assert!(
        DtmfCommandMap::new(vec![CommandMapping::new(
            "1".repeat(63),
            LinkAction::Status
        )])
        .is_ok()
    );
}

#[test]
fn parses_node_and_node_free_actions_without_mutating_failed_results() {
    let map = DtmfCommandMap::new(mappings()).unwrap();
    let cases = [
        ("1524950", LinkAction::Disconnect, "524950"),
        ("2524950", LinkAction::Monitor, "524950"),
        ("3524950", LinkAction::Transceive, "524950"),
        ("4524950", LinkAction::Command, "524950"),
        ("70", LinkAction::Status, ""),
        ("806", LinkAction::DisconnectAll, ""),
        ("72", LinkAction::LastKeyed, ""),
        ("75524950", LinkAction::LocalMonitor, "524950"),
        ("811524950", LinkAction::DisconnectPermanent, "524950"),
        ("812524950", LinkAction::PermanentMonitor, "524950"),
        ("813524950", LinkAction::PermanentTransceive, "524950"),
        ("73", LinkAction::FullStatus, ""),
        ("816", LinkAction::ReconnectAll, ""),
        ("818524950", LinkAction::PermanentLocalMonitor, "524950"),
        ("722", LinkAction::Time, ""),
    ];
    for (digits, action, node) in cases {
        let command = map.parse(digits).unwrap();
        assert_eq!(command.action, action);
        assert_eq!(command.node, node);
        assert!(map.parse(&format!("{digits}A")).is_none());
    }
    assert_eq!(map.parse("30").unwrap().node, "0");
    for invalid in [
        "", "3", "*3524950", "3524950#", "700", "80", "81", "99", "511", "6",
    ] {
        assert!(
            map.parse(invalid).is_none(),
            "unexpectedly valid {invalid:?}"
        );
    }
}

#[test]
fn validates_prefixes_by_the_longer_mapping_kind() {
    let valid_node_free = [
        CommandMapping::new("1", LinkAction::Disconnect),
        CommandMapping::new("10", LinkAction::DisconnectNonPermanentAll),
    ];
    assert!(DtmfCommandMap::validate(&valid_node_free).is_ok());

    let valid_two_stage = [
        CommandMapping::new("72", LinkAction::LastKeyed),
        CommandMapping::new("722", LinkAction::Time),
    ];
    assert!(DtmfCommandMap::validate(&valid_two_stage).is_ok());

    let invalid_longer_node = [
        CommandMapping::new("7", LinkAction::Status),
        CommandMapping::new("72", LinkAction::Transceive),
    ];
    assert!(DtmfCommandMap::validate(&invalid_longer_node).is_err());

    let invalid_equal = [
        CommandMapping::new("7", LinkAction::Status),
        CommandMapping::new("7", LinkAction::FullStatus),
    ];
    assert!(DtmfCommandMap::validate(&invalid_equal).is_err());
}

#[test]
fn collector_defers_extendable_node_free_commands_and_honors_termination() {
    let map = DtmfCommandMap::default();
    let mut collector = map.collector();
    assert_eq!(collector.feed('*', 5), None);
    assert_eq!(collector.feed('1', 100), None);
    assert_eq!(collector.feed('0', 100), Some("10".to_owned()));

    assert_eq!(collector.feed('*', 500), None);
    assert_eq!(collector.feed('7', 500), None);
    assert_eq!(collector.feed('2', 500), None);
    assert_eq!(collector.feed('#', 500), Some("72".to_owned()));

    assert_eq!(collector.feed('*', 600), None);
    assert_eq!(collector.feed('7', 600), None);
    assert_eq!(collector.feed('2', 600), None);
    assert_eq!(collector.feed('2', 600), Some("722".to_owned()));

    assert_eq!(collector.feed('*', 700), None);
    assert_eq!(collector.feed('3', 700), None);
    assert_eq!(collector.feed('1', 700), None);
    assert_eq!(collector.feed('2', 700), None);
    assert_eq!(collector.feed('\0', 3_699), None);
    assert_eq!(collector.feed('\0', 3_700), Some("312".to_owned()));
}

#[test]
fn collector_cancels_on_invalid_digit_and_bounds_input() {
    let map = DtmfCommandMap::default();
    let mut collector = map.collector();
    assert_eq!(collector.feed('*', 0), None);
    assert_eq!(collector.feed('x', 0), None);
    assert!(!collector.active());

    assert_eq!(collector.feed('*', 0), None);
    for _ in 0..127 {
        assert_eq!(collector.feed('9', 0), None);
    }
    assert_eq!(collector.feed('9', 0), None);
    assert!(!collector.active());
}
