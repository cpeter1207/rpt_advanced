use super::Engine;
use crate::{
    fixture::{self, station::LOCAL},
    lifecycle::{SKIP_TICKER_THREAD, tests as lifecycle},
};
use rpt_advanced_core::{
    command::{Command, LinkAction},
    config::resources::candidate::{ResourceKind as Kind, ResourceScope as Scope},
    management::routes::Target,
    runtime::{RuntimeClock, dtmf::DigitOperation},
    schedule::{CivilTime, Weekday},
};
use serde_json::{Value, json};
use std::{
    ptr,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

fn at(hour: u8, minute: u8, second: u8) -> RuntimeClock {
    let seconds = u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second);
    RuntimeClock {
        now_ms: seconds * 1000,
        wall_seconds: 1_795_586_400 + seconds as i64,
        wall_subsecond_ms: 0,
        civil: Some((
            CivilTime::new(2026, 11, 25, Weekday::Wednesday, hour, minute).unwrap(),
            second,
        )),
    }
}

fn settle(engine: &Arc<Engine>, clock: RuntimeClock) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !lifecycle::fixture_jobs_finished(engine) {
        assert!(Instant::now() < deadline, "fixture dial did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    engine.pump(clock, false).unwrap();
    engine.refresh_domain(None, true);
}

#[test]
fn cached_calendar_handoff_reports_boundary_disconnect_and_dial_outcomes() {
    let _serial = fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(lifecycle::fixture_stop(), 0);
    fixture::PEER_DIAL_RESULT.store(0, Ordering::Release);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let local = LOCAL.parse().unwrap();
    let scope = Scope::Port(local);
    let configuration = fixture::station::prepare(
        &[(LOCAL, "1000", json!({}))],
        &[
            (
                LOCAL,
                Kind::Interface,
                "",
                json!({"audio":{"device_identifier":"usb"}}),
            ),
            (
                LOCAL,
                Kind::Allstarlink,
                "",
                json!({
                    "node_number":1000, "iax":{"registration_url":""},
                    "static_nodes":{"2000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"}}
                }),
            ),
            (
                LOCAL,
                Kind::Macro,
                "connect",
                json!({"actions":[{"action":"connect","parameter":"2000"}]}),
            ),
            (
                LOCAL,
                Kind::Macro,
                "disconnect",
                json!({"actions":[{"action":"disconnect","parameter":"2000"}]}),
            ),
            (
                LOCAL,
                Kind::Macro,
                "warning",
                json!({"actions":[{"action":"rx_mute_toggle","target":"port"}]}),
            ),
            (
                LOCAL,
                Kind::Event,
                "coffee",
                json!({
                    "start":{"at":"every weekday at 09:30 in America/Chicago","macro":"connect"},
                    "end":{"at":"every weekday at 09:32 in America/Chicago","macro":"disconnect",
                           "warnings":[{"at":"every weekday at 09:31 in America/Chicago","macro":"warning"}]}
                }),
            ),
        ],
    );
    let event = configuration.runtime().scopes[&scope].events.events[0]
        .binding
        .id;
    // SAFETY: these linked fixture table entries and the leaked table live through Product stop.
    let mut table = unsafe { fixture::host_descriptor().read() };
    table.asl_provider = ptr::from_ref(&fixture::asl::PROVIDER);
    let engine = unsafe {
        lifecycle::fixture_engine_with_site(
            configuration,
            Box::leak(Box::new(table)),
            fixture::control_descriptor(),
            None,
            Some(at(9, 29, 59)),
        )
    }
    .unwrap();
    let observe = || {
        let observed = engine.modular_observed();
        assert!(observed.owners.contains_key(&local));
        let peer_attached = observed.domain.ports[&local]
            .peers
            .iter()
            .any(|peer| peer.kind == "allstarlink" && peer.node.as_deref() == Some("2000"));
        (
            observed
                .domain
                .fixture_status_fields(scope, None, Target::Metadata),
            observed
                .domain
                .fixture_status_fields(scope, Some(event), Target::Member(Kind::Event)),
            peer_attached,
        )
    };

    let before = at(9, 29, 59);
    let start = at(9, 30, 0);
    let warning = at(9, 31, 0);
    let end = at(9, 32, 0);

    engine.pump(before, true).unwrap();
    engine.pump(start, true).unwrap();
    settle(&engine, start);
    // Dial-job completion publishes the redirect. The peer worker independently
    // acknowledges it before the real HAL snapshot can report the attached ingress.
    let deadline = Instant::now() + Duration::from_secs(5);
    let started = loop {
        let (started, _, peer_attached) = observe();
        if peer_attached {
            break started;
        }
        assert!(
            Instant::now() < deadline,
            "scheduled dial attached its actual peer"
        );
        std::thread::sleep(Duration::from_millis(1));
        engine.pump(start, false).unwrap();
        engine.refresh_domain(None, true);
    };

    engine.pump(warning, true).unwrap();
    settle(&engine, warning);
    let (warned, _, peer_attached) = observe();
    assert!(peer_attached);
    assert_eq!(warned["receiver"]["muted"], true, "warning macro ran");

    engine.pump(end, true).unwrap();
    settle(&engine, end);
    let (ended, event_status, peer_attached) = observe();
    assert!(
        !peer_attached,
        "scheduled disconnect removed the actual peer"
    );
    assert_eq!(event_status.get("issue"), Some(&Value::Null));

    fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    let attempt = engine
        .operation(
            LOCAL,
            DigitOperation::new(
                Command {
                    node: "2000".to_owned(),
                    action: LinkAction::Transceive,
                },
                None,
            ),
        )
        .unwrap();
    engine
        .background(
            LOCAL.to_owned(),
            attempt,
            engine.revision.load(Ordering::Acquire),
            false,
        )
        .unwrap();
    settle(&engine, end);
    let (failed, _, peer_attached) = observe();
    assert!(!peer_attached);
    assert_eq!(fixture::PEER_DIAL_RESULT.load(Ordering::Acquire), 0);
    assert_eq!(lifecycle::fixture_stop(), 0);

    let boundary = |clock: RuntimeClock| {
        json!({
            "nominal_wall_seconds":clock.wall_seconds,
            "observed_wall_seconds":clock.wall_seconds,
            "outcome":"completed"
        })
    };
    assert_eq!(
        json!({
            "calendar":ended["calendar"],
            "start":event_status["last_start"],
            "end":event_status["last_end"],
            "warning":event_status["last_warning"],
            "disconnect":ended["last_disconnect"],
            "attached_dial":started["last_dial"],
            "failed_dial":failed["last_dial"]
        }),
        json!({
            "calendar":{"previous_wall_seconds":warning.wall_seconds,"current_wall_seconds":end.wall_seconds},
            "start":boundary(start),
            "end":boundary(end),
            "warning":{"boundary":"end","result":boundary(warning)},
            "disconnect":{"selected_count":1,"removed_count":1,"peer_nodes":["2000"]},
            "attached_dial":{"node":"2000","attached":true},
            "failed_dial":{"node":"2000","attached":false}
        }),
        "cached calendar handoff status must carry actual boundary, disconnect, and dial outcomes"
    );
}

#[test]
fn site_resources_are_never_projected_on_a_radio_port() {
    let sample = crate::management::resources::ResourceSampler::default()
        .poll(1, || None)
        .unwrap();
    let expected = serde_json::to_value(&sample).unwrap();
    let domain = crate::management::live::Domain {
        site_resources: Some(Arc::new(sample)),
        ..Default::default()
    };
    let id = LOCAL.parse().unwrap();
    assert_eq!(
        domain.fixture_status_fields(Scope::Site(id), None, Target::Metadata)["resources"],
        expected
    );
    assert!(
        domain
            .fixture_status_fields(Scope::Port(id), None, Target::Metadata)
            .get("resources")
            .is_none()
    );
}
