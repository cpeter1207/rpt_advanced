//! Shared Host uses the existing live radio handle for station site observations.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use rpt_advanced_core::{
    config::resources::candidate::{ResourceKind as Kind, ResourceScope},
    site::ReadingQuality,
};

use crate::fixture::station::{self, LOCAL, SECOND_LOCAL};
use crate::management::PreparedConfiguration;
use serde_json::{Value, json};

type Row = (&'static str, Kind, &'static str, Value);

fn bridge_interface(device: &str) -> Value {
    json!({"audio":{"device_identifier":device},"gpio":{"wiring_profile":"dudeusb","pins":{"7":"out0","8":"in"}}})
}

fn pin(kind: Kind, key: &'static str, number: u8) -> Row {
    (
        LOCAL,
        kind,
        key,
        json!({"enabled":true,"source":{"type":"interface","node":"1000","property":format!("/gpio/pins/{number}")}}),
    )
}

fn bank_config(extra: &[Row], second: bool, pulse_ms: u64) -> PreparedConfiguration {
    let mut relay = pin(Kind::Output, "relay", 7);
    relay.3["startup_state"] = json!("on");
    relay.3["timing"] = json!({"pulse_ms":pulse_ms});
    let mut rows = vec![(LOCAL, Kind::Interface, "", bridge_interface("3-1")), relay];
    rows.extend_from_slice(extra);
    let mut stations = vec![(LOCAL, "1000", json!({}))];
    if second {
        rows.push((SECOND_LOCAL, Kind::Interface, "", bridge_interface("3-2")));
        stations.push((SECOND_LOCAL, "2000", json!({})));
    }
    station::prepare(&stations, &rows)
}

fn door() -> Row {
    pin(Kind::Input, "door", 8)
}

fn notice() -> Row {
    (
        LOCAL,
        Kind::Message,
        "notice",
        json!({"locales":{"en-US":{"text":"Notice","tts":"Notice","morse":"E"}}}),
    )
}

struct BankFixture {
    generation: u64,
    mask: u32,
    high: u32,
    writes: Vec<(u32, u32)>,
}
static BANK: Mutex<BankFixture> = Mutex::new(BankFixture {
    generation: 7,
    mask: 0,
    high: 0,
    writes: Vec::new(),
});
unsafe extern "C" fn bank_snapshot(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    result: *mut abi::UrpAstSiteSnapshot,
) -> i32 {
    let state = BANK.lock().unwrap();
    unsafe {
        (*result).generation = state.generation;
        (*result).output_mask = 0xff;
        (*result).input_mask = 0xff;
        (*result).input_quality = 1;
        (*result).observed_at_valid = 1;
        (*result).claimed_mask = state.mask;
        (*result).requested_high_mask = state.high;
        (*result).submitted_high_mask = state.high;
        (*result).serviced_high_mask = state.high;
        (*result).submitted_generation = state.generation;
        (*result).serviced_generation = state.generation;
    }
    0
}
unsafe extern "C" fn bank_bind(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteBinding,
) -> i32 {
    let value = unsafe { &*value };
    let mut state = BANK.lock().unwrap();
    assert_eq!(value.generation, state.generation);
    let added = value.output_mask & !state.mask;
    state.high = (state.high & !added) | (value.initial_high_mask & added);
    state.mask = value.output_mask;
    state.generation += 13;
    let write = (state.mask, state.high);
    state.writes.push(write);
    0
}
unsafe extern "C" fn bank_publish(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteRequest,
) -> i32 {
    let value = unsafe { &*value };
    let mut state = BANK.lock().unwrap();
    assert_eq!(value.generation, state.generation);
    state.high = value.high_mask;
    let write = (state.mask, state.high);
    state.writes.push(write);
    0
}
fn bank_host() -> Host {
    bank_host_with(&[], false, 500)
}
fn bank_host_with(extra: &[Row], second: bool, pulse_ms: u64) -> Host {
    bank_host_with_publish(bank_config(extra, second, pulse_ms), bank_publish)
}
fn bank_host_with_publish(
    configuration: PreparedConfiguration,
    publish: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        u32,
        *const abi::UrpAstSiteRequest,
    ) -> i32,
) -> Host {
    *BANK.lock().unwrap() = BankFixture {
        generation: 7,
        mask: 0,
        high: 0,
        writes: Vec::new(),
    };
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.asl_provider = &crate::fixture::asl::PROVIDER;
    table.radio_site_snapshot = Some(bank_snapshot);
    table.radio_site_bind = Some(bank_bind);
    table.radio_site_publish = Some(publish);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::fixture_start_with_site(
        configuration,
        media(),
        services,
        Instant::now(),
        clock(),
        Some(crate::site::backends::endpoint::tests::backends()),
    )
    .unwrap();
    host.poll_sites(0);
    host.poll_sites(1);
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    host
}

