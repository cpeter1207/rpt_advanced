use rpt_advanced_core::{
    command::{CommandMapError, CommandMapping, DtmfCommandMap, LinkAction},
    phone_dtmf::{PhoneDtmfAction as Action, PhoneDtmfGrammar, PhoneDtmfOptions},
};

fn grammar() -> PhoneDtmfGrammar {
    PhoneDtmfGrammar::new(
        &DtmfCommandMap::standard(),
        PhoneDtmfOptions {
            telephone_egress: true,
            manual_admission: true,
            ..PhoneDtmfOptions::default()
        },
    )
    .unwrap()
}

fn input(grammar: &mut PhoneDtmfGrammar, text: &str, start: u64) -> Vec<Action> {
    text.chars()
        .enumerate()
        .flat_map(|(index, digit)| grammar.digit(digit, start + index as u64))
        .collect()
}

fn telephone(text: &str) -> Vec<Action> {
    text.chars().map(Action::TelephoneDigit).collect()
}

#[test]
fn phone_only_default_target_uses_the_real_bound_node_and_keeps_explicit_routes() {
    for target in ["local-repeater", "1000"] {
        let mut grammar = PhoneDtmfGrammar::new(
            &DtmfCommandMap::standard(),
            PhoneDtmfOptions {
                default_command_target: Some(target.into()),
                ..PhoneDtmfOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            input(&mut grammar, "*4#", 0),
            [Action::SelectTarget(target.into())]
        );
        assert_eq!(grammar.selected_target(), Some(target));
        assert_eq!(
            input(&mut grammar, "*70**#", 10),
            "*70#"
                .chars()
                .map(|digit| Action::NodeDigit {
                    target: target.into(),
                    digit
                })
                .collect::<Vec<_>>()
        );
        input(&mut grammar, "#", 20);
        assert_eq!(
            input(&mut grammar, "*42000#", 30),
            [Action::SelectTarget("2000".into())]
        );
        input(&mut grammar, "#", 40);
        assert_eq!(input(&mut grammar, "*3#", 50), [Action::Attach(None)]);
    }
    assert_eq!(
        input(&mut grammar(), "*4#", 0),
        [Action::CollectionDiscarded]
    );
    for target in ["", "bad name", "a[b", &"a".repeat(64)] {
        assert!(
            PhoneDtmfGrammar::new(
                &DtmfCommandMap::standard(),
                PhoneDtmfOptions {
                    default_command_target: Some(target.into()),
                    ..PhoneDtmfOptions::default()
                }
            )
            .is_err()
        );
    }
    let mut mappings = DtmfCommandMap::standard().mappings().to_vec();
    mappings
        .iter_mut()
        .find(|mapping| mapping.action == LinkAction::Command)
        .unwrap()
        .digits
        .clear();
    let map = DtmfCommandMap::new(mappings).unwrap();
    let mut disabled = PhoneDtmfGrammar::new(
        &map,
        PhoneDtmfOptions {
            default_command_target: Some("local-repeater".into()),
            ..PhoneDtmfOptions::default()
        },
    )
    .unwrap();
    assert!(
        !input(&mut disabled, "*4#", 0)
            .iter()
            .any(|action| matches!(action, Action::SelectTarget(_)))
    );
    input(&mut disabled, "*", 100);
    assert_eq!(disabled.finish(), [Action::CollectionDiscarded]);
}

#[test]
fn ordinary_digits_and_nonrouting_star_menus_remain_telephone_digits() {
    assert_eq!(
        input(&mut grammar(), "12#ABCD*70#*91*90", 0),
        telephone("12#ABCD*70#*91*90")
    );
}

#[test]
fn explicit_attachment_and_addressed_commands_are_distinct_requests() {
    let mut grammar = grammar();
    assert_eq!(input(&mut grammar, "*3#", 0), [Action::Attach(None)]);
    assert_eq!(
        input(&mut grammar, "*31000#", 10),
        [Action::Attach(Some("1000".into()))]
    );
    assert_eq!(
        input(&mut grammar, "*41000#", 20),
        [Action::SelectTarget("1000".into())]
    );
    assert_eq!(grammar.selected_target(), Some("1000"));
    let actions = input(&mut grammar, "*70**#", 30);
    assert_eq!(
        actions,
        "*70#"
            .chars()
            .map(|digit| Action::NodeDigit {
                target: "1000".into(),
                digit
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(grammar.selected_target(), Some("1000"));
    assert_eq!(input(&mut grammar, "#", 40), [Action::ExitCommandMode]);
    assert_eq!(grammar.selected_target(), None);
}

#[test]
fn quoting_consumes_one_symbol_and_preserves_literal_star_and_pound_menus() {
    assert_eq!(input(&mut grammar(), "***3***4**#", 0), telephone("*3*4#"));
}

#[test]
fn manual_actions_are_call_local_and_vad_admission_never_changes() {
    for manual_admission in [false, true] {
        let mut grammar = PhoneDtmfGrammar::new(
            &DtmfCommandMap::standard(),
            PhoneDtmfOptions {
                manual_admission,
                ..PhoneDtmfOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            input(&mut grammar, "*41000#", 0),
            [Action::SelectTarget("1000".into())]
        );
        assert_eq!(
            input(&mut grammar, "*91", 10),
            [if manual_admission {
                Action::ManualKey
            } else {
                Action::ManualUnavailable
            }]
        );
        assert_eq!(
            input(&mut grammar, "*90", 20),
            [if manual_admission {
                Action::ManualUnkey
            } else {
                Action::ManualUnavailable
            }]
        );
        assert_eq!(grammar.selected_target(), Some("1000"));
    }
}

#[test]
fn reverse_calls_have_no_ordinary_digit_echo_or_implicit_attachment() {
    let mut grammar =
        PhoneDtmfGrammar::new(&DtmfCommandMap::standard(), PhoneDtmfOptions::default()).unwrap();
    assert!(input(&mut grammar, "1234#***3**#", 0).is_empty());
    assert_eq!(input(&mut grammar, "*3#", 30), [Action::Attach(None)]);
}

#[test]
fn mapping_and_manual_remapping_use_the_effective_namespace() {
    let mappings = DtmfCommandMap::standard()
        .mappings()
        .iter()
        .map(|mapping| {
            CommandMapping::new(
                match mapping.action {
                    LinkAction::Transceive => "33",
                    LinkAction::Command => "44",
                    _ => &mapping.digits,
                },
                mapping.action,
            )
        })
        .collect();
    let map = DtmfCommandMap::new(mappings).unwrap();
    let mut grammar = PhoneDtmfGrammar::new(
        &map,
        PhoneDtmfOptions {
            telephone_egress: true,
            manual_admission: true,
            manual_key: "92".into(),
            manual_unkey: "93".into(),
            ..PhoneDtmfOptions::default()
        },
    )
    .unwrap();
    assert_eq!(input(&mut grammar, "*33#", 0), [Action::Attach(None)]);
    assert_eq!(
        input(&mut grammar, "*441000#", 10),
        [Action::SelectTarget("1000".into())]
    );
    assert_eq!(
        input(&mut grammar, "*92*93", 20),
        [Action::ManualKey, Action::ManualUnkey]
    );
}

#[test]
fn manual_prefixes_cannot_overlap_commands_macros_remote_base_or_each_other() {
    let standard = DtmfCommandMap::standard();
    for prefix in ["3", "31", "4", "41", "800", "8001", "8", "90", "9"] {
        let options = PhoneDtmfOptions {
            manual_key: prefix.into(),
            ..PhoneDtmfOptions::default()
        };
        assert_eq!(
            PhoneDtmfGrammar::new(&standard, options).err(),
            Some(CommandMapError::AmbiguousPrefix)
        );
    }
    for prefix in [
        String::new(),
        "*91".into(),
        "9#".into(),
        "a".into(),
        "9".repeat(64),
    ] {
        let options = PhoneDtmfOptions {
            manual_key: prefix,
            ..PhoneDtmfOptions::default()
        };
        assert_eq!(
            PhoneDtmfGrammar::new(&standard, options).err(),
            Some(CommandMapError::InvalidPrefix)
        );
    }
    let map = standard.with_macro_triggers(vec!["95".into()]).unwrap();
    let options = PhoneDtmfOptions {
        manual_key: "951".into(),
        ..PhoneDtmfOptions::default()
    };
    assert_eq!(
        PhoneDtmfGrammar::new(&map, options).err(),
        Some(CommandMapError::AmbiguousPrefix)
    );
    let mut mappings = standard.mappings().to_vec();
    mappings
        .iter_mut()
        .find(|mapping| mapping.action == LinkAction::RemoteBase)
        .unwrap()
        .digits = "9".into();
    let map = DtmfCommandMap::new(mappings).unwrap();
    assert_eq!(
        PhoneDtmfGrammar::new(&map, PhoneDtmfOptions::default()).err(),
        Some(CommandMapError::AmbiguousPrefix)
    );
}

#[test]
fn collection_completes_at_three_seconds_and_unfinished_quotes_are_discarded() {
    let mut grammar = grammar();
    assert!(input(&mut grammar, "*3", 100).is_empty());
    assert!(grammar.tick(3100).is_empty());
    assert_eq!(grammar.tick(3101), [Action::Attach(None)]);
    assert!(input(&mut grammar, "**", 4000).is_empty());
    assert_eq!(grammar.tick(7001), [Action::CollectionDiscarded]);
    assert!(grammar.tick(8000).is_empty());
    assert_eq!(input(&mut grammar, "4", 8001), telephone("4"));
    assert!(input(&mut grammar, "*41000", 9000).is_empty());
    assert_eq!(grammar.finish(), [Action::SelectTarget("1000".into())]);
}

#[test]
fn oversized_or_malformed_routes_are_private_and_do_not_replay_their_tail() {
    let mut grammar = grammar();
    assert_eq!(
        input(&mut grammar, &format!("*3{}#", "1".repeat(63)), 0),
        [Action::Attach(Some("1".repeat(63)))]
    );
    for suffix in ["1".repeat(64), "A".into(), String::new()] {
        assert_eq!(
            input(&mut grammar, &format!("*4{suffix}#"), 200),
            [Action::CollectionDiscarded]
        );
    }
    assert_eq!(
        input(&mut grammar, &format!("*3{}123#", "1".repeat(127)), 400),
        [Action::CollectionDiscarded]
    );
    assert_eq!(input(&mut grammar, "5", 1000), telephone("5"));
}

#[test]
fn node_mode_routing_and_quoted_manual_prefixes_reach_only_the_selected_parser() {
    let mut grammar = grammar();
    input(&mut grammar, "*41000#", 0);
    let text = "*31000**#***91";
    let expected = "*31000#*91"
        .chars()
        .map(|digit| Action::NodeDigit {
            target: "1000".into(),
            digit,
        })
        .collect::<Vec<_>>();
    assert_eq!(input(&mut grammar, text, 20), expected);
    assert_eq!(grammar.selected_target(), Some("1000"));
}

#[test]
fn cancellation_clears_quotes_targets_and_collection_without_replay() {
    for prefix in ["**", "*31000", "*41000#**"] {
        let mut grammar = grammar();
        input(&mut grammar, prefix, 0);
        grammar.cancel();
        assert_eq!(grammar.selected_target(), None);
        assert!(grammar.tick(4000).is_empty());
        assert_eq!(input(&mut grammar, "3", 5000), telephone("3"));
    }
}

#[test]
fn credentials_are_target_digits_and_debug_output_is_redacted() {
    let mut grammar = grammar();
    input(&mut grammar, "*41000#", 0);
    let actions = input(&mut grammar, "*8001234**#", 20);
    let expected = "*8001234#"
        .chars()
        .map(|digit| Action::NodeDigit {
            target: "1000".into(),
            digit,
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, expected);
    let debug = format!("{actions:?}");
    assert!(!debug.contains("'1'"), "digit Debug must be redacted");
    assert!(!debug.contains("1000"), "target Debug must be redacted");
}

#[test]
fn incomplete_selection_and_invalid_symbols_recover_without_replaying_control_digits() {
    let mut grammar = grammar();
    assert_eq!(
        input(&mut grammar, "*312*3#", 0),
        [Action::CollectionDiscarded, Action::Attach(None)]
    );
    assert_eq!(grammar.digit('!', 100), [Action::CollectionDiscarded]);
    assert!(input(&mut grammar, "123#", 101).is_empty());
    assert_eq!(input(&mut grammar, "5", 200), telephone("5"));
    assert_eq!(grammar.digit('\0', 201), [Action::CollectionDiscarded]);
    assert_eq!(input(&mut grammar, "*3#", 202), [Action::Attach(None)]);
    assert_eq!(grammar.digit('é', 300), [Action::CollectionDiscarded]);
    assert!(grammar.finish().is_empty());
    assert!(grammar.finish().is_empty());
    assert!(input(&mut grammar, "*", 400).is_empty());
    assert_eq!(grammar.finish(), [Action::CollectionDiscarded]);
}

#[test]
fn node_partial_prefixes_and_quote_expiry_preserve_the_selected_sink() {
    let mut grammar = grammar();
    input(&mut grammar, "*41000#", 0);
    let node = |text: &str| {
        text.chars()
            .map(|digit| Action::NodeDigit {
                target: "1000".into(),
                digit,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(input(&mut grammar, "*9*70", 20), node("*9*70"));
    assert!(input(&mut grammar, "*9", 100).is_empty());
    assert_eq!(grammar.finish(), node("*9"));
    assert!(input(&mut grammar, "**", 200).is_empty());
    assert!(grammar.tick(199).is_empty());
    assert_eq!(grammar.tick(3201), [Action::CollectionDiscarded]);
    assert_eq!(grammar.selected_target(), Some("1000"));
    assert!(input(&mut grammar, "*9", 4000).is_empty());
    assert_eq!(input(&mut grammar, "#", 4002), [Action::ExitCommandMode]);
}

#[test]
fn disabled_routes_and_legacy_public_prefix_overlap_do_not_create_implicit_commands() {
    let mappings = DtmfCommandMap::standard()
        .mappings()
        .iter()
        .cloned()
        .map(|mut mapping| {
            if matches!(mapping.action, LinkAction::Transceive | LinkAction::Command) {
                mapping.digits.clear();
            }
            mapping
        })
        .collect();
    let map = DtmfCommandMap::new(mappings).unwrap();
    let mut disabled = PhoneDtmfGrammar::new(
        &map,
        PhoneDtmfOptions {
            telephone_egress: true,
            ..PhoneDtmfOptions::default()
        },
    )
    .unwrap();
    assert_eq!(input(&mut disabled, "*3*41000#", 0), telephone("*3*41000#"));
    assert!(input(&mut disabled, "*", 100).is_empty());
    assert_eq!(disabled.finish(), [Action::CollectionDiscarded]);
    let map = DtmfCommandMap::new(vec![
        CommandMapping::new("3", LinkAction::Transceive),
        CommandMapping::new("4", LinkAction::Command),
        CommandMapping::new("31", LinkAction::Status),
    ])
    .unwrap();
    let mut overlap = PhoneDtmfGrammar::new(&map, PhoneDtmfOptions::default()).unwrap();
    assert_eq!(
        input(&mut overlap, "*31#", 0),
        [Action::CollectionDiscarded]
    );
}

#[test]
fn distinct_manual_prefix_families_remain_configurable() {
    let mut grammar = PhoneDtmfGrammar::new(
        &DtmfCommandMap::standard(),
        PhoneDtmfOptions {
            manual_admission: true,
            manual_unkey: "A0".into(),
            ..PhoneDtmfOptions::default()
        },
    )
    .unwrap();
    input(&mut grammar, "*41000#", 0);
    assert_eq!(input(&mut grammar, "*A0", 100), [Action::ManualUnkey]);
}

#[test]
fn every_action_and_pending_collection_redacts_payloads_in_debug() {
    let actions = [
        Action::TelephoneDigit('8'),
        Action::Attach(Some("10001234".into())),
        Action::SelectTarget("10001234".into()),
        Action::NodeDigit {
            target: "10001234".into(),
            digit: '8',
        },
        Action::ExitCommandMode,
        Action::ManualKey,
        Action::ManualUnkey,
        Action::ManualUnavailable,
        Action::CollectionDiscarded,
    ];
    let debug = format!("{actions:?}");
    assert!(!debug.contains("10001234"));
    assert!(!debug.contains("'8'"));
    let mut grammar = grammar();
    input(&mut grammar, "*310001234", 0);
    assert!(!format!("{grammar:?}").contains("10001234"));
}
