use super::*;
use crate::fixture::station::{self, LOCAL, SECOND_LOCAL};
use crate::link::ring::InboundPolicy;
use crate::management::PreparedConfiguration;
use rpt_advanced_core::config::resources::candidate::ResourceKind as Kind;
use serde_json::{Value, json};
#[path = "host_command_review_tests.rs"]
pub(crate) mod commands;
#[path = "egress_load_tests.rs"]
mod egress_load;
#[path = "native_maintenance_host_tests.rs"]
mod native_maintenance;
#[path = "host_phone_tests.rs"]
mod phone;
#[path = "host_remote_tests.rs"]
mod remote;
#[path = "site_host_tests.rs"]
mod site;
use crate::{
    abi,
    link::session::tests::{Input, peer, peer_with_async},
};
use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::dtmf::{DigitEvent, DigitOperation},
};
use std::{ffi::c_void, path::Path, ptr, sync::Barrier, time::Duration};

unsafe extern "C" fn reaper() {}
fn media() -> NativeMediaPreparer {
    unsafe {
        NativeMediaPreparer::from_descriptors(
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            1000,
            (reaper, reaper),
        )
    }
    .unwrap()
}
fn service_table() -> abi::rptadv_host_services_v11 {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.asl_provider = &crate::fixture::asl::PROVIDER;
    table
}
fn services() -> HostServices {
    unsafe { HostServices::open(Box::leak(Box::new(service_table()))) }.unwrap()
}
fn clock() -> RuntimeClock {
    RuntimeClock {
        now_ms: 10,
        wall_seconds: 0,
        wall_subsecond_ms: 0,
        civil: Some((
            rpt_advanced_core::schedule::CivilTime::new(
                2030,
                1,
                1,
                rpt_advanced_core::schedule::Weekday::Tuesday,
                9,
                7,
            )
            .unwrap(),
            3,
        )),
    }
}
fn stations(
    ports: &[(&str, &str, &str, Value)],
    extra: &[(&str, Kind, &str, Value)],
) -> PreparedConfiguration {
    let mut members = Vec::new();
    let mut metadata = Vec::new();
    for (local, wire, device, fields) in ports {
        metadata.push((*local, *wire, fields.clone()));
        for (kind, base) in [
            (
                Kind::Interface,
                json!({"audio":{"device_identifier":device}}),
            ),
            (Kind::Radio, json!({"duplex_mode":"full"})),
            (
                Kind::Allstarlink,
                json!({"enabled":true,"node_number":wire.parse::<u64>().unwrap(),
                "static_nodes":{"3":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"},"2000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"}}}),
            ),
        ] {
            let mut fields = base.as_object().unwrap().clone();
            if let Some((_, _, _, overrides)) = extra
                .iter()
                .find(|(owner, k, _, _)| owner == local && *k == kind)
            {
                fields.extend(overrides.as_object().unwrap().clone());
            }
            members.push((*local, kind, "", Value::Object(fields)));
        }
    }
    members.extend(
        extra
            .iter()
            .filter(|(owner, k, _, _)| {
                !ports.iter().any(|(id, _, _, _)| id == owner)
                    || !matches!(k, Kind::Interface | Kind::Radio | Kind::Allstarlink)
            })
            .cloned(),
    );
    station::prepare(&metadata, &members)
}

fn configuration(device: &str, extra: &[(Kind, &str, Value)]) -> PreparedConfiguration {
    let resources: Vec<_> = extra
        .iter()
        .map(|(k, name, v)| (LOCAL, *k, *name, v.clone()))
        .collect();
    stations(&[(LOCAL, "1000", device, json!({}))], &resources)
}
fn echo_station(config: PreparedConfiguration, local: &str, media: NativeMediaPreparer) -> Host {
    use crate::management::native::AccountCredentials;
    use rpt_advanced_core::config::resources::profiles::credentials::CredentialSlot;
    let mut table = crate::link::session::tests::peer_table(true, true, true);
    table.asl_provider = &crate::fixture::asl::PROVIDER;
    table.context = ptr::without_provenance_mut(1);
    table.managed_echo_provider = &crate::fixture::echo::PROVIDER;
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let scope = rpt_advanced_core::config::resources::candidate::ResourceScope::Port(
        local.parse().unwrap(),
    );
    let owner = config
        .runtime()
        .registry
        .binding(scope, Kind::Echolink)
        .unwrap()
        .id;
    let version = "dddddddd-dddd-4ddd-8ddd-dddddddddddd".parse().unwrap();
    let credentials =
        AccountCredentials::fixture(&[(owner, CredentialSlot::Directory, version, b"fixture")]);
    Host::fixture_start_with_credentials(
        config,
        media,
        services,
        Instant::now(),
        clock(),
        None,
        credentials,
    )
    .unwrap()
}

fn echo_profile() -> Value {
    json!({"enabled":true,"callsign":"N0CALL-R","node_number":123456,"announcement":"callsign"})
}

fn echo_host() -> Host {
    echo_station(
        configuration(
            "usb",
            &[
                (Kind::Echolink, "", echo_profile()),
                (
                    Kind::TelemetrySettings,
                    "",
                    json!({"morse":{"timing":{"speed_wpm":100}}}),
                ),
            ],
        ),
        LOCAL,
        media(),
    )
}

fn asl_route(host: &Host, remote: &str, source: Option<&str>) -> Result<String, RuntimeError> {
    let selected = host.asl_account(LOCAL.parse().unwrap())?;
    let source = source.map(|s| s.parse().unwrap());
    let route = selected
        .directory
        .resolve(host.services, remote, source)
        .map_err(|_| RuntimeError::Rejected)?;
    route
        .destination(&selected.directory, remote, source)
        .map(str::to_owned)
        .ok_or(RuntimeError::Rejected)
}

fn play_message(host: &mut Host, name: &str) {
    let action = rpt_advanced_core::command_language::prepare_runtime_action(
        &json!({"action":"message","message":name,"destination":"local"}),
    )
    .unwrap();
    let (local, effect) = host
        .runtime
        .management_action(LOCAL.parse().unwrap(), "operator", &action, clock())
        .unwrap();
    host.immediate(&local, effect, clock()).unwrap();
}

fn mapped_host(mappings: Value) -> Host {
    Host::fixture_start(
        configuration("usb", &[(Kind::Command, "", json!({"mappings":mappings}))]),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}
pub(super) fn host() -> Host {
    Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}

#[test]
fn mapped_dtmf_macro_advances_without_civil_scheduling_and_reload_cancels_due_work() {
    use rpt_advanced_core::command_language::MacroOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let config = || {
        configuration(
            "usb",
            &[
                (
                    Kind::Command,
                    "",
                    json!({"mappings":{"status":{"command":"*95","protected":false,"action":"call_macro","macro":"report"}}}),
                ),
                (
                    Kind::Macro,
                    "report",
                    json!({"actions":[{"action":"link_status"},{"action":"pause","timing":{"duration_ms":2000}},{"action":"network_status"}]}),
                ),
            ],
        )
    };
    let mut host =
        Host::fixture_start(config(), media(), services(), Instant::now(), clock()).unwrap();
    let now = RuntimeClock {
        civil: None,
        ..clock()
    };
    for digit in "*95".chars() {
        assert!(
            host.runtime
                .digit(LOCAL, DigitEvent::Digit { digit, now_ms: 10 })
                .is_none()
        );
    }
    let Some(DigitOutcome::Mapped(input)) = host.runtime.digit(
        LOCAL,
        DigitEvent::Digit {
            digit: '#',
            now_ms: 10,
        },
    ) else {
        panic!("completed mapping")
    };
    let LinkEffect::CommandQueued(response) =
        host.runtime.dispatch_mapping(LOCAL, input, now).unwrap()
    else {
        panic!("admitted sequence")
    };
    let Some((effect @ LinkEffect::Telemetry(LinkAction::Status, None), receipt)) =
        host.runtime.advance_commands(LOCAL, now).unwrap()
    else {
        panic!("first status action")
    };
    host.immediate(LOCAL, effect, now).unwrap();
    receipt.settle(true);
    assert!(host.runtime.advance_commands(LOCAL, now).unwrap().is_none());
    assert!(
        host.runtime
            .advance_commands(
                LOCAL,
                RuntimeClock {
                    now_ms: 2009,
                    ..now
                }
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(response.outcome(), None);
    host.fixture_reload(
        config(),
        RuntimeClock {
            now_ms: 2010,
            ..now
        },
    )
    .unwrap();
    assert_eq!(response.outcome(), Some(MacroOutcome::Cancelled));
    assert!(
        host.runtime
            .advance_commands(
                LOCAL,
                RuntimeClock {
                    now_ms: 2011,
                    ..now
                }
            )
            .unwrap()
            .is_none()
    );
    assert!(host.fixture_stop(2020));
}

#[test]
fn echo_alias_routing_precedes_allstar_directory_lookup() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = echo_host();
    for (alias, destination) in [
        ("3123456", "EchoLink/123456"),
        ("31002775", "EchoLink/1002775"),
        ("34294967295", "EchoLink/4294967295"),
    ] {
        assert_eq!(host.lookup(LOCAL, alias, None).unwrap(), destination);
        assert_eq!(
            host.lookup(LOCAL, alias, Some("127.0.0.1")),
            Err(RuntimeError::Rejected),
            "a directory address is not Echo QSO admission proof"
        );
    }
    for alias in ["3000000", "34294967296", "300012x"] {
        assert_eq!(host.lookup(LOCAL, alias, None), Err(RuntimeError::Rejected));
    }
    assert_eq!(
        asl_route(&host, "2000", None).unwrap(),
        "radio@127.0.0.1:4569/2000"
    );
    assert_eq!(
        host.lookup("missing", "3123456", None),
        Err(RuntimeError::MissingNode)
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn incoming_echo_uses_its_validated_qso_and_protocol_qualified_link_key() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = echo_host();
    let (echo, state) = crate::link::session::tests::source_peer_on(LOCAL);
    state.lock().unwrap().continuous_async = true;
    assert_eq!(echo.identity().node(), Some("2000"));
    host.incoming_peer(echo, "192.0.2.2", clock()).unwrap();
    pump_until(&mut host, |host| {
        named_peer_ready(host, "3002000")
            && host
                .runtime
                .status(clock().now_ms)
                .iter()
                .all(|(_, status)| status.retiring.is_none())
    });
    assert!(host.peers.iter().any(|peer| peer.remote == "3002000"));
    let bound = state.lock().unwrap().bound_radio.clone();
    assert_eq!(bound.as_deref(), Some(LOCAL));
    let (duplicate, rejected) = crate::link::session::tests::source_peer_on(LOCAL);
    assert!(host.incoming_peer(duplicate, "192.0.2.2", clock()).is_err());
    assert_eq!(rejected.lock().unwrap().drops, 1);
    assert!(rejected.lock().unwrap().bound_radio.is_none());
    // A numeric collision is a different protocol, but IAX still needs its own
    // source authentication before the controller's common admission policy.
    let (iax, rejected) = peer(8000);
    assert!(host.incoming_peer(iax, "192.0.2.2", clock()).is_err());
    assert_eq!(rejected.lock().unwrap().drops, 1);
    let (iax, _) = peer(8000);
    assert!(asl_route(&host, "2000", Some("192.0.2.2")).is_err());
    assert!(asl_route(&host, "2000", Some("127.0.0.1")).is_ok());
    host.runtime
        .authorize_incoming(LOCAL, "2000", true)
        .unwrap();
    host.accept_verified_peer(LOCAL, "2000", iax, clock())
        .unwrap();
    assert_eq!(host.peers.len(), 2);
    assert!(host.peers.iter().any(|peer| peer.remote == "2000"));
    let origin = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .text_origin();
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin,
            source: "2000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("literal crossover text"),
        },
    );
    pump_until(&mut host, |_| {
        state.lock().unwrap().texts == [b"2000>literal crossover text".to_vec()]
    });
    assert!(host.fixture_stop(20));
    assert_eq!(state.lock().unwrap().drops, 1);
    assert_eq!(
        state.lock().unwrap().texts,
        [b"2000>literal crossover text".to_vec()]
    );
}

#[test]
fn echo_disconnect_announces_and_releases_the_verified_peer() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = echo_host();
    let (echo, state) = crate::link::session::tests::source_peer_on(LOCAL);
    state.lock().unwrap().continuous_async = true;
    host.incoming_peer(echo, "192.0.2.2", clock()).unwrap();
    pump_until(&mut host, |host| {
        named_peer_ready(host, "3002000")
            && host
                .runtime
                .status(clock().now_ms)
                .iter()
                .all(|(_, status)| status.retiring.is_none())
    });
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    assert!(render_queued_telemetry(&mut host, &mut owners));
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::Disconnect, "3002000"),
            clock(),
            false,
        )
        .unwrap();
    host.immediate(LOCAL, effect, clock()).unwrap();
    assert!(render_queued_telemetry(&mut host, &mut owners));
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
    assert!(host.peers.is_empty());
    assert!(host.status_text(LOCAL).unwrap().contains("no active links"));
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.fixture_stop(20));
}

