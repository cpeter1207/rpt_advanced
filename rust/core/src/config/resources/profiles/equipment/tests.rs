use super::*;
use crate::config::{
    ResolvedMorseSettings, ResolvedSpeechSettings,
    resources::candidate::{CapturedResource, ResourceRegistry},
};
use crate::messages::MessageCatalog;
use serde_json::json;

#[test]
fn scoped_equipment_preserves_real_interface_observation_and_site_fan_ownership() {
    let id = |n: u8| {
        format!("929e52be-6854-4c46-85f2-{n:012}")
            .parse::<ResourceId>()
            .unwrap()
    };
    let site = ResourceScope::Site(id(1));
    let port = ResourceScope::Port(id(2));
    let b = |n, scope, kind| ResourceBinding {
        id: id(n),
        scope,
        kind,
    };
    let mut rows = vec![
        (
            b(1, site, ResourceKind::Metadata),
            None,
            json!({"inherit_from":null,"name":"Equipment"}),
        ),
        (
            b(3, site, ResourceKind::Fan),
            Some("shared"),
            json!({"enabled":true,"mode":"combined","temperature_sensor":"temperature","on_temperature":36.5,"off_temperature":34.0,"output":"blower"}),
        ),
        (
            b(7, site, ResourceKind::Sensor),
            Some("temperature"),
            json!({"enabled":true,"label":"Air temperature","source":{"type":"hwmon","device":"board","channel":"temp1_input"}}),
        ),
        (
            b(8, site, ResourceKind::Output),
            Some("blower"),
            json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"pwm1"}}),
        ),
        (
            b(9, site, ResourceKind::Alarm),
            Some("hot"),
            json!({"enabled":true,"source":"temperature","condition":"above","trip_value":37,"clear_value":34,"trip_message":"High temperature"}),
        ),
    ];
    for with_port in [false, true] {
        if with_port {
            rows.extend([
                    (b(2,port,ResourceKind::Metadata),None,json!({"inherit_from":"site","name":"local"})),
                    (b(4,site,ResourceKind::Sensor),Some("door"),json!({"enabled":true,"source":{"type":"interface","node":"a / b","property":"/gpio/pins/1"},"unit":"V","calibration":{"mode":"two_point","input":{"low":0,"high":1},"value":{"low":10,"high":20}}})),
                    (b(5,port,ResourceKind::Sensor),Some("door"),json!({"precision":4,"source":{"property":"/gpio/pins/99"},"timing":{"smoothing_ms":12}})),
                    (b(6,port,ResourceKind::Fan),Some("local"),json!({"enabled":false})),
                ]);
        }
        let members = rows
            .iter()
            .filter_map(|(b, k, _)| k.map(|k| (*b, PortKey::new(k))))
            .collect::<Vec<_>>();
        let ports = if with_port {
            vec![(id(2), PortKey::new("a / b"))]
        } else {
            Vec::new()
        };
        let registry = ResourceRegistry::with_members(id(1), &ports, &[], &members).unwrap();
        let docs = rows
            .iter()
            .map(|(b, k, body)| {
                let mut doc =
                    json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":body});
                if let Some(k) = k {
                    doc["key"] = json!(k);
                }
                (*b, serde_json::to_vec(&doc).unwrap())
            })
            .collect::<Vec<_>>();
        let caps = docs
            .iter()
            .map(|(b, json)| CapturedResource { binding: *b, json })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(&registry, &caps).unwrap();
        let scope = if with_port { port } else { site };
        let commands = candidate.command_language(scope).unwrap();
        let catalog = MessageCatalog::from_sources(
            "en-US",
            include_str!("../../../../../../../messages/en-US.ftl"),
            None,
        )
        .unwrap();
        let messages = candidate
            .message_profiles(
                scope,
                catalog,
                &"en-US".parse().unwrap(),
                &ResolvedSpeechSettings {
                    voice: String::new(),
                    speed_percent: 100,
                    level_db: 0,
                },
                &ResolvedMorseSettings {
                    frequency_hz: 800,
                    speed_wpm: 20,
                    level_db: -6,
                },
            )
            .unwrap();
        let profile = candidate
            .equipment_profiles(
                scope,
                |_, interface| {
                    (interface.property == "/gpio/pins/1").then_some(InterfaceCapability::Boolean)
                },
                &commands,
                &messages,
            )
            .unwrap();
        assert!(
            profile
                .settings
                .fans
                .iter()
                .find(|f| f.name == "shared")
                .unwrap()
                .ptt_nodes
                .is_empty()
        );
        let shared = profile
            .settings
            .fans
            .iter()
            .find(|f| f.name == "shared")
            .unwrap();
        assert!(profile.labels.contains_key("temperature"));
        assert!(
            profile
                .alarm_messages
                .contains_key(&("hot".to_owned(), "trip_message"))
        );
        let mut fan = crate::site::FanState::new(shared.clone());
        assert_eq!(
            fan.advance(
                100,
                &Default::default(),
                Some(39.0),
                None,
                crate::site::FanOutput {
                    available: true,
                    pwm: true
                }
            )
            .duty,
            1.0
        );
        if with_port {
            let sensor = profile
                .settings
                .sensors
                .iter()
                .find(|s| s.name == "door")
                .unwrap();
            assert_eq!(
                profile
                    .source(
                        ResourceKind::Sensor,
                        &PortKey::new("door"),
                        &["source", "property"]
                    )
                    .unwrap()
                    .id,
                id(4)
            );
            assert!(profile.local[&id(5)].get("source").is_none());
            assert_eq!(sensor.precision, 4);
            let source = sensor.interface.as_ref().unwrap().clone();
            assert_eq!(source.port, id(2));
            assert_eq!(source.key.as_str(), "a / b");
            assert!(sensor.endpoint.is_none());
            assert_eq!(
                profile
                    .settings
                    .fans
                    .iter()
                    .find(|f| f.name == "local")
                    .unwrap()
                    .ptt_nodes,
                [PortKey::new("a / b")]
            );
            let mut observations = Default::default();
            let mut cache = BTreeMap::from([(
                source.clone(),
                InterfaceObservation::Boolean(Observation {
                    value: Some(true),
                    observed_at_ms: Some(17),
                    quality: ReadingQuality::Valid,
                }),
            )]);
            profile.cached_observations(&cache, &mut observations);
            assert_eq!(
                observations.sensors["door"],
                Observation {
                    value: Some(1.0),
                    observed_at_ms: Some(17),
                    quality: ReadingQuality::Valid
                }
            );
            assert_eq!(
                sensor
                    .calibration
                    .apply(observations.sensors["door"].value.unwrap()),
                Some(20.0)
            );
            cache.insert(
                source,
                InterfaceObservation::Boolean(Observation {
                    value: None,
                    observed_at_ms: Some(21),
                    quality: ReadingQuality::Fault,
                }),
            );
            profile.cached_observations(&cache, &mut observations);
            assert_eq!(
                observations.sensors["door"],
                Observation {
                    value: None,
                    observed_at_ms: Some(21),
                    quality: ReadingQuality::Fault
                }
            );
            assert!(
                candidate
                    .equipment_profiles(
                        scope,
                        |_, _| Some(InterfaceCapability::DigitalOutput),
                        &commands,
                        &messages
                    )
                    .is_err()
            );
        }
    }
}

