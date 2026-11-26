//! Account resolution must not borrow another station's identity or alter radio gains.
use rpt_advanced_core::config::{
    self, EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport,
    resources::{
        ResourceWarning,
        candidate::{CandidateWarning, ResourceKind},
        profiles::{ProtocolProfileError, ResolvedProtocolProfiles},
    },
};
use serde_json::{Value, json};
#[path = "support/capture.rs"]
mod capture;

const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ONE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const TWO: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

fn profiles(
    rows: &[(&str, ResourceKind, &str, Value)],
) -> Result<ResolvedProtocolProfiles, ProtocolProfileError> {
    let fixture = capture::Fixture::new(&[(ONE, "524950"), (TWO, "508422")], rows);
    fixture.candidate().resolve_protocol_profiles()
}

fn resolve(site: Value, local: Value, proxy: Value) -> (EchoLinkSettings, Vec<CandidateWarning>) {
    let mut rows = vec![(ONE, ResourceKind::Echolink, "", local)];
    if !site.is_null() {
        rows.push((SITE, ResourceKind::Echolink, "", site));
    }
    if !proxy.is_null() {
        rows.push((ONE, ResourceKind::EcholinkProxy, "", proxy));
    }
    let p = profiles(&rows).unwrap();
    (
        p.port(ONE.parse().unwrap())
            .unwrap()
            .echolink
            .as_ref()
            .unwrap()
            .settings
            .clone(),
        p.warnings().to_vec(),
    )
}

#[test]
fn echolink_defaults_are_disabled_and_have_no_identity_or_secrets() {
    let (value, warnings) = resolve(Value::Null, json!({}), Value::Null);
    assert!(warnings.is_empty());
    assert!(!value.enabled);
    assert!(value.callsign.is_empty());
    assert_eq!(value.node_number, 0);
    assert_eq!(
        value.directory_servers,
        ["servers.echolink.org", "backup.echolink.org"]
    );
    assert_eq!((value.max_stations, value.max_pending_handshakes), (20, 20));
    assert_eq!((value.audio_port, value.proxy_port), (5198, 8100));
    assert_eq!(
        (value.heartbeat_interval_ms, value.heartbeat_timeout_ms),
        (10000, 100000)
    );
    assert_eq!((value.receive_gain_db, value.transmit_gain_db), (-3.0, 3.0));
    assert!(value.secrets_file.is_none());
}