#[test]
fn echo_account_status_is_visible_without_active_links() {
    unsafe extern "C" fn failed_status(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: abi::rptadv_text_sink_v1,
        _: *mut c_void,
    ) -> i32 {
        -1
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let text = host.status_text(LOCAL).unwrap();
    assert!(text.contains("no active links"), "{text}");
    assert!(text.contains("echo-account fixture-state=online"), "{text}");
    let mut table = service_table();
    table.echo_account_status = Some(failed_status);
    host.services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let text = host.status_text(LOCAL).unwrap();
    assert!(text.contains("no active links"), "{text}");
    assert!(text.contains("echo-account status-unavailable"), "{text}");
    table.echo_account_status = None;
    host.services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let text = host.status_text(LOCAL).unwrap();
    assert!(text.contains("no active links"), "{text}");
    assert!(!text.contains("echo-account"), "{text}");
    assert!(host.fixture_stop(20));
}

#[test]
fn echo_status_reports_provider_metadata_from_the_peer_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = echo_host();
    let (echo, state) = crate::link::session::tests::source_peer_on(LOCAL);
    let mut observation = abi::rptadv_peer_observation_v1 {
        struct_size: std::mem::size_of::<abi::rptadv_peer_observation_v1>() as u32,
        abi_version: abi::RPTADV_PEER_OBSERVATION_ABI_VERSION,
        echo_valid: 1,
        echo_state: 1,
        echo_codec: 1,
        echo_transport: 1,
        echo_raw_receiving: 1,
        echo_selected: 1,
        echo_last_valid_age_ms: 42,
        echo_heartbeat_due_ms: u64::MAX,
        echo_liveness_due_ms: u64::MAX,
        echo_ingress_bytes: 128,
        ingress_capacity_packets: 64,
        // SAFETY: the observation is a scalar-only ABI record with optional zero counters.
        ..unsafe { std::mem::zeroed() }
    };
    observation.echo_cause_detail[..6].copy_from_slice(b"Active");
    {
        let mut state = state.lock().unwrap();
        state.continuous_async = true;
        state.echo_observation = Some(observation);
    }
    host.incoming_peer(echo, "192.0.2.2", clock()).unwrap();
    pump_until(&mut host, |host| {
        host.status_text(LOCAL)
            .unwrap()
            .contains("echo state=connected")
    });
    let status = host.status_text(LOCAL).unwrap();
    assert!(status.contains("cause=none detail=\"Active\""), "{status}");
    assert!(
        status.contains("codec=speex transport=proxy raw-receiving=1 selected=1"),
        "{status}"
    );
    assert!(status.contains("last-valid-age=42ms"), "{status}");
    assert!(status.contains("heartbeat-in=inactive"), "{status}");
    assert!(status.contains("ingress-retained-bytes=128"), "{status}");
    assert!(state.lock().unwrap().observe_calls > 0);
    assert!(host.fixture_stop(20));
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn echo_ring_policy_survives_prepared_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = echo_host();
    let (io, _) = crate::link::session::tests::source_peer_on(LOCAL);
    let (_, outbound) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
    let (session, control) = PeerSession::prepare(io, outbound, "1000", "2000", true).unwrap();
    host.peers.push(PeerOwner {
        superseded: false,
        local: LOCAL.into(),
        remote: "2000".into(),
        mode: Mode::TRANSCEIVE,
        group: None,
        announced: false,
        control,
    });
    let candidate = prepared(
        LOCAL,
        host.runtime.settings(LOCAL).unwrap(),
        StationPeers {
            links: &host.peers,
            calls: Some(&host.phone_calls),
            applications: &[],
        },
        media_worker(&mut host.media_workers, LOCAL).unwrap(),
        &mut host.text,
        host.services,
        StationAudio::Modular(host.radio_preparation.active_plan(LOCAL).unwrap()),
    )
    .unwrap();
    assert_eq!(candidate.control.redirects.len(), 1);
    let PeerCommand::Redirect {
        inbound,
        dtmf_muting: true,
        ..
    } = &candidate.control.redirects[0].1
    else {
        panic!("missing replacement ring");
    };
    assert!(matches!(inbound.observer().policy(), InboundPolicy::Echo));
    session.stop();
    drop(candidate);
    assert!(host.fixture_stop(20));
}

#[test]
fn text_forwarding_host_uses_current_connection_and_never_reflects_to_ingress() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let egress = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let (io, audio_only) = crate::link::session::tests::peer_without_text(48000);
    host.runtime.incoming(LOCAL, "6000", true).unwrap();
    host.attach_peer(LOCAL, "6000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    ingress
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"M 4000 0 literal human text"));
    pump_until(&mut host, |_| {
        egress
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"M 4000 0 literal human text")
    });
    assert!(
        !ingress
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text.starts_with(b"M "))
    );
    assert_eq!(
        egress
            .lock()
            .unwrap()
            .texts
            .iter()
            .filter(|text| text.starts_with(b"M "))
            .count(),
        1
    );
    assert!(
        !audio_only
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text.starts_with(b"M ")),
        "audio-only peers must not inherit globally available text callbacks"
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn text_lifecycle_directed_message_never_plays_at_another_local_station() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let config = stations(
        &[
            (LOCAL, "1000", "usb", json!({})),
            (SECOND_LOCAL, "1001", "usb2", json!({})),
        ],
        &[
            (SECOND_LOCAL, Kind::Echolink, "", echo_profile()),
            (
                station::SITE,
                Kind::TelemetrySettings,
                "",
                json!({"speech":{"voice":"voice.onnx","level_db":-6}}),
            ),
        ],
    );
    let mut host = echo_station(
        config,
        SECOND_LOCAL,
        crate::media::text::tests::preparer(Arc::default()),
    );
    let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let egress = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    host.runtime.incoming(SECOND_LOCAL, "5000", true).unwrap();
    let (io, other_station) = peer(48000);
    host.attach_peer(SECOND_LOCAL, "5000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    // A different station may be earlier in both ingress and forwarding scans.
    host.peers.rotate_right(1);
    let origin = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .text_origin();
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin: origin.clone(),
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Node("1001".into()),
            body: Arc::from("B for the other station"),
        },
    );
    assert_eq!(host.text.observation().admitted, 0);
    for peer in [&ingress, &egress, &other_station] {
        assert!(
            !peer
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text.starts_with(b"M "))
        );
    }
    for (destination, expected_admitted) in
        [("3123456", 0), ("N0CALL-R", 0), ("9000", 0), ("1000", 1)]
    {
        host.human_text(
            LOCAL,
            rpt_advanced_core::link::HumanMessage {
                origin: origin.clone(),
                source: "4000".into(),
                represented_source: None,
                audience: rpt_advanced_core::link::TextAudience::Node(destination.into()),
                body: Arc::from("B directed speech"),
            },
        );
        assert_eq!(host.text.observation().admitted, expected_admitted);
    }
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin: origin.clone(),
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("B for this station and its peers"),
        },
    );
    assert_eq!(host.text.observation().admitted, 2);
    host.peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .stop();
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin,
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("B from an ended connection"),
        },
    );
    assert_eq!(host.text.observation().admitted, 2);
    assert!(host.fixture_stop(20));
}

#[test]
fn replacing_a_peer_quiesces_only_the_retired_connection() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mapped_host(
        json!({"check":{"command":"6","protected":false,"action":"rx_mute_toggle","target":"port"}}),
    );
    attach(&mut host, "4000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let old = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let retired = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .connection_generation();
    // Logical removal and physical detach run as separate executor operations.
    // An incoming replacement may be admitted between those operations.
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::Disconnect, "2000"),
            clock(),
            false,
        )
        .unwrap();
    assert!(matches!(effect, LinkEffect::Detach(_)));
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    let (io, replacement) = peer(48000);
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    let current = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000" && !peer.superseded)
        .unwrap()
        .control
        .connection_generation();
    assert_ne!(retired, current);
    let origin = host
        .peers
        .iter()
        .find(|peer| peer.remote == "4000")
        .unwrap()
        .control
        .text_origin();
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin,
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("one copy per connection"),
        },
    );
    pump_until(&mut host, |_| {
        old.lock().unwrap().drops == 1
            && replacement
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text == b"M 4000 0 one copy per connection")
    });
    let count = |state: &Mutex<crate::link::session::tests::State>| {
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .filter(|text| text.starts_with(b"M "))
            .count()
    };
    assert_eq!(count(&old), 0);
    assert_eq!(count(&replacement), 1);
    let surviving = host
        .peers
        .iter()
        .find(|peer| peer.control.connection_generation() == current)
        .unwrap()
        .control
        .reply_target();
    host.detach_connection(LOCAL, "2000", Some(retired), clock().now_ms);
    assert!(!surviving.ended());
    let links = host.runtime.node(LOCAL).unwrap().links();
    assert!(
        links
            .peer_digit("2000", '6', clock().now_ms, surviving.clone())
            .is_none()
    );
    let Some(DigitOutcome::Mapped(input)) =
        links.peer_digit("2000", '#', clock().now_ms, surviving)
    else {
        panic!("live source mapping")
    };
    assert!(host.runtime.dispatch_mapping(LOCAL, input, clock()).is_ok());
    assert!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .radio_receive_status()
            .muted
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn registered_control_digit_and_logical_disconnect_choose_current_transport() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let old = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    host.runtime
        .command(
            LOCAL,
            operation(LinkAction::Disconnect, "2000"),
            clock(),
            false,
        )
        .unwrap();
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    let (io, current) = peer(48000);
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    let logical = host
        .peers
        .iter()
        .find(|p| !p.superseded && p.remote == "2000")
        .unwrap()
        .control
        .reply_target();
    host.immediate(
        LOCAL,
        LinkEffect::ForwardDigits(vec![("2000".into(), "D 2000 1000 1 5".into())]),
        clock(),
    )
    .unwrap();
    pump_until(&mut host, |_| {
        current
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|t| t == b"D 2000 1000 1 5")
    });
    assert!(
        !old.lock()
            .unwrap()
            .texts
            .iter()
            .any(|t| t == b"D 2000 1000 1 5")
    );
    host.detach(LOCAL, "2000", clock().now_ms);
    assert!(logical.ended());
    assert!(
        host.peers
            .iter()
            .all(|p| p.remote != "2000" || p.superseded)
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn text_forwarding_backpressure_does_not_block_local_speech() {
    struct Resume(Arc<Barrier>);
    impl Drop for Resume {
        fn drop(&mut self) {
            self.0.wait();
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let egress = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let origin = host.peers[0].control.text_origin();
    // Park the owner before filling: the text callback locks after the queue pop.
    let pause = Arc::new(Barrier::new(2));
    egress.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    let control = &mut host.peers[1].control;
    while control
        .send(PeerCommand::Text {
            text: CString::new("queue pressure").unwrap(),
            advisory: false,
        })
        .is_ok()
    {}
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin,
            source: "2000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("B still local"),
        },
    );
    assert_eq!(host.text.observation().forwarding_rejected, 1);
    assert_eq!(host.text.observation().admitted, 1);
    drop(resume);
    assert!(host.fixture_stop(20));
}

#[test]
fn local_radio_digits_reach_the_same_control_dispatch_as_peer_digits() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mapped_host(
        json!({"test":{"command":"*AB","protected":false,"action":"rx_mute_toggle","target":"port"}}),
    );
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    for (row, column) in [
        (941.0, 1209.0),
        (697.0, 1633.0),
        (770.0, 1633.0),
        (941.0, 1477.0),
    ] {
        for block in 0..5 {
            let mut pcm = (0..612)
                .map(|offset| {
                    let time = (block * 612 + offset) as f32 / 48000.0;
                    if block < 2 {
                        0.031
                            * ((std::f32::consts::TAU * row * time).sin()
                                + (std::f32::consts::TAU * column * time).sin())
                    } else {
                        0.0
                    }
                })
                .collect::<Vec<_>>();
            owners
                .0
                .acquire()
                .unwrap()
                .state()
                .process(true, &mut pcm, 1000);
        }
    }
    host.pump(clock()).unwrap();
    assert!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .radio_receive_status()
            .muted,
        "decoded RF symbols must invoke the managed port mapping"
    );
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.fixture_stop(20));
}

