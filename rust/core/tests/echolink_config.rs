//! Account resolution must not borrow another node's identity or alter radio gains.
use rpt_advanced_core::config::{
    ConfigDocument, EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport,
    NodeId, Schema,
};

fn node() -> NodeId {
    NodeId::new("524950").unwrap()
}
fn resolve(
    source: &str,
) -> Result<
    rpt_advanced_core::config::Resolution<EchoLinkSettings>,
    rpt_advanced_core::config::ConfigError,
> {
    EchoLinkSettings::resolve(&ConfigDocument::parse(source).unwrap(), &node())
}

#[test]
fn echolink_defaults_are_disabled_and_have_no_identity_or_secrets() {
    let document = ConfigDocument::parse("[524950]\n[echolink]\n").unwrap();
    assert_eq!(document.nodes(), ["524950"]);
    let settings = EchoLinkSettings::resolve(&document, &node()).unwrap();
    assert!(settings.warnings.is_empty());
    let value = settings.value;
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
fn echolink_node_values_override_valid_global_defaults_without_identity_inheritance() {
    let source = "[524950]\n[508422]\n[echolink]\nmax_stations=30\nreceive_gain_db=-4\nannouncement=callsign\ntelemetry=follow\ncallsign=N0WRONG-R\nnode_number=123456\n[echolink 524950]\nenabled=yes\ncallsign=n0call-r\nnode_number=1002775\nmax_stations=bad\ntransmit_gain_db=2.5\n";
    let settings = resolve(source).unwrap();
    assert_eq!(settings.value.callsign, "N0CALL-R");
    assert_eq!(settings.value.node_number, 1002775);
    assert_eq!(settings.value.max_stations, 30);
    assert_eq!(
        (
            settings.value.receive_gain_db,
            settings.value.transmit_gain_db
        ),
        (-4.0, 2.5)
    );
    assert_eq!(settings.value.announcement, EchoLinkAnnouncement::Callsign);
    assert_eq!(settings.value.telemetry, EchoLinkTelemetry::Follow);
    assert!(
        settings
            .warnings
            .iter()
            .any(|warning| warning.key == "max_stations" && warning.fallback == "30")
    );
    let other = EchoLinkSettings::resolve(
        &ConfigDocument::parse(source).unwrap(),
        &NodeId::new("508422").unwrap(),
    )
    .unwrap()
    .value;
    assert!(other.callsign.is_empty());
    assert_eq!(other.node_number, 0);
}

#[test]
fn echolink_metadata_modes_and_proxy_settings_are_typed() {
    let source = "[524950]\n[echolink 524950]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123456\nname=Test station\nlocation=Test city\nemail=operator@example.invalid\nstatus_message=Station information\nlatitude=44.95\nlongitude=-93.1\nfrequency_mhz=146.94\ntone_hz=114.8\npower_w=25\nantenna_height_m=15\nantenna_gain_db=3\nantenna_direction_deg=180\ntransport=proxy\nproxy_host=proxy.example.invalid\nproxy_port=8101\ntelemetry=on\nannouncement=both\ntelemetry_dynamic=no\nallow=N0*,*TEST*\ndeny=N0BAD-L\nsecrets_file=private.conf\ndirectory_timeout_ms=12000\nregistration_refresh_ms=300001\ndirectory_refresh_ms=240001\ndirectory_retry_ms=20001\ndirectory_freshness_ms=900001\nconnect_timeout_ms=31000\n";
    let value = resolve(source).unwrap().value;
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
    assert_eq!(
        value.secrets_file.as_deref(),
        Some(std::path::Path::new("private.conf"))
    );
}

#[test]
fn echolink_invalid_optional_values_warn_and_keep_defaults_without_secret_diagnostics() {
    let source = "[524950]\n[echolink 524950]\ntransport=unknown\ntelemetry=unknown\nannouncement=unknown\nlatitude=NaN\nlongitude=181\naudio_port=65535\nheartbeat_interval_ms=0\nreceive_gain_db=NaN\nallow=bad pattern\ndirectory_password=private-placeholder\n";
    let settings = resolve(source).unwrap();
    assert_eq!(settings.value.transport, EchoLinkTransport::Direct);
    assert_eq!(settings.value.audio_port, 5198);
    assert_eq!(settings.value.latitude, 0.0);
    assert_eq!(settings.value.longitude, 0.0);
    assert_eq!(settings.value.heartbeat_interval_ms, 10000);
    assert_eq!(settings.value.receive_gain_db, -3.0);
    assert_eq!(settings.value.telemetry, EchoLinkTelemetry::Demand);
    assert!(settings.warnings.len() >= 10);
    assert!(!format!("{:?}", settings.warnings).contains("private-placeholder"));
}

#[test]
fn echolink_enabled_accounts_need_unique_identity_and_unambiguous_endpoints() {
    for source in [
        "[524950]\n[echolink]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123456\n",
        "[524950]\n[echolink 524950]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=0\n",
        "[524950]\n[echolink 524950]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123456\ntransport=proxy\n",
        "[524950]\n[508422]\n[echolink 524950]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123456\n[echolink 508422]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123457\naudio_port=5200\n",
        "[524950]\n[508422]\n[echolink 524950]\nenabled=yes\ncallsign=N0CALL-R\nnode_number=123456\n[echolink 508422]\nenabled=yes\ncallsign=N0OTHER-R\nnode_number=123457\naudio_port=5199\n",
    ] {
        let document = ConfigDocument::parse(source).unwrap();
        assert!(Schema::validate(&document).is_err(), "{source}");
    }
    assert!(
        resolve("[524950]\n[echolink 524950]\nenabled=yes\ncallsign=*TEST*\nnode_number=123456\n")
            .is_ok()
    );
}