#[test]
fn echolink_omnidirectional_antenna_keeps_the_explicit_sentinel() {
    let (v, w) = resolve(
        Value::Null,
        json!({"antenna_direction_deg":-1}),
        Value::Null,
    );
    assert!(w.is_empty());
    assert_eq!(v.antenna_direction_deg, -1.0);
}
#[test]
fn echolink_aprs_endpoint_inherits_and_can_be_explicitly_disabled() {
    assert_eq!(
        resolve(Value::Null, json!({}), Value::Null).0.aprs_server,
        "aprs.echolink.org:5199"
    );
    for (parent, child, result) in [
        ("127.0.0.1:40000", None, "127.0.0.1:40000"),
        ("127.0.0.1:40000", Some(""), ""),
        ("", Some("local.example:5199"), "local.example:5199"),
    ] {
        let local = child.map_or(json!({}), |v| json!({"aprs_server":v}));
        let (v, w) = resolve(json!({"aprs_server":parent}), local, Value::Null);
        assert!(w.is_empty());
        assert_eq!(v.aprs_server, result);
    }
    for endpoint in [
        "host",
        "host:0",
        "host:65536",
        "http://host:5199",
        "[::1]:5199",
        "\u{e9}.example:5199",
        ":5199",
        "bad host:5199",
    ] {
        let (v, w) = resolve(Value::Null, json!({"aprs_server":endpoint}), Value::Null);
        assert!(
            w.iter()
                .any(|r| r.warning == ResourceWarning::InvalidValue("aprs_server"))
        );
        assert_eq!(v.aprs_server, "aprs.echolink.org:5199");
    }
    let (v, w) = resolve(
        json!({"aprs_server":"local.example:5199"}),
        json!({"transport":"proxy","aprs_server":"local.example:40000"}),
        Value::Null,
    );
    assert_eq!(v.aprs_server, "local.example:5199");
    assert!(
        w.iter()
            .any(|r| r.warning == ResourceWarning::InvalidValue("aprs_server"))
    );
    let (v, w) = resolve(
        Value::Null,
        json!({"transport":"proxy","aprs_server":""}),
        Value::Null,
    );
    assert!(w.is_empty());
    assert!(v.aprs_server.is_empty());
}
#[test]
fn echolink_node_values_override_valid_global_defaults_without_attaching_another_account() {
    let p=profiles(&[(SITE,ResourceKind::Echolink,"",json!({"max_stations":30,"rx":{"gain_db":-4},"announcement":"callsign","telemetry":"follow"})),
      (ONE,ResourceKind::Echolink,"",json!({"enabled":true,"callsign":"n0call-r","node_number":1002775,"max_stations":"bad","tx":{"gain_db":2.5}}))]).unwrap();
    let v = &p
        .port(ONE.parse().unwrap())
        .unwrap()
        .echolink
        .as_ref()
        .unwrap()
        .settings;
    assert_eq!(
        (v.callsign.as_str(), v.node_number, v.max_stations),
        ("N0CALL-R", 1002775, 30)
    );
    assert_eq!((v.receive_gain_db, v.transmit_gain_db), (-4.0, 2.5));
    assert_eq!(
        (v.announcement, v.telemetry),
        (EchoLinkAnnouncement::Callsign, EchoLinkTelemetry::Follow)
    );
    assert!(
        p.warnings()
            .iter()
            .any(|r| r.warning == ResourceWarning::InvalidValue("max_stations"))
    );
    assert!(p.port(TWO.parse().unwrap()).unwrap().echolink.is_none());
}
#[test]
fn echolink_metadata_modes_and_proxy_settings_are_typed() {
    let (value, warnings) = resolve(
        Value::Null,
        json!({
        "enabled":true,"callsign":"N0CALL-R","node_number":123456,"name":"Test station","location":"Test city",
        "email":"operator@example.invalid","status_message":"Station information","latitude":44.95,"longitude":-93.1,
        "frequency_mhz":146.94,"tone_hz":114.8,"power_w":25,"antenna_height_m":15,"antenna_gain_db":3,"antenna_direction_deg":180,
        "transport":"proxy","telemetry":"on","announcement":"both","telemetry_dynamic":false,
        "allow":["N0*","*TEST*"],"deny":["N0BAD-L"],"timing":{"directory_timeout_ms":12000,"registration_refresh_ms":300001,
        "directory_refresh_ms":240001,"directory_retry_ms":20001,"directory_freshness_ms":900001,"connect_timeout_ms":31000}}),
        json!({"host":"proxy.example.invalid","port":8101}),
    );
    assert!(warnings.is_empty());
    assert_eq!(value.transport, EchoLinkTransport::Proxy);
    assert_eq!(value.proxy_host, "proxy.example.invalid");
    assert_eq!(value.proxy_port, 8101);
    assert_eq!(value.name, "Test station");
    assert_eq!(value.location, "Test city");
    assert_eq!(value.email, "operator@example.invalid");
    assert_eq!(value.status_message, "Station information");
    assert_eq!((value.latitude, value.longitude), (44.95, -93.1));
    assert_eq!((value.frequency_mhz, value.tone_hz), (146.94, 114.8));
    assert_eq!(
        (
            value.power_w,
            value.antenna_height_m,
            value.antenna_gain_db,
            value.antenna_direction_deg
        ),
        (25.0, 15.0, 3.0, 180.0)
    );
    assert_eq!(value.allow, ["N0*", "*TEST*"]);
    assert_eq!(value.deny, ["N0BAD-L"]);
    assert_eq!(value.telemetry, EchoLinkTelemetry::On);
    assert_eq!(value.announcement, EchoLinkAnnouncement::Both);
    assert!(!value.telemetry_dynamic);
    assert_eq!(value.directory_timeout_ms, 12000);
    assert_eq!(value.registration_refresh_ms, 300001);
    assert_eq!(value.directory_refresh_ms, 240001);
    assert_eq!(value.directory_retry_ms, 20001);
    assert_eq!(value.directory_freshness_ms, 900001);
    assert_eq!(value.connect_timeout_ms, 31000);
    assert!(value.secrets_file.is_none());
}
#[test]
fn echolink_invalid_optional_values_warn_and_keep_defaults_without_secret_diagnostics() {
    let (value, w) = resolve(
        Value::Null,
        json!({"transport":"unknown","telemetry":"unknown","announcement":"unknown","latitude":"NaN","longitude":181,
        "audio_port":65535,"timing":{"heartbeat_interval_ms":0},"rx":{"gain_db":"NaN"},"allow":["bad pattern"],"directory_password":"private-placeholder"}),
        Value::Null,
    );
    assert_eq!(value.transport, EchoLinkTransport::Direct);
    assert_eq!(
        (
            value.audio_port,
            value.latitude,
            value.longitude,
            value.heartbeat_interval_ms,
            value.receive_gain_db
        ),
        (5198, 0.0, 0.0, 10000, -3.0)
    );
    assert_eq!(value.telemetry, EchoLinkTelemetry::Demand);
    assert!(w.len() >= 10);
    assert!(!format!("{w:?}").contains("private-placeholder"));
}
#[test]
fn incomplete_accounts_stay_disabled() {
    for body in [
        json!({"enabled":true}),
        json!({"enabled":true,"callsign":"N0CALL-R","node_number":0}),
        json!({"enabled":true,"callsign":"N0CALL-R","node_number":123456,"transport":"proxy"}),
    ] {
        let (v, w) = resolve(Value::Null, body, Value::Null);
        assert!(!v.enabled);
        assert!(
            w.iter()
                .any(|r| r.warning == ResourceWarning::InvalidValue("enabled"))
        );
    }
    assert!(
        resolve(
            Value::Null,
            json!({"enabled":true,"callsign":"*TEST*","node_number":123456}),
            Value::Null
        )
        .0
        .enabled
    );
}
#[test]
fn malformed_account_values_warn_and_resolve_without_crossing_account_boundaries() {
    for (key, input) in [
        ("callsign", ""),
        ("callsign", "N0CALL01234567890"),
        ("callsign", "**"),
        ("callsign", "*BAD!*"),
        ("callsign", "NOCALL"),
        ("callsign", "12345"),
        ("callsign", "N0!CALL"),
        ("latitude", "91"),
        ("antenna_gain_db", "inf"),
        ("name", "bad\u{7}name"),
        ("status_message", "bad\u{7}status"),
    ] {
        let (v, w) = resolve(Value::Null, json!({(key):input}), Value::Null);
        assert!(
            w.iter()
                .any(|r| r.warning == ResourceWarning::InvalidValue(key)),
            "{key}"
        );
        assert!(!v.enabled);
        assert_eq!(v.node_number, 0);
        assert!(v.callsign.is_empty());
    }
    for callsign in ["N0CALL", "N0CALL-L", "N0CALL-R", "W1/N0CALL", "*TEST_NET*"] {
        let (v, w) = resolve(Value::Null, json!({"callsign":callsign}), Value::Null);
        assert!(w.is_empty());
        assert_eq!(v.callsign, callsign);
    }
}
#[test]
fn separate_accounts_reject_only_ambiguous_identities_or_transport_endpoints() {
    for (first, second, valid) in [
        (
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            (
                json!({"bind_address": "127.0.0.2", "audio_port": 5198}),
                Value::Null,
            ),
            true,
        ),
        (
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5200}),
                Value::Null,
            ),
            true,
        ),
        (
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5200}),
                Value::Null,
            ),
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            true,
        ),
        (
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            false,
        ),
        (
            (
                json!({"bind_address": "0.0.0.0", "audio_port": 5198}),
                Value::Null,
            ),
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            false,
        ),
        (
            (
                json!({"bind_address": "127.0.0.1", "audio_port": 5198}),
                Value::Null,
            ),
            (
                json!({"bind_address": "0.0.0.0", "audio_port": 5198}),
                Value::Null,
            ),
            false,
        ),
        (
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example", "port": 8100}),
            ),
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example", "port": 8100}),
            ),
            false,
        ),
        (
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example", "port": 8100}),
            ),
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example", "port": 8101}),
            ),
            true,
        ),
        (
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example", "port": 8100}),
            ),
            (
                json!({"transport": "proxy"}),
                json!({"host": "two.example", "port": 8100}),
            ),
            true,
        ),
        (
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example"}),
            ),
            (json!({"transport": "direct"}), Value::Null),
            true,
        ),
        (
            (json!({"transport": "direct"}), Value::Null),
            (
                json!({"transport": "proxy"}),
                json!({"host": "one.example"}),
            ),
            true,
        ),
        (
            (json!({}), Value::Null),
            (
                json!({"node_number": 123456, "audio_port": 5200}),
                Value::Null,
            ),
            false,
        ),
    ] {
        let mut rows = Vec::new();
        for ((fields, proxy), port, number, callsign) in [
            (first, ONE, 123456, "N0CALL-R"),
            (second, TWO, 654321, "W1OTHER-L"),
        ] {
            let mut body = json!({"enabled":true,"node_number":number,"callsign":callsign});
            json_patch::merge(&mut body, &fields);
            rows.push((port, ResourceKind::Echolink, "", body));
            if !proxy.is_null() {
                rows.push((port, ResourceKind::EcholinkProxy, "", proxy));
            }
        }
        assert_eq!(profiles(&rows).is_ok(), valid, "{rows:?}");
    }
}