#[test]
fn text_lifecycle_host_admits_local_speech_and_fences_same_device_reload() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let config = || {
        configuration(
            "usb",
            &[(
                Kind::TelemetrySettings,
                "",
                json!({"speech":{"voice":"voice.onnx","level_db":-6}}),
            )],
        )
    };
    let speech = Arc::<crate::media::text::tests::SpeechFixture>::default();
    let mut host = Host::fixture_start(
        config(),
        crate::media::text::tests::preparer(speech.clone()),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let egress = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let old = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .text_origin();
    ingress
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"M 4000 0 B literal speech"));
    pump_until(&mut host, |host| {
        host.text.observation().admitted == 1
            && !speech.opened.lock().unwrap().is_empty()
            && egress
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text == b"M 4000 0 B literal speech")
    });
    assert_eq!(
        speech.opened.lock().unwrap()[0],
        "Node 4,0,0,0 says: B literal speech"
    );
    ingress
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"M 4000 0 A stalled producer"));
    pump_until(&mut host, |host| {
        let observation = host.text.observation();
        observation.admitted == 2 && observation.active == 1 && observation.queued == 0
    });
    host.fixture_reload(config(), clock()).unwrap();
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    let current = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .text_origin();
    assert_ne!(old.station_generation, current.station_generation);
    assert_eq!(old.connection_generation, current.connection_generation);
    // A delayed event from the retired audio generation is inert on both paths.
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin: old,
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("B stale speech"),
        },
    );
    assert_eq!(host.text.observation().admitted, 0);
    assert!(
        !egress
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"M 4000 0 B stale speech")
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn text_lifecycle_speech_failure_keeps_forwarding_and_logs_no_human_body() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::fixture_start(
        configuration(
            "usb",
            &[(
                Kind::TelemetrySettings,
                "",
                json!({"speech":{"voice":"voice.onnx","level_db":-6}}),
            )],
        ),
        crate::media::text::tests::preparer(Arc::default()),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let egress = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    crate::fixture::ERROR_LOGS.with_borrow_mut(Vec::clear);
    ingress
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"M 4000 0 F failure sensitive human body"));
    pump_until(&mut host, |host| {
        host.text.observation().failures == 1
            && egress
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text == b"M 4000 0 F failure sensitive human body")
    });
    host.pump(clock()).unwrap();
    let logs = crate::fixture::ERROR_LOGS.with_borrow(|logs| logs.clone());
    assert_eq!(logs.len(), 1);
    assert!(logs[0].1.contains("peer=2000"));
    assert!(logs[0].1.contains("ProcessFailed"));
    assert!(!logs[0].1.contains("sensitive human body"));
    assert!(!logs[0].1.contains("voice.onnx"));
    let origin = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .unwrap()
        .control
        .text_origin();
    host.human_text(
        LOCAL,
        rpt_advanced_core::link::HumanMessage {
            origin,
            source: "4000".into(),
            represented_source: None,
            audience: rpt_advanced_core::link::TextAudience::Broadcast,
            body: Arc::from("F failure second body"),
        },
    );
    pump_until(&mut host, |host| host.text.observation().failures == 2);
    host.report_text_failures(clock().now_ms);
    assert_eq!(crate::fixture::ERROR_LOGS.with_borrow(Vec::len), 1);
    assert!(host.peers.iter().all(|peer| !peer.control.ended()));
    assert!(host.fixture_stop(20));
}

#[test]
fn text_lifecycle_host_stream_reaches_local_callback_before_telemetry_is_ready() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::fixture_start(
        configuration(
            "usb",
            &[(
                Kind::TelemetrySettings,
                "",
                json!({"speech":{"voice":"voice.onnx","level_db":-6}}),
            )],
        ),
        crate::media::text::tests::preparer(Arc::default()),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let ingress = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    ingress
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"M 4000 0 B immediate incoming speech"));
    pump_until(&mut host, |host| host.text.observation().admitted == 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut heard = false;
    while !heard && Instant::now() < deadline {
        let mut pcm = [0.0; 16];
        let mut generation = owners.1.acquire().unwrap();
        let transmit = generation.state();
        transmit
            .adapter
            .process(
                &mut transmit.controller,
                false,
                &mut pcm,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        heard = pcm.contains(&0.5);
        std::thread::yield_now();
    }
    assert!(
        heard,
        "incoming text must use the callback's local bus, not polite telemetry"
    );
    assert!(
        !ingress
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text.starts_with(b"M "))
    );
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.fixture_stop(20));
}

#[test]
fn remote_selection_precedes_the_next_digit_in_one_host_batch() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for asynchronous in [false, true] {
        for suffix in ["5", "#5#"] {
            let mut host = mapped_host(
                json!({"mute":{"command":"5","protected":false,"action":"rx_mute_toggle","target":"port"}}),
            );
            host.runtime.incoming(LOCAL, "2000", true).unwrap();
            let (io, state) = peer_with_async(48000, asynchronous);
            state.lock().unwrap().continuous_async = true;
            host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
                .unwrap();
            pump_until(&mut host, |host| {
                named_peer_ready(host, "2000") && host.runtime.status(10)[0].1.retiring.is_none()
            });
            let _other = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
            // The selected account resolves the requested node before the following symbol.
            state
                .lock()
                .unwrap()
                .input
                .extend(format!("*43#{suffix}").bytes().map(Input::Digit));
            let mut outcomes = Vec::new();
            for _ in 0..100 {
                host.pump(clock()).unwrap();
                outcomes.extend(host.take_operations());
                std::thread::sleep(Duration::from_millis(1));
            }
            if suffix == "5" {
                assert!(
                    matches!(outcomes.as_slice(), [(_, DigitOutcome::Operation(operation))]
                    if operation.command.node == "3" && operation.digit == Some('5')),
                    "{outcomes:?}"
                );
            } else {
                assert!(
                    host.runtime
                        .node(LOCAL)
                        .unwrap()
                        .radio_receive_status()
                        .muted
                );
                assert!(outcomes.is_empty());
            }
            assert!(host.fixture_stop(20));
        }
    }
}

#[test]
fn remote_digits_use_shared_policy_for_polling_and_async_hosts() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for asynchronous in [false, true] {
        let mut host = mapped_host(
            json!({"mute":{"command":"5","protected":false,"action":"rx_mute_toggle","target":"port"}}),
        );
        host.runtime.incoming(LOCAL, "2000", true).unwrap();
        let (io, state) = peer_with_async(48000, asynchronous);
        state.lock().unwrap().continuous_async = true;
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
            .unwrap();
        pump_until(&mut host, |host| {
            named_peer_ready(host, "2000") && host.runtime.status(10)[0].1.retiring.is_none()
        });
        let other = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
        state.lock().unwrap().input.extend([
            Input::Text(b"D 3000 4000 11 9"),
            Input::Text(b"D 1000 4000 12 5"),
            Input::Text(b"D 1000 4000 13 #"),
        ]);
        pump_until(&mut host, |host| {
            host.runtime
                .node(LOCAL)
                .unwrap()
                .radio_receive_status()
                .muted
                && other
                    .lock()
                    .unwrap()
                    .texts
                    .iter()
                    .any(|text| text == b"D 3000 4000 11 9")
        });
        assert!(
            !state
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text.starts_with(b"D "))
        );
        assert!(other.lock().unwrap().digits.is_empty());
        assert!(host.fixture_stop(20));
    }
}

#[test]
fn same_device_reload_refreshes_local_ring_without_reopening_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let calls = Mutex::new(native_maintenance::Calls::default());
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        native_maintenance::tracked_services(&calls),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(host.status_text(LOCAL).unwrap().contains("reserve=0ms"));
    host.fixture_reload(
        configuration(
            "usb",
            &[
                (
                    Kind::RxSignaling,
                    "",
                    json!({"timing":{"squelch_delay_ms":20}}),
                ),
                (
                    Kind::Radio,
                    "",
                    json!({"timing":{"status_snapshot_interval_ms":1}}),
                ),
            ],
        ),
        clock(),
    )
    .unwrap();
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    let status = host.status_text(LOCAL).unwrap();
    assert!(
        status.contains("reserve=20ms target=20ms configured-delay=20ms"),
        "{status}"
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops,
        "same-device settings must preserve the active hardware stream"
    );
    assert!(host.fixture_stop(10));
}

#[test]
fn local_ring_reload_retains_update_until_worker_can_accept_it() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let initial = host.runtime.status(10)[0].1.active.unwrap();
    {
        let mut lease = host.leases[0].1.lock().unwrap();
        assert!(
            lease
                .worker
                .as_mut()
                .unwrap()
                .update_local_ring(initial, LocalRingUpdate::prepare(0).unwrap())
                .is_ok()
        );
    }
    host.fixture_reload(
        configuration(
            "usb",
            &[(
                Kind::RxSignaling,
                "",
                json!({"timing":{"squelch_delay_ms":20}}),
            )],
        ),
        clock(),
    )
    .unwrap();
    let active = host.runtime.status(10)[0].1.active.unwrap();
    let control = host.runtime.adapter_control_mut(LOCAL, active).unwrap();
    assert!(control.awaiting_local);
    assert!(control.local_update.is_some());

    assert!(host.leases[0].1.lock().unwrap().quiesce());
    host.pump(clock()).unwrap();
    let control = host.runtime.adapter_control_mut(LOCAL, active).unwrap();
    assert!(control.awaiting_local);
    assert!(control.local_update.is_some());
    assert!(host.fixture_stop(20));
}

