//! Real, hardware-independent USBRadioPlus peer-role preparation.
use rptadv_product::configuration_radio::RadioPreparationApi;

#[test]
fn absent_roles_compare_after_dropping_the_original_api_and_can_construct_distinct_live_peers() {
    let api = RadioPreparationApi::open().expect("matched product capability");
    let left = api.prepare_link(None).unwrap();
    let right = api.prepare_link(None).unwrap();
    drop(api);
    assert!(left.equivalent(&right).unwrap());
    let mut a = left.prepare_peer(8000, 160).unwrap();
    let mut b = right.prepare_peer(16000, 320).unwrap();
    drop((left, right));
    let mut x = [8000i16; 160];
    let mut y = [-8000i16; 320];
    a.process(&mut x).unwrap();
    b.process(&mut y).unwrap();
    assert_eq!(x, [8000i16; 160]);
    assert_eq!(y, [-8000i16; 320]);
}

#[test]
fn refuses_a_loaded_product_without_the_required_preparation_capability() {
    // This fixture is the maintained image's separate, previously released product SONAME.
    if let Some(path) = std::env::var_os("RPT_TEST_PRIOR_URP") {
        // SAFETY: the fixture supplies the separately verified prior release.
        assert!(unsafe { RadioPreparationApi::open_at(std::path::Path::new(&path)) }.is_err());
    }
}

use rpt_advanced_core::config::resources::{
    candidate::ResourceKind as K, profiles::radio::RadioPortProfile,
};
use serde_json::{Value, json};

fn station(f: impl FnOnce(&RadioPortProfile)) {
    station_with(&[], f)
}

fn station_with(extra: &[(K, Value)], f: impl FnOnce(&RadioPortProfile)) {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceRegistry,
            ResourceScope,
        },
    };
    let id = |i| {
        format!("00000000-0000-4000-8000-{i:012x}")
            .parse::<ResourceId>()
            .unwrap()
    };
    let site = id(1);
    let port = id(2);
    let mut body = vec![
        (
            K::Interface,
            json!({"audio":{"device_identifier":" exact; # port ", "input_gain_db":0.0}}),
        ),
        (K::PeerAudio, json!({"input_gain_db":-6})),
        (K::Radio, json!({"duplex_mode":"full"})),
        (
            K::RxSignaling,
            json!({"squelch":{"level":500},"timing":{"on_delay_ms":21}}),
        ),
        (K::TxSignaling, json!({"ptt":{"timing":{"settle_ms":77}}})),
    ];
    for (kind, configuration) in extra {
        if let Some((_, existing)) = body.iter_mut().find(|(k, _)| k == kind) {
            *existing = configuration.clone()
        } else {
            body.push((*kind, configuration.clone()))
        }
    }
    let bindings: Vec<_> = body
        .iter()
        .enumerate()
        .map(|(i, (kind, _))| ResourceBinding {
            scope: ResourceScope::Port(port),
            kind: *kind,
            id: id(3 + i),
        })
        .collect();
    let registry = ResourceRegistry::new(
        site,
        &[(port, PortKey::new("literal display #"))],
        &bindings,
    )
    .unwrap();
    let bodies: Vec<_> = registry
        .bindings()
        .map(|b| {
            let mut r = json!({"id":b.id.to_string(),"kind":b.kind.as_str()});
            r["configuration"] = if b.id == site {
                json!({"inherit_from":null,"name":"site"})
            } else if b.id == port {
                r["key"] = json!("literal display #");
                json!({"inherit_from":"site","name":"local"})
            } else {
                body[bindings.iter().position(|v| v.id == b.id).unwrap()]
                    .1
                    .clone()
            };
            (b, serde_json::to_vec(&r).unwrap())
        })
        .collect();
    let captured = bodies
        .iter()
        .map(|(b, v)| CapturedResource {
            binding: *b,
            json: v,
        })
        .collect::<Vec<_>>();
    let candidate = ModularCandidate::assemble(&registry, &captured).unwrap();
    let radios = candidate.resolve_radio_profiles().unwrap();
    f(radios.port(port).unwrap())
}