type Row = (u8, u8, ResourceKind, &'static str, Value);
fn identity(n: u8) -> ResourceId {
    format!("929e52be-6854-4c46-85f2-{n:012}").parse().unwrap()
}
fn owner(n: u8) -> ResourceScope {
    if n == 1 {
        ResourceScope::Site(identity(n))
    } else {
        ResourceScope::Port(identity(n))
    }
}
fn projected(rows: &[Row]) -> Result<Vec<ScopedEquipment>, EquipmentError> {
    let bind = |id, scope, kind| ResourceBinding {
        id: identity(id),
        scope: owner(scope),
        kind,
    };
    let members = rows
        .iter()
        .filter(|(_, _, kind, _, _)| kind.is_named())
        .map(|(id, s, k, n, _)| (bind(*id, *s, *k), PortKey::new(*n)))
        .collect::<Vec<_>>();
    let sections = rows
        .iter()
        .filter(|(_, _, kind, _, _)| !kind.is_named())
        .map(|(id, s, kind, _, _)| bind(*id, *s, *kind))
        .collect::<Vec<_>>();
    let ports = [
        (identity(2), PortKey::new("a")),
        (identity(3), PortKey::new("b")),
    ];
    let reg = ResourceRegistry::with_members(identity(1), &ports, &sections, &members).unwrap();
    let mut docs = Vec::new();
    for n in 1..=3 {
        let b = bind(n, n, ResourceKind::Metadata);
        docs.push((b,serde_json::to_vec(&json!({"id":b.id.to_string(),"kind":"metadata","configuration":{"inherit_from":if n==1{Value::Null}else{json!("site")},"name":format!("owner{n}")}})).unwrap()));
    }
    for (id, scope, kind, key, body) in rows {
        let b = bind(*id, *scope, *kind);
        let mut doc = json!({"id":b.id.to_string(),"kind":kind.as_str(),"configuration":body});
        if kind.is_named() {
            doc["key"] = json!(key);
        }
        docs.push((b, serde_json::to_vec(&doc).unwrap()));
    }
    let caps = docs
        .iter()
        .map(|(b, bytes)| CapturedResource {
            binding: *b,
            json: bytes,
        })
        .collect::<Vec<_>>();
    let candidate = ModularCandidate::assemble(&reg, &caps).unwrap();
    (1..=3)
        .map(|n| {
            let scope = owner(n);
            let commands = candidate.command_language(scope).unwrap();
            let catalog = MessageCatalog::from_sources(
                "en-US",
                include_str!("../../../../../../../messages/en-US.ftl"),
                None,
            )
            .unwrap();
            let messages = candidate
                .message_profiles(
                    scope,
                    catalog,
                    &"en-US".parse().unwrap(),
                    &ResolvedSpeechSettings {
                        voice: String::new(),
                        speed_percent: 100,
                        level_db: 0,
                    },
                    &ResolvedMorseSettings {
                        frequency_hz: 800,
                        speed_wpm: 20,
                        level_db: -6,
                    },
                )
                .unwrap();
            candidate.equipment_profiles(
                scope,
                |_, _| Some(InterfaceCapability::Boolean),
                &commands,
                &messages,
            )
        })
        .collect()
}
fn gpio(line: &str) -> Value {
    json!({"enabled":true,"source":{"type":"gpiod","device":"gpiochip0","line":line}})
}
#[test]
fn inherited_fan_claims_only_its_declared_sink() {
    use ResourceKind::*;
    let site_output: Row = (
        10,
        1,
        Output,
        "blower",
        json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"pwm1"}}),
    );
    let child_output: Row = (12, 2, Output, "blower", gpio("2"));
    let site_fan: Row = (
        11,
        1,
        Fan,
        "cooler",
        json!({"enabled":true,"output":"blower"}),
    );
    let child_input: Row = (
        13,
        2,
        Input,
        "switch",
        json!({"enabled":true,"source":{"type":"gpiod","device":"gpiochip0","line":"3"},"on_active":["output:blower:on"]}),
    );
    let child_macro: Row = (
        14,
        2,
        Macro,
        "switch-on",
        json!({"actions":[{"action":"set_output","name":"blower","state":"on"}]}),
    );
    let child_mapping: Row = (
        15,
        2,
        Command,
        "",
        json!({"mappings":{"switch":{"command":"B","protected":true,"action":"set_output","name":"blower","state":"on"}}}),
    );
    let child_fan: Row = (
        16,
        2,
        Fan,
        "second",
        json!({"enabled":true,"output":"blower"}),
    );
    let temperature: Row = (
        17,
        1,
        Sensor,
        "temp",
        json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"temp1_input"}}),
    );
    let thermal = json!({
        "enabled":true,"output":"blower","mode":"temperature","temperature_sensor":"temp",
        "temperature_curve":[{"temperature":30,"duty":0.25},{"temperature":45,"duty":1.0}]
    });
    let thermal_site = (11, 1, Fan, "cooler", thermal.clone());
    let thermal_child = (16, 2, Fan, "second", thermal);
    let cases = [
        (
            "named input action",
            vec![child_output.clone(), child_input.clone()],
            &[][..],
        ),
        (
            "named macro",
            vec![child_output.clone(), child_macro.clone()],
            &[],
        ),
        (
            "DTMF mapping",
            vec![child_output.clone(), child_mapping.clone()],
            &[],
        ),
        (
            "declared PWM fan with digital child output",
            vec![child_output.clone(), temperature.clone(), thermal_site],
            &[],
        ),
        (
            "named action addresses shared sink",
            vec![child_input],
            &[11, 13],
        ),
        ("macro addresses shared sink", vec![child_macro], &[11]),
        (
            "DTMF mapping addresses shared sink",
            vec![child_mapping],
            &[11],
        ),
        (
            "two fans address shared sink",
            vec![child_fan.clone()],
            &[16],
        ),
        (
            "two fans address independent sinks",
            vec![child_output.clone(), child_fan],
            &[],
        ),
        (
            "declared child fan requires a PWM sink",
            vec![child_output, temperature, thermal_child],
            &[16],
        ),
    ];
    let mut mismatches = Vec::new();
    for (case, extra, expected) in cases {
        let mut rows = vec![site_output.clone(), site_fan.clone()];
        for row in extra {
            if row.0 == 11 {
                rows[1] = row;
            } else {
                rows.push(row);
            }
        }
        let result = projected(&rows).and_then(|all| {
            let key = (Output, PortKey::new("blower"));
            let child = rows.iter().any(|row| row.0 == 12);
            let child_id = if child { 12 } else { 10 };
            for (i, output_id) in [10, child_id, 10].into_iter().enumerate() {
                assert_eq!(all[i].effective[&key].0.id, identity(output_id), "{case}");
                assert_eq!(
                    all[i].settings.outputs[0]
                        .endpoint
                        .as_ref()
                        .unwrap()
                        .channel,
                    if output_id == 12 { "2" } else { "pwm1" },
                    "{case}"
                );
            }
            ScopedEquipment::validate_owners(&all)
        });
        let matches = match result {
            Ok(()) => expected.is_empty(),
            Err(EquipmentError::Relationship(binding)) => {
                expected.iter().any(|id| binding.id == identity(*id))
            }
            Err(EquipmentError::Scope) => false,
        };
        if !matches {
            mismatches.push(format!(
                "{case}: expected relationship IDs {expected:?}, got {result:?}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn round_one_shared_fan_prohibits_sibling_fan_and_direct_action() {
    use ResourceKind::*;
    for other in [
        (
            12,
            3,
            Fan,
            "second",
            json!({"enabled":true,"output":"blower"}),
        ),
        (
            12,
            3,
            Input,
            "switch",
            json!({"enabled":true,"source":{"type":"gpiod","device":"gpiochip0","line":"3"},"on_active":["output:blower:on"]}),
        ),
    ] {
        let all = projected(&[
            (10, 1, Output, "blower", gpio("1")),
            (
                11,
                2,
                Fan,
                "cooler",
                json!({"enabled":true,"output":"blower"}),
            ),
            other,
        ])
        .unwrap();
        assert!(ScopedEquipment::validate_owners(&all).is_err());
    }
    let shared = projected(&[
        (10, 1, Output, "blower", gpio("1")),
        (
            11,
            1,
            Fan,
            "cooler",
            json!({"enabled":true,"output":"blower"}),
        ),
    ])
    .unwrap();
    assert!(ScopedEquipment::validate_owners(&shared).is_ok());
}
#[test]
fn round_one_gpio_aliases_match_physical_line_within_and_between_scopes() {
    use ResourceKind::*;
    for second in [2, 3] {
        let found = projected(&[
            (10, 2, Output, "one", gpio("1")),
            (11, second, Output, "two", gpio("01")),
        ]);
        assert!(
            found
                .and_then(|p| ScopedEquipment::validate_owners(&p))
                .is_err()
        );
    }
}
#[test]
fn round_one_view_override_preserves_acquisition_and_changed_source_owns_job() {
    use ResourceKind::*;
    let all = projected(&[
        (
            10,
            1,
            Sensor,
            "temp",
            json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"temp1"}}),
        ),
        (
            11,
            2,
            Sensor,
            "temp",
            json!({"precision":4,"timing":{"poll_interval_ms":75}}),
        ),
        (12, 3, Sensor, "temp", json!({"source":{"channel":"temp2"}})),
    ])
    .unwrap();
    assert_eq!(all[0].owned(owner(1)).sensors.len(), 1);
    assert!(all[1].owned(owner(2)).sensors.is_empty());
    assert_eq!(all[2].owned(owner(3)).sensors.len(), 1);
    assert_eq!(all[1].settings.sensors[0].precision, 4);
    let jobs = ScopedEquipment::acquisition_plan(&all).unwrap();
    assert_eq!(jobs[&owner(1)].sensors[0].poll_interval_ms, 75);
    assert_eq!(all[0].settings.sensors[0].poll_interval_ms, 1000);
    assert!(jobs[&owner(2)].sensors.is_empty());
    assert_eq!(
        all[1]
            .acquisition_owner(Sensor, &PortKey::new("temp"))
            .unwrap()
            .id,
        identity(10)
    );
}
#[test]
fn round_one_zero_rpm_inherits_positive_or_fails_unusable_relationship() {
    use ResourceKind::*;
    let mut rows = vec![
        (10, 1, Output, "blower", gpio("1")),
        (
            11,
            1,
            Sensor,
            "tach",
            json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"fan1_input"}}),
        ),
        (
            12,
            1,
            Fan,
            "cooler",
            json!({"enabled":true,"output":"blower","rpm_sensor":"tach","minimum_rpm":300}),
        ),
        (13, 2, Fan, "cooler", json!({"minimum_rpm":0})),
    ];
    let all = projected(&rows).unwrap();
    assert_eq!(all[1].settings.fans[0].minimum_rpm, Some(300.0));
    assert!(!all[1].warnings.is_empty());
    rows[2].4["minimum_rpm"] = json!(0);
    assert!(projected(&rows).is_err());
}
#[test]
fn round_one_fallback_provenance_and_finite_calibration_spans() {
    use ResourceKind::*;
    let good = json!({"mode":"two_point","input":{"low":0,"high":2},"value":{"low":0,"high":1}});
    let bad =
        json!({"mode":"two_point","input":{"low":-1e308,"high":1e308},"value":{"low":0,"high":1}});
    let mut rows = vec![
        (
            10,
            1,
            Sensor,
            "temp",
            json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"temp1"},"calibration":good,"timing":{"stale_after_ms":6000}}),
        ),
        (
            11,
            2,
            Sensor,
            "temp",
            json!({"timing":{"poll_interval_ms":7000},"calibration":bad}),
        ),
    ];
    let all = projected(&rows).unwrap();
    let key = PortKey::new("temp");
    assert_eq!(all[1].settings.sensors[0].stale_after_ms, 7001);
    assert!(
        all[1]
            .source(Sensor, &key, &["timing", "stale_after_ms"])
            .is_none()
    );
    assert_eq!(all[1].settings.sensors[0].calibration.apply(1.0), Some(0.5));
    assert!(all[1].local[&identity(11)].get("calibration").is_none());
    rows[0].4["calibration"] = bad;
    let all = projected(&rows).unwrap();
    assert!(all[0].local[&identity(10)].get("calibration").is_none());
    assert!(
        all[0]
            .source(Sensor, &key, &["calibration", "mode"])
            .is_none()
    );
    assert_eq!(
        all[0].effective[&(Sensor, key)].1["calibration"]["mode"],
        "none"
    );
}

