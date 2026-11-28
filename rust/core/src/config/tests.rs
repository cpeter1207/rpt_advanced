//! Station settings projected from the registered JSON scopes.
use crate::{
    command::{CommandMapping, DtmfCommandMap, LinkAction},
    config::{ResolvedDtmfRegenerationSettings, resources::candidate::ResourceKind},
    station_fixture::{PORT, SITE, configuration},
};
use serde_json::json;

#[test]
fn status_snapshot_interval_inherits_node_override_and_default() {
    let settings = |resources: &[_]| {
        configuration(&[(PORT, "radio")], resources)
            .station_settings(PORT.parse().unwrap())
            .unwrap()
    };
    assert_eq!(settings(&[]).status_snapshot_interval_ms, 50);
    let global = (
        SITE,
        ResourceKind::Radio,
        "",
        json!({"timing":{"status_snapshot_interval_ms":80}}),
    );
    assert_eq!(
        settings(std::slice::from_ref(&global)).status_snapshot_interval_ms,
        80
    );
    assert_eq!(
        settings(&[
            global,
            (
                PORT,
                ResourceKind::Radio,
                "",
                json!({"timing":{"status_snapshot_interval_ms":25}})
            )
        ])
        .status_snapshot_interval_ms,
        25
    );
}
#[test]
fn dtmf_regeneration_inherits_and_invalid_overrides_retain_safe_values() {
    let configured = |port| {
        configuration(&[(PORT,"radio")],&[
        (SITE,ResourceKind::Command,"",json!({"regeneration":{"enabled":true,"level_dbfs":-60,"timing":{"duration_ms":1,"gap_ms":1000}}})),
        (PORT,ResourceKind::Command,"",port)
    ]).station_settings(PORT.parse().unwrap()).unwrap().dtmf_regeneration
    };
    let inherited = ResolvedDtmfRegenerationSettings {
        enabled: true,
        duration_ms: 1,
        gap_ms: 1000,
        level_dbfs: -60,
    };
    for fields in [
        json!({}),
        json!({"enabled":"maybe"}),
        json!({"timing":{"duration_ms":0,"gap_ms":0}}),
        json!({"timing":{"duration_ms":1001,"gap_ms":1.5}}),
        json!({"level_dbfs":-61}),
        json!({"level_dbfs":1}),
    ] {
        assert_eq!(configured(json!({"regeneration":fields})), inherited);
    }
    assert_eq!(
        configured(
            json!({"regeneration":{"enabled":false,"timing":{"duration_ms":1000,"gap_ms":1},"level_dbfs":0}})
        ),
        ResolvedDtmfRegenerationSettings {
            enabled: false,
            duration_ms: 1000,
            gap_ms: 1,
            level_dbfs: 0
        }
    );
}
#[test]
fn command_prefix_overlap_depends_on_the_longer_mapping() {
    for (left, left_digits, right, right_digits, valid) in [
        (LinkAction::Status, "7", LinkAction::Time, "72", true),
        (LinkAction::Status, "7", LinkAction::Disconnect, "72", false),
        (LinkAction::Disconnect, "72", LinkAction::Status, "7", false),
        (LinkAction::Status, "7", LinkAction::FullStatus, "7", false),
    ] {
        let mappings = vec![
            CommandMapping::new(left_digits, left),
            CommandMapping::new(right_digits, right),
        ];
        assert_eq!(DtmfCommandMap::new(mappings).is_ok(), valid);
    }
}
#[test]
fn telemetry_duck_override_uses_bounded_signed_decibels() {
    for db in [-60, -17, 0] {
        let domain = configuration(
            &[(PORT, "radio")],
            &[(
                PORT,
                ResourceKind::TelemetrySettings,
                "",
                json!({"duck_db":db}),
            )],
        );
        assert_eq!(
            domain
                .station_settings(PORT.parse().unwrap())
                .unwrap()
                .telemetry_duck_db,
            db
        );
    }
}
#[test]
fn activity_ctcss_hang_accepts_valid_global_value_and_ignores_invalid_value() {
    let domain = configuration(
        &[(PORT, "radio")],
        &[
            (
                SITE,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"hang_ms":500}},"ctcss":{"on_input":true,"timing":{"hang_ms":250}}}),
            ),
            (
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ctcss":{"timing":{"hang_ms":501}}}),
            ),
        ],
    );
    let station = domain.station_settings(PORT.parse().unwrap()).unwrap();
    assert!(station.ctcss_encode_on_input);
    assert_eq!((station.hang_ms, station.ctcss_hang_ms), (500, 250));
}
#[test]
fn parrot_enabled_defaults_off_and_inherits_with_port_precedence() {
    let station = |rows: &[_]| {
        configuration(&[(PORT, "radio")], rows)
            .station_settings(PORT.parse().unwrap())
            .unwrap()
    };
    let site = (
        SITE,
        ResourceKind::Radio,
        "",
        json!({"parrot":{"enabled":true}}),
    );
    assert!(!station(&[]).parrot_enabled);
    assert!(station(std::slice::from_ref(&site)).parrot_enabled);
    assert!(
        !station(&[
            site,
            (
                PORT,
                ResourceKind::Radio,
                "",
                json!({"parrot":{"enabled":false}})
            )
        ])
        .parrot_enabled
    );
}