#[test]
fn failed_local_ring_reload_preserves_the_active_configuration() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let generation = host.runtime.status(10)[0].1.active;
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    // The fixed local ring holds 300 ms, so this candidate cannot be prepared.
    assert!(
        host.fixture_reload(
            configuration(
                "usb",
                &[(
                    Kind::RxSignaling,
                    "",
                    json!({"timing":{"squelch_delay_ms":301}})
                )]
            ),
            clock()
        )
        .is_err()
    );
    assert_eq!(host.runtime.status(10)[0].1.active, generation);
    assert!(
        host.status_text(LOCAL)
            .unwrap()
            .contains("reserve=0ms target=0ms configured-delay=0ms")
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    assert!(host.fixture_stop(10));
}

#[test]
fn egress_admission_host_reservation_failure_preserves_running_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let generation = host.runtime.status(10)[0].1.active;
    let mut probe = Vec::<PeerOwner>::new();
    probe.try_reserve(1).unwrap();
    let bytes = probe.capacity() * std::mem::size_of::<PeerOwner>();
    let (io, state) = peer(48000);
    let result = crate::fixture::fail_allocation(bytes, || {
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
    });
    assert_eq!(result, Err(RuntimeError::Preparation));
    assert_eq!(host.runtime.status(10)[0].1.active, generation);
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.fixture_stop(20));
}

#[test]
fn parrot_live_state_effect_needs_no_external_host_action() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    assert!(
        host.immediate(LOCAL, LinkEffect::ParrotEnabled(true), clock())
            .is_ok()
    );
}

fn pump_until(host: &mut Host, mut condition: impl FnMut(&mut Host) -> bool) {
    for _ in 0..200 {
        host.pump(clock()).unwrap();
        if condition(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("host did not reach expected state");
}
fn named_peer_ready(host: &Host, remote: &str) -> bool {
    host.peers.iter().any(|peer| {
        peer.local == LOCAL
            && peer.remote == remote
            && peer.control.owner_thread().is_some()
            && peer
                .control
                .snapshot()
                .is_ok_and(|snapshot| !snapshot.ended)
    })
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation::new(
        Command {
            action,
            node: remote.into(),
        },
        None,
    )
}
fn attach(
    host: &mut Host,
    remote: &str,
    action: LinkAction,
    mode: Mode,
) -> Arc<Mutex<crate::link::session::tests::State>> {
    let effect = host
        .runtime
        .command(LOCAL, operation(action, remote), clock(), false)
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected a new dial");
    };
    assert!(
        host.runtime
            .finish_connect(LOCAL, attempt, true, clock())
            .unwrap()
    );
    let (io, state) = peer(48000);
    host.attach_peer(LOCAL, remote, mode, io, clock()).unwrap();
    pump_until(host, |host| {
        named_peer_ready(host, remote)
            && host.peers.iter().all(|peer| {
                peer.control
                    .snapshot()
                    .is_ok_and(|snapshot| !snapshot.ended)
            })
            && host
                .runtime
                .status(clock().now_ms)
                .iter()
                .any(|(node, status)| {
                    node == LOCAL && status.active.is_some() && status.retiring.is_none()
                })
    });
    state
}

fn render_queued_telemetry(host: &mut Host, owners: &mut AudioOwners) -> bool {
    let mut heard = false;
    for block in 0..1024_u64 {
        let mut audio = [0.0; 960];
        owners
            .0
            .acquire()
            .unwrap()
            .state()
            .process(false, &mut audio, block * 20);
        let mut generation = owners.1.acquire().unwrap();
        let transmit = generation.state();
        let keyed = transmit
            .adapter
            .process(
                &mut transmit.controller,
                false,
                &mut audio,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        drop(generation);
        heard |= audio.iter().any(|sample| *sample != 0.0);
        host.pump(clock()).unwrap();
        if heard && !keyed {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn scheduled_and_manual_messages_share_streaming_and_cancel_in_both_hosts() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // The calendar provider evaluates the UTC epoch and its local civil projection together.
    let clock = || RuntimeClock {
        wall_seconds: 1_893_488_823,
        ..clock()
    };
    for device in ["native-cm119", "RadioPlus/1000"] {
        let mut host = Host::fixture_start(
            configuration(device,&[
                (Kind::TxSignaling,"",json!({"ptt":{"timing":{"hang_ms":0}}})),
                (Kind::RxSignaling,"",json!({"timing":{"kerchunk_max_ms":0}})),
                (Kind::Message,"report",json!({"content":"{ attention } { call-id }","dtmf_id":"9"})),
                (Kind::Message,"attention",json!({"tone":{"sequence":"600/5000"}})),
                (Kind::Message,"call-id",json!({"morse":{"text":"E"}})),
                (Kind::Macro,"speak",json!({"actions":[{"action":"message","message":"{ report }","destination":"local"}]})),
                (Kind::Event,"first-report",json!({"start":{"at":"every day at 09:07 in UTC","macro":"speak"}})),
                (Kind::Event,"next-report",json!({"start":{"at":"every day at 09:08 in UTC","macro":"speak"}})),
            ]),
            media(), services(), Instant::now(), clock(),
        ).unwrap();
        let lease = Arc::clone(&host.leases[0].1);
        assert!(lease.lock().unwrap().quiesce());
        let mut owners = lease.lock().unwrap().owners.take().unwrap();
        for manual in [false, true] {
            if manual {
                play_message(&mut host, "report");
            } else {
                let Some((effect, receipt)) =
                    host.runtime.advance_commands(LOCAL, clock()).unwrap()
                else {
                    panic!("due scheduled message")
                };
                host.immediate(LOCAL, effect, clock()).unwrap();
                receipt.settle(true);
            }
            let mut heard = false;
            for _ in 0..1000 {
                let mut output = [0.0; 48];
                owners
                    .1
                    .acquire()
                    .unwrap()
                    .state()
                    .controller
                    .process_audio(false, false, &[], &mut output);
                host.pump(clock()).unwrap();
                if output.iter().any(|sample| *sample != 0.0) {
                    heard = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                heard,
                "real shared producer must render {device}, manual={manual}"
            );
            let effect = host
                .runtime
                .command(
                    LOCAL,
                    operation(LinkAction::CancelMessage, ""),
                    clock(),
                    false,
                )
                .unwrap();
            host.immediate(LOCAL, effect, clock()).unwrap();
            let mut after = [0.0; 3000];
            let keyed = owners
                .1
                .acquire()
                .unwrap()
                .state()
                .controller
                .process_audio(false, false, &[], &mut after);
            assert!(
                !keyed && after.iter().all(|sample| *sample == 0.0),
                "cancel must remove unread producer PCM, {device}"
            );
            host.pump(clock()).unwrap();
            assert!(
                host.runtime
                    .advance_commands(LOCAL, clock())
                    .unwrap()
                    .is_none()
            );
        }
        let mut next = clock();
        next.civil = Some((
            rpt_advanced_core::schedule::CivilTime::new(
                2030,
                1,
                1,
                rpt_advanced_core::schedule::Weekday::Tuesday,
                9,
                8,
            )
            .unwrap(),
            0,
        ));
        next.now_ms += 57_000;
        next.wall_seconds += 57;
        assert!(
            host.runtime
                .advance_commands(LOCAL, next)
                .unwrap()
                .is_some(),
            "manual playback cannot consume the next clock occurrence"
        );
        lease.lock().unwrap().owners = Some(owners);
        assert!(host.fixture_stop(30));
    }
}

#[test]
fn composition_shared_host_pump_preserves_receive_interruption() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // Both composition hosts hand the same prepared program to this shared product.
    for (device, local) in [("native-cm119", true), ("RadioPlus/1000", false)] {
        let config = configuration(
            device,
            &[
                (
                    Kind::RxSignaling,
                    "",
                    json!({"timing":{"kerchunk_max_ms":0}}),
                ),
                (
                    Kind::Message,
                    "attention",
                    json!({"tone":{"sequence":"600/1000"}}),
                ),
                (
                    Kind::Message,
                    "notice",
                    json!({"content":"{ attention } { call-id }","dtmf_id":"1"}),
                ),
                (Kind::Message, "call-id", json!({"morse":{"text":"E"}})),
            ],
        );
        let mut host =
            Host::fixture_start(config, media(), services(), Instant::now(), clock()).unwrap();
        let lease = Arc::clone(&host.leases[0].1);
        assert!(lease.lock().unwrap().quiesce());
        let mut owners = lease.lock().unwrap().owners.take().unwrap();
        play_message(&mut host, "notice");
        let mut heard_primary = false;
        for _ in 0..1000 {
            let mut audio = [0.0; 48];
            owners
                .1
                .acquire()
                .unwrap()
                .state()
                .controller
                .process_audio(false, false, &[], &mut audio);
            host.pump(clock()).unwrap();
            if audio.iter().any(|sample| *sample != 0.0) {
                heard_primary = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(heard_primary);
        // The receive edge cancels a one-second attention segment. Its full Morse
        // replacement must finish far sooner, even after receive falls again.
        owners
            .1
            .acquire()
            .unwrap()
            .state()
            .controller
            .process_audio(local, !local, &[], &mut [0.0; 48]);
        let mut heard_morse = false;
        let mut finished = false;
        let mut audible = 0;
        for _ in 0..1000 {
            let mut audio = [0.0; 48];
            let keyed = owners
                .1
                .acquire()
                .unwrap()
                .state()
                .controller
                .process_audio(false, false, &[], &mut audio);
            audible += audio.iter().filter(|sample| **sample != 0.0).count();
            heard_morse |= audible != 0;
            host.pump(clock()).unwrap();
            if heard_morse && !keyed {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            finished && audible < 10_000,
            "primary must not resume: {device}, audible={audible}"
        );
        lease.lock().unwrap().owners = Some(owners);
        assert!(host.fixture_stop(30));
    }
}

#[test]
fn peer_attach_and_detach_queue_lifecycle_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();

    assert!(render_queued_telemetry(&mut host, &mut owners));

    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::Disconnect, "2000"),
            clock(),
            false,
        )
        .unwrap();
    host.immediate(LOCAL, effect, clock()).unwrap();
    assert!(render_queued_telemetry(&mut host, &mut owners));

    lease.lock().unwrap().owners = Some(owners);
    assert!(host.fixture_stop(30));
}

#[test]
fn peer_sessions_share_one_io_owner_outside_the_control_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    attach(&mut host, "2001", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.peers
            .iter()
            .all(|peer| peer.control.owner_thread().is_some())
    });
    let owners: Vec<_> = host
        .peers
        .iter()
        .map(|peer| peer.control.owner_thread().unwrap())
        .collect();
    assert!(owners.iter().all(|owner| *owner == owners[0]));
    assert_ne!(owners[0], std::thread::current().id());
    assert!(host.fixture_stop(30));
}

#[test]
fn blocked_peer_io_does_not_block_control_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(Arc::clone(&pause));
    let release = std::thread::spawn(move || {
        pause.wait();
        std::thread::sleep(Duration::from_millis(100));
        pause.wait();
    });

    let started = Instant::now();
    host.pump(clock()).unwrap();
    let elapsed = started.elapsed();
    release.join().unwrap();
    assert!(elapsed < Duration::from_millis(50), "pump took {elapsed:?}");
    assert!(host.fixture_stop(30));
}

#[test]
fn status_reports_cached_peer_jitter_and_ingress_metrics() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected a new dial");
    };
    assert!(
        host.runtime
            .finish_connect(LOCAL, attempt, true, clock())
            .unwrap()
    );
    let (io, state) = peer_with_async(48000, true);
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.peers.iter().any(|peer| {
            peer.control
                .snapshot()
                .is_ok_and(|snapshot| snapshot.peer.is_some())
        })
    });
    let status = host.status_text(LOCAL).unwrap();
    assert!(
        status.contains("jitter delay=10ms buffered-span=13ms queued=3"),
        "{status}"
    );
    assert!(status.contains("missing-samples=160 insertion-samples=80"));
    assert!(status.contains("overload-drops=5 overload-span-samples=800"));
    assert!(status.contains("ingress-queued=7/64 ingress-overflow-drops=6"));
    assert!(status.contains(
        "egress datagrams=10 socket-blocked=2 reliable-retries=1 reliable-pending=3 reply-pending=0"
    ));
    assert!(status.contains("media accepted=0 blocked=0 expired-native-samples=0"));
    assert_eq!(state.lock().unwrap().observe_calls, 1);
    assert!(host.fixture_stop(20));
}

