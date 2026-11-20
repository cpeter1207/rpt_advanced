use super::*;
use crate::{
    command::LinkAction,
    remote_base_command::{RemoteCommand, parse_remote_command},
};
use serde_json::{Value, json};
#[test]
fn prepared_macros_require_destinations_only_for_link_actions_that_use_them() {
    let parse = |body| prepare_macro_action(&body);
    assert!(parse(json!({"action":"disconnect_all"})).is_ok());
    assert!(parse(json!({"action":"connect_permanent"})).is_err());
    assert!(parse(json!({"action":"connect_permanent","parameter":"0"})).is_ok());
    assert!(parse(json!({"action":"connect","parameter":"0002","enabled":true})).is_err());
}
#[test]
fn every_example_has_an_exact_typed_shape_and_persisted_source_policy() {
    let examples: Value = serde_json::from_str(include_str!(
        "../../../../examples/controller-actions.example.json"
    ))
    .unwrap();
    for (i, v) in examples["actions"].as_array().unwrap().iter().enumerate() {
        assert!(prepare_runtime_action(v).is_ok(), "runtime example {i}");
        let source_only = matches!(
            v["action"].as_str().unwrap(),
            "unlock_administration"
                | "lock_administration"
                | "start_pad_test"
                | "phone_session"
                | "controller_lifecycle"
                | "controller_admission"
                | "radio_host_query"
                | "feed_dtmf_symbol"
                | "actual_receiver_unkey"
                | "controller_input"
        ) || v["action"].as_str().unwrap().starts_with("phone_");
        assert_eq!(
            prepare_macro_action(v).is_err(),
            source_only,
            "persisted example {i}"
        );
    }
}
#[test]
fn defaults_match_actual_radio_decoder_and_keep_raw_tails() {
    let catalog = CommandLanguage::standard();
    let inventory: Value =
        serde_json::from_str(include_str!("../../../../docs/dtmf-mapping-inventory.json")).unwrap();
    assert_eq!(catalog.mappings.len(), 78);
    for (key, body) in inventory["canonical_default_mappings"].as_object().unwrap() {
        assert_eq!(
            catalog.mappings[key].command,
            body["command"].as_str().unwrap()
        );
        assert_eq!(
            catalog.mappings[key].protected,
            body["protected"].as_bool().unwrap()
        );
    }
    for (prefix, tail) in [
        ("*640", ""),
        ("*6711", ""),
        ("*6997", "0000"),
        ("*6997", "0001"),
        ("*62", "100*0"),
        ("*691", "1*1*0012*2"),
    ] {
        let digits = format!("{prefix}{tail}");
        let m = catalog.match_completed(&digits).unwrap();
        assert_eq!(m.parameter, tail);
        let matched = m.decode().unwrap();
        assert!(
            matches!(matched,Action::Radio(RadioAction::Command(c)) if Some(c)==parse_remote_command(&digits[2..]))
        );
    }
    assert!(catalog.match_completed("*6401").unwrap().decode().is_err());
    assert!(matches!(
        catalog.match_completed("*699700").unwrap().decode(),
        Ok(Action::Radio(RadioAction::Command(
            RemoteCommand::Priority(None)
        )))
    ));
    assert!(
        matches!(catalog.match_completed("*816").unwrap().decode(),Ok(Action::Link(c)) if c.action==LinkAction::ReconnectPermanentAll)
    );
    assert!(
        matches!(catalog.match_completed("*806").unwrap().decode(),Ok(Action::Link(c)) if c.action==LinkAction::DisconnectPermanentAll)
    );
    assert!(catalog.match_completed("*99").unwrap().mapping.phone_only);
}
#[test]
fn final_mapping_and_macro_graph_validate_after_both_layers() {
    let site = json!({"macros":{"x":{"actions":[{"action":"call_macro","macro":"y"}]}}, "mappings":{"status":{"command":"A","protected":true},"long":{"command":"AB","protected":true,"action":"call_macro","macro":"x"}}});
    let port = json!({"macros":{"y":{"actions":[{"action":"pause"}]}},"mappings":{"status":{"command":""}}});
    let c = CommandLanguage::resolve(&site, &port).unwrap();
    assert!(c.match_completed("A").is_none());
    let m = c.match_completed("AB001").unwrap();
    assert_eq!(m.parameter, "001");
    assert!(m.mapping.protected && m.decode().is_err());
    for v in [
        json!({}),
        json!({"macros":{"y":{"actions":[{"action":"call_macro","macro":"x"}]}}}),
    ] {
        assert!(CommandLanguage::resolve(&site, &v).is_err());
    }
    for command in ["*70", "A#"] {
        assert!(CommandLanguage::resolve(&json!({}),&json!({"mappings":{"new":{"action":"announce_time","command":command,"protected":false}}})).is_err());
    }
}