#[test]
fn site_host_alarm_actions_do_not_wait_for_full_telemetry_queue() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        &[
            door(),
            (
                LOCAL,
                Kind::Allstarlink,
                "",
                json!({"enabled":true,"node_number":1000}),
            ),
            (
                LOCAL,
                Kind::Alarm,
                "open",
                json!({"enabled":true,"source":"door","condition":"inactive","on_trip":["output:relay:off","inhibit:on","macro:stop"],"trip_message":"notice","repeat_message":"notice","timing":{"repeat_interval_ms":1}}),
            ),
            (
                LOCAL,
                Kind::Macro,
                "stop",
                json!({"actions":[{"action":"connect","parameter":"2000"}]}),
            ),
            notice(),
        ],
        false,
        500,
    );
    for _ in 0..4 {
        assert!(host.runtime.queue_named_message(LOCAL, "notice").unwrap());
    }
    assert!(!host.runtime.queue_named_message(LOCAL, "notice").unwrap());
    let mut operations = Vec::new();
    for now_ms in 10..14 {
        let clock = RuntimeClock { now_ms, ..clock() };
        host.pump(clock).unwrap();
        operations.extend(host.take_site_links());
        if let Some((effect, receipt)) = host.runtime.advance_commands(LOCAL, clock).unwrap() {
            operations.push((LOCAL.to_owned(), effect));
            receipt.settle(true);
        }
    }
    let snapshot = host.runtime.node(LOCAL).unwrap().site().snapshot(14);
    assert!(snapshot.alarms["open"].active);
    assert!(snapshot.transmit_inhibited);
    assert_eq!(BANK.lock().unwrap().high, 0);
    assert_eq!(
        operations.len(),
        1,
        "repeated notices must not replay actions"
    );
    assert!(matches!(&operations[0].1, LinkEffect::Connect(attempt) if attempt.remote() == "2000"));
    drop(operations);
    // Site pins must be released while the radio owner still services commands.
    for now in 14..20 {
        if host.site_shutdown(now) == Ok(true) {
            break;
        }
    }
    assert_eq!(host.site_shutdown(20), Ok(true));
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let mut audio = [0.25; 48];
    let keyed = owners
        .1
        .acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(true, false, &[], &mut audio);
    assert!(!keyed);
    assert_eq!(audio, [0.0; 48]);
    lease.lock().unwrap().owners = Some(owners);
    for now in 20..28 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(28));
}

#[test]
fn site_commands_preserve_protection_and_reject_during_owner_handoff() {
    use rpt_advanced_core::site::{OutputRequest, SiteCommand};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = bank_host_with(
        &[
            door(),
            (
                LOCAL,
                Kind::Alarm,
                "open",
                json!({"enabled":true,"source":"door","condition":"inactive","latched":true,"on_trip":["inhibit:on"],"on_clear":["inhibit:off"]}),
            ),
        ],
        false,
        500,
    );
    for now_ms in 10..14 {
        host.pump(RuntimeClock { now_ms, ..clock() }).unwrap();
    }
    assert!(
        host.site_command(LOCAL, SiteCommand::Inhibit(false), clock())
            .is_ok()
    );
    assert!(
        host.site_command(LOCAL, SiteCommand::AlarmReset("open".into()), clock())
            .is_err()
    );
    assert!(
        host.site_command(LOCAL, SiteCommand::AlarmAcknowledge("open".into()), clock())
            .is_ok()
    );
    host.site_command(
        LOCAL,
        SiteCommand::Output {
            name: "relay".into(),
            request: OutputRequest::Off,
        },
        clock(),
    )
    .unwrap();
    host.poll_sites(14);
    assert_eq!(BANK.lock().unwrap().high, 0);
    let snapshot = host.runtime.node(LOCAL).unwrap().site().snapshot(14);
    assert!(snapshot.transmit_inhibited);
    assert!(snapshot.alarms["open"].active && snapshot.alarms["open"].acknowledged);
    assert!(
        host.site_command("missing", SiteCommand::Inhibit(false), clock())
            .is_err()
    );
    assert!(
        host.site_command(
            LOCAL,
            SiteCommand::Output {
                name: "ptt".into(),
                request: OutputRequest::On,
            },
            clock()
        )
        .is_err()
    );
    let _ = host.site_shutdown(15);
    assert!(
        host.site_command(LOCAL, SiteCommand::Inhibit(false), clock())
            .is_err()
    );
    for now in 16..24 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(24));
}

