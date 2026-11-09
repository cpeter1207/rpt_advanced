use super::*;

fn settings(commands: &str) -> ResolvedDtmfMacroSettings {
    ResolvedDtmfMacroSettings {
        name: "status".into(),
        trigger: "*95".into(),
        protected: false,
        commands: commands.into(),
    }
}

fn compile_text(commands: &str) -> Result<CompiledDtmfMacro, MacroCompileError> {
    compile(
        &settings(commands),
        DtmfMacroLimits::default(),
        &DtmfCommandMap::standard(),
        "1000",
        &["*95".into()],
    )
}

#[test]
fn dtmf_macro_compiler_consumes_selector_terminator_and_adds_comma_pauses() {
    let compiled = compile_text(" *41000# *70#,, *73# ").unwrap();
    assert_eq!(
        compiled.steps,
        [
            MacroStep::SelectTarget("1000".into()),
            MacroStep::Command("70".into()),
            MacroStep::Pause(4000),
            MacroStep::Command("73".into()),
        ]
    );
}

#[test]
fn dtmf_macro_compiler_rejects_partial_credentials_nested_and_unverified_targets() {
    for text in [
        "",
        "*70",
        "*3,123#",
        "*70#oops",
        "*800123456#",
        "*801123456#",
        "*95#",
        "*42000#*70#",
        "*82#",
        "*70#;*73#",
        "*70#,",
    ] {
        assert!(compile_text(text).is_err(), "{text}");
    }
    assert_eq!(
        compile_text("*801#*804#").unwrap().steps,
        [
            MacroStep::Command("801".into()),
            MacroStep::Command("804".into())
        ]
    );
}

#[test]
fn dtmf_macro_compiler_bounds_symbols_and_total_pause_without_overflow() {
    let mut limits = DtmfMacroLimits {
        max_symbols: 8,
        ..Default::default()
    };
    let map = DtmfCommandMap::standard();
    assert!(compile(&settings("*70#*73#"), limits, &map, "1000", &[]).is_ok());
    limits.max_symbols = 7;
    assert!(compile(&settings("*70#*73#"), limits, &map, "1000", &[]).is_err());
    limits = DtmfMacroLimits {
        pause_ms: 120_000,
        ..Default::default()
    };
    assert!(compile(&settings("*70#,*73#"), limits, &map, "1000", &[]).is_ok());
    assert!(compile(&settings("*70#,,*73#"), limits, &map, "1000", &[]).is_err());
    limits.pause_ms = u64::MAX;
    assert!(compile(&settings("*70#,,*73#"), limits, &map, "1000", &[]).is_err());
    let text = "*70#".repeat(1024);
    assert!(compile_text(&text).is_ok());
    assert!(compile_text(&(text + "*70#")).is_err());
}

#[test]
fn dtmf_macro_compiler_uses_effective_remapped_credential_and_selector_actions() {
    let mappings = DtmfCommandMap::standard()
        .mappings()
        .iter()
        .cloned()
        .map(|mut mapping| {
            mapping.digits = match mapping.action {
                LinkAction::AdminUnlock => "96".into(),
                LinkAction::AdminLock => "97".into(),
                LinkAction::Command => "98".into(),
                _ => mapping.digits,
            };
            mapping
        })
        .collect();
    let map = DtmfCommandMap::new(mappings).unwrap();
    for text in ["*961234#", "*971234#", "*8001234#", "*4#"] {
        assert!(
            compile(
                &settings(text),
                DtmfMacroLimits::default(),
                &map,
                "1000",
                &[]
            )
            .is_err()
        );
    }
    assert_eq!(
        compile(
            &settings("*981000#*97#*3911#*3999#"),
            DtmfMacroLimits::default(),
            &map,
            "1000",
            &[]
        )
        .unwrap()
        .steps,
        [
            MacroStep::SelectTarget("1000".into()),
            MacroStep::Command("97".into()),
            MacroStep::Command("3911".into()),
            MacroStep::Command("3999".into())
        ]
    );
}

#[test]
fn dtmf_macro_compiler_validates_external_trigger_and_stream_boundaries() {
    let map = DtmfCommandMap::standard();
    for trigger in [
        "".into(),
        "95".into(),
        "*".into(),
        "*x".into(),
        format!("*{}", "9".repeat(64)),
    ] {
        let mut definition = settings("*70#");
        definition.trigger = trigger;
        assert!(compile(&definition, DtmfMacroLimits::default(), &map, "1000", &[]).is_err());
    }
    for trigger in ["*AB".into(), format!("*{}", "9".repeat(63))] {
        let mut definition = settings("*70#");
        definition.trigger = trigger;
        assert!(compile(&definition, DtmfMacroLimits::default(), &map, "1000", &[]).is_ok());
    }
    for (trigger, others) in [
        ("*3", vec![]),
        ("*391", vec![]),
        ("*7", vec![]),
        ("*95", vec!["*95", "*95"]),
        ("*95", vec!["*951"]),
        ("*951", vec!["*95"]),
    ] {
        let mut definition = settings("*70#");
        definition.trigger = trigger.into();
        let others = others.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            compile(
                &definition,
                DtmfMacroLimits::default(),
                &map,
                "1000",
                &others
            )
            .is_err()
        );
    }
    for commands in [" ", "#", "*#", "70#", ",*70#", "*70#*", "*70#,,"] {
        assert!(compile_text(commands).is_err(), "{commands}");
    }
    for limits in [
        DtmfMacroLimits {
            max_symbols: 0,
            ..Default::default()
        },
        DtmfMacroLimits {
            max_duration_ms: 0,
            ..Default::default()
        },
        DtmfMacroLimits {
            pause_ms: 0,
            ..Default::default()
        },
        DtmfMacroLimits {
            pause_ms: u64::MAX,
            max_duration_ms: u64::MAX,
            ..Default::default()
        },
    ] {
        assert!(compile(&settings("*70#,,*73#"), limits, &map, "1000", &[]).is_err());
    }
}

#[test]
fn dtmf_macro_compiler_accepts_the_enabled_typed_remote_base_family() {
    let mappings = DtmfCommandMap::standard()
        .mappings()
        .iter()
        .cloned()
        .map(|mut mapping| {
            if mapping.action == LinkAction::RemoteBase {
                mapping.digits = "6".into();
            }
            mapping
        })
        .collect();
    let map = DtmfCommandMap::new(mappings).unwrap();
    let compiled = compile(
        &settings("*651#"),
        DtmfMacroLimits::default(),
        &map,
        "1000",
        &[],
    )
    .unwrap();
    assert_eq!(compiled.steps, [MacroStep::Command("651".into())]);
}