#[test]
fn partial_radio_arguments_and_mapped_fixed_state_never_replace_authored_values() {
    let lang=CommandLanguage::resolve(&json!({}),&json!({"mappings":{
        "frequency":{"command":"A","protected":false,"action":"radio","operation":"set_frequency","offset_direction":"minus"},
        "setting":{"command":"B","protected":false,"action":"radio","operation":"set_setting","selector":7},
        "tone":{"command":"C","protected":false,"action":"radio","operation":"set_tone","direction":"tx","frequency_hz":100.0}
    }})).unwrap();
    assert!(matches!(
        lang.match_completed("A146*52*1").unwrap().decode(),
        Ok(Action::Radio(RadioAction::Command(
            RemoteCommand::Frequency {
                hertz: 146520000,
                ..
            }
        )))
    ));
    assert!(lang.match_completed("A146*52*3").unwrap().decode().is_err());
    assert!(lang.match_completed("B7*0*12*0").unwrap().decode().is_ok());
    assert!(lang.match_completed("B1*0*12*0").unwrap().decode().is_err());
    assert!(lang.match_completed("C100*0").unwrap().decode().is_err());
}
#[test]
fn stale_and_invalid_fields_are_absent_from_sparse_projection() {
    let site = json!({"mappings":{"custom":{"action":"radio","operation":"set_frequency","frequency_mhz":146.52,"offset_direction":"plus","command":"A","protected":true}}});
    let port = json!({"mappings":{"custom":{"operation":"set_tone","direction":"rx","frequency_hz":100,"offset_direction":"plus","unknown":"opaque"},"status":{"protected":"no"}},"inactivity_timeout_ms":0});
    let lang = CommandLanguage::resolve(&site, &port).unwrap();
    assert_eq!(
        lang.local[1]["mappings"]["custom"],
        json!({"operation":"set_tone","direction":"rx","frequency_hz":100})
    );
    assert_eq!(lang.local[1]["mappings"]["status"], json!({}));
    assert_eq!(lang.inactivity_timeout_ms, 3000);
    assert!(lang.mappings["custom"].protected);
    let original = json!({"action":"radio","operation":"set_scan_dwell","timing":{"duration_ms":125},"command":"A","protected":true});
    let c = CommandLanguage::resolve(
        &json!({"mappings":{"custom":original}}),
        &json!({"mappings":{"custom":{"timing":{"duration_ms":false,"extra":1}}}}),
    )
    .unwrap();
    assert!(matches!(
        c.match_completed("A").unwrap().decode(),
        Ok(Action::Radio(RadioAction::Command(RemoteCommand::Dwell(
            125
        ))))
    ));
    assert_eq!(c.local[1]["mappings"]["custom"]["timing"], json!({}));
}
#[test]
fn prepared_macro_and_mapping_configuration_cannot_retain_credential_inputs() {
    for body in [
        json!({"macros":{"x":{"actions":[{"action":"execute_dtmf","digits":"*8001234#"}]}}}),
        json!({"macros":{"x":{"actions":[{"action":"lock_administration","parameter":""}]}}}),
        json!({"mappings":{"status":{"parameter":"001"}}}),
        json!({"mappings":{"status":{"code":"001"}}}),
    ] {
        assert!(matches!(
            CommandLanguage::resolve(&body, &json!({})),
            Err(LanguageError::SourcePolicy)
        ));
    }
}