#[test]
fn actual_prepared_station_visits_typed_values_and_uses_port_uuid_identity() {
    station(|p| {
        let api = RadioPreparationApi::open().unwrap();
        let a = api.prepare_station(p, "6").unwrap();
        let b = api.prepare_station(p, "6").unwrap();
        drop(api);
        assert!(a.equivalent(&b).unwrap());
        assert_eq!(a.port_id(), p.id);
        let inspection = a.inspect().unwrap();
        assert_eq!(
            inspection.device_identifier.as_deref(),
            Some(" exact; # port ")
        );
        assert_eq!(inspection.receive_on_delay_blocks, 2);
        assert_eq!(inspection.settle_ms, 77);
        assert!(inspection.full_duplex);
        assert!(a.remote_commands().unwrap().is_none());
        let mut x = a.prepare_peer(8000, 160).unwrap();
        drop((a, b));
        let mut pcm = [8000i16; 160];
        x.process(&mut pcm).unwrap();
    });
}

#[test]
fn a_matching_product_can_compare_neutral_and_configured_peer_roles_after_station_preparation() {
    station(|p| {
        let api = RadioPreparationApi::open().unwrap();
        let neutral = api.prepare_link(None).unwrap();
        let selected = api.prepare_link(p.processing.link.as_ref()).unwrap();
        assert!(!neutral.equivalent(&selected).unwrap());
        let mut active = selected.prepare_peer(8000, 160).unwrap();
        let mut pcm = [8000i16; 160];
        active.process(&mut pcm).unwrap();
        assert!(pcm[120] < 8000);
    });
}

#[test]
fn native_create_rejects_an_empty_provider_manifest_and_keeps_the_prepared_station() {
    use rptadv_product::configuration_radio::NativeBindings;
    use std::sync::Arc;
    station(|p| {
        let api = RadioPreparationApi::open().unwrap();
        let prepared = api.prepare_station(p, "6").unwrap();
        // SAFETY: these zero-filled ABI records contain no callbacks, pointers or borrowed resources.
        // They intentionally advertise an unsupported manifest. The owning Arc remains alive.
        let bindings = unsafe {
            NativeBindings::from_owned(Arc::new(()), std::mem::zeroed(), std::mem::zeroed())
        };
        assert!(prepared.create_stopped(bindings, 1, 160).is_err());
        assert_eq!(prepared.inspect().unwrap().settle_ms, 77);
    });
}