#[test]
fn site_host_status_separates_requested_submitted_and_unavailable_readback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _hardware = crate::site::backends::endpoint::tests::physical_fixture();
    let mut host = bank_host_with(
        &[
            door(),
            (
                LOCAL,
                Kind::Sensor,
                "battery",
                json!({"enabled":true,"source":{"type":"hwmon","device":"fixture","channel":"in1_input"}}),
            ),
        ],
        false,
        500,
    );
    // Reproduce loss of the external acquisition while retaining the radio bank.
    drop(std::mem::take(&mut host.site_acquisition));
    host.pump(RuntimeClock {
        now_ms: 10,
        ..clock()
    })
    .unwrap();
    host.epoch = Instant::now() - std::time::Duration::from_secs(6);
    let text = host.status_text(LOCAL).unwrap();
    assert!(text.contains("site transmit-inhibited=false"), "{text}");
    assert!(
        text.contains("input door value=Some(false) quality=Valid age-ms=Some("),
        "{text}"
    );
    assert!(
        text.contains("sensor battery value=None quality=Unavailable"),
        "{text}"
    );
    assert!(text.contains("output relay requested-on=true"), "{text}");
    assert!(text.contains("submitted=Some((1.0,"), "{text}");
    assert!(text.contains("readback=unavailable"), "{text}");
    for now in 6000..6008 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(6008));
}

#[test]
fn site_host_shutdown_acknowledges_safe_release_before_destroying_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    for now in 2..8 {
        if host.stop(now) {
            break;
        }
        assert_eq!(
            crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
            drops
        );
    }
    assert!(host.fixture_stop(8));
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
}

#[test]
fn site_host_reload_release_then_abort_restores_live_demand_without_reopening() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host();
    let revision = host.runtime.revision();
    let backends = Arc::new(crate::site::backends::SiteBackends::load_scopes([]).unwrap());
    let mut candidate = Some(
        Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(RadioSite::default());
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        host.begin_site_reload(
            revision + 1,
            &mut candidate,
            &mut radio,
            &Default::default()
        ),
        Err(RuntimeError::Rejected)
    );
    assert!(candidate.is_some() && radio.is_some());
    assert_eq!(
        host.begin_site_reload(revision, &mut None, &mut radio, &Default::default()),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        host.begin_site_reload(revision, &mut candidate, &mut None, &Default::default()),
        Err(RuntimeError::Rejected)
    );
    let busy = Acquisition::prepare_reload(
        &Default::default(),
        Arc::new(crate::site::backends::SiteBackends::load_scopes([]).unwrap()),
        host.epoch,
        host.site_acquisition.keys(),
    )
    .unwrap();
    assert!(
        host.site_acquisition
            .begin(busy, &Default::default())
            .is_ok()
    );
    assert_eq!(
        host.begin_site_reload(revision, &mut candidate, &mut radio, &Default::default()),
        Err(RuntimeError::Busy)
    );
    assert!(candidate.is_some() && radio.is_some());
    drop(host.site_acquisition.abort());
    host.begin_site_reload(revision, &mut candidate, &mut radio, &Default::default())
        .unwrap();
    assert_eq!(
        host.begin_site_reload(revision, &mut candidate, &mut radio, &Default::default()),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(host.site_shutdown(2), Err(RuntimeError::Busy));
    let mut ready = false;
    for now in 2..8 {
        ready = host.site_barrier(now).unwrap();
        if ready {
            break;
        }
    }
    assert!(ready);
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
    drop(host.abort_sites());
    host.poll_sites(10);
    host.poll_sites(11);
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    for now in 12..18 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(18));
}

