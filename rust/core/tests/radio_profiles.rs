//! Behavior of radio settings assembled from identified modular resources.
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{
        CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind as Kind,
        ResourceRegistry, ResourceScope,
    },
};
use serde_json::{Value, json};

fn id(n: u32) -> ResourceId {
    format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
}

fn with_port(rows: &[(bool, Kind, Option<Value>)], verify: impl FnOnce(ModularCandidate<'_>)) {
    with_scopes(true, rows, verify);
}
fn with_scopes(
    registered: bool,
    rows: &[(bool, Kind, Option<Value>)],
    verify: impl FnOnce(ModularCandidate<'_>),
) {
    let port = id(2);
    let site = id(1);
    let bindings: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(idx, (local, kind, _))| ResourceBinding {
            id: id(3 + idx as u32),
            scope: if *local {
                ResourceScope::Port(port)
            } else {
                ResourceScope::Site(site)
            },
            kind: *kind,
        })
        .collect();
    let ports = registered.then(|| (port, PortKey::new("Écho ; #")));
    let registry = ResourceRegistry::new(site, ports.as_slice(), &bindings).unwrap();
    let bodies: Vec<_> = registry
        .bindings()
        .filter_map(|b| {
            let mut body = json!({"id":b.id.to_string(),"kind":b.kind.as_str()});
            body["configuration"] = if b.id == site {
                json!({"name":"site","inherit_from":null})
            } else if b.id == port {
                body["key"] = json!("Écho ; #");
                json!({"name":"port","inherit_from":"site"})
            } else {
                rows[(bindings.iter().position(|binding| binding.id == b.id))?]
                    .2
                    .clone()?
            };
            Some((b, serde_json::to_vec(&body).unwrap()))
        })
        .collect();
    let captures = bodies
        .iter()
        .map(|(binding, bytes)| CapturedResource {
            binding: *binding,
            json: bytes,
        })
        .collect::<Vec<_>>();
    verify(ModularCandidate::assemble(&registry, &captures).unwrap())
}

#[test]
fn memories_are_one_based_and_decimal_exact_while_absent_empty_and_invalid_overrides_differ() {
    let one = json!({"frequency":146.940001,"offset":{"enabled":true,"direction":"minus","frequency":0.600001},
        "rx_ctcss":{"enabled":false,"frequency":100.0},"tx_ctcss":{"enabled":true,"frequency":114.8},"mode":"FM"});
    let invalid = json!({"entries":[{"frequency":1}]});
    for (local, count, warned) in [
        (None, 1, false),
        (Some(json!({})), 1, false),
        (Some(json!({"entries":[]})), 0, false),
        (Some(invalid), 1, true),
    ] {
        with_port(
            &[
                (
                    false,
                    Kind::Memories,
                    Some(json!({"entries":[one.clone()]})),
                ),
                (true, Kind::Memories, local),
            ],
            |candidate| {
                let profiles = candidate.resolve_radio_profiles().unwrap();
                let memories = &profiles.port(id(2)).unwrap().memories;
                assert_eq!(memories.len(), count);
                if let Some(m) = memories.first() {
                    assert_eq!(
                        (m.selector, m.receive_hz, m.transmit_hz),
                        (1, 146_940_001, 146_340_000)
                    );
                    assert!(!m.rx_ctcss.enabled);
                    assert_eq!((m.tx_ctcss.enabled, m.tx_ctcss.tenths_hz), (true, 1148));
                }
                assert_eq!(
                    profiles.warnings().iter().any(|w| w.resource.id == id(4)),
                    warned
                );
            },
        )
    }
}

#[test]
fn named_scans_inherit_members_and_keep_priority_and_dwell_with_their_owner() {
    let memory = json!({"frequency":146.52,"offset":{"enabled":false,"direction":"plus","frequency":0},
        "rx_ctcss":{"enabled":false,"frequency":100},"tx_ctcss":{"enabled":false,"frequency":100},"mode":"FM"});
    with_port(
        &[
            (
                false,
                Kind::Memories,
                Some(json!({"entries":[memory.clone(),memory]})),
            ),
            (
                false,
                Kind::ScanLists,
                Some(
                    json!({"local":{"selector":7,"members":[2,1],"priority":2,"timing":{"dwell_ms":610}}}),
                ),
            ),
            (
                true,
                Kind::ScanLists,
                Some(json!({"local":{"timing":{"settle_ms":19}}})),
            ),
        ],
        |candidate| {
            let profiles = candidate.resolve_radio_profiles().unwrap();
            let scans = &profiles.port(id(2)).unwrap().scan_lists;
            let scan = &scans["local"];
            assert_eq!(scan.members, [2, 1]);
            assert_eq!(
                (
                    scan.selector,
                    scan.priority,
                    scan.dwell_ms,
                    scan.settle_ms,
                    scan.resume_delay_ms
                ),
                (7, Some(2), 610, 19, 2000)
            );
            assert!(profiles.warnings().is_empty());
        },
    );
}

#[test]
fn changing_memory_order_revalidates_inherited_scan_references() {
    let memory = json!({"frequency":146.52,"offset":{"enabled":false,"direction":"minus","frequency":0.6},
        "rx_ctcss":{"enabled":false,"frequency":100},"tx_ctcss":{"enabled":false,"frequency":100},"mode":"FM"});
    with_port(
        &[
            (
                false,
                Kind::Memories,
                Some(json!({"entries":[memory.clone(),memory.clone()]})),
            ),
            (
                false,
                Kind::ScanLists,
                Some(json!({"net":{"selector":1,"members":[2,1]}})),
            ),
            (true, Kind::Memories, Some(json!({"entries":[memory]}))),
        ],
        |candidate| {
            let failure = candidate.resolve_radio_profiles().unwrap_err();
            assert_eq!((failure.resource.id, failure.field), (id(4), "members"));
        },
    );
}

#[test]
fn peer_audio_is_independent_and_distinguishes_neutral_shipped_and_disabled_roles() {
    for (local, present, enabled) in [
        (None, false, true),
        (Some(json!({})), true, true),
        (Some(json!({"enabled":false})), true, false),
    ] {
        with_port(&[(true, Kind::PeerAudio, local)], |candidate| {
            let profiles = candidate.resolve_radio_profiles().unwrap();
            let port = profiles.port(id(2)).unwrap();
            assert_eq!(port.processing.link.is_some(), present);
            if let Some(role) = &port.processing.link {
                assert_eq!(role.enabled, enabled)
            }
            assert!(port.processing.local.is_none() && port.processing.voice_telemetry.is_none());
        })
    }
}

#[test]
fn stage_fields_inherit_individually_and_provider_projection_preserves_roles_and_three_bands() {
    with_port(
        &[
            (
                false,
                Kind::PeerAudio,
                Some(
                    json!({"enabled":true,"input_gain_db":2,"equalizer":{"mid":{"gain_db":3}},"compressor":{"low":{"ratio":3.5}}}),
                ),
            ),
            (
                true,
                Kind::PeerAudio,
                Some(
                    json!({"input_gain_db":999,"equalizer":{"high":{"gain_db":-2}},"compressor":{"enabled":true},
            "stage_order":["equalizer","compressor"],"processing":{"legacy":1}}),
                ),
            ),
            (true, Kind::RxAudio, Some(json!({"processing":{}}))),
            (
                true,
                Kind::TxAudio,
                Some(json!({"processing":{"enabled":false,"bandpass":{"enabled":true}}})),
            ),
        ],
        |candidate| {
            let profiles = candidate.resolve_radio_profiles().unwrap();
            let roles = &profiles.port(id(2)).unwrap().processing;
            let link = roles.link.as_ref().unwrap();
            let dto = serde_json::to_value(link).unwrap();
            assert_eq!(dto["input_gain_db"], 2.0);
            assert_eq!(dto["equalizer"]["mid_gain_db"], 3.0);
            assert_eq!(dto["equalizer"]["high_gain_db"], -2.0);
            assert_eq!(dto["compressor"]["layout"], "three_band");
            assert_eq!(dto["compressor"]["low"]["ratio"], 3.5);
            assert_eq!(dto["stage_order"], json!(["equalizer", "compressor"]));
            assert!(roles.local.as_ref().unwrap().enabled);
            assert!(!roles.voice_telemetry.as_ref().unwrap().enabled);
            let local = &profiles.local(id(4)).unwrap().configuration;
            assert!(!local.contains_key("processing") && !local.contains_key("input_gain_db"));
            assert_eq!(profiles.warnings().len(), 2);
        },
    )
}

#[test]
fn exact_device_spans_mixer_intention_and_audio_enables_survive_resource_inheritance() {
    with_port(
        &[
            (
                false,
                Kind::Interface,
                Some(
                    json!({"audio":{"device_identifier":"  #device ;  ","usb_serial":" s#1; ","input_gain_db":0.0,"output_device_channels":2},"parallel":{"outputs":{"2":"ptt"},"inputs":{"10":"cor"}}}),
                ),
            ),
            (
                true,
                Kind::Interface,
                Some(
                    json!({"audio":{"timing":{"input_extra_buffer_ms":501,"output_extra_buffer_ms":12}}}),
                ),
            ),
            (
                true,
                Kind::Radio,
                Some(json!({"duplex_mode":"full","full_duplex":true})),
            ),
            (
                true,
                Kind::RxAudio,
                Some(json!({"enabled":false,"deemphasis":{"enabled":true,"corner_hz":250}})),
            ),
            (
                true,
                Kind::TxAudio,
                Some(json!({"enabled":true,"preemphasis":{"enabled":true,"corner_hz":290}})),
            ),
        ],
        |candidate| {
            let p = candidate.resolve_radio_profiles().unwrap();
            let radio = p.port(id(2)).unwrap();
            assert_eq!(radio.interface.audio.device_identifier, "  #device ;  ");
            assert_eq!(radio.interface.audio.usb_serial, " s#1; ");
            assert_eq!(radio.interface.audio.input_gain_db, Some(0.0));
            assert_eq!(radio.interface.audio.output_a_gain_db, None);
            assert_eq!(radio.interface.audio.input_extra_buffer_ms, 0);
            assert_eq!(radio.interface.audio.output_extra_buffer_ms, 12);
            assert!(!radio.rx_audio.enabled && radio.tx_audio.enabled);
            assert_eq!(radio.rx_audio.deemphasis.corner_hz, 250.0);
            assert_eq!(radio.tx_audio.preemphasis.corner_hz, 290.0);
            assert!(radio.general.full_duplex());
            assert_eq!(p.warnings().len(), 2);
        },
    );
}

#[test]
fn unavailable_serial_acquisition_falls_back_to_domain_correct_inherited_squelch() {
    with_port(
        &[
            (
                false,
                Kind::RxSignaling,
                Some(
                    json!({"squelch":{"source":"parallel","pin":[10,12],"inverted":true,"level":600},
             "vad":{"enabled":true,"mode":"continuous"},"timing":{"on_delay_ms":21}}),
                ),
            ),
            (
                true,
                Kind::RxSignaling,
                Some(json!({"squelch":{"source":"serial","pin":["CTS"],"level":700}})),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ptt":{"source":"key_down","scancode":33,"timing":{"settle_ms":77}}})),
            ),
        ],
        |candidate| {
            let p = candidate.resolve_radio_profiles().unwrap();
            let radio = p.port(id(2)).unwrap();
            assert_eq!(radio.signals.rx.squelch.level, 700);
            assert!(radio.signals.rx.squelch.binding.inverted);
            assert_eq!(radio.signals.rx.squelch.binding.pins, [10, 12]);
            assert_eq!(radio.signals.rx.on_delay_blocks, 2);
            assert_eq!(radio.signals.tx.settle_ms, 77);
            assert!(p.warnings().iter().any(|w| w.resource.id == id(4)));
            assert!(p.warnings().iter().any(|w| w.resource.id == id(5)));
        },
    );
}

#[test]
fn enabled_ports_cannot_claim_the_same_exact_sound_card() {
    assert_shared_interface_conflict(
        json!({"audio":{"device_identifier":"shared ; serial"}}),
        "device_identifier",
    );
}

#[test]
fn enabled_ports_cannot_claim_the_same_active_parallel_endpoint() {
    assert_shared_interface_conflict(
        json!({"parallel":{"device":"shared ; serial","inputs":{"10":"in"}}}),
        "parallel_device",
    );
}

fn assert_shared_interface_conflict(settings: serde_json::Value, field: &str) {
    let site = id(1);
    let ports = [(id(2), PortKey::new("a")), (id(3), PortKey::new("b"))];
    let interface = ResourceBinding {
        id: id(4),
        scope: ResourceScope::Site(site),
        kind: Kind::Interface,
    };
    let registry = ResourceRegistry::new(site, &ports, &[interface]).unwrap();
    let bodies: Vec<_> = registry
        .bindings()
        .map(|b| {
            let mut v = json!({"id":b.id.to_string(),"kind":b.kind.as_str()});
            v["configuration"] = match (b.scope, b.kind) {
                (_, Kind::Interface) => settings.clone(),
                (ResourceScope::Site(_), _) => json!({"inherit_from":null,"name":"site"}),
                (_, _) => {
                    let key = ports.iter().find(|(id, _)| *id == b.id).unwrap().1.as_str();
                    v["key"] = json!(key);
                    json!({"inherit_from":"site","name":key})
                }
            };
            (b, serde_json::to_vec(&v).unwrap())
        })
        .collect();
    let captures = bodies
        .iter()
        .map(|(b, json)| CapturedResource { binding: *b, json })
        .collect::<Vec<_>>();
    let c = ModularCandidate::assemble(&registry, &captures).unwrap();
    let err = c.resolve_radio_profiles().unwrap_err();
    assert_eq!(err.resource.id, id(3));
    assert_eq!(err.conflicting.unwrap().id, id(2));
    assert_eq!(err.field, field);
    assert!(!err.to_string().contains("shared ; serial"));
}

#[test]
fn hamlib_controls_keep_data_lossless_and_limits_use_integral_hz_and_milliwatts() {
    with_port(
        &[
            (
                false,
                Kind::Control,
                Some(
                    json!({"enabled":true,"backend":"hamlib","hamlib":{"model":"RIG_MODEL_DUMMY","device":" /a;b#c ","options":{"custom":" \"raw\" "}}}),
                ),
            ),
            (
                true,
                Kind::Control,
                Some(json!({"hamlib":{"options":{"retry":"9","next":"a;b#c"}}})),
            ),
            (
                true,
                Kind::Limits,
                Some(
                    json!({"modes":["FM"],"rx_ranges_mhz":[[144.000001,148.0]],"tx_ranges_mhz":[]}),
                ),
            ),
            (
                true,
                Kind::Power,
                Some(json!({"max_w":5.101,"low_w":0.001,"high_w":5.101})),
            ),
            (true, Kind::Radio, Some(json!({"startup_memory":1}))),
            (
                true,
                Kind::Memories,
                Some(
                    json!({"entries":[{"frequency":146.94,"offset":{"enabled":false,"direction":"minus","frequency":0.6},"mode":"FM","rx_ctcss":{"enabled":false,"frequency":100},"tx_ctcss":{"enabled":false,"frequency":100}}]}),
                ),
            ),
        ],
        |c| {
            let profiles = c.resolve_radio_profiles().unwrap();
            let r = &profiles.port(id(2)).unwrap().remote;
            assert!(r.enabled);
            assert_eq!(r.receive_ranges, [[144000001, 148000000]]);
            assert!(r.transmit_ranges.is_empty());
            assert_eq!(r.maximum_power_mw, Some(5101));
            assert_eq!(r.hamlib_options.get("rig_pathname").unwrap(), " /a;b#c ");
            assert_eq!(r.hamlib_options.get("custom").unwrap(), " \"raw\" ");
            assert!(!r.hamlib_options.contains_key("retry"));
            assert!(profiles.warnings().iter().any(|w| w.resource.id == id(4)));
        },
    );
}

#[test]
fn remote_projection_applies_offset_once_and_carries_safe_unestablished_power() {
    with_port(
        &[
            (
                true,
                Kind::Control,
                Some(
                    json!({"enabled":true,"backend":"hamlib","hamlib":{"model":"RIG_MODEL_DUMMY"}}),
                ),
            ),
            (true, Kind::Radio, Some(json!({"startup_memory":1}))),
            (
                true,
                Kind::Limits,
                Some(
                    json!({"modes":["FM"],"rx_ranges_mhz":[[144,148]],"tx_ranges_mhz":[[144,148]]}),
                ),
            ),
            (true, Kind::Power, Some(json!({"max_w":15,"low_w":2}))),
            (
                true,
                Kind::Memories,
                Some(
                    json!({"entries":[{"frequency":146.940001,"mode":"FM","offset":{"enabled":true,"direction":"minus","frequency":0.600001},"tx_ctcss":{"enabled":true,"frequency":114.8},"rx_ctcss":{"enabled":false,"frequency":100}}]}),
                ),
            ),
        ],
        |c| {
            let profiles = c.resolve_radio_profiles().unwrap();
            let port = profiles.port(id(2)).unwrap();
            let dto = serde_json::to_value(port.remote_settings("376").unwrap()).unwrap();
            let state = &dto["memories"][0]["state"];
            assert_eq!(state["transmit_hz"], 146940001);
            assert_eq!(state["offset_hz"], -600001);
            assert_eq!(port.memories[0].transmit_hz, 146340000);
            assert_eq!(dto["memories"][0]["name"], "1");
            assert_eq!(state["encode"]["tone_tenths_hz"], 1148);
            assert!(state["encode"].get("control_source").is_none());
            assert_eq!(state["decode"]["enabled"], false);
            assert!(state["power_mw"].is_null());
            assert_eq!(dto["maximum_power_mw"], 15000);
            assert_eq!(dto["dtmf_prefix"], "376");
            assert!(dto["offset_magnitude_hz"].is_null());
        },
    );
}

#[test]
fn invalid_startup_selection_falls_back_to_valid_ancestor_and_fractional_rx_output_gain_is_rejected()
 {
    with_port(
        &[
            (false, Kind::Radio, Some(json!({"startup_memory":1}))),
            (true, Kind::Radio, Some(json!({"startup_memory":2}))),
            (false, Kind::RxAudio, Some(json!({"output_gain_db":-3}))),
            (true, Kind::RxAudio, Some(json!({"output_gain_db":-0.5}))),
            (
                true,
                Kind::Memories,
                Some(
                    json!({"entries":[{"frequency":146.94,"mode":"FM","offset":{"enabled":false,"direction":"minus","frequency":0.6},"tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}}]}),
                ),
            ),
        ],
        |c| {
            let profiles = c.resolve_radio_profiles().unwrap();
            let p = profiles.port(id(2)).unwrap();
            assert_eq!(p.general.startup_memory, Some(1));
            assert_eq!(p.rx_audio.output_gain_db, -3.0);
            assert!(p.memories[0].offset_minus);
            assert_eq!(p.memories[0].offset_magnitude_hz, 600000);
            assert_eq!(p.memories[0].offset_hz, 0);
        },
    );
}

#[test]
fn selected_parallel_signal_pins_build_the_exact_physical_assignments() {
    use rpt_advanced_core::config::resources::profiles::radio::{ParallelInput, ParallelOutput};
    with_port(
        &[
            (
                true,
                Kind::RxSignaling,
                Some(
                    json!({"squelch":{"source":"parallel","pin":[10,15]},"ctcss":{"enabled":true,"source":"parallel","pin":[12]}}),
                ),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ptt":{"source":"parallel","pin":[3,7]}})),
            ),
        ],
        |c| {
            let profiles = c.resolve_radio_profiles().unwrap();
            let p = profiles.port(id(2)).unwrap();
            assert_eq!(
                p.interface.parallel.inputs,
                [
                    ParallelInput::Carrier,
                    ParallelInput::Ctcss,
                    ParallelInput::Unassigned,
                    ParallelInput::Carrier
                ]
            );
            assert_eq!(p.interface.parallel.outputs[1], ParallelOutput::PushToTalk);
            assert_eq!(p.interface.parallel.outputs[5], ParallelOutput::PushToTalk);
        },
    );
}
#[test]
fn changing_to_dsp_discards_obsolete_pin_and_scancode_but_preserves_independent_settings() {
    with_port(
        &[(
            true,
            Kind::RxSignaling,
            Some(json!({"squelch":{"source":"dsp","pin":[12],"scancode":2,"level":610}})),
        )],
        |candidate| {
            let p = candidate.resolve_radio_profiles().unwrap();
            let body = &p.local(id(3)).unwrap().configuration;
            assert_eq!(body["squelch"], json!({"source":"dsp","level":610}));
        },
    );
}
#[test]
fn conflicting_parallel_carrier_and_tone_claims_cannot_admit_one_shared_pin() {
    with_port(
        &[(
            true,
            Kind::RxSignaling,
            Some(
                json!({"squelch":{"source":"parallel","pin":[12]},"ctcss":{"enabled":true,"source":"parallel","pin":[12]}}),
            ),
        )],
        |candidate| {
            assert!(candidate.resolve_radio_profiles().is_err());
        },
    );
}

#[test]
fn scan_settle_overflow_resumes_an_inherited_interval_and_retains_valid_siblings() {
    let m = json!({"frequency":146.94,"offset":{"enabled":false,"direction":"minus","frequency":0.6},"mode":"FM","tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}});
    with_port(
        &[
            (true, Kind::Memories, Some(json!({"entries":[m]}))),
            (
                false,
                Kind::ScanLists,
                Some(
                    json!({"scan":{"selector":1,"members":[1],"timing":{"dwell_ms":501,"settle_ms":50}}}),
                ),
            ),
            (
                true,
                Kind::ScanLists,
                Some(json!({"scan":{"timing":{"settle_ms":u64::MAX,"resume_delay_ms":642}}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let s = &p.port(id(2)).unwrap().scan_lists["scan"];
            assert_eq!((s.dwell_ms, s.settle_ms, s.resume_delay_ms), (501, 50, 642));
            assert!(p.warnings().iter().any(|w| w.resource.id == id(5)));
        },
    );
}

#[test]
fn reserved_sound_chip_outputs_and_unwired_clip_indicators_fall_back_before_sdk_preparation() {
    use rpt_advanced_core::config::resources::profiles::radio::GpioMode;
    with_port(
        &[
            (
                false,
                Kind::Interface,
                Some(
                    json!({"gpio":{"wiring_profile":"dudeusb","pins":{"1":"out0"},"clip_led_pin":"1"}}),
                ),
            ),
            (
                true,
                Kind::Interface,
                Some(json!({"gpio":{"pins":{"3":"out1"},"clip_led_pin":4}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let g = &p.port(id(2)).unwrap().interface.gpio;
            assert_eq!(g.pins[2], GpioMode::Input);
            assert_eq!(g.clip_led_pin, Some(1));
            assert!(p.warnings().iter().any(|w| w.resource.id == id(4)));
        },
    );
}

#[test]
fn active_tone_maps_keep_both_compatible_inherited_arrays_and_independent_siblings() {
    with_port(
        &[
            (
                false,
                Kind::RxSignaling,
                Some(json!({"ctcss":{"enabled":true,"tones_hz":[100,103.5]}})),
            ),
            (
                false,
                Kind::TxSignaling,
                Some(json!({"ctcss":{"enabled":true,"tones_hz":[123,127.3]}})),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ctcss":{"tones_hz":[67],"level_dbfs":-28.0}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let signals = &p.port(id(2)).unwrap().signals;
            assert_eq!(signals.rx.ctcss.tones.ordered, [1000, 1035]);
            assert_eq!(signals.tx.ctcss.tones.ordered, [1230, 1273]);
            assert_eq!(signals.tx.ctcss.level_dbfs, -28.0);
            assert!(p.warnings().iter().any(|w| w.resource.id == id(5)));
        },
    );
}

#[test]
fn r1_both_tone_protocols_are_rejected_after_each_direction_inherits() {
    for kind in [Kind::RxSignaling, Kind::TxSignaling] {
        for (site, port) in [
            (
                json!({}),
                json!({"ctcss":{"enabled":true},"dcs":{"enabled":true}}),
            ),
            (
                json!({"ctcss":{"enabled":true}}),
                json!({"dcs":{"enabled":true}}),
            ),
            (
                json!({"dcs":{"enabled":true}}),
                json!({"ctcss":{"enabled":true}}),
            ),
        ] {
            with_port(
                &[(false, kind, Some(site)), (true, kind, Some(port))],
                |c| {
                    let error = c
                        .resolve_radio_profiles()
                        .expect_err("one qualifier would be lost");
                    assert_eq!(error.resource.id, id(4));
                },
            );
        }
        with_port(
            &[
                (false, kind, Some(json!({"ctcss":{"enabled":true}}))),
                (
                    true,
                    kind,
                    Some(json!({"ctcss":{"enabled":false},"dcs":{"enabled":true}})),
                ),
            ],
            |c| assert!(c.resolve_radio_profiles().is_ok()),
        );
    }
}
#[test]
fn r1_exact_selected_pins_replace_only_cor_tone_and_ptt_roles() {
    use rpt_advanced_core::config::resources::profiles::radio::{
        ParallelInput as I, ParallelOutput as O,
    };
    let dev = json!({"parallel":{"inputs":{"10":"cor","12":"ctcss","15":"in"},"outputs":{"2":"ptt","5":"out1"}}});
    for explicit in [true, false] {
        let rx = if explicit {
            json!({"squelch":{"source":"parallel","pin":[12]},"ctcss":{"enabled":true,"source":"parallel","pin":[13]}})
        } else {
            json!({"squelch":{"source":"parallel"},"ctcss":{"enabled":true,"source":"parallel"}})
        };
        let tx = if explicit {
            json!({"ptt":{"source":"parallel","pin":[3]}})
        } else {
            json!({"ptt":{"source":"parallel"}})
        };
        with_port(
            &[
                (true, Kind::Interface, Some(dev.clone())),
                (true, Kind::RxSignaling, Some(rx)),
                (true, Kind::TxSignaling, Some(tx)),
            ],
            |c| {
                let resolved = c.resolve_radio_profiles().unwrap();
                let pins = &resolved.port(id(2)).unwrap().interface.parallel;
                let (inputs, keys) = if explicit {
                    (
                        [I::Unassigned, I::Carrier, I::Ctcss, I::Input],
                        [O::Unassigned, O::PushToTalk],
                    )
                } else {
                    (
                        [I::Carrier, I::Ctcss, I::Unassigned, I::Input],
                        [O::PushToTalk, O::Unassigned],
                    )
                };
                assert_eq!(pins.inputs, inputs);
                assert_eq!(pins.outputs[..2], keys);
                assert_eq!(pins.outputs[3], O::High);
            },
        );
    }
}
#[test]
fn r1_unknown_source_keeps_valid_local_pin_for_inherited_parallel() {
    with_port(
        &[
            (
                false,
                Kind::RxSignaling,
                Some(json!({"squelch":{"source":"parallel","pin":[10]}})),
            ),
            (
                true,
                Kind::RxSignaling,
                Some(json!({"squelch":{"source":"typo","pin":[12],"level":611}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let sq = &p.port(id(2)).unwrap().signals.rx.squelch;
            assert_eq!(sq.binding.pins, [12]);
            assert_eq!(sq.level, 611);
            assert_eq!(
                p.local(id(4)).unwrap().configuration["squelch"],
                json!({"pin":[12],"level":611})
            );
            assert!(p.warnings().iter().any(|w| w.resource.id == id(4)));
        },
    );
}
#[test]
fn r1_raw_parallel_reservations_cover_raw_only_and_automatic_fallback() {
    let site = id(1);
    let ports = [(id(2), PortKey::new("a")), (id(3), PortKey::new("b"))];
    let slots = [0, 1].map(|i| ResourceBinding {
        id: id(4 + i),
        scope: ResourceScope::Port(id(2 + i)),
        kind: Kind::Interface,
    });
    let registry = ResourceRegistry::new(site, &ports, &slots).unwrap();
    for (paths, bases, pins, collision) in [
        (["", ""], [888, 888], [true, true], true),
        (
            ["/dev/parport0", "/dev/parport1"],
            [888, 888],
            [true, true],
            true,
        ),
        (["", ""], [888, 632], [true, true], false),
        (["", ""], [888, 888], [true, false], false),
    ] {
        let bodies:Vec<_>=registry.bindings().map(|b|{
            let mut v=json!({"id":b.id.to_string(),"kind":b.kind.as_str()});
            v["configuration"]=if let Some(i)=slots.iter().position(|s|s.id==b.id) {
                json!({"parallel":{"device":paths[i],"base_address":bases[i],"inputs":{"10":if pins[i]{"in"}else{""}}}})
            }else if b.id==site {json!({"inherit_from":null,"name":"site"})}else{
                let key=ports.iter().find(|(id,_)|*id==b.id).unwrap().1.as_str();v["key"]=json!(key);json!({"inherit_from":"site","name":key})
            };(b,serde_json::to_vec(&v).unwrap())
        }).collect();
        let captures: Vec<_> = bodies
            .iter()
            .map(|(b, json)| CapturedResource { binding: *b, json })
            .collect();
        let c = ModularCandidate::assemble(&registry, &captures).unwrap();
        let result = c.resolve_radio_profiles();
        if collision
            && cfg!(all(
                target_os = "linux",
                any(target_arch = "x86", target_arch = "x86_64")
            ))
        {
            let e = result.unwrap_err();
            assert_eq!(
                (e.resource.id, e.conflicting.unwrap().id, e.field),
                (id(3), id(2), "parallel_base_address")
            );
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn ambiguous_active_scalar_and_programmed_rx_map_cannot_discard_either_intention() {
    with_port(
        &[(
            true,
            Kind::RxSignaling,
            Some(json!({"ctcss":{"enabled":true,"frequency_hz":123,"tones_hz":[100,123]}})),
        )],
        |candidate| {
            let e = candidate.resolve_radio_profiles().unwrap_err();
            assert_eq!((e.resource.id, e.field), (id(3), "frequency_hz"));
        },
    );
}
#[test]
fn repeated_transmit_tones_retain_warning_free_sparse_and_effective_maps() {
    with_port(
        &[
            (
                true,
                Kind::RxSignaling,
                Some(json!({"ctcss":{"enabled":true,"tones_hz":[100,123]}})),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ctcss":{"enabled":true,"tones_hz":[88.5,88.5]}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            assert_eq!(
                p.local(id(4)).unwrap().configuration["ctcss"]["tones_hz"],
                json!([88.5, 88.5])
            );
            assert!(p.warnings().is_empty());
        },
    );
}

#[test]
fn a_zero_port_site_resolves_radio_and_independent_processing_without_claiming_hardware() {
    let site = id(1);
    let bindings = [
        (Kind::Radio, json!({"duplex_mode":"full"})),
        (Kind::PeerAudio, json!({})),
        (
            Kind::Interface,
            json!({"audio":{"device_identifier":"site policy; no open"}}),
        ),
    ];
    let slots: Vec<_> = bindings
        .iter()
        .enumerate()
        .map(|(i, (kind, _))| ResourceBinding {
            id: id(i as u32 + 3),
            scope: ResourceScope::Site(site),
            kind: *kind,
        })
        .collect();
    let registry = ResourceRegistry::new(site, &[], &slots).unwrap();
    let docs: Vec<_> = registry
        .bindings()
        .map(|b| {
            let settings = slots
                .iter()
                .position(|r| r.id == b.id)
                .map(|i| bindings[i].1.clone())
                .unwrap_or_else(|| json!({"name":"site","inherit_from":null}));
            (
                b,
                serde_json::to_vec(
                    &json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":settings}),
                )
                .unwrap(),
            )
        })
        .collect();
    let captures: Vec<_> = docs
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let candidate = ModularCandidate::assemble(&registry, &captures).unwrap();
    let resolved = candidate.resolve_radio_profiles().unwrap();
    let policy = resolved.site();
    assert!(policy.general.full_duplex());
    assert_eq!(
        policy.interface.audio.device_identifier,
        "site policy; no open"
    );
    assert!(policy.processing.link.is_some());
    assert!(policy.processing.local.is_none());
    assert!(resolved.port(site).is_none());
}

#[test]
fn site_radio_projection_and_registered_port_use_their_own_precedence_and_capture() {
    with_port(
        &[
            (
                false,
                Kind::RxSignaling,
                Some(json!({"squelch":{"level":713}})),
            ),
            (
                true,
                Kind::RxSignaling,
                Some(json!({"squelch":{"level":206}})),
            ),
            (true, Kind::PeerAudio, Some(json!({}))),
        ],
        |c| {
            let resolved = c.resolve_radio_profiles().unwrap();
            assert_eq!(resolved.site().signals.rx.squelch.level, 713);
            assert!(resolved.site().processing.link.is_none());
            let local = resolved.port(id(2)).unwrap();
            assert_eq!(local.signals.rx.squelch.level, 206);
            assert!(local.processing.link.is_some());
        },
    );
}

#[test]
fn site_unresolved_scan_keeps_its_typed_selectors_while_a_real_port_supplies_its_memories() {
    let m = json!({"frequency":146.52,"offset":{"enabled":false,"direction":"minus","frequency":0.6},"mode":"FM","tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}});
    with_port(
        &[
            (false, Kind::Radio, Some(json!({"startup_memory":1}))),
            (
                false,
                Kind::ScanLists,
                Some(json!({"north":{"selector":2,"members":[1],"priority":1}})),
            ),
            (true, Kind::Memories, Some(json!({"entries":[m]}))),
            (true, Kind::PeerAudio, None),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            assert_eq!(p.site().scan_lists["north"].members, [1]);
            assert_eq!(p.site().general.startup_memory, Some(1));
            assert!(
                p.site_diagnostics()
                    .iter()
                    .any(|d| d.resource.id == id(4) && d.field == "members")
            );
            assert_eq!(
                p.port(id(2)).unwrap().settings.scan_lists["north"].members,
                [1]
            );
            assert_eq!(
                p.source(ResourceScope::Site(id(1)), Kind::ScanLists)
                    .unwrap()
                    .id,
                id(4)
            );
            assert!(
                p.source(ResourceScope::Port(id(2)), Kind::PeerAudio)
                    .is_none()
            );
        },
    );
}
#[test]
fn public_radio_timings_and_hamlib_device_are_lossless_before_native_projection() {
    with_port(
        &[
            (
                false,
                Kind::RxSignaling,
                Some(json!({"timing":{"on_delay_ms":21}})),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ptt":{"timing":{"off_delay_ms":39}}})),
            ),
            (
                false,
                Kind::Control,
                Some(
                    json!({"hamlib":{"device":" /dev/station ","options":{"custom":" keep ","rig_pathname":"ignored"}}}),
                ),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let site = p.site();
            assert_eq!(
                (site.signals.rx.on_delay_ms, site.signals.rx.on_delay_blocks),
                (21, 2)
            );
            let radio = p.port(id(2)).unwrap();
            assert_eq!(
                (
                    radio.signals.tx.off_delay_ms,
                    radio.signals.tx.off_delay_blocks
                ),
                (39, 2)
            );
            assert_eq!(radio.general.duplex_mode.as_str(), "half");
            assert_eq!(radio.remote.hamlib_device(), Some(" /dev/station "));
            assert_eq!(
                radio
                    .remote
                    .authored_hamlib_options()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect::<Vec<_>>(),
                [("custom", " keep ")]
            );
            assert_eq!(
                radio.remote.hamlib_options["rig_pathname"],
                " /dev/station "
            );
        },
    );
}

#[test]
fn site_scan_rows_without_selectors_or_members_remain_typed_and_editable() {
    with_port(
        &[(
            false,
            Kind::ScanLists,
            Some(json!({"north":{"timing":{"dwell_ms":750},"priority":2}})),
        )],
        |candidate| {
            let resolved = candidate.resolve_radio_profiles().unwrap();
            let scan = &resolved.partial_site_scans()["north"];
            assert_eq!(
                (scan.selector, scan.members.as_deref(), scan.priority),
                (None, None, Some(2))
            );
            assert_eq!(scan.dwell_ms, 750);
            assert!(resolved.port(id(2)).unwrap().scan_lists.is_empty());
        },
    );
}

#[test]
fn c1_site_ptt_retains_parallel_source_when_only_the_port_supplies_the_connector() {
    with_port(
        &[
            (
                false,
                Kind::TxSignaling,
                Some(json!({"ptt":{"source":"parallel","inverted":true}})),
            ),
            (
                true,
                Kind::Interface,
                Some(json!({"parallel":{"outputs":{"2":"ptt"}}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            let site = &p.site().signals.tx.ptt;
            assert_eq!(site.source.as_str(), "parallel");
            assert!(site.pins.is_empty() && site.inverted);
            assert!(
                p.site_diagnostics()
                    .iter()
                    .any(|d| d.resource.id == id(3) && d.field == "pin")
            );
            let child = &p.port(id(2)).unwrap().signals.tx.ptt;
            assert_eq!(child.source.as_str(), "parallel");
            assert_eq!(child.pins, [2]);
            assert!(child.inverted);
        },
    );
}
#[test]
fn c1_site_clip_indicator_retains_pin_when_only_the_port_supplies_output_direction() {
    with_port(
        &[
            (
                false,
                Kind::Interface,
                Some(json!({"gpio":{"clip_led_pin":1}})),
            ),
            (
                true,
                Kind::Interface,
                Some(json!({"gpio":{"pins":{"1":"out0"}}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            assert_eq!(p.site().interface.gpio.clip_led_pin, Some(1));
            assert!(
                p.site_diagnostics()
                    .iter()
                    .any(|d| d.resource.id == id(3) && d.field == "clip_led_pin")
            );
            assert_eq!(p.port(id(2)).unwrap().interface.gpio.clip_led_pin, Some(1));
            assert_eq!(
                p.port(id(2)).unwrap().interface.gpio.pins[0].as_str(),
                "out0"
            );
        },
    );
}
#[test]
fn c1_site_ctcss_hang_retains_duration_when_only_the_port_supplies_the_ptt_hang() {
    with_port(
        &[
            (
                false,
                Kind::TxSignaling,
                Some(json!({"ctcss":{"timing":{"hang_ms":100}}})),
            ),
            (
                true,
                Kind::TxSignaling,
                Some(json!({"ptt":{"timing":{"hang_ms":200}}})),
            ),
        ],
        |c| {
            let p = c.resolve_radio_profiles().unwrap();
            assert_eq!(
                (
                    p.site().signals.tx.ctcss.hang_ms,
                    p.site().signals.tx.hang_ms
                ),
                (100, 0)
            );
            assert!(
                p.site_diagnostics()
                    .iter()
                    .any(|d| d.resource.id == id(3) && d.field == "hang_ms")
            );
            let tx = &p.port(id(2)).unwrap().signals.tx;
            assert_eq!((tx.ctcss.hang_ms, tx.hang_ms), (100, 200));
        },
    );
}
#[test]
fn c2_only_the_inheriting_port_projects_its_signaling_pin_onto_the_interface() {
    for registered in [false, true] {
        with_scopes(
            registered,
            &[
                (
                    false,
                    Kind::Interface,
                    Some(json!({"parallel":{"outputs":{"2":"ptt","5":"out1"}}})),
                ),
                (
                    false,
                    Kind::TxSignaling,
                    Some(json!({"ptt":{"source":"parallel","pin":[3]}})),
                ),
            ],
            |c| {
                let p = c.resolve_radio_profiles().unwrap();
                assert_eq!(
                    p.site().interface.parallel.outputs[..4]
                        .iter()
                        .map(|o| o.as_str())
                        .collect::<Vec<_>>(),
                    ["ptt", "", "", "out1"]
                );
                assert_eq!(p.site().signals.tx.ptt.pins, [3]);
                assert!(p.site_diagnostics().is_empty());
                if !registered {
                    assert!(p.port(id(2)).is_none());
                    return;
                }
                let child = p.port(id(2)).unwrap();
                assert_eq!(
                    child.interface.parallel.outputs[..4]
                        .iter()
                        .map(|o| o.as_str())
                        .collect::<Vec<_>>(),
                    ["", "ptt", "", "out1"]
                );
            },
        );
    }
}