#[test]
fn released_remote_engine_admits_a_policy_and_preserves_the_prepared_owner_when_another_is_rejected()
 {
    let radio = json!({"startup_memory":1,"duplex_mode":"full"});
    let mem = json!({"entries":[{"frequency":146.940001,"offset":{"enabled":true,"direction":"minus","frequency":0.600001},"mode":"FM","tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}}]});
    let control = json!({"enabled":true,"backend":"hamlib","access":"open","hamlib":{"model":"RIG_MODEL_DUMMY","device":" /optional #; device ","options":{"literal":" a;b# "}}});
    let mut rows = vec![
        (K::Radio, radio),
        (K::Control, control),
        (
            K::Limits,
            json!({"rx_ranges_mhz":[[144,148]],"tx_ranges_mhz":[[144,148]],"modes":["FM"]}),
        ),
        (K::Memories, mem),
    ];
    let api = RadioPreparationApi::open().unwrap();
    let mut current = None;
    station_with(&rows, |p| {
        current = Some(api.prepare_station(p, "782").unwrap())
    });
    let current = current.unwrap();
    assert_eq!(current.remote_commands().unwrap().unwrap().prefix, "782");
    // The public memory has no established RF power. The provider must enforce the authored ceiling.
    rows.push((K::Power, json!({"max_w":15.25})));
    station_with(&rows, |p| match api.prepare_station(p, "782") {
        Err(rptadv_product::configuration_radio::PreparationError::Rejected { field, status }) => {
            assert_ne!(status, 0);
            assert_eq!(field.as_deref(), Some("remote"));
        }
        _ => panic!("provider must enforce maximum RF power"),
    });
    assert_eq!(current.remote_commands().unwrap().unwrap().prefix, "782");
}

#[test]
fn controller_owned_tx_admission_is_independent_from_native_audio_and_rollback_equality() {
    station(|baseline| {
        let api = RadioPreparationApi::open().unwrap();
        let active = api.prepare_station(baseline, "6").unwrap();
        station_with(
            &[
                (K::TxAudio, json!({"enabled":false})),
                (
                    K::Radio,
                    json!({"duplex_mode":"full","parrot":{"enabled":true}}),
                ),
            ],
            |changed| {
                let prepared = api.prepare_station(changed, "6").unwrap();
                assert!(!active.equivalent(&prepared).unwrap());
                let policy = prepared.controller_policy();
                assert!(!policy.transmit_enabled);
                let mut settings = Default::default();
                policy.apply(&mut settings);
                assert!(settings.parrot_enabled);
            },
        );
    });
}

#[test]
fn r1_inverted_parallel_arrays_require_per_pin_polarity_and_health() {
    use rptadv_product::configuration_radio::PreparationError;
    // The product exposes an online, OR-reduced observation. The radio descriptor inverts that single bit.
    let samples = [
        (true, [false, false], true, true),
        (true, [false, true], true, false),
        (true, [true, false], true, false),
        (true, [true, true], false, false),
        (false, [false, false], false, true),
    ];
    for (online, pins, wanted, aggregate) in samples {
        assert_eq!(online && pins.iter().any(|p| !*p), wanted);
        assert_eq!(!(online && pins.iter().any(|p| *p)), aggregate);
    }
    let api = RadioPreparationApi::open().unwrap();
    station(|baseline| {
        let previous = api.prepare_station(baseline, "6").unwrap();
        for name in ["squelch", "ctcss"] {
            for pins in [vec![10], vec![10, 12]] {
                let rows = [(
                    K::RxSignaling,
                    json!({(name):{"enabled":true,"source":"parallel","pin":pins,"inverted":true}}),
                )];
                station_with(&rows, |p| {
                    assert!(matches!(
                        api.prepare_station(p, "6"),
                        Err(PreparationError::Unsupported("inverted_parallel_input"))
                    ))
                });
            }
        }
        assert_eq!(previous.inspect().unwrap().settle_ms, 77);
    });
}
#[test]
fn r1_operating_ctcss_frequency_changes_the_visited_decoder_mask_and_semantic_equality() {
    use rpt_advanced_core::config::resources::profiles::radio::Tones;
    let api = RadioPreparationApi::open().unwrap();
    let mut previous = None;
    for hz in [100, 123] {
        station_with(
            &[(
                K::RxSignaling,
                json!({"ctcss":{"enabled":true,"source":"dsp","frequency_hz":hz}}),
            )],
            |p| {
                let candidate = api.prepare_station(p, "6").unwrap();
                candidate
                    .visit(|s| {
                        // SAFETY: the provider lends the radio and its decoder mask for this visit.
                        let r = unsafe { s.radio.as_ref() }.unwrap();
                        assert_eq!(
                            r.receive.ctcss_tone_mask,
                            1 << Tones::native_index(hz * 10).unwrap()
                        );
                    })
                    .unwrap();
                if let Some(old) = previous.as_ref() {
                    assert!(!candidate.equivalent(old).unwrap())
                }
                previous = Some(candidate);
            },
        );
    }
}
#[test]
fn r1_many_to_one_ctcss_map_preserves_each_native_decoder_slot() {
    use rpt_advanced_core::config::resources::profiles::radio::Tones;
    let api = RadioPreparationApi::open().unwrap();
    station_with(
        &[
            (
                K::RxSignaling,
                json!({"ctcss":{"enabled":true,"tones_hz":[100,123]}}),
            ),
            (
                K::TxSignaling,
                json!({"ctcss":{"enabled":true,"tones_hz":[88.5,88.5]}}),
            ),
        ],
        |p| {
            assert_eq!(p.signals.tx.ctcss.tones.ordered, [885, 885]);
            api.prepare_station(p, "6")
                .unwrap()
                .visit(|s| {
                    // SAFETY: these indexed rows are borrowed from the live native visitor.
                    let r = unsafe { s.radio.as_ref() }.unwrap();
                    for hz in [1000, 1230] {
                        assert_eq!(
                            r.transmit.mapped_ctcss_frequency_tenths_hz
                                [Tones::native_index(hz).unwrap()],
                            885
                        );
                    }
                })
                .unwrap();
        },
    );
}

#[test]
fn explicit_parallel_signal_roles_prepare_the_same_native_owner_with_or_without_stale_interface_assignments()
 {
    let api = RadioPreparationApi::open().unwrap();
    let mut previous = None;
    for stale in [false, true] {
        let mut interface = json!({"audio":{"device_identifier":"test"},"parallel":{"outputs":{"5":"out1"},"inputs":{"15":"in"}}});
        if stale {
            interface["parallel"]["outputs"]["2"] = json!("ptt");
            interface["parallel"]["inputs"]["10"] = json!("cor");
        }
        let rows = [
            (K::Interface, interface),
            (
                K::RxSignaling,
                json!({"squelch":{"source":"parallel","pin":[12]}}),
            ),
            (
                K::TxSignaling,
                json!({"ptt":{"source":"parallel","pin":[3]}}),
            ),
            (K::Radio, json!({"startup_memory":1})),
            (
                K::Control,
                json!({"enabled":true,"backend":"hamlib","hamlib":{"model":"RIG_MODEL_DUMMY"}}),
            ),
            (
                K::Limits,
                json!({"rx_ranges_mhz":[[144,148]],"tx_ranges_mhz":[[144,148]],"modes":["FM"]}),
            ),
            (
                K::Memories,
                json!({"entries":[{"frequency":146.52,"offset":{"enabled":false,"direction":"minus","frequency":0.6},"mode":"FM","tx_ctcss":{"enabled":false,"frequency":100},"rx_ctcss":{"enabled":false,"frequency":100}}]}),
            ),
        ];
        station_with(&rows, |p| {
            let prepared = api.prepare_station(p, "782").unwrap();
            if let Some(old) = previous.as_ref() {
                assert!(prepared.equivalent(old).unwrap());
            }
            previous = Some(prepared);
        });
    }
}

#[test]
fn r2_disabled_parallel_ctcss_preserves_the_dormant_frequency_and_prepares_without_a_decoder() {
    let api = RadioPreparationApi::open().unwrap();
    station_with(
        &[(
            K::RxSignaling,
            json!({"ctcss":{"enabled":false,"source":"parallel","pin":[10],"frequency_hz":123.4}}),
        )],
        |p| {
            let tone = &p.signals.rx.ctcss;
            assert!(!tone.enabled);
            assert_eq!(tone.tones.frequency, 1234);
            let station = api
                .prepare_station(p, "6")
                .expect("disabled physical qualifier with a valid dormant setting");
            station
                .visit(|s| {
                    // SAFETY: the matched provider lends this radio record only for this visit.
                    let r = unsafe { s.radio.as_ref() }.unwrap();
                    assert_eq!(r.receive.ctcss_enabled, 0);
                    assert_eq!(r.qualification.subaudible_source, 0);
                })
                .unwrap();
            assert_eq!(tone.tones.ordered, [1000]);
        },
    );
}

#[test]
fn r2_parallel_decoder_length_fallback_keeps_the_native_default_table() {
    let api = RadioPreparationApi::open().unwrap();
    let rx = json!({"ctcss":{"enabled":true,"source":"parallel","pin":[10],"frequency_hz":123.4,"tones_hz":[100,123]}});
    let tx = json!({"ctcss":{"enabled":true,"source":"dsp","tones_hz":[88.5]}});
    station_with(&[(K::RxSignaling, rx), (K::TxSignaling, tx)], |p| {
        assert_eq!(p.signals.rx.ctcss.tones.frequency, 1234);
        let station = api
            .prepare_station(p, "6")
            .expect("parallel input and one effective map entry");
        station
            .visit(|s| {
                // SAFETY: the matched provider lends this radio record only for this visit.
                let r = unsafe { s.radio.as_ref() }.unwrap();
                assert_eq!(r.receive.ctcss_enabled, 0);
                assert_eq!(r.qualification.subaudible_source, 4);
            })
            .unwrap();
        assert_eq!(p.signals.rx.ctcss.tones.ordered, [1000]);
        assert_eq!(p.signals.tx.ctcss.tones.ordered, [885]);
    });
}