#[test]
fn site_host_fan_reports_delivery_failure_and_recovery_only_for_its_node() {
    use rpt_advanced_core::site::SiteCommand;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        &[(
            LOCAL,
            Kind::Fan,
            "cooling",
            json!({"enabled":true,"output":"relay","mode":"ptt","timing":{"run_on_ms":0}}),
        )],
        true,
        500,
    );
    host.advance_sites(clock());
    for (duty, expected, fault) in [
        (Some(1.0), 0x40, false),
        (None, 0, false),
        (Some(0.5), 0, true),
        (Some(1.0), 0x40, false),
    ] {
        let applied = host.site_command(
            LOCAL,
            SiteCommand::Fan {
                name: "cooling".into(),
                duty,
            },
            clock(),
        );
        assert_eq!(
            applied,
            if fault {
                Err(rpt_advanced_core::site::SiteError::Unavailable)
            } else {
                Ok(())
            }
        );
        host.poll_sites(1);
        host.poll_sites(2);
        let text = host.status_text(LOCAL).unwrap();
        assert!(text.contains("fan cooling requested-duty="), "{text}");
        assert_eq!(
            text.contains("site-action cooling fault=Unavailable"),
            fault,
            "{text}"
        );
        assert!(
            !host
                .status_text(SECOND_LOCAL)
                .unwrap()
                .contains("site-action cooling")
        );
        assert_eq!(BANK.lock().unwrap().high, expected);
    }
    for now in 3..12 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(12));
}

#[test]
fn site_host_reports_unavailable_actions_and_retired_notification_names() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _hardware = crate::site::backends::endpoint::tests::physical_fixture();
    let mut host = bank_host_with(
        &[
            door(),
            (
                LOCAL,
                Kind::Output,
                "external",
                json!({"enabled":true,"source":{"type":"gpiod","device":"fixture","line":"7"}}),
            ),
            (
                LOCAL,
                Kind::Alarm,
                "open",
                json!({"enabled":true,"source":"door","condition":"inactive","on_trip":["output:external:on","macro:stop"],"trip_message":"notice"}),
            ),
            (
                LOCAL,
                Kind::Macro,
                "stop",
                json!({"actions":[{"action":"disconnect_all"}]}),
            ),
            notice(),
        ],
        false,
        500,
    );
    drop(std::mem::take(&mut host.site_acquisition));
    let mut settings = host.runtime.modular_configuration().unwrap().scopes
        [&ResourceScope::Port(LOCAL.parse().unwrap())]
        .equipment
        .settings
        .clone();
    // Reproduce a queued notification whose catalog identity has been retired.
    settings.alarms[0].trip_message = "missing".into();
    host.runtime
        .node(LOCAL)
        .unwrap()
        .site()
        .reconfigure(settings);
    host.advance_sites(clock());
    let text = host.status_text(LOCAL).unwrap();
    for expected in [
        "alarm open active=true",
        "site-action external fault=Unavailable",
        "site-action alarm:open fault=Unavailable",
        "site-action missing fault=Unavailable",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(!text.contains("site-action stop"), "{text}");
    assert!(host.take_site_links().is_empty());
    for now in 3..12 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(12));
}

#[test]
fn site_host_basis_fences_only_removed_disabled_or_replaced_radios() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host();
    let next = |enabled: bool, device: &str, radio: Value| {
        station::prepare(
            &[(LOCAL, "1000", json!({"enabled": enabled}))],
            &[
                (LOCAL, Kind::Interface, "", bridge_interface(device)),
                (LOCAL, Kind::Radio, "", radio),
            ],
        )
    };
    for (next, replaced) in [
        (next(true, "3-1", json!({})), false),
        (station::prepare(&[], &[]), true),
        (next(false, "3-1", json!({})), true),
        (next(true, "3-3", json!({})), true),
        (next(true, "3-1", json!({"duplex_mode":"full"})), true),
    ] {
        let ticket = host.stage_radio_plans(next.runtime()).unwrap();
        let basis = host.modular_site_basis(&ticket).unwrap();
        assert_eq!(basis.revision, host.runtime.revision());
        assert_eq!(basis.replaced.contains(LOCAL), replaced);
        assert!(basis.known.is_empty());
    }
    assert_eq!(host.commit_sites(), Ok(()));
    let _ = host.site_shutdown(2);
    host.advance_sites(clock());
    assert_eq!(host.commit_sites(), Ok(()));
    for now in 3..12 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(12));
}