#[test]
fn peers_survive_reload_route_events_and_detach_before_generation_reclamation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mapped_host(
        json!({"mute":{"command":"*AB","protected":false,"action":"rx_mute_toggle","target":"peer"}}),
    );
    assert!(host.status_text(LOCAL).unwrap().contains("no active links"));
    let local = host.status_text(LOCAL).unwrap();
    assert!(
        local.contains("local-rx:"),
        "status must identify the local receive ring"
    );
    assert!(local.contains("reset-failures=0"));
    assert!(local.contains("pending-reset-dropped=0"));
    assert!(matches!(
        host.status_text("missing"),
        Err(RuntimeError::MissingNode)
    ));
    assert_eq!(
        asl_route(&host, "2000", Some("127.0.0.1")).unwrap(),
        "radio@127.0.0.1:4569/2000"
    );
    assert!(matches!(
        host.lookup("missing", "2000", None),
        Err(RuntimeError::MissingNode)
    ));
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert!(
        host.status_text(LOCAL)
            .unwrap()
            .contains("2000: transceive")
    );
    host.immediate(
        LOCAL,
        LinkEffect::Text(vec![("2000".into(), "L T3000".into())]),
        clock(),
    )
    .unwrap();
    host.immediate(
        LOCAL,
        LinkEffect::ForwardDigits(vec![("2000".into(), "D 2000 1000 1 5".into())]),
        clock(),
    )
    .unwrap();
    host.immediate(LOCAL, LinkEffect::None, clock()).unwrap();
    host.immediate(LOCAL, LinkEffect::SelectedRemote, clock())
        .unwrap();
    state.lock().unwrap().input.extend([
        Input::Text(b"invalid"),
        Input::Text(b"L T3000"),
        Input::Digit(b'*'),
        Input::Digit(b'A'),
        Input::Digit(b'B'),
        Input::Digit(b'2'),
        Input::Digit(b'0'),
        Input::Digit(b'0'),
        Input::Digit(b'0'),
        Input::Digit(b'#'),
    ]);
    let source = host.peers[0].control.reply_target();
    pump_until(&mut host, |_| source.rx_muted());
    assert!(
        !host
            .runtime
            .node(LOCAL)
            .unwrap()
            .radio_receive_status()
            .muted
    );
    assert!(host.take_operations().is_empty());
    assert!(host.status_text(LOCAL).unwrap().contains("3000"));
    let guard = state.lock().unwrap();
    assert!(guard.texts.iter().any(|text| text == b"D 2000 1000 1 5"));
    assert!(guard.digits.is_empty());
    drop(guard);
    host.runtime.digit(
        LOCAL,
        DigitEvent::Digit {
            digit: '*',
            now_ms: 10,
        },
    );
    host.lost_digits();
    host.fixture_reload(
        configuration(
            "usb",
            &[(Kind::Allstarlink, "", json!({"callsign":"W1NEW"}))],
        ),
        clock(),
    )
    .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(state.lock().unwrap().drops, 0);
    for action in [
        LinkAction::Status,
        LinkAction::FullStatus,
        LinkAction::LastKeyed,
        LinkAction::Time,
    ] {
        host.immediate(LOCAL, LinkEffect::Telemetry(action, None), clock())
            .unwrap();
    }
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!!DISCONNECT!!"));
    pump_until(&mut host, |host| host.peers.is_empty());
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.status_text(LOCAL).unwrap().contains("no active links"));
    assert!(host.fixture_stop(20));
}

#[test]
fn status_reports_modes_permanent_retries_and_failed_resource_preparation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    for (remote, action, mode, expected) in [
        ("2000", LinkAction::Monitor, Mode::MONITOR, "monitor"),
        (
            "3000",
            LinkAction::PermanentLocalMonitor,
            Mode::LOCAL_MONITOR,
            "local-monitor (permanent)",
        ),
    ] {
        attach(&mut host, remote, action, mode);
        pump_until(&mut host, |host| {
            host.runtime
                .status(10)
                .iter()
                .all(|(_, status)| status.retiring.is_none())
        });
        assert!(
            host.status_text(LOCAL)
                .unwrap()
                .contains(&format!("{remote}: {expected}"))
        );
    }
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::PermanentTransceive, "4000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected dial");
    };
    assert!(
        !host
            .runtime
            .finish_connect(LOCAL, attempt, false, clock())
            .unwrap()
    );
    assert!(host.status_text(LOCAL).unwrap().contains("(retrying)"));
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::DisconnectPermanentAll, ""),
            clock(),
            false,
        )
        .unwrap();
    host.immediate(LOCAL, effect, clock()).unwrap();
    assert!(host.status_text(LOCAL).unwrap().contains("(paused)"));
    assert!(
        host.immediate(
            LOCAL,
            LinkEffect::ForwardDigits(vec![("missing".into(), "D 3000 1000 1 1".into())]),
            clock()
        )
        .is_err()
    );
    assert!(host.text(LOCAL, "missing", "L", false).is_err());
    assert!(host.refresh("missing", clock()).is_err());
    let (io, state) = peer(0);
    assert!(
        host.attach_peer(LOCAL, "5000", Mode::TRANSCEIVE, io, clock())
            .is_err()
    );
    assert_eq!(state.lock().unwrap().drops, 1);
    let (io, state) = peer(48000);
    assert!(
        host.attach_peer("missing", "5000", Mode::TRANSCEIVE, io, clock())
            .is_err()
    );
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.fixture_stop(30));
}

#[test]
fn host_audio_owner_publishes_one_configured_group_winner_to_status() {
    use std::num::NonZeroUsize;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let config = configuration(
        "usb",
        &[(
            Kind::PermanentGroup,
            "network",
            json!({"remote_node":["2000","3000"], "group_name":"Network"}),
        )],
    );
    let mut host =
        Host::fixture_start(config, media(), services(), Instant::now(), clock()).unwrap();
    let mut states = Vec::new();
    for (index, remote) in ["2000", "3000"].into_iter().enumerate() {
        host.runtime.incoming(LOCAL, remote, true).unwrap();
        let (io, state) = peer(48000);
        host.attach_peer(LOCAL, remote, Mode::TRANSCEIVE, io, clock())
            .unwrap();
        states.push(state);
        let expected = &["2000", "3000"][..=index];
        pump_until(&mut host, |host| {
            expected.iter().all(|remote| named_peer_ready(host, remote))
                && host.runtime.status(10).iter().any(|(node, status)| {
                    node == LOCAL && status.active.is_some() && status.retiring.is_none()
                })
        });
    }
    let selection = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .and_then(|peer| peer.group.as_ref())
        .expect("configured group member is published")
        .selection()
        .clone();

    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let mut audio = [0.0; 960];
    owners
        .0
        .acquire()
        .unwrap()
        .state()
        .process(false, &mut audio, 0);
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    let status = host.status_text(LOCAL).unwrap();
    assert!(status.contains("2000: transceive"));
    assert!(status.contains("3000: monitor"));

    lease.lock().unwrap().owners = Some(owners);
    assert!(host.fixture_stop(30));
    drop(states);
}

#[test]
fn reload_handoff_rolls_back_failed_open_and_releases_removed_devices() {
    fn reload_after_busy(
        host: &mut Host,
        make: impl Fn() -> PreparedConfiguration,
    ) -> Result<(), RuntimeError> {
        let mut outcome = None;
        pump_until(host, |host| match host.fixture_reload(make(), clock()) {
            Err(RuntimeError::Busy) => false,
            result => {
                outcome = Some(result);
                true
            }
        });
        outcome.expect("reload produced a non-Busy result")
    }

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    reload_after_busy(&mut host, || {
        configuration(
            "usb2",
            &[(Kind::Allstarlink, "", json!({"lookup_method":"dns"}))],
        )
    })
    .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(
        asl_route(&host, "2000", None).unwrap(),
        "radio@127.0.0.1:4569/2000"
    );
    let revision = host.runtime.revision();
    crate::fixture::RADIO_OPEN_RESULT.store(1, std::sync::atomic::Ordering::Release);
    assert_eq!(
        reload_after_busy(&mut host, || configuration(
            "usb3",
            &[(Kind::Allstarlink, "", json!({"lookup_method":"file"}))]
        )),
        Err(RuntimeError::Device)
    );
    assert_eq!(
        crate::fixture::RADIO_OPEN_RESULT.load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert_eq!(host.runtime.revision(), revision);
    reload_after_busy(&mut host, || {
        configuration(
            "usb2",
            &[(Kind::Allstarlink, "", json!({"lookup_method":"file"}))],
        )
    })
    .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(
        asl_route(&host, "2000", None).unwrap(),
        "radio@127.0.0.1:4569/2000"
    );
    reload_after_busy(&mut host, || stations(&[], &[])).unwrap();
    assert!(host.runtime.node_names().is_empty());
    pump_until(&mut host, |host| host.runtime.status(40).is_empty());
    assert!(host.media_workers.is_empty());
    assert!(host.fixture_stop(40));
    assert!(host.leases.is_empty());
}

#[test]
fn egress_retirement_waits_for_dispatch_and_redirect() {
    struct Resume(Arc<std::sync::Barrier>);
    impl Drop for Resume {
        fn drop(&mut self) {
            self.0.wait();
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    let pause = Arc::new(std::sync::Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    for _ in 0..64 {
        host.peers[0]
            .control
            .send(PeerCommand::Text {
                text: c"D 2000 1000 1 1".into(),
                advisory: false,
            })
            .ok()
            .unwrap();
    }
    assert!(host.text(LOCAL, "2000", "\0", false).is_err());
    assert!(host.text(LOCAL, "2000", "L", false).is_err());
    assert!(
        host.immediate(
            LOCAL,
            LinkEffect::ForwardDigits(vec![("2000".into(), "D 2000 1000 1 2".into())]),
            clock()
        )
        .is_err()
    );
    host.fixture_reload(
        configuration(
            "usb",
            &[(Kind::Allstarlink, "", json!({"callsign":"W1NEW"}))],
        ),
        clock(),
    )
    .unwrap();
    let active = host.runtime.status(10)[0].1.active.unwrap();
    assert_eq!(
        host.runtime
            .adapter_control_mut(LOCAL, active)
            .unwrap()
            .redirects
            .len(),
        1
    );
    let retiring = host.runtime.status(10)[0].1.retiring.unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .adapter_control_mut(LOCAL, retiring)
            .unwrap()
            .dispatcher
            .is_quiescent()
    });
    assert_eq!(
        host.runtime.status(10)[0].1.retiring,
        Some(retiring),
        "dispatcher quiescence cannot bypass the blocked peer's redirect acknowledgment"
    );
    drop(resume);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });

    // Direct publication needs quiescent audio; retain the peer owner for its pending ack.
    assert!(host.leases[0].1.lock().unwrap().quiesce());
    let candidate = prepared(
        LOCAL,
        host.runtime.settings(LOCAL).unwrap(),
        StationPeers {
            links: &host.peers,
            calls: Some(&host.phone_calls),
            applications: &[],
        },
        media_worker(&mut host.media_workers, LOCAL).unwrap(),
        &mut host.text,
        host.services,
        StationAudio::Modular(host.radio_preparation.active_plan(LOCAL).unwrap()),
    )
    .unwrap();
    host.runtime.replace_adapter(LOCAL, candidate, 11).unwrap();
    host.reject_peer(LOCAL, "2000", 12);
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
    assert_eq!(state.lock().unwrap().drops, 1);
    host.reject_peer(LOCAL, "missing", 12);
    assert!(host.fixture_stop(13));
}