#[test]
fn real_generation_source_and_protection_survive_parameter_conversion_and_nested_waits() {
    use crate::{
        config::resources::candidate::ResourceScope,
        link::{PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource},
        runtime::{GenerationSettings, NodeHost, RuntimeGeneration},
    };
    use std::sync::Arc;
    let mut host = NodeHost::new(
        RuntimeGeneration::prepare(
            4,
            GenerationSettings {
                node: "1234".into(),
                device: "test".into(),
                receive_maximum: 480,
                transmit_maximum: 480,
                squelch_delay_ms: 0,
                status_snapshot_interval_ms: 50,
            },
            (),
            (),
        )
        .unwrap(),
    );
    let lang=Arc::new(CommandLanguage::resolve(&json!({"macro_limits":{"max_actions":2,"max_symbols":2,"max_depth":2,"timing":{"max_duration_ms":3}},"mappings":{"remote":{"command":"A"},"long":{"action":"connect_permanent","command":"AA","protected":true}}}),&json!({})).unwrap());
    let mut ctx = InvocationContext {
        owner: ResourceScope::Port("11111111-1111-4111-8111-111111111111".parse().unwrap()),
        generation: Arc::new(host.control().work().unwrap()),
        source: CommandSource::Receiver,
        requires_grant: false,
    };
    let mut without_port = ctx.clone();
    without_port.owner =
        ResourceScope::Site("11111111-1111-4111-8111-111111111111".parse().unwrap());
    let connect = prepare_macro_action(&json!({"action":"connect","parameter":"1"})).unwrap();
    assert!(connect.admit(&without_port, true).is_err());
    assert!(
        lang.match_completed("*31")
            .unwrap()
            .admit(&without_port, true, true, false)
            .is_err()
    );
    let query =
        prepare_runtime_action(&json!({"action":"radio_host_query","operation":"receive_policy"}))
            .unwrap();
    without_port.source = CommandSource::Host;
    assert!(matches!(
        query.admit(&without_port, false),
        Err(ActionError::SourceOnly)
    ));
    let mut port_host = ctx.clone();
    port_host.source = CommandSource::Host;
    assert!(query.admit(&port_host, false).is_ok());
    drop(port_host);
    drop(without_port);
    let matched = lang.match_completed("AA0").unwrap();
    assert!(matched.admit(&ctx, false, true, false).is_err());
    let admitted = matched.admit(&ctx, true, true, false).unwrap();
    assert!(admitted.context.requires_grant);
    assert_eq!(admitted.origin, ParameterOrigin::Dtmf);
    assert_eq!(admitted.parameter.as_deref().map(|v| v.as_str()), Some("0"));
    assert_eq!(
        prepare_macro_action(&json!({"action":"connect","parameter":"0"}))
            .unwrap()
            .admit(&ctx, false)
            .unwrap()
            .origin,
        ParameterOrigin::Typed
    );
    let mut budget = MacroBudget::start(lang.clone(), admitted.context.clone(), 10);
    assert!(budget.resume(true, 13).is_ok());
    assert_eq!(budget.resume(false, 13), Err(MacroOutcome::Cancelled));
    assert_eq!(budget.resume(true, 13), Err(MacroOutcome::Cancelled));
    drop(budget);
    for fail in 0..4 {
        let mut budget = MacroBudget::start(lang.clone(), admitted.context.clone(), 10);
        budget.enter_child(false).unwrap();
        budget.charge(&Action::ExecuteDtmf("12".into())).unwrap();
        let result = match fail {
            0 => budget.enter_child(false),
            1 => budget.charge(&Action::ExecuteDtmf("3".into())),
            2 => budget.resume(true, 14),
            _ => {
                budget.charge(&Action::Pause(None)).unwrap();
                budget.charge(&Action::Pause(None))
            }
        };
        assert_eq!(result, Err(MacroOutcome::Limit));
    }
    let mut budget = MacroBudget::start(lang.clone(), admitted.context.clone(), 10);
    budget.enter_child(false).unwrap();
    assert_eq!(budget.finish_frame(), None);
    assert_eq!(budget.finish_frame(), Some(MacroOutcome::Completed));
    ctx.source = CommandSource::Scheduler;
    assert!(matched.admit(&ctx, true, true, true).is_err());
    let mute = lang.match_completed("*99").unwrap();
    assert!(mute.admit(&ctx, false, true, true).is_err());
    let phone = PeerSignals::for_phone(
        PhoneSource::new(
            PhoneAccountId::new("account").unwrap(),
            1,
            PhoneCallId::new([3; 16]).unwrap(),
            2,
            3,
        )
        .unwrap(),
    );
    ctx.source = CommandSource::Connection {
        reply: phone.reply_target(),
        origin: PhoneCallId::new([3; 16]).unwrap().to_string(),
    };
    assert!(mute.admit(&ctx, false, true, false).is_err());
    assert!(phone.authenticate_phone());
    assert!(matches!(
        mute.admit(&ctx, false, true, false).unwrap().action,
        Action::Mute(MuteTarget::PhonePatch(None))
    ));
    phone.end();
    assert!(mute.admit(&ctx, false, true, false).is_err());
    host.control().stop(15).unwrap();
    assert!(admitted.context.recheck(true).is_err());
    drop(budget);
    drop(admitted);
    drop(ctx);
    host.control().mark_detached(4).unwrap();
    assert!(host.reclaim());
}
#[test]
fn registry_macro_override_uses_the_actual_invoking_port_and_rejects_absent_equipment() {
    use crate::config::resources::{PortKey, candidate::*};
    let id = |n: u8| {
        format!("00000000-0000-4000-8000-{n:012}")
            .parse::<ResourceId>()
            .unwrap()
    };
    let site = ResourceScope::Site(id(1));
    let port = ResourceScope::Port(id(2));
    let other = ResourceScope::Port(id(3));
    let mut owned = vec![];
    let mut members = vec![];
    let mut ordinary = vec![];
    for (i, scope, kind, key, body) in [
        (
            1,
            site,
            ResourceKind::Metadata,
            None,
            json!({"name":"test","inherit_from":null}),
        ),
        (
            2,
            port,
            ResourceKind::Metadata,
            None,
            json!({"name":"port","inherit_from":"site"}),
        ),
        (
            3,
            other,
            ResourceKind::Metadata,
            None,
            json!({"name":"other","inherit_from":"site"}),
        ),
        (
            4,
            site,
            ResourceKind::Macro,
            Some("a/b"),
            json!({"actions":[{"action":"announce_time"}]}),
        ),
        (
            5,
            port,
            ResourceKind::Macro,
            Some("a/b"),
            json!({"actions":[{"action":"set_output","name":"fan","state":"on"}]}),
        ),
        (6, port, ResourceKind::Output, Some("fan"), json!({})),
        (
            8,
            site,
            ResourceKind::Sensor,
            Some("meter"),
            json!({"dtmf_id":"1"}),
        ),
        (
            9,
            port,
            ResourceKind::Sensor,
            Some("meter"),
            json!({"dtmf_id":""}),
        ),
        (
            10,
            other,
            ResourceKind::Input,
            Some("switch"),
            json!({"dtmf_id":"1"}),
        ),
        (
            11,
            site,
            ResourceKind::Macro,
            Some("report"),
            json!({"actions":[{"action":"site_status","selector":"1"}]}),
        ),
        (
            7,
            site,
            ResourceKind::Command,
            None,
            json!({"mappings":{"status":{"action":"call_macro","macro":"a/b"}},"macros":{"bogus":{"actions":[{"action":"link_status"}]}}}),
        ),
    ] {
        let binding = ResourceBinding {
            id: id(i),
            scope,
            kind,
        };
        let mut v = json!({"id":id(i).to_string(),"kind":kind.as_str(),"configuration":body});
        if let Some(key) = key {
            v["key"] = json!(key);
            members.push((binding, PortKey::new(key)));
        } else if kind != ResourceKind::Metadata {
            ordinary.push(binding);
        }
        owned.push((binding, serde_json::to_vec(&v).unwrap()));
    }
    let registry = ResourceRegistry::with_members(
        id(1),
        &[(id(2), PortKey::new("p")), (id(3), PortKey::new("q"))],
        &ordinary,
        &members,
    )
    .unwrap();
    let mut captures: Vec<_> = owned
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let c = ModularCandidate::assemble(&registry, &captures).unwrap();
    assert!(matches!(
        c.command_language(other),
        Err(LanguageError::MissingResource)
    ));
    captures.retain(|c| c.binding.kind != ResourceKind::Input);
    {
        let c = ModularCandidate::assemble(&registry, &captures).unwrap();
        let p = c.command_language(port).unwrap();
        assert!(
            p.language
                .warnings
                .contains(&LanguageWarning::Invalid("dtmf_id"))
        );
        let q = c.command_language(other).unwrap();
        assert_eq!(p.named_macro("a/b").unwrap().0.id, id(5));
        assert_eq!(q.named_macro("a/b").unwrap().0.id, id(4));
        assert!(matches!(
            p.named_macro("a/b").unwrap().1[0].action,
            Action::Site(_)
        ));
        assert!(matches!(
            q.named_macro("a/b").unwrap().1[0].action,
            Action::Link(_)
        ));
        assert!(p.named_macro("bogus").is_none());
        assert!(p.local(id(7)).unwrap().get("macros").is_none());
    }
    captures.retain(|c| c.binding.kind != ResourceKind::Output);
    let c = ModularCandidate::assemble(&registry, &captures).unwrap();
    assert!(matches!(
        c.command_language(port),
        Err(LanguageError::MissingResource)
    ));
    assert!(c.command_language(other).is_ok());
}