#[test]
fn site_host_failed_safe_write_blocks_reload_and_preserves_the_live_owner() {
    static FAIL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    unsafe extern "C" fn publish(
        context: *mut c_void,
        radio: *mut c_void,
        bank: u32,
        value: *const abi::UrpAstSiteRequest,
    ) -> i32 {
        if FAIL.load(std::sync::atomic::Ordering::Relaxed) {
            -1
        } else {
            unsafe { bank_publish(context, radio, bank, value) }
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    FAIL.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut host = bank_host_with_publish(bank_config(&[], false, 500), publish);
    let backends = Arc::new(crate::site::backends::SiteBackends::load_scopes([]).unwrap());
    let mut candidate = Some(
        Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(RadioSite::default());
    host.begin_site_reload(
        host.runtime.revision(),
        &mut candidate,
        &mut radio,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(host.site_barrier(2), Err(RuntimeError::Device));
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    drop(host.abort_sites());
    FAIL.store(false, std::sync::atomic::Ordering::Relaxed);
    for now in 3..12 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(12));
}

#[test]
fn site_host_reload_keeps_unchanged_output_timers_and_alarm_protection_running() {
    use rpt_advanced_core::site::{OutputRequest, SiteCommand};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        &[
            door(),
            (
                LOCAL,
                Kind::Alarm,
                "open",
                json!({"enabled":true,"source":"door","condition":"inactive","timing":{"trip_delay_ms":50},"on_trip":["inhibit:on"]}),
            ),
        ],
        false,
        20,
    );
    host.advance_sites(RuntimeClock {
        now_ms: 0,
        ..clock()
    });
    for request in [OutputRequest::Off, OutputRequest::Pulse] {
        host.site_command(
            LOCAL,
            SiteCommand::Output {
                name: "relay".into(),
                request,
            },
            RuntimeClock {
                now_ms: 0,
                ..clock()
            },
        )
        .unwrap();
    }
    let backends = Arc::new(crate::site::backends::SiteBackends::load_scopes([]).unwrap());
    let mut candidate = Some(
        Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(
        RadioSite::prepare(
            &[(
                LOCAL.into(),
                host.runtime.modular_configuration().unwrap().acquisition
                    [&ResourceScope::Port(LOCAL.parse().unwrap())]
                    .clone(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap(),
    );
    host.begin_site_reload(
        host.runtime.revision(),
        &mut candidate,
        &mut radio,
        &Default::default(),
    )
    .unwrap();
    // Hold publication as though another physical owner has not acknowledged yet.
    host.advance_sites(RuntimeClock {
        now_ms: 100,
        ..clock()
    });
    host.poll_sites(101);
    let state = host.runtime.node(LOCAL).unwrap().site().snapshot(101);
    assert!(!state.outputs["relay"].requested_on);
    assert_eq!(state.outputs["relay"].pulse_deadline_ms, None);
    assert!(state.alarms["open"].active);
    assert!(state.transmit_inhibited);
    assert_eq!(BANK.lock().unwrap().high, 0);
    drop(host.abort_sites());
    for now in 102..110 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.fixture_stop(110));
}

unsafe extern "C" fn observation(
    context: *mut c_void,
    handle: *mut c_void,
    bank: u32,
    output: *mut abi::UrpAstSiteSnapshot,
) -> i32 {
    // SAFETY: both fixtures retain the expected station name and live radio handle.
    let owner = unsafe { std::ffi::CStr::from_ptr(context.cast()) }
        .to_str()
        .unwrap();
    assert_eq!(unsafe { crate::fixture::radio_name(handle) }, owner);
    assert_eq!(bank, 0);
    unsafe {
        (*output).generation = 7;
        (*output).input_mask = 0x80;
        (*output).input_high_mask = 0x80;
        (*output).input_quality = 1;
        (*output).observed_at_ms = 950;
        (*output).observed_at_valid = 1;
        (*output).snapshot_at_ms = 1000;
    }
    0
}

#[test]
fn site_host_publishes_site_replacement_with_runtime_before_post_reload_polling() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for notification_error in [false, true] {
        let mut host = bank_host();
        let backends = Arc::new(crate::site::backends::SiteBackends::load_scopes([]).unwrap());
        let mut candidate = Some(
            Acquisition::prepare_reload(
                &Default::default(),
                backends,
                host.epoch,
                host.site_acquisition.keys(),
            )
            .unwrap(),
        );
        let mut radio = Some(RadioSite::default());
        host.begin_site_reload(
            host.runtime.revision(),
            &mut candidate,
            &mut radio,
            &Default::default(),
        )
        .unwrap();
        for now in 2..8 {
            if host.site_barrier(now).unwrap() {
                break;
            }
        }
        let mut session = None;
        if notification_error {
            host.runtime.incoming(LOCAL, "2000", true).unwrap();
            host.runtime.incoming(LOCAL, "3000", true).unwrap();
            let (io, state) = peer(48000);
            let (_producer, outbound) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
            let (pending, control) =
                PeerSession::prepare(io, outbound, "1000", "2000", true).unwrap();
            host.peers.push(PeerOwner {
                superseded: false,
                local: LOCAL.into(),
                remote: "2000".into(),
                mode: Mode::TRANSCEIVE,
                group: None,
                announced: true,
                control,
            });
            session = Some((pending, state));
        }
        let revision = host.runtime.revision();
        let next = station::prepare(
            &[(LOCAL, "1000", json!({}))],
            &[(LOCAL, Kind::Interface, "", bridge_interface("3-1"))],
        );
        let ticket = host.stage_radio_plans(next.runtime()).unwrap();
        host.prepare_core(&ticket, next.runtime().clone(), clock())
            .unwrap();
        let credentials = host
            .bind_core_credentials(&ticket, next.runtime(), Default::default())
            .unwrap();
        let mut result = host.begin_core_adoption(
            ticket,
            "post-site".into(),
            credentials,
            next.inputs().clone(),
            clock(),
        );
        let timeout = Instant::now() + std::time::Duration::from_secs(5);
        while matches!(result, Ok(false)) {
            assert!(Instant::now() < timeout);
            result = host.poll_reload(clock());
            std::thread::yield_now();
        }
        result.unwrap();
        if let Some((pending, state)) = &mut session {
            // A managed station only accepts D frames from its currently attached ingress.
            let reply = host.peers[0].control.reply_target();
            host.runtime
                .node(LOCAL)
                .unwrap()
                .links()
                .attach_ingress(PeerIdentity::from_operator("2000").unwrap(), reply)
                .unwrap();
            state
                .lock()
                .unwrap()
                .input
                .push_back(Input::Text(b"D 3000 4000 11 9"));
            pending.step(clock().now_ms).unwrap();
        }
        let result = host.pump(clock());
        assert_eq!(
            result,
            if notification_error {
                Err(RuntimeError::Rejected)
            } else {
                Ok(())
            }
        );
        assert!(host.runtime.revision() > revision);
        host.poll_sites(12);
        let removed = host.site_radio.output_status(LOCAL, "relay").is_none();
        drop(host.abort_sites());
        for now in 13..20 {
            if host.stop(now) {
                break;
            }
        }
        assert!(host.fixture_stop(20));
        assert!(
            removed,
            "published configuration must not retain the removed site binding"
        );
        drop(session);
    }
}

#[test]
fn site_engine_shutdown_drives_safe_barriers_before_stopping_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let api = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    *BANK.lock().unwrap() = BankFixture {
        generation: 7,
        mask: 0,
        high: 0,
        writes: Vec::new(),
    };
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.radio_site_snapshot = Some(bank_snapshot);
    table.radio_site_bind = Some(bank_bind);
    table.radio_site_publish = Some(bank_publish);
    // SAFETY: the immutable leaked table and static control provider outlive all Engine calls.
    let engine = unsafe {
        crate::lifecycle::tests::fixture_engine(
            bank_config(&[], false, 500),
            Box::leak(Box::new(table)),
            crate::fixture::control_descriptor(),
        )
    }
    .unwrap();
    drop(engine);
    let (engine, call) = crate::lifecycle::selected().unwrap();
    engine
        .run(|host| {
            host.poll_sites(1);
            host.poll_sites(2);
            Ok(())
        })
        .unwrap();
    drop(call);
    let stopped = unsafe { api.stop.unwrap()() };
    // Clean the real fixture even while the missing lifecycle drive is under test.
    for _ in 0..5 {
        if unsafe { api.stop.unwrap()() } == 0 {
            break;
        }
    }
    assert_eq!(stopped, 0);
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
}

#[test]
fn site_host_polls_configured_owner_without_a_second_radio_reservation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = c"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        .as_ptr()
        .cast_mut()
        .cast();
    table.radio_site_snapshot = Some(observation);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let config = station::prepare(
        &[(LOCAL, "1000", json!({}))],
        &[
            (LOCAL, Kind::Interface, "", bridge_interface("3-1")),
            (
                LOCAL,
                Kind::Input,
                "door",
                json!({"enabled":true,"active_level":"low","source":{"type":"interface","node":"1000","property":"/gpio/pins/8"}}),
            ),
        ],
    );
    let mut host = Host::fixture_start(config, media(), services, Instant::now(), clock()).unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    let values = host.poll_sites(2000);
    let input = &values[LOCAL].inputs["door"];
    assert_eq!(input.value, Some(false));
    assert_eq!(input.observed_at_ms, Some(1950));
    assert_eq!(input.quality, ReadingQuality::Valid);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    assert!(host.fixture_stop(2100));
}

#[test]
fn round_one_host_merges_cached_sensor_calibration_quality_and_original_time() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let identity = c"929e52be-6854-4c46-85f2-000000000002";
    let owner = identity.to_str().unwrap();
    table.context = identity.as_ptr().cast_mut().cast();
    table.radio_site_snapshot = Some(observation);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let config = station::prepare(
        &[(owner, "1000", json!({}))],
        &[
            (owner, Kind::Interface, "", bridge_interface("3-1")),
            (
                owner,
                Kind::Input,
                "door",
                json!({"enabled":true,"active_level":"low","source":{"type":"interface","node":"1000","property":"/gpio/pins/8"}}),
            ),
            (
                owner,
                Kind::Sensor,
                "sense",
                json!({"enabled":true,"source":{"type":"interface","node":"1000","property":"/gpio/pins/8"},"unit":"V","precision":1,"timing":{"poll_interval_ms":100,"stale_after_ms":5000},"calibration":{"mode":"two_point","input":{"low":0,"high":1},"value":{"low":5,"high":20}}}),
            ),
        ],
    );
    let sensor = config.runtime().scopes[&ResourceScope::Port(owner.parse().unwrap())]
        .equipment
        .settings
        .sensors[0]
        .clone();
    let mut state = rpt_advanced_core::site::SensorState::new(sensor);
    let mut host = Host::fixture_start(config, media(), services, Instant::now(), clock()).unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    let values = host.poll_sites(2000);
    let raw = &values[owner].sensors["sense"];
    assert_eq!(
        (raw.value, raw.quality, raw.observed_at_ms),
        (Some(1.0), ReadingQuality::Valid, Some(1950))
    );
    state.observe(raw.clone(), 2000);
    let first = state.snapshot(2000);
    assert_eq!(first.current.value, Some(20.0));
    assert_eq!(first.current.observed_at_ms, Some(1950));
    // The cached owner returns the same device snapshot on a later control tick.
    // Its last observation must keep its original time and expire at the original deadline.
    let again = host.poll_sites(2010);
    assert_eq!(again[owner].sensors["sense"].observed_at_ms, Some(1950));
    assert_eq!(again[owner].inputs["door"].observed_at_ms, Some(1950));
    state.observe(again[owner].sensors["sense"].clone(), 2010);
    assert_eq!(state.snapshot(2010), first);
    let expired = host.poll_sites(7000);
    state.observe(expired[owner].sensors["sense"].clone(), 7000);
    assert_eq!(state.snapshot(7000).current.quality, ReadingQuality::Stale);
    let input = &values[owner].inputs["door"];
    assert_eq!(input.value, Some(false));
    assert_eq!(input.observed_at_ms, Some(1950));
    assert_eq!(input.quality, ReadingQuality::Valid);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    assert!(host.fixture_stop(7100));
}