#[test]
fn withdrawn_peer_clears_a_queued_redirect_and_acknowledgment() {
    struct Resume(Arc<Barrier>);
    impl Drop for Resume {
        fn drop(&mut self) {
            self.0.wait();
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    for _ in 0..64 {
        host.peers[0]
            .control
            .send(PeerCommand::Text {
                text: c"D 2000 1000 1 1".into(),
                advisory: false,
            })
            .ok()
            .unwrap();
    }
    host.fixture_reload(
        configuration(
            "usb",
            &[(Kind::Allstarlink, "", json!({"callsign":"W1NEW"}))],
        ),
        clock(),
    )
    .unwrap();
    let active = host.runtime.status(10)[0].1.active.unwrap();
    assert_eq!(
        host.runtime
            .adapter_control_mut(LOCAL, active)
            .unwrap()
            .redirects
            .len(),
        1
    );
    host.reject_peer(LOCAL, "2000", 12);
    host.pump(clock()).unwrap();
    let control = host.runtime.adapter_control_mut(LOCAL, active).unwrap();
    assert!(control.redirects.is_empty());
    assert!(control.awaiting.is_empty());
    drop(resume);
    assert!(host.fixture_stop(13));
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn ended_ingress_is_omitted_and_unknown_peer_events_do_not_change_routes() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    host.runtime.node(LOCAL).unwrap().links().ended("2000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"K? * 3000 0 0"));
    std::thread::sleep(Duration::from_millis(5));
    host.pump(clock()).unwrap();
    host.peers[0].control.stop();
    let candidate = prepared(
        LOCAL,
        host.runtime.settings(LOCAL).unwrap(),
        StationPeers {
            links: &host.peers,
            calls: Some(&host.phone_calls),
            applications: &[],
        },
        media_worker(&mut host.media_workers, LOCAL).unwrap(),
        &mut host.text,
        host.services,
        StationAudio::Modular(host.radio_preparation.active_plan(LOCAL).unwrap()),
    )
    .unwrap();
    assert!(candidate.control.redirects.is_empty());
    host.pump(clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(host.fixture_stop(20));
}

#[test]
fn new_node_reload_and_missing_or_busy_operations_leave_live_owners_intact() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = Host::fixture_start(
        stations(&[], &[]),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    host.fixture_reload(
        stations(
            &[
                (LOCAL, "1000", "usb", json!({})),
                (SECOND_LOCAL, "2000", "usb2", json!({})),
            ],
            &[],
        ),
        clock(),
    )
    .unwrap();
    let effect = host
        .runtime
        .command(
            LOCAL,
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    assert!(host.immediate(LOCAL, effect, clock()).is_err());
    let first = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let (io, second) = peer(48000);
    host.runtime.incoming(SECOND_LOCAL, "3000", true).unwrap();
    host.attach_peer(SECOND_LOCAL, "3000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    host.immediate(
        SECOND_LOCAL,
        LinkEffect::ForwardDigits(vec![("3000".into(), "D 3000 2000 1 9".into())]),
        clock(),
    )
    .unwrap();
    host.text(SECOND_LOCAL, "3000", "L", false).unwrap();
    assert!(
        host.status_text(SECOND_LOCAL)
            .unwrap()
            .contains("3000: transceive")
    );
    pump_until(&mut host, |_| {
        second
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text.starts_with(b"D "))
    });
    let first_state = first.lock().unwrap();
    let second_state = second.lock().unwrap();
    assert!(!first_state.texts.iter().any(|text| text.starts_with(b"D ")));
    assert!(
        second_state
            .texts
            .iter()
            .any(|text| text == b"D 3000 2000 1 9")
    );
    assert!(second_state.digits.is_empty());
    drop(second_state);
    drop(first_state);
    host.reject_peer(SECOND_LOCAL, "3000", 15);
    pump_until(&mut host, |_| second.lock().unwrap().drops == 1);
    assert_eq!(second.lock().unwrap().drops, 1);
    assert_eq!(first.lock().unwrap().drops, 0);
    assert!(host.refresh("missing", clock()).is_err());
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    host.fixture_reload(stations(&[], &[]), clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(host.fixture_stop(20));
}

#[test]
fn device_handoff_retains_unique_owners_and_refuses_failed_or_duplicate_open() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut first = host();
    let mut second = host();
    let first_lease = first.leases[0].1.clone();
    {
        let mut lease = first_lease.lock().unwrap();
        lease.binding.next = lease.binding.active.clone();
    }
    let second_lease = second.leases[0].1.clone();
    let settings = GenerationSettings {
        node: LOCAL.into(),
        device: "usb".into(),
        receive_maximum: MAXIMUM_FRAMES,
        transmit_maximum: MAXIMUM_FRAMES,
        squelch_delay_ms: 0,
        status_snapshot_interval_ms: 50,
    };
    let mut device = Device {
        lease: first_lease.clone(),
        name: LOCAL.into(),
        services: services(),
    };
    assert!(!device.open(&settings));
    assert!(device.quiesce());
    assert!(!device.open(&settings));
    let owners = first_lease.lock().unwrap().owners.take().unwrap();
    assert!(!first_lease.lock().unwrap().attach(owners));
    device.close();
    assert!(device.open(&settings));
    {
        let mut lease = second_lease.lock().unwrap();
        assert!(lease.quiesce());
        let owners = lease.owners.take().unwrap();
        assert!(!first_lease.lock().unwrap().attach(owners));
        lease.owners = first_lease.lock().unwrap().owners.take();
    }
    assert!(first.fixture_stop(10));
    assert!(second.fixture_stop(10));

    let lease = Arc::new(Mutex::new(Lease {
        #[cfg(target_os = "linux")]
        binding: native::LeaseBindings {
            next: Some(Arc::new(crate::fixture::station::binding(services()))),
            ..Default::default()
        },
        maintenance: None,
        quiesced: false,
        quiescing: false,
        owners: None,
        worker: None,
        epoch: Instant::now(),
        status: Default::default(),
    }));
    let mut device = Device {
        lease: lease.clone(),
        name: LOCAL.into(),
        services: services(),
    };
    RadioWorker::fail_next_for_test(crate::worker::TestFailure::Prepare);
    let before = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(!device.open(&settings));
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        before + 1
    );
    assert!(device.open(&settings));
    assert!(device.quiesce());
    assert!(lease.lock().unwrap().owners.is_none());
    device.close();
    assert!(device.open(&settings));
    device.close();
}

#[test]
fn pending_radio_retirement_is_pumped_without_reopening_or_discarding_owners() {
    thread_local! { static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    unsafe extern "C" fn quiesce(_: *mut c_void, _: *mut c_void) -> i32 {
        i32::from(PENDING.get())
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut table = service_table();
    table.radio_quiesce = Some(quiesce);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let lease = Arc::clone(&host.leases[0].1);
    let mut device = Device {
        name: LOCAL.into(),
        lease: Arc::clone(&lease),
        services,
    };
    PENDING.set(true);
    device.close();
    assert!(lease.lock().unwrap().quiescing);
    assert!(lease.lock().unwrap().worker.is_some());
    host.pump(clock()).unwrap();
    assert!(lease.lock().unwrap().quiescing);
    PENDING.set(false);
    host.pump(clock()).unwrap();
    assert!(lease.lock().unwrap().worker.is_none());
    assert!(lease.lock().unwrap().owners.is_some());
    device.close();
    assert!(host.fixture_stop(20));
}

#[test]
fn peer_radio_unkey_completes_remote_commands_with_the_original_reply_target() {
    use rpt_advanced_core::remote_base_command::RemoteCommand;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::fixture_start(
        remote::remote_configuration(),
        media(),
        remote::remote_services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    let (io, state) = peer(48000);
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    pump_until(&mut host, |host| {
        named_peer_ready(host, "2000") && host.runtime.status(10)[0].1.retiring.is_none()
    });
    let reply = host.peers[0].control.reply_target();
    state.lock().unwrap().input.extend([
        Input::Text(b"!NEWKEY!"),
        Input::RadioKey,
        Input::Digit(b'*'),
        Input::Digit(b'6'),
        Input::Digit(b'5'),
        Input::Digit(b'1'),
        Input::RadioUnkey,
    ]);
    // Read the actual peer owner's events, separating the dispatch receipt from
    // submission so this test can verify which transport owns the answer.
    let deadline = Instant::now() + Duration::from_secs(3);
    let input = loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for peer unkey"
        );
        match host.peers[0].control.received_event() {
            Some(Event::ReceivedDigit(symbol, receipt)) => {
                assert_eq!(receipt.reply, reply);
                assert!(
                    host.runtime
                        .node(LOCAL)
                        .unwrap()
                        .links()
                        .peer_digit("2000", symbol, receipt.at, receipt.reply)
                        .is_none()
                );
            }
            Some(Event::ReceivedUnkey(receipt)) => {
                assert_eq!(receipt.reply, reply);
                let mut outcomes = host
                    .runtime
                    .node(LOCAL)
                    .unwrap()
                    .links()
                    .peer_remote_unkey(receipt.at, receipt.reply);
                assert_eq!(outcomes.len(), 1);
                let DigitOutcome::Mapped(input) = outcomes.pop().unwrap() else {
                    panic!("unkey must complete a mapped radio command")
                };
                break input;
            }
            Some(_) => {}
            None => std::thread::yield_now(),
        }
    };
    let LinkEffect::RemoteBase(operation) = host
        .runtime
        .dispatch_mapping(LOCAL, input, clock())
        .unwrap()
    else {
        panic!("radio operation")
    };
    assert_eq!(operation.remote, Some(RemoteCommand::Status { long: true }));
    assert_eq!(operation.reply.as_ref(), Some(&reply));
    assert_eq!(operation.origin.as_deref(), Some("2000"));
    assert!(host.fixture_stop(20));
}

#[test]
fn pending_audio_generation_bounds_adapter_refresh_retries() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    assert!(host.leases[0].1.lock().unwrap().quiesce());
    host.fixture_reload(
        configuration(
            "usb",
            &[(Kind::Allstarlink, "", json!({"callsign":"W1NEW"}))],
        ),
        clock(),
    )
    .unwrap();
    assert!(matches!(
        host.refresh(LOCAL, clock()),
        Err(RuntimeError::Busy)
    ));
    assert!(host.fixture_stop(20));
}

#[test]
fn saturated_media_publication_keeps_the_running_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let active = host.runtime.status(10)[0].1.active;
    let mut held = Vec::new();
    // The worker retains two contexts plus two queued publications. The active
    // generation owns one slot; holding three candidates fills every other slot.
    for _ in 0..3 {
        loop {
            match prepared(
                LOCAL,
                host.runtime.settings(LOCAL).unwrap(),
                StationPeers {
                    links: &host.peers,
                    calls: Some(&host.phone_calls),
                    applications: &[],
                },
                media_worker(&mut host.media_workers, LOCAL).unwrap(),
                &mut host.text,
                host.services,
                StationAudio::Modular(host.radio_preparation.active_plan(LOCAL).unwrap()),
            ) {
                Ok(candidate) => {
                    held.push(candidate);
                    break;
                }
                // Let a queued candidate move into an unused context slot.
                Err(RuntimeError::Busy) => std::thread::yield_now(),
                Err(error) => panic!("unexpected preparation failure: {error:?}"),
            }
        }
    }
    assert!(matches!(
        host.refresh(LOCAL, clock()),
        Err(RuntimeError::Busy)
    ));
    assert_eq!(host.runtime.status(10)[0].1.active, active);
    assert!(matches!(
        host.fixture_reload(
            configuration(
                "usb",
                &[(
                    Kind::TxSignaling,
                    "",
                    json!({"ptt":{"timing":{"hang_ms":10}}})
                )]
            ),
            clock()
        ),
        Err(RuntimeError::Busy)
    ));
    assert_eq!(host.runtime.status(10)[0].1.active, active);
    drop(held);
    assert!(host.fixture_stop(20));
}

#[test]
fn queued_peer_events_after_runtime_stop_cannot_reenter_removed_node_policy() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(host.site_shutdown(20), Ok(true));
    assert!(!host.runtime.stop(20));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"K? * 3000 0 0"));
    for _ in 0..100 {
        if state.lock().unwrap().input.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(1));
    host.pump(clock()).unwrap();
    assert!(host.take_operations().is_empty());
    assert!(host.fixture_stop(30));
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn failed_audio_preparation_and_attachment_preserve_start_and_reload_state() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    RadioWorker::fail_next_for_test(crate::worker::TestFailure::Prepare);
    assert!(matches!(
        Host::fixture_start(
            configuration("usb", &[]),
            media(),
            services(),
            Instant::now(),
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    let mut host = host();
    let old_document = Arc::clone(host.runtime.modular_configuration().unwrap());
    let revision = host.runtime.revision();
    let lease = Arc::clone(&host.leases[0].1);
    crate::fixture::RADIO_ACTIVATE_RESULT.store(1, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        host.fixture_reload(
            stations(
                &[
                    (LOCAL, "1000", "usb", json!({})),
                    (SECOND_LOCAL, "2000", "usb2", json!({}))
                ],
                &[]
            ),
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &old_document
    ));
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(host.runtime.node_names(), [LOCAL]);
    assert!(Arc::ptr_eq(&host.leases[0].1, &lease));
    assert!(lease.lock().unwrap().worker.is_some());
    assert!(host.runtime.node(LOCAL).unwrap().register_audio().is_none());
    assert!(host.fixture_stop(20));
}

#[test]
fn peer_processing_preparation_and_retirement_follow_the_audio_generation() {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct State {
        reject: AtomicBool,
        next: AtomicUsize,
        prepared: Mutex<BTreeMap<usize, f64>>,
        destroyed: Mutex<BTreeSet<usize>>,
        processed: AtomicUsize,
    }
    // The SDK has no application context on its link descriptor. This fixture is
    // serialized by LIFECYCLE, and its state outlives every media callback.
    static TRACKER: std::sync::LazyLock<State> = std::sync::LazyLock::new(|| State {
        reject: AtomicBool::new(false),
        next: AtomicUsize::new(1),
        prepared: Default::default(),
        destroyed: Default::default(),
        processed: AtomicUsize::new(0),
    });
    unsafe extern "C" fn process(
        context: *mut c_void,
        _: u32,
        rate: u32,
        _: *mut i16,
        _: u32,
    ) -> i32 {
        if rate != 48000 {
            return -1;
        }
        TRACKER
            .processed
            .store(unsafe { *context.cast::<usize>() }, Ordering::Release);
        0
    }
    unsafe extern "C" fn destroy(context: *mut c_void) {
        let id = unsafe { Box::from_raw(context.cast::<usize>()) };
        assert!(
            TRACKER.destroyed.lock().unwrap().insert(*id),
            "each prepared processor retires once"
        );
    }
    unsafe extern "C" fn role(
        bytes: *const u8,
        len: u32,
        _: abi::UrpNativeProcessingDiagnostic,
        _: *mut c_void,
        out: *mut *mut abi::UrpPreparedLinkRole,
    ) -> i32 {
        let raw = unsafe { std::slice::from_raw_parts(bytes, len as usize) };
        let Ok(dto) = serde_json::from_slice::<Value>(raw) else {
            return -1;
        };
        let Some(threshold) = dto
            .pointer("/link/limiter/low/threshold_dbfs")
            .and_then(Value::as_f64)
        else {
            return -1;
        };
        unsafe { out.write(Box::into_raw(Box::new(threshold)).cast()) };
        0
    }
    unsafe extern "C" fn role_equal(
        a: *const abi::UrpPreparedLinkRole,
        b: *const abi::UrpPreparedLinkRole,
        out: *mut u32,
    ) -> i32 {
        unsafe { out.write(u32::from(*a.cast::<f64>() == *b.cast::<f64>())) };
        0
    }
    unsafe extern "C" fn role_destroy(role: *mut abi::UrpPreparedLinkRole) {
        if !role.is_null() {
            drop(unsafe { Box::from_raw(role.cast::<f64>()) });
        }
    }
    unsafe extern "C" fn prepare(
        role: *const abi::UrpPreparedLinkRole,
        ffmpeg: *const c_void,
        rate: u32,
        frames: u32,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { out.write(ptr::null_mut()) };
        if TRACKER.reject.load(Ordering::Acquire)
            || ffmpeg.is_null()
            || rate != 48000
            || frames as usize != MAXIMUM_FRAMES
        {
            return -1;
        }
        let id = TRACKER.next.fetch_add(1, Ordering::Relaxed);
        let threshold = unsafe { *role.cast::<f64>() };
        assert!(
            TRACKER
                .prepared
                .lock()
                .unwrap()
                .insert(id, threshold)
                .is_none()
        );
        unsafe { out.write(Box::into_raw(Box::new(id)).cast()) };
        0
    }
    fn only_live(state: &State, expected_threshold: f64) -> usize {
        let prepared = state.prepared.lock().unwrap().clone();
        let destroyed = state.destroyed.lock().unwrap().clone();
        assert!(destroyed.iter().all(|id| prepared.contains_key(id)));
        let live: Vec<_> = prepared
            .iter()
            .filter(|(id, _)| !destroyed.contains(id))
            .collect();
        assert_eq!(
            live.len(),
            1,
            "discarded candidates retire; one adopted processor remains live"
        );
        let (&id, &threshold) = live[0];
        assert_eq!(threshold, expected_threshold);
        id
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let state = &*TRACKER;
    let api = unsafe {
        crate::configuration_radio::RadioPreparationApi::fixture_descriptors(|ordinary, roles| {
            ordinary.link_process = Some(process);
            ordinary.link_destroy = Some(destroy);
            roles.prepare = Some(role);
            roles.equal = Some(role_equal);
            roles.link_prepare = Some(prepare);
            roles.destroy = Some(role_destroy);
        })
    }
    .unwrap();
    let config = |threshold: i32| {
        configuration(
            "usb",
            &[(
                Kind::PeerAudio,
                "",
                json!({"multiband_limiter":{"low":{"threshold_dbfs":threshold}}}),
            )],
        )
    };
    let mut host = Host::management_only(media(), services(), Instant::now()).unwrap();
    host.radio_preparation.fixture_set_api(api);
    host.fixture_reload(config(-10), clock()).unwrap();
    assert!(state.prepared.lock().unwrap().is_empty());
    let _peer = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    let original = only_live(state, -10.0);
    pump_until(&mut host, |_| {
        state.processed.load(Ordering::Acquire) == original
    });
    let prior_document = Arc::clone(host.runtime.modular_configuration().unwrap());
    let prior_revision = host.runtime.revision();
    let prior_generation = host.runtime.status(10)[0].1.active;
    let prepared = state.prepared.lock().unwrap().clone();
    let destroyed = state.destroyed.lock().unwrap().clone();
    state.reject.store(true, Ordering::Relaxed);
    assert_eq!(
        host.fixture_reload(config(-11), clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &prior_document
    ));
    assert_eq!(host.runtime.revision(), prior_revision);
    assert_eq!(host.runtime.status(10)[0].1.active, prior_generation);
    let after_prepared = state.prepared.lock().unwrap().clone();
    let after_destroyed = state.destroyed.lock().unwrap().clone();
    assert_eq!(after_prepared, prepared);
    assert_eq!(after_destroyed, destroyed);
    assert_eq!(only_live(state, -10.0), original);
    state.processed.store(0, Ordering::Release);
    pump_until(&mut host, |_| {
        state.processed.load(Ordering::Acquire) == original
    });
    state.reject.store(false, Ordering::Relaxed);
    let mut accepted = None;
    pump_until(&mut host, |host| {
        match host.fixture_reload(config(-11), clock()) {
            Err(RuntimeError::Busy) => {
                // Discarded candidates may still occupy dispatcher publication credit.
                // Busy never changes the adopted configuration or processor.
                assert!(Arc::ptr_eq(
                    host.runtime.modular_configuration().unwrap(),
                    &prior_document
                ));
                assert_eq!(host.runtime.revision(), prior_revision);
                assert_eq!(host.runtime.status(10)[0].1.active, prior_generation);
                assert_eq!(only_live(state, -10.0), original);
                false
            }
            result => {
                accepted = Some(result);
                true
            }
        }
    });
    accepted
        .expect("bounded control pump observed a non-Busy result")
        .unwrap();
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    let replacement = only_live(state, -11.0);
    assert_ne!(replacement, original);
    pump_until(&mut host, |_| {
        state.processed.load(Ordering::Acquire) == replacement
    });
    assert!(state.destroyed.lock().unwrap().contains(&original));
    assert!(host.fixture_stop(20));
    let prepared: BTreeSet<_> = state.prepared.lock().unwrap().keys().copied().collect();
    let destroyed = state.destroyed.lock().unwrap().clone();
    assert_eq!(
        prepared, destroyed,
        "every successful preparation, including discarded candidates, retires at shutdown"
    );
}

#[test]
fn failed_device_restoration_leaves_inactive_generations_safe_to_pump_and_stop() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    static FAIL: AtomicBool = AtomicBool::new(false);
    static OPEN_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn open(
        context: *mut c_void,
        plan: *const c_void,
        maximum: usize,
        output: *mut *mut c_void,
    ) -> i32 {
        OPEN_ATTEMPTS.fetch_add(1, Ordering::AcqRel);
        if FAIL.load(Ordering::Acquire) {
            unsafe {
                output.write(std::ptr::null_mut());
            }
            -1
        } else {
            unsafe {
                crate::fixture::station::BINDING.0.open_prepared.unwrap()(
                    context, plan, maximum, output,
                )
            }
        }
    }
    unsafe extern "C" fn destroy(context: *mut c_void, handle: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::AcqRel);
        unsafe { (*crate::fixture::host_descriptor()).radio_destroy.unwrap()(context, handle) };
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    FAIL.store(false, Ordering::Release);
    OPEN_ATTEMPTS.store(0, Ordering::Release);
    DESTROYS.store(0, Ordering::Release);
    let mut table = service_table();
    table.context = ptr::without_provenance_mut(1);
    let mut binding = crate::fixture::station::BINDING.0;
    binding.open_prepared = Some(open);
    table.station_binding = Box::leak(Box::new(binding));
    table.radio_destroy = Some(destroy);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    // Retain the real TX guard through the busy rejection and the subsequent pending handoff.
    let lease = host.leases[0].1.clone();
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let held = owners.1.acquire().unwrap();
    // Quiescing gave us exclusive callback storage. Reopen the incumbent plan
    // so the typed provider can classify the next change on its actual handle.
    let mut incumbent = Device {
        name: LOCAL.into(),
        lease: lease.clone(),
        services: host.services,
    };
    incumbent.close();
    assert!(incumbent.restore(held.settings()));
    let active = host.runtime.status(10)[0].1.active;
    assert_eq!(active, Some(held.id()));
    assert_eq!(host.runtime.status(10)[0].1.protected_owners, 1);
    let previous = Arc::clone(host.runtime.modular_configuration().unwrap());
    let revision = host.runtime.revision();
    let resources = || {
        (
            OPEN_ATTEMPTS.load(Ordering::Acquire),
            DESTROYS.load(Ordering::Acquire),
        )
    };
    let before = resources();
    assert_eq!(before, (2, 1));
    let work = host.runtime.node(LOCAL).unwrap().work().unwrap();
    FAIL.store(true, Ordering::Release);
    // Exercise the physical rejection itself before applying any account edits.
    let first = configuration("unavailable", &[]);
    let ticket = host.stage_radio_plans(first.runtime()).unwrap();
    host.prepare_core(&ticket, first.runtime().clone(), clock())
        .unwrap();
    assert_eq!(
        host.begin_core_adoption(
            ticket,
            "work-held".into(),
            Default::default(),
            first.inputs().clone(),
            clock()
        ),
        Err(RuntimeError::Busy)
    );
    assert_eq!(host.runtime.status(10)[0].1.active, active);
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &previous
    ));
    assert_eq!(host.runtime.revision(), revision);
    assert!(!host.runtime.reload_pending());
    assert_eq!(resources(), before);
    assert!(work.is_current());
    drop(work);

    let mut adoption = host
        .fixture_begin_reload(configuration("unavailable", &[]), clock())
        .unwrap();
    assert!(host.runtime.reload_pending());
    assert_eq!(adoption.poll(&mut host, clock()), Ok(false));
    assert_eq!(host.runtime.status(10)[0].1.active, active);
    assert_eq!(host.runtime.status(10)[0].1.protected_owners, 1);
    assert!(host.runtime.node(LOCAL).unwrap().work().is_none());
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &previous
    ));
    assert_eq!(host.runtime.revision(), revision);
    // The incumbent physical callbacks have quiesced, while the outstanding
    // software guard still prevents the candidate or reverse open.
    assert_eq!(resources(), (before.0, before.1 + 1));

    drop(held);
    lease.lock().unwrap().owners = Some(owners);
    assert_eq!(host.runtime.status(10)[0].1.protected_owners, 0);
    assert_eq!(
        adoption.poll(&mut host, clock()),
        Err(RuntimeError::Restore)
    );
    assert!(Arc::ptr_eq(
        host.runtime.modular_configuration().unwrap(),
        &previous
    ));
    assert_eq!(host.runtime.revision(), revision);
    assert!(!host.runtime.reload_pending());
    assert_eq!(resources(), (before.0 + 2, before.1 + 1));
    assert!(host.runtime.status(10)[0].1.active.is_none());
    assert!(!host.status_text(LOCAL).unwrap().contains("local-rx:"));
    // A late peer acknowledgment cannot reactivate a failed radio generation.
    let (inbound, _input) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    let (_, outbound) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    host.peers[0]
        .control
        .send(PeerCommand::Redirect {
            inbound,
            ingress_graph_generation: 0,
            dtmf_muting: true,
            outbound,
        })
        .ok()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2));
    pump_until(&mut host, |host| {
        !host.peers[0].control.snapshot().unwrap().ended
    });
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    // Failed restore retains the configured node after reclaiming its audio generation.
    assert!(host.runtime.status(10)[0].1.active.is_none());
    host.pump(clock()).unwrap();
    assert!(host.fixture_stop(20));
    assert_eq!(state.lock().unwrap().drops, 1);
    assert_eq!(resources(), (before.0 + 2, before.1 + 1));
}