#[test]
fn all_fifty_one_radio_aliases_select_the_existing_payload_for_their_source_spelling() {
    let c = CommandLanguage::standard();
    let audit: Value =
        serde_json::from_str(include_str!("../../../../docs/dtmf-mapping-inventory.json")).unwrap();
    let radios = audit["radio_mappings"].as_array().unwrap();
    assert_eq!(radios.len(), 51);
    for r in radios {
        let prefix = r["command"].as_str().unwrap();
        let tail = if r["match_kind"] == "fixed" {
            ""
        } else {
            match r["operation"].as_str().unwrap() {
                "set_frequency" => "146*52000*3",
                "set_tone" => "100*0",
                "set_setting" => "17*1*4200*3",
                _ => "001",
            }
        };
        let text = format!("{prefix}{tail}");
        let m = c.match_completed(&text).unwrap();
        assert_eq!(m.mapping.command, prefix);
        assert_eq!(m.parameter, tail);
        let expected = parse_remote_command(text.strip_prefix("*6").unwrap()).unwrap();
        assert!(
            matches!(m.decode(),Ok(Action::Radio(RadioAction::Command(actual))) if actual==expected)
        );
        if tail.is_empty() {
            let extra = format!("{prefix}1");
            let m = c.match_completed(&extra).unwrap();
            assert!(m.decode().is_err());
        }
    }
    let fallback = c.match_completed("*688").unwrap();
    assert_eq!(fallback.mapping.command, "*6");
    assert_eq!(fallback.parameter, "88");
    assert!(fallback.decode().is_err());
    let schema: Value =
        serde_json::from_str(include_str!("../../../../repeater-management.schema.json")).unwrap();
    let builtins: Value = serde_json::from_str(include_str!("defaults.json")).unwrap();
    assert_eq!(
        schema["$defs"]["commandSettings"]["properties"]["mappings"]["default"],
        builtins
    );
}
#[test]
fn strict_native_units_bounds_and_variant_fields_survive_exact_json_decoding() {
    let parse = |s: &[u8]| prepare_runtime_action(&rptadv_json::decode_value(s).unwrap());
    assert!(matches!(
        parse(br#"{"action":"radio","operation":"step","step_mhz":-9.223372036854775808e12}"#)
            .unwrap()
            .action,
        Action::Radio(RadioAction::Command(RemoteCommand::Step(i64::MIN)))
    ));
    assert!(parse(br#"{"action":"radio","operation":"step","step_mhz":1e-7}"#).is_err());
    assert!(matches!(parse(br#"{"action":"radio","operation":"set_frequency","offset_direction":"plus","frequency_mhz":18446744073709.551615}"#).unwrap().action,Action::Radio(RadioAction::Command(RemoteCommand::Frequency{hertz:u64::MAX,..}))));
    for v in [
        json!({"action":"radio_maintenance","operation":"transmit","keyed":false,"ctcss_frequency_hz":100}),
        json!({"action":"radio_maintenance","operation":"set_mixer","level":1000,"target":"receive"}),
        json!({"action":"radio_maintenance","operation":"write_eeprom","words":vec![0;63]}),
        json!({"action":"interface_control","operation":"write_pin","pin":{"domain":"parallel","pin":1},"state":"on"}),
        json!({"action":"interface_control","operation":"write_pin","pin":{"domain":"parallel","pin":2},"state":"pulse","timing":{"duration_ms":150}}),
        json!({"action":"rx_mute_toggle","target":"port","link_type":"echolink"}),
        json!({"action":"site_status","selector":""}),
    ] {
        assert!(prepare_runtime_action(&v).is_err());
    }
    let hw = json!({"command":"A","protected":true,"action":"interface_control","operation":"write_pin","pin":{"domain":"parallel","pin":2},"state":"pulse","timing":{"duration_ms":200}});
    let c = CommandLanguage::resolve(&json!({"mappings":{"pulse":hw}}), &json!({})).unwrap();
    assert!(matches!(
        c.match_completed("A").unwrap().decode(),
        Ok(Action::Interface(InterfaceAction::Pulse(
            Pin::Parallel(2),
            2
        )))
    ));
}

#[test]
fn unknown_nested_fields_and_invalid_radio_overrides_keep_independent_state_leaves() {
    let site = json!({"mappings":{"rig":{"action":"radio","operation":"set","command":"A","protected":true,"state":{"mode":"FM","rx_frequency_mhz":146.52,"encode":{"enabled":true,"ctcss":{"frequency_hz":100.0}}}}}});
    let port = json!({"mappings":{"rig":{"state":{"mode":"future","offset_mhz":-0.6,"encode":{"unrecognized":"omitted","enabled":false,"ctcss":{"frequency_hz":"bad","mystery":8}}}}},"macros":{"m":{"actions":[{"action":"pause","timing":{"extra":"ignored","duration_ms":200}}]}}});
    let c = CommandLanguage::resolve(&site, &port).unwrap();
    let Action::Radio(RadioAction::Set(state)) = c.match_completed("A").unwrap().decode().unwrap()
    else {
        panic!("expected prepared operating state")
    };
    assert_eq!(state.mode, Some("FM"));
    assert_eq!(state.offset_hz, Some(-600000));
    assert_eq!(state.rx_hz, Some(146520000));
    let signal = state.encode.as_ref().unwrap();
    assert_eq!(signal.enabled, Some(false));
    assert_eq!(signal.ctcss, Some(1000));
    assert_eq!(
        c.local[1]["mappings"]["rig"]["state"],
        json!({"offset_mhz":-0.6,"encode":{"enabled":false,"ctcss":{}}})
    );
    assert_eq!(
        c.local[1]["macros"]["m"]["actions"][0]["timing"],
        json!({"duration_ms":200})
    );
}

#[test]
fn fixed_injection_bindings_obey_the_effective_ingress_policy() {
    let site = json!({"mappings":{"inject":{"action":"execute_dtmf","digits":"A#","command":"B","protected":true}}});
    let port = json!({"mappings":{"inject_source":{"action":"start_pad_test","command":"A","protected":false}}});
    assert!(matches!(
        CommandLanguage::resolve(&site, &port),
        Err(LanguageError::SourcePolicy)
    ));
    assert!(CommandLanguage::resolve(&site, &json!({})).is_ok());
    // An accepted site literal remains public even if the port replaces its digits.
    let protected = json!({"mappings":{"inject_source":{"action":"unlock_administration","command":"A","protected":false},"inject":{"digits":"C#"}}});
    assert!(matches!(
        CommandLanguage::resolve(&site, &protected),
        Err(LanguageError::SourcePolicy)
    ));
}
#[test]
fn invalid_radio_operation_keeps_the_inherited_binding() {
    let c = CommandLanguage::resolve(
        &json!({}),
        &json!({"mappings":{"radio_status_51":{"operation":42,"protected":true}}}),
    )
    .unwrap();
    let m = c.match_completed("*651").unwrap();
    assert!(matches!(
        m.decode(),
        Ok(Action::Radio(RadioAction::Command(RemoteCommand::Status {
            long: true
        })))
    ));
    assert!(m.mapping.protected);
    assert_eq!(
        c.local[1]["mappings"]["radio_status_51"],
        json!({"protected":true})
    );
    assert!(c.warnings.contains(&LanguageWarning::Invalid("operation")));
}
#[test]
fn injected_completed_calls_use_the_effective_static_call_graph() {
    let site = json!({"mappings":{"call":{"action":"call_macro","macro":"x","command":"A","protected":true}}});
    let port = |digits: &str| json!({"maximum_digits":2,"macros":{"x":{"actions":[{"action":"execute_dtmf","digits":digits}]}}});
    for digits in ["A#", "A", "00#A"] {
        assert!(matches!(
            CommandLanguage::resolve(&site, &port(digits)),
            Err(LanguageError::Cycle)
        ));
    }
    for digits in ["000#", "A1#", "B#"] {
        assert!(CommandLanguage::resolve(&site, &port(digits)).is_ok());
    }
    let mut aliases = site.clone();
    aliases["mappings"]["call"]["command"] = json!("AAA");
    assert!(CommandLanguage::resolve(&aliases, &port("AAA#")).is_ok());
    aliases["mappings"]["indirect"] =
        json!({"action":"execute_dtmf","command":"B","protected":false,"digits":"A#"});
    aliases["mappings"]["call"]["command"] = json!("A");
    assert!(matches!(
        CommandLanguage::resolve(&aliases, &port("B#")),
        Err(LanguageError::Cycle)
    ));
}
#[test]
fn mixer_target_falls_back_without_losing_level() {
    let site = json!({"mappings":{"mix":{"action":"radio_maintenance","operation":"set_mixer","command":"A","protected":true,"target":"receive","level":100}}});
    let c = CommandLanguage::resolve(
        &site,
        &json!({"mappings":{"mix":{"target":"future","level":125}}}),
    )
    .unwrap();
    assert!(matches!(
        c.match_completed("A").unwrap().decode(),
        Ok(Action::Maintenance(MaintenanceAction::SetMixer(
            MixerTarget::Receive,
            125
        )))
    ));
    assert_eq!(c.local[1]["mappings"]["mix"], json!({"level":125}));
    assert!(c.warnings.contains(&LanguageWarning::Invalid("target")));
}
#[test]
fn invalid_optional_macro_mode_preserves_replacement_order() {
    let site = json!({"macros":{"x":{"actions":[{"action":"cancel_message"}]}}});
    let c = CommandLanguage::resolve(&site, &json!({"macros":{"x":{"actions":[{"action":"connect","parameter":"1234","mode":"future"},{"action":"announce_time"}]}}})).unwrap();
    let actions = &c.macros["x"];
    assert_eq!(actions.len(), 2);
    assert!(
        matches!(&actions[0].action, Action::Link(c) if c.action==LinkAction::Transceive && c.node=="1234")
    );
    assert!(matches!(&actions[1].action, Action::Link(c) if c.action==LinkAction::Time));
    assert_eq!(
        c.local[1]["macros"]["x"]["actions"][0],
        json!({"action":"connect","parameter":"1234"})
    );
    assert!(c.warnings.contains(&LanguageWarning::Invalid("mode")));
}
#[test]
fn fixed_setting_value_constrains_raw_decimal_input() {
    let c = CommandLanguage::resolve(&json!({"mappings":{"rig":{"action":"radio","operation":"set_setting","value":12,"command":"A","protected":true}}}), &json!({})).unwrap();
    for (tail, magnitude, places) in [("7*0*12*0", 12, 0), ("7*0*120*1", 120, 1)] {
        let digits = format!("A{tail}");
        assert!(matches!(
            c.match_completed(&digits).unwrap().decode(),
            Ok(Action::Radio(RadioAction::ConstrainedSetting {
                selector: 7, raw, value: Some(CapabilityValue::Number(n)), units: None
            })) if n.as_u64() == Some(12) && !raw.negative && raw.magnitude == magnitude && raw.decimal_places == places
        ));
    }
    for digits in ["A7*0*13*0", "A7*1*12*0"] {
        assert!(matches!(
            c.match_completed(digits).unwrap().decode(),
            Err(ActionError::Parameter)
        ));
    }
    let c = CommandLanguage::resolve(&json!({"mappings":{"rig":{"action":"radio","operation":"set_setting","value":"auto","units":"declared","command":"A","protected":true}}}), &json!({})).unwrap();
    assert!(
        matches!(c.match_completed("A7*1*0*0").unwrap().decode(), Ok(Action::Radio(RadioAction::ConstrainedSetting {selector:7,raw,value:Some(CapabilityValue::Text(name)),units:Some(units)})) if raw.negative && raw.magnitude==0 && name=="auto" && units=="declared")
    );
}

#[test]
fn fixed_numeric_setting_preserves_its_authored_type_for_provider_validation() {
    let c = CommandLanguage::resolve(
        &json!({"mappings":{"typed":{"action":"radio","operation":"set_setting","value":1,"command":"A","protected":true}}}),
        &json!({}),
    ).unwrap();
    // The provider declares selector 16 (encode_enabled) boolean. Its raw keypad
    // representation alone cannot distinguish an authored JSON number from a boolean.
    let raw = crate::remote_base_command::DecimalValue {
        negative: false,
        magnitude: 1,
        decimal_places: 0,
    };
    assert!(matches!(
        c.match_completed("*69116*0*1*0").unwrap().decode(),
        Ok(Action::Radio(RadioAction::Command(RemoteCommand::SetSetting { selector:16, value }))) if value == raw
    ));
    assert!(matches!(
        c.match_completed("A16*0*1*0").unwrap().decode(),
        Ok(Action::Radio(RadioAction::ConstrainedSetting {selector:16, raw: tuple, value:Some(CapabilityValue::Number(n)), units:None}))
        if tuple == raw && n.as_u64() == Some(1)
    ));
}

#[test]
fn authenticated_phone_grammar_reports_its_unavailable_owner() {
    use crate::{
        config::resources::candidate::ResourceScope,
        link::{PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource},
        runtime::{GenerationSettings, NodeHost, RuntimeGeneration},
    };
    use std::sync::Arc;
    let mut host = NodeHost::new(
        RuntimeGeneration::prepare(
            4,
            GenerationSettings {
                node: "1234".into(),
                device: "test".into(),
                receive_maximum: 480,
                transmit_maximum: 480,
                squelch_delay_ms: 0,
                status_snapshot_interval_ms: 50,
            },
            (),
            (),
        )
        .unwrap(),
    );

    let mut ctx = InvocationContext {
        owner: ResourceScope::Port("11111111-1111-4111-8111-111111111111".parse().unwrap()),
        generation: Arc::new(host.control().work().unwrap()),
        source: CommandSource::Scheduler,
        requires_grant: false,
    };
    let a = prepare_runtime_action(&json!({"action":"phone_manual_key"})).unwrap();
    assert!(matches!(a.admit(&ctx, false), Err(ActionError::SourceOnly)));
    let phone = PeerSignals::for_phone(
        PhoneSource::new(
            PhoneAccountId::new("account").unwrap(),
            1,
            PhoneCallId::new([3; 16]).unwrap(),
            2,
            3,
        )
        .unwrap(),
    );
    ctx.source = CommandSource::Connection {
        reply: phone.reply_target(),
        origin: PhoneCallId::new([3; 16]).unwrap().to_string(),
    };

    assert!(matches!(a.admit(&ctx, false), Err(ActionError::SourceOnly)));
    assert!(phone.authenticate_phone());
    assert!(matches!(
        a.admit(&ctx, false),
        Err(ActionError::Unavailable)
    ));
    phone.end();
    assert!(matches!(a.admit(&ctx, false), Err(ActionError::SourceOnly)));
}