#[test]
fn round_two_equipment_discarded_fan_override_keeps_one_enabled_site_policy() {
    use ResourceKind::*;
    let all=projected(&[
        (10,1,Output,"blower",gpio("1")),
        (11,1,Sensor,"tach",json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"fan1_input"}})),
        (12,1,Fan,"cooler",json!({"enabled":true,"output":"blower","rpm_sensor":"tach","minimum_rpm":300,"ptt_nodes":[]})),
        (13,2,Fan,"cooler",json!({"minimum_rpm":0,"timing":{"run_on_ms":"bad"}})),
    ]).unwrap();
    assert!(all[1].settings.fans[0].ptt_nodes.is_empty());
    assert_eq!(all[1].settings.fans[0].minimum_rpm, Some(300.0));
    assert!(all[1].local[&identity(13)].get("minimum_rpm").is_none());
    assert!(!all[1].warnings.is_empty());
    let plan = ScopedEquipment::acquisition_plan(&all).unwrap();
    assert_eq!(plan[&owner(1)].fans.len(), 1);
    assert!(plan[&owner(2)].fans.is_empty());
    assert!(plan[&owner(3)].fans.is_empty());
    assert_eq!(
        all[1]
            .source(Fan, &PortKey::new("cooler"), &["minimum_rpm"])
            .unwrap()
            .id,
        identity(12)
    );
}
#[test]
fn round_two_equipment_unsafe_calibration_requires_explicit_valid_recovery() {
    use ResourceKind::*;
    for (cal, enabled, expected) in [
        (json!({}), false, 0.0),
        (json!({"unknown":1}), false, 0.0),
        (json!({"mode":"invalid"}), false, 0.0),
        (json!({"input":{"low":0,"high":1}}), false, 0.0),
        (json!({"mode":"none"}), true, 1.0),
        (
            json!({"mode":"two_point","input":{"low":0,"high":2},"value":{"low":0,"high":1}}),
            true,
            0.5,
        ),
    ] {
        let all=projected(&[
            (10,1,Sensor,"temp",json!({"enabled":true,"source":{"type":"hwmon","device":"board","channel":"temp1"},"calibration":{"mode":"two_point","input":{"low":1,"high":1},"value":{"low":0,"high":1}}})),
            (11,2,Sensor,"temp",json!({"calibration":cal})),
        ]).unwrap();
        let key = PortKey::new("temp");
        let plan = ScopedEquipment::acquisition_plan(&all).unwrap();
        assert!(!all[0].settings.sensors[0].enabled);
        assert_eq!(all[1].settings.sensors[0].enabled, enabled);
        assert_eq!(
            all[1].acquisition_owner(Sensor, &key).map(|b| b.id),
            enabled.then_some(identity(11))
        );
        assert_eq!(
            plan[&owner(2)].sensors.iter().filter(|s| s.enabled).count(),
            usize::from(enabled)
        );
        if enabled {
            assert_eq!(
                all[1].settings.sensors[0].calibration.apply(1.0),
                Some(expected)
            );
        }
    }
}
#[test]
fn round_two_equipment_incomplete_endpoints_do_not_own_any_acquisition() {
    use ResourceKind::*;
    let all = projected(&[
        (10, 1, Input, "door", json!({"enabled":true})),
        (
            11,
            2,
            Sensor,
            "temp",
            json!({"enabled":true,"source":{"type":"hwmon"}}),
        ),
    ])
    .unwrap();
    let plan = ScopedEquipment::acquisition_plan(&all).unwrap();
    for view in &all {
        assert!(!view.settings.inputs[0].enabled);
        assert!(
            view.acquisition_owner(Input, &PortKey::new("door"))
                .is_none()
        );
        assert!(view.settings.sensors.iter().all(|s| !s.enabled));
        assert!(
            view.acquisition_owner(Sensor, &PortKey::new("temp"))
                .is_none()
        );
    }
    assert!(
        plan.values()
            .all(|group| group.sensors.is_empty() && group.inputs.is_empty())
    );
}