#[test]
fn stopped_host_status_does_not_retain_a_retired_radio_lease() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    assert!(host.fixture_stop(20));
    host.prune_leases();
    assert!(host.leases.is_empty());
    assert_eq!(host.status_text(LOCAL), Err(RuntimeError::MissingNode));
}

#[test]
fn peer_ending_before_refresh_is_reaped_without_a_connected_announcement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let (io, state) = peer_with_async(48000, true);
    {
        // This case requires an ended session before the first station refresh.
        let mut provider = state.lock().unwrap();
        provider.fail_ready = true;
        provider.finish_first_step_before_registration = true;
    }
    assert_eq!(
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock()),
        Ok(())
    );
    assert!(host.peers.is_empty());
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
    assert_eq!(state.lock().unwrap().drops, 1);
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    assert!(!render_queued_telemetry(&mut host, &mut owners));
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.status_text(LOCAL).unwrap().contains("no active links"));
    assert!(host.fixture_stop(20));
}

#[test]
fn replacement_retry_classification_preserves_non_busy_failures() {
    assert!(matches!(replacement_completed(Ok(1)), Ok(true)));
    assert!(matches!(
        replacement_completed(Err(RuntimeError::Busy)),
        Ok(false)
    ));
    for error in [
        RuntimeError::Preparation,
        RuntimeError::MissingNode,
        RuntimeError::Device,
    ] {
        assert_eq!(replacement_completed(Err(error.clone())), Err(error));
    }
}

#[test]
fn peer_without_a_live_local_radio_is_rejected_before_starting_media() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (local, expected) in [
        ("missing", RuntimeError::MissingNode),
        (LOCAL, RuntimeError::Device),
    ] {
        let mut host = host();
        assert!(host.leases[0].1.lock().unwrap().quiesce());
        let (io, state) = peer(48000);
        assert_eq!(
            host.attach_peer(local, "2000", Mode::TRANSCEIVE, io, clock()),
            Err(expected)
        );
        assert!(host.peers.is_empty());
        let state = state.lock().unwrap();
        assert!(
            state.texts.is_empty(),
            "link setup must not start before radio binding"
        );
        assert_eq!(state.drops, 1);
        assert!(host.fixture_stop(20));
    }
}

#[test]
fn rejected_radio_binding_drops_peer_without_starting_media_or_admitting_it() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let (io, state) = peer(8000);
    {
        let mut state = state.lock().unwrap();
        state.fail_bind = true;
        state.input.push_back(Input::Audio(vec![0.25; 160]));
    }
    assert_eq!(
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(host.peers.is_empty());
    {
        let state = state.lock().unwrap();
        assert!(state.texts.is_empty() && state.audio.is_empty());
        assert_eq!(state.input.len(), 1, "no frame may be read before binding");
        assert_eq!(state.drops, 1);
    }
    assert!(host.fixture_stop(20));
}

#[test]
fn stopped_peer_io_owner_rejects_and_releases_a_prepared_session() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    host.peer_io.stop();
    let (io, state) = peer(48000);
    assert_eq!(
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, clock()),
        Err(RuntimeError::Rejected)
    );
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.fixture_stop(20));
}

#[test]
fn incoming_and_outgoing_peers_bind_to_their_nodes_existing_radio_leases() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::fixture_start(
        stations(
            &[
                (LOCAL, "1000", "west-radio", json!({})),
                (SECOND_LOCAL, "2000", "east-radio", json!({})),
            ],
            &[],
        ),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let opens = crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed);
    for (local, remote, incoming) in [(LOCAL, "3000", true), (SECOND_LOCAL, "4000", false)] {
        if incoming {
            host.runtime.incoming(local, remote, true).unwrap();
        } else {
            let LinkEffect::Connect(attempt) = host
                .runtime
                .command(
                    local,
                    operation(LinkAction::Transceive, remote),
                    clock(),
                    false,
                )
                .unwrap()
            else {
                panic!("expected outbound dial");
            };
            assert!(
                host.runtime
                    .finish_connect(local, attempt, true, clock())
                    .unwrap()
            );
        }
        let (io, state) = peer(8000);
        host.attach_peer(local, remote, Mode::TRANSCEIVE, io, clock())
            .unwrap();
        assert_eq!(state.lock().unwrap().bound_radio.as_deref(), Some(local));
    }
    assert_eq!(
        crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
        opens,
        "binding must reuse existing radio reservations"
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn live_vad_policy_updates_ring_reserve_without_reopening_the_radio() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::fixture::RADIO_VAD_MODE.store(0, std::sync::atomic::Ordering::Release);
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _restore = Restore;
    crate::fixture::RADIO_VAD_MODE.store(1, std::sync::atomic::Ordering::Release);
    let mut host = host();
    assert!(
        host.status_text(LOCAL).unwrap().contains("reserve=160ms"),
        "initial policy must reserve onset before callbacks activate"
    );
    let opens = crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed);
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    let generation = host.runtime.status(10)[0].1.active;
    for (mode, expected) in [
        (0, "reserve=0ms"),
        (1, "reserve=160ms"),
        (2, "reserve=160ms"),
        (0, "reserve=0ms"),
    ] {
        crate::fixture::RADIO_VAD_MODE.store(mode, std::sync::atomic::Ordering::Release);
        pump_until(&mut host, |host| {
            host.status_text(LOCAL).unwrap().contains(expected)
        });
        assert_eq!(host.runtime.status(10)[0].1.active, generation);
        assert_eq!(
            crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
            opens
        );
        assert_eq!(
            crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
            drops
        );
    }
    assert!(host.fixture_stop(20));
}

// Install an actual registered command owner on the native host used by the media race.
pub(crate) fn with_ingress_mute(
    run: impl FnOnce(
        &rpt_advanced_core::link::PeerSignals,
        &mut Host,
        rpt_advanced_core::config::resources::candidate::ResourceId,
        RuntimeClock,
        [DigitOutcome; 2],
    ),
) {
    use rpt_advanced_core::link::{PeerIdentity, PeerSignals};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let port = LOCAL.parse().unwrap();
    let signals = PeerSignals::new();
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    host.runtime
        .node(LOCAL)
        .unwrap()
        .links()
        .attach_ingress(
            PeerIdentity::from_operator("2000").unwrap(),
            signals.reply_target(),
        )
        .unwrap();
    // Capture the actual receipt-time regeneration tokens; publication after a mute must recheck them.
    let received = ['5', '5'].map(|symbol| {
        DigitOutcome::ReceivedDigit(
            symbol,
            signals.reply_target().regeneration_origin().unwrap(),
        )
    });
    run(&signals, &mut host, port, clock(), received);
    host.runtime.peer_detached(LOCAL, "2000", clock().now_ms);
    assert!(host.fixture_stop(20));
}

#[cfg(target_os = "linux")]
#[test]
fn status_keeps_network_connect_and_disconnect_between_captures() {
    use crate::management::status::edges::Payload;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, peer(8000).0, clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.peers
            .first()
            .is_some_and(|peer| peer.control.status_connected)
    });
    assert!(host.peers[0].announced);
    let connection = host.peers[0].control.connection_generation();
    host.detach(LOCAL, "2000", clock().now_ms);
    let (lost, edges) = host.status_edges.drain();
    assert!(!lost);
    let observed: Vec<_> = edges
        .into_iter()
        .filter_map(|edge| match edge.payload {
            Payload::Peer {
                id,
                connection: generation,
                kind,
                value,
            } => {
                assert_eq!(id, "allstarlink:2000");
                assert_eq!(generation, connection);
                Some((kind, value))
            }
            _ => None,
        })
        .collect();
    let [(connected, Some(status)), (disconnected, None)] = observed.as_slice() else {
        panic!("expected one exact connect and disconnect");
    };
    assert_eq!(*connected, "link.connected");
    assert_eq!(*disconnected, "link.disconnected");
    assert_eq!(status["state"], "connected");
    assert_eq!(status["direction"], "incoming");
    assert_eq!(status["connection_generation"], connection.to_string());
    assert!(host.fixture_stop(20));
}
