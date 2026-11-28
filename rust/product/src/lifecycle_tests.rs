use super::*;
#[path = "lifecycle_fixture_tests.rs"]
mod fixture;
pub(crate) use fixture::{
    fixture_engine, fixture_engine_with_site, fixture_jobs_finished, fixture_stop,
};
use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::dtmf::DigitOperation,
};
use std::{
    ffi::c_void,
    ptr,
    sync::atomic::Ordering,
    time::{Duration, Instant, UNIX_EPOCH},
};

impl Engine {
    /// Inspect a published generation once the core reload has left its publication fence.
    pub(crate) fn fixture_published_generation(&self, revision: &str, port: &str) -> Option<u64> {
        if self.reloading.load(Ordering::Acquire) {
            return None;
        }
        let mut host = self.host.lock().unwrap();
        let host = host.as_mut()?;
        if !host.modular_revision_matches(revision) {
            return None;
        }
        Some(host.runtime.node(port)?.command_work()?.id())
    }

    /// Force the same scheduled cycle the real ticker runs once per civil second.
    pub(crate) fn fixture_schedule_tick(self: &Arc<Self>) -> Result<(), RuntimeError> {
        self.pump(self.clock(), true)
    }

    pub(crate) fn start_fixture_ticker(self: &Arc<Self>) {
        self.start_ticker().unwrap();
    }
}

fn descriptor() -> &'static abi::rptadv_product_descriptor_v1 {
    unsafe { &*rptadv_product_descriptor_v1() }
}

use crate::{
    fixture::station::{self, LOCAL, SECOND_LOCAL},
    management::{
        AdoptionOutcome, PreparedConfiguration, Runtime as ManagementRuntime, native::NativeRuntime,
    },
};
use rpt_advanced_core::config::resources::candidate::ResourceKind as Kind;
use serde_json::{Value, json};

type Row = (&'static str, Kind, &'static str, Value);

fn active_configuration(rows: &[Row]) -> PreparedConfiguration {
    let mut fields = rows.to_vec();
    if !fields
        .iter()
        .any(|row| row.0 == LOCAL && row.1 == Kind::Interface)
    {
        fields.push(radio("usb"));
    }
    if !fields.iter().any(|row| row.1 == Kind::Allstarlink) {
        fields.push((LOCAL, Kind::Allstarlink, "", json!({"node_number":1000,"lookup_method":"file","directory_file":"fixture","static_nodes":{"2000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"}},"iax":{"registration_url":""}})));
    }
    station::prepare(&[(LOCAL, "1000", json!({}))], &fields)
}

fn idle_configuration() -> PreparedConfiguration {
    station::prepare(&[], &[])
}

fn radio(device: &str) -> Row {
    (
        LOCAL,
        Kind::Interface,
        "",
        json!({"audio":{"device_identifier":device},"gpio":{"wiring_profile":"dudeusb","pins":{"7":"out0","8":"in"}}}),
    )
}

fn door() -> Row {
    (
        LOCAL,
        Kind::Input,
        "door",
        json!({"enabled":true,"source":{"type":"interface","node":"1000","property":"/gpio/pins/8"}}),
    )
}

fn battery() -> Row {
    (
        LOCAL,
        Kind::Sensor,
        "battery",
        json!({"enabled":false,"unit":"volts","dtmf_id":"1"}),
    )
}

// Each synchronous originate hands its exact identity to the peer metadata callback.
thread_local! {
    static DIAL_REMOTE: std::cell::Cell<&'static [u8]> = const { std::cell::Cell::new(b"2000") };
}
type HostDial = unsafe extern "C" fn(
    *mut c_void,
    *const std::ffi::c_char,
    usize,
    *const std::ffi::c_char,
    usize,
    usize,
    abi::rptadv_current_v1,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
static DIAL_CALLBACK: Mutex<Option<HostDial>> = Mutex::new(None);

unsafe extern "C" fn originate(
    context: *mut c_void,
    identity: *const abi::rptadv_asl_identity_v1,
    remote: abi::rptadv_peer_bytes_v1,
    endpoint: abi::rptadv_peer_bytes_v1,
    maximum: usize,
    current: abi::rptadv_current_v1,
    state: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    let id = unsafe { std::slice::from_raw_parts(remote.data, remote.length) };
    let Some(name) = [
        b"2000", b"2001", b"2002", b"2003", b"3000", b"3500", b"3600", b"4000",
    ]
    .into_iter()
    .find(|n| n.as_slice() == id) else {
        return -1;
    };
    let dial = *DIAL_CALLBACK.lock().unwrap();
    let Some(dial) = dial else { return -1 };
    let local = unsafe { (*identity).wire }.to_string();
    let status = unsafe {
        dial(
            context,
            endpoint.data.cast(),
            endpoint.length,
            local.as_ptr().cast(),
            local.len(),
            maximum,
            current,
            state,
            out,
        )
    };
    if status == 0 {
        DIAL_REMOTE.set(name);
    }
    status
}

unsafe extern "C" fn info(
    context: *mut c_void,
    peer: *const c_void,
    out: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    let result = unsafe { crate::fixture::peer_info(context, peer, out) };
    if result == 0 {
        let bytes = DIAL_REMOTE.get();
        unsafe {
            (*out).remote = abi::rptadv_peer_bytes_v1 {
                data: bytes.as_ptr(),
                length: bytes.len(),
            }
        };
    }
    result
}

unsafe fn start_table(
    configuration: PreparedConfiguration,
    table: *const abi::rptadv_host_services_v11,
) -> i32 {
    if table.is_null() {
        return i32::from(
            unsafe { fixture_engine(configuration, table, crate::fixture::control_descriptor()) }
                .is_err(),
        );
    }
    let mut table = unsafe { table.read() };
    static PROVIDER: std::sync::OnceLock<abi::rptadv_asl_provider_v2> = std::sync::OnceLock::new();
    table.asl_provider = PROVIDER.get_or_init(|| {
        let mut provider = unsafe { ptr::from_ref(&crate::fixture::asl::PROVIDER).read() };
        provider.originate = Some(originate);
        provider
    });
    *DIAL_CALLBACK.lock().unwrap() = table.peer_dial;
    if table.peer_info.is_some_and(|callback| {
        std::ptr::fn_addr_eq(
            callback,
            crate::fixture::peer_info as unsafe extern "C" fn(_, _, _) -> _,
        )
    }) {
        table.peer_info = Some(info);
    }
    DIAL_REMOTE.set(b"2000");
    i32::from(
        unsafe {
            fixture_engine(
                configuration,
                Box::leak(Box::new(table)),
                crate::fixture::control_descriptor(),
            )
        }
        .is_err(),
    )
}

unsafe fn start(configuration: PreparedConfiguration) -> i32 {
    unsafe { start_table(configuration, crate::fixture::host_descriptor()) }
}

/// Await the real worker redirect acknowledgment before testing commands on its ingress.
fn wait_for_ingress(engine: &Arc<Engine>, remote: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        let remote = remote.to_owned();
        if engine
            .run(move |host| {
                Ok(host.runtime.node(LOCAL).is_some_and(|node| {
                    node.links()
                        .ingress_receive_status()
                        .any(|(identity, _)| identity.node() == Some(remote.as_str()))
                }))
            })
            .unwrap()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "peer redirect was not acknowledged"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn admission(remote: &str, source: &str) -> Result<*mut c_void, RuntimeError> {
    let (engine, _guard) = selected()?;
    let identity = engine.run(|host| Ok(host.asl_account(LOCAL.parse().unwrap())?.identity()))?;
    let span = |s: &str| abi::rptadv_peer_bytes_v1 {
        data: s.as_ptr(),
        length: s.len(),
    };
    let mut proof = ptr::null_mut();
    let status = unsafe {
        super::asl::ADMISSION.authorize.unwrap()(&identity, span(remote), span(source), &mut proof)
    };
    if status == 0 && !proof.is_null() {
        Ok(proof)
    } else {
        Err(RuntimeError::Rejected)
    }
}

pub(super) fn reload(
    engine: &Arc<Engine>,
    configuration: PreparedConfiguration,
) -> Result<AdoptionOutcome, RuntimeError> {
    let runtime = NativeRuntime::new(engine.clone());
    let revision = format!("edit-{}", engine.revision.load(Ordering::Acquire));
    let mut prepared = runtime
        .prepare(&configuration, &revision)
        .map_err(|_| RuntimeError::Preparation)?;
    runtime
        .prepare_credentials(
            &mut prepared,
            &crate::configuration_private::Credentials::default(),
        )
        .map_err(|_| RuntimeError::Preparation)?;
    engine.apply_modular(prepared)
}

#[test]
fn startup_waits_for_the_radio_activation_acknowledgement() {
    use std::sync::atomic::AtomicUsize;
    static PENDING: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        i32::from(
            PENDING
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok(),
        )
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    // Initial station activation needs the actual fake RX/TX callbacks as well
    // as the delayed readiness receipt; null context intentionally idles them.
    table.context = ptr::without_provenance_mut(1);
    table.radio_ready = Some(ready);
    table.asl_provider = &crate::fixture::asl::PROVIDER;
    PENDING.store(10, Ordering::Release);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let source = active_configuration(&[]);
    assert_eq!(unsafe { start_table(source, &table) }, 0);
    assert_eq!(PENDING.load(Ordering::Acquire), 0);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn activation_reload_poll_stays_admitted_until_commit_or_safe_rollback() {
    use std::sync::atomic::AtomicI32;
    static ACTIVATION: AtomicI32 = AtomicI32::new(0);
    unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
        let status = ACTIVATION.load(Ordering::Acquire);
        if status < 0 {
            ACTIVATION.store(0, Ordering::Release);
        }
        status
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let api = descriptor();
    unsafe { api.stop.unwrap()() };
    ACTIVATION.store(0, Ordering::Release);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_ready = Some(ready);
    table.asl_provider = &crate::fixture::asl::PROVIDER;
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    for (device, terminal, expected) in [
        ("new", 0, AdoptionOutcome::Confirmed),
        ("rejected", -123, AdoptionOutcome::RolledBack),
    ] {
        ACTIVATION.store(0, Ordering::Release);
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        assert_eq!(
            unsafe { start_table(active_configuration(&[radio("old")]), &table) },
            0
        );
        let (engine, call) = selected().unwrap();
        ACTIVATION.store(1, Ordering::Release);
        std::thread::scope(|threads| {
            let applying =
                threads.spawn(|| reload(&engine, active_configuration(&[radio(device)])));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !engine.reloading.load(Ordering::Acquire) {
                assert!(!applying.is_finished() && Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(engine.enter().is_err());
            ACTIVATION.store(terminal, Ordering::Release);
            assert_eq!(applying.join().unwrap(), Ok(expected));
        });
        let retained = engine
            .run(|host| {
                Ok(host
                    .runtime
                    .modular_configuration()
                    .unwrap()
                    .radio
                    .port(LOCAL.parse().unwrap())
                    .unwrap()
                    .interface
                    .audio
                    .device_identifier
                    .clone())
            })
            .unwrap();
        assert_eq!(retained, if terminal == 0 { device } else { "old" });
        drop(call);
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    }
}

#[test]
fn rejected_rx_audio_commit_reports_station_phase_and_keeps_tx_inhibited() {
    unsafe extern "C" fn staged(_: *mut c_void, _: *mut c_void, _: *const c_void) -> i32 {
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(fixture_stop(), 0);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let mut binding = unsafe { table.station_binding.read() };
    // Accept the same-radio update. The fixture finish callback rejects commit.
    binding.stage_update = Some(staged);
    table.station_binding = Box::leak(Box::new(binding));
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start_table(active_configuration(&[]), &table) }, 0);
    let (engine, call) = selected().unwrap();
    clear_scheduler_notices();
    let configuration =
        active_configuration(&[(LOCAL, Kind::RxAudio, "", json!({"output_gain_db": -6}))]);
    let outcome = reload(&engine, configuration);
    let notices = scheduler_notices();
    let ready = engine.modular_observed().status_ready;
    drop(call);
    assert_eq!(fixture_stop(), 0);
    assert_eq!(outcome, Err(RuntimeError::Restore));
    assert!(!ready);
    assert!(
        notices
            .iter()
            .any(|line| line == "configuration adoption failed during station handoff: restore")
    );
}

fn scheduler_notices() -> Vec<String> {
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clone()
}

fn clear_scheduler_notices() {
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
}

#[test]
fn scheduler_health_status_counts_submission_failure_not_stale_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    engine.reloading.store(true, Ordering::Release);
    assert_eq!(engine.enter().err(), Some(RuntimeError::Rejected));
    let stale = engine.run(|_| Ok(()));
    engine.reloading.store(false, Ordering::Release);
    assert!(engine.enter().is_ok());
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    let failed = engine.run(|_| Ok(()));
    let lost_digits = engine.lost_digits.load(Ordering::Acquire);
    let notices = scheduler_notices();
    assert_eq!(
        reload(&engine, active_configuration(&[])),
        Ok(AdoptionOutcome::Confirmed)
    );
    let mut status = Vec::<u8>::new();
    unsafe extern "C" fn sink(context: *mut c_void, text: *const std::ffi::c_char, length: usize) {
        let status = unsafe { &mut *context.cast::<Vec<u8>>() };
        status.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast(), length) });
    }
    let result = unsafe {
        descriptor().link_status.unwrap()(
            LOCAL.as_ptr().cast(),
            LOCAL.len(),
            Some(sink),
            ptr::from_mut(&mut status).cast(),
        )
    };
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(stale, Err(RuntimeError::Rejected));
    assert_eq!(failed, Err(RuntimeError::Rejected));
    assert!(lost_digits);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("task submission failed: Backend"));
    assert_eq!(result, 0);
    let status = String::from_utf8(status).unwrap();
    assert!(
        status.contains("scheduler submission_failures=1"),
        "{status}"
    );
}

#[test]
fn scheduler_health_reports_live_cycle_failure_but_not_generation_retirement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    engine.pump_observed(engine.clock(), false);
    let mut healthy_status = String::new();
    engine.health.status(&mut healthy_status);
    let healthy_notices = scheduler_notices();
    crate::fixture::CONTROL_REVISION.store(
        ptr::from_ref(engine.revision.as_ref()).cast_mut(),
        Ordering::Release,
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(3, Ordering::Release);
    engine.pump_observed(engine.clock(), false);
    crate::fixture::CONTROL_REVISION.store(ptr::null_mut(), Ordering::Release);
    let mut retired_status = String::new();
    engine.health.status(&mut retired_status);
    let retired_notices = scheduler_notices();
    // An accepted cycle can fail independently of executor admission.
    let host = engine.host.lock().unwrap().take();
    engine.pump_observed(engine.clock(), false);
    *engine.host.lock().unwrap() = host;
    for _ in 0..2 {
        crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
        engine.pump_observed(engine.clock(), false);
    }
    let mut failed_status = String::new();
    engine.health.status(&mut failed_status);
    let failed_notices = scheduler_notices();
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert!(
        healthy_status.contains("submission_failures=0 cycle_failures=0"),
        "{healthy_status}"
    );
    assert!(healthy_notices.is_empty(), "{healthy_notices:?}");
    assert!(
        retired_status.contains("submission_failures=0 cycle_failures=0"),
        "{retired_status}"
    );
    assert!(retired_notices.is_empty(), "{retired_notices:?}");
    assert!(
        failed_status.contains("submission_failures=2 cycle_failures=3"),
        "{failed_status}"
    );
    assert_eq!(failed_notices.len(), 1, "{failed_notices:?}");
    assert!(failed_notices[0].contains("control cycle failed"));
}

#[test]
fn scheduler_health_queue_latency_has_its_own_warning_budget() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    clear_scheduler_notices();
    let (engine, call) = selected().unwrap();
    // Inject enqueue times, not scheduling sleeps or performance assertions.
    for _ in 0..2 {
        engine.queue_started(Instant::now() - Duration::from_secs(2));
    }
    let mut status = String::new();
    engine.health.status(&mut status);
    let notices = scheduler_notices();
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert!(status.contains("delayed_tasks=2"), "{status}");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("control queue start delayed"));
}

#[test]
fn site_lifecycle_installs_configured_acquisition_before_station_polling() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration(&[radio("usb"), door()]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let (engine, call) = selected().unwrap();
    let readings = engine.run(|host| Ok(host.poll_sites(0))).unwrap();
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(
        readings[LOCAL].inputs["door"].quality,
        rpt_advanced_core::site::ReadingQuality::Unavailable
    );
}

#[test]
fn site_backends_reload_rejection_preserves_live_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();
    let active = engine
        .run(|host| Ok(host.runtime.status(0)[0].1.active))
        .unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    let configuration = active_configuration(&[(
        LOCAL,
        Kind::Input,
        "door",
        json!({"enabled":true,"source":{"type":"gpiod","device":"/no-device","line":"0"}}),
    )]);
    let result = reload(&engine, configuration);
    let after = engine
        .run(|host| Ok(host.runtime.status(0)[0].1.active))
        .unwrap();
    let after_drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_ne!(result, Ok(AdoptionOutcome::Confirmed));
    assert_eq!(after, active);
    assert_eq!(after_drops, drops);
}

#[test]
fn site_lifecycle_reload_replaces_bindings_without_reopening_same_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[radio("usb")])) }, 0);
    let drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    let configuration = active_configuration(&[radio("usb"), door()]);
    let (engine, call) = selected().unwrap();
    let result = reload(&engine, configuration);
    let readings = engine.run(|host| Ok(host.poll_sites(0))).unwrap();
    let after_drops = crate::fixture::RADIO_DROPS.load(Ordering::Relaxed);
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(result, Ok(AdoptionOutcome::Confirmed));
    assert_eq!(after_drops, drops);
    assert!(
        readings
            .get(LOCAL)
            .is_some_and(|node| node.inputs.contains_key("door"))
    );
}

#[test]
fn site_alarm_link_action_is_dispatched_without_a_schedule_tick() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    let configuration = active_configuration(&[
        radio("usb"),
        door(),
        (
            LOCAL,
            Kind::Alarm,
            "missing",
            json!({"enabled":true,"source":"door","condition":"unavailable","on_trip":["macro:connect"]}),
        ),
        (
            LOCAL,
            Kind::Macro,
            "connect",
            json!({"actions":[{"action":"connect","parameter":"2000"}]}),
        ),
    ]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let (engine, call) = selected().unwrap();
    engine.pump(engine.clock(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        engine.pump(engine.clock(), false).unwrap();
        if engine.jobs.lock().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "site action dial did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        engine
            .run(|host| host.status_text(LOCAL))
            .unwrap()
            .contains("  2000: transceive")
    );
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

fn operation(action: LinkAction, node: &str) -> DigitOperation {
    DigitOperation::new(
        Command {
            action,
            node: node.into(),
        },
        None,
    )
}

#[test]
fn descriptor_rejects_bad_composition_and_preserves_reload_state() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let configuration = idle_configuration;
    assert_eq!(unsafe { start_table(configuration(), ptr::null()) }, 1);
    assert_eq!(unsafe { start(configuration()) }, 0);
    assert_eq!(unsafe { start(configuration()) }, 1);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"missing".as_ptr(),
                    7,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        -1
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );
    assert_eq!(unsafe { descriptor().reload_poll.unwrap()() }, -1);
    let (engine, guard) = selected().unwrap();
    assert_eq!(
        reload(&engine, configuration()),
        Ok(AdoptionOutcome::Confirmed)
    );
    drop(guard);
    let mut completed = 0;
    assert_eq!(
        unsafe { descriptor().digit.unwrap()(c"missing".as_ptr(), 7, b'?', &mut completed) },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().link_command.unwrap()(c"missing".as_ptr(), 7, c"2000".as_ptr(), 4, 0)
        },
        -1
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn active_host_owns_radio_and_incoming_peer_until_quiescent_stop() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::fixture::RADIO_OPENS.store(0, Ordering::Relaxed);
    crate::fixture::RADIO_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let radio_ready_before = crate::fixture::RADIO_READY.swap(1, Ordering::AcqRel);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let configuration = active_configuration(&[battery()]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let proof = admission("2000", "127.0.0.1").unwrap();
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe { super::asl::ADMISSION.complete.unwrap()(proof, peer) },
        0
    );
    let mut status = Vec::<u8>::new();
    unsafe extern "C" fn sink(context: *mut c_void, text: *const std::ffi::c_char, length: usize) {
        let status = unsafe { &mut *context.cast::<Vec<u8>>() };
        status.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast(), length) });
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        status.clear();
        assert_eq!(
            unsafe {
                descriptor().link_status.unwrap()(
                    LOCAL.as_ptr().cast(),
                    LOCAL.len(),
                    Some(sink),
                    ptr::from_mut(&mut status).cast(),
                )
            },
            0
        );
        let text = std::str::from_utf8(&status).unwrap();
        assert!(
            text.contains("sensor battery value=None quality=Unavailable"),
            "{text}"
        );
        if text.contains("2000") {
            break;
        }
        assert!(Instant::now() < deadline, "unexpected link status: {text}");
        std::thread::sleep(Duration::from_millis(1));
    }
    let rejected = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2001".as_ptr(),
                    4,
                    c"192.0.2.2".as_ptr(),
                    9,
                ),
                rejected,
            )
        },
        -1
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    crate::fixture::RADIO_READY.store(radio_ready_before, Ordering::Release);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(Ordering::Relaxed),
        crate::fixture::RADIO_OPENS.load(Ordering::Relaxed)
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );

    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
}

#[test]
fn incoming_peer_is_rejected_and_released_when_refresh_cannot_prepare_audio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    crate::fixture::PEER_DROPS.store(0, Ordering::Relaxed);
    crate::fixture::PEER_OPENS.store(0, Ordering::Relaxed);
    let configuration = active_configuration(&[]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let proof = admission("2000", "127.0.0.1").unwrap();
    crate::fixture::PEER_PREPARE_TEXT_RESULT.store(1, Ordering::Release);
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe { super::asl::ADMISSION.complete.unwrap()(proof, peer) },
        -1
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Relaxed),
        crate::fixture::PEER_OPENS.load(Ordering::Relaxed)
    );
}

#[test]
fn failed_dial_thread_runs_reserved_attempt_synchronously() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let configuration = || active_configuration(&[]);
    assert_eq!(unsafe { start(configuration()) }, 0);
    {
        let (engine, _call) = selected().unwrap();
        let effect = engine
            .operation(
                LOCAL,
                DigitOperation::new(
                    Command {
                        action: LinkAction::Transceive,
                        node: "2000".into(),
                    },
                    None,
                ),
            )
            .unwrap();
        let LinkEffect::Connect(attempt) = effect else {
            panic!("connect operation must reserve a dial");
        };
        FAIL_DIAL_THREAD.store(true, Ordering::Release);
        engine
            .spawn_dial(
                LOCAL.into(),
                Dial::Connect(attempt),
                engine.revision.load(Ordering::Acquire),
                false,
            )
            .unwrap();
        assert!(
            engine
                .run(|host| host.status_text(LOCAL))
                .unwrap()
                .contains("2000")
        );
    }
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn local_rf_loop_rejection_is_a_completed_command_with_queued_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);

    for (index, digit) in b"*31000#".iter().enumerate() {
        let mut completed = 99;
        assert_eq!(
            unsafe {
                descriptor().digit.unwrap()(
                    LOCAL.as_ptr().cast(),
                    LOCAL.len(),
                    *digit,
                    &mut completed,
                )
            },
            0,
            "digit index {index}"
        );
        assert_eq!(completed, u32::from(index == 6));
    }
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn rejected_dtmf_routes_keep_local_or_originating_peer_telemetry() {
    use rpt_advanced_core::{link::PeerSignals, runtime::dtmf::DigitOutcome};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();
    assert!(matches!(
        engine.execute(LOCAL.into(), operation(LinkAction::Command, "3002000")),
        Err(RuntimeError::Link(
            rpt_advanced_core::link::AdmissionError::UnsupportedRoute
        ))
    ));
    for (action, remote) in [
        (LinkAction::Transceive, "1000"),
        (LinkAction::Command, "3002000"),
    ] {
        for linked_source in [false, true] {
            let source = PeerSignals::new();
            let mut operation = operation(action, remote);
            operation.reply = linked_source.then(|| source.reply_target());
            assert_eq!(
                engine.execute_digit(LOCAL.into(), DigitOutcome::Operation(operation)),
                Ok(())
            );
        }
    }
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn incoming_preauthorization_never_adopts_a_foreign_or_already_owned_peer() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    for protocol in [1, 2] {
        let peer = crate::fixture::peer();
        let mut request = crate::fixture::incoming_request(
            c"1000".as_ptr(),
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        request.protocol = protocol;
        assert_eq!(
            unsafe { descriptor().authorize_incoming.unwrap()(&request, peer) },
            -1
        );
        unsafe { crate::fixture::release_peer(peer) };
    }
    for wrong_protocol in [false, true] {
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        let peer = crate::fixture::peer();
        let mut request = crate::fixture::incoming_request(
            if wrong_protocol {
                c"1000".as_ptr()
            } else {
                c"9999".as_ptr()
            },
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        if wrong_protocol {
            request.protocol = 2;
        }
        assert_eq!(
            unsafe { descriptor().incoming.unwrap()(&request, peer) },
            -1
        );
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire),
            before + 1
        );
    }
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn public_phone_admission_borrows_metadata_and_releases_accountless_calls() {
    use std::sync::atomic::AtomicUsize;
    static METADATA_MODE: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn phone_info(
        context: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { crate::fixture::peer_info(context, peer, output) };
        let output = unsafe { &mut *output };
        output.protocol = 3;
        output.local = abi::rptadv_peer_bytes_v1 {
            data: LOCAL.as_ptr(),
            length: LOCAL.len(),
        };
        output.capabilities = 32;
        output.account_generation = 1;
        output.qso_generation = 2;
        output.remote = abi::rptadv_peer_bytes_v1 {
            data: b"aabbccddeeff00112233445566778899".as_ptr(),
            length: 32,
        };
        output.provider_account = abi::rptadv_peer_bytes_v1 {
            data: b"provider".as_ptr(),
            length: 8,
        };
        match METADATA_MODE.load(Ordering::Acquire) {
            1 => {
                // A well-formed ASL handle is foreign to this Phone request.
                output.protocol = 1;
                output.capabilities = 12;
                output.account_generation = 0;
                output.qso_generation = 0;
                output.provider_account = abi::rptadv_peer_bytes_v1 {
                    data: ptr::null(),
                    length: 0,
                };
                output.local = abi::rptadv_peer_bytes_v1 {
                    data: b"1000".as_ptr(),
                    length: 4,
                };
                output.remote = abi::rptadv_peer_bytes_v1 {
                    data: b"2000".as_ptr(),
                    length: 4,
                };
            }
            2 => {
                // Phone ownership is a port UUID, including for a foreign port.
                output.local = abi::rptadv_peer_bytes_v1 {
                    data: SECOND_LOCAL.as_ptr(),
                    length: SECOND_LOCAL.len(),
                };
            }
            _ => {}
        }
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_info = Some(phone_info);
    let source = active_configuration(&[]);
    assert_eq!(unsafe { start_table(source, &table) }, 0);
    let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
    let peer = crate::fixture::peer();
    let mut request = crate::fixture::incoming_request(
        LOCAL.as_ptr().cast(),
        LOCAL.len(),
        c"aabbccddeeff00112233445566778899".as_ptr(),
        32,
        c"claimed-origin".as_ptr(),
        14,
    );
    request.protocol = 3;
    assert_eq!(
        unsafe { descriptor().authorize_incoming.unwrap()(&request, ptr::null_mut()) },
        -1
    );
    assert_eq!(
        unsafe { descriptor().authorize_incoming.unwrap()(&request, peer) },
        -1
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Acquire),
        before,
        "inspection must not adopt the pending handle"
    );
    request.remote = abi::rptadv_peer_bytes_v1 {
        data: b"112233445566778899aabbccddeeff00".as_ptr(),
        length: 32,
    };
    assert_eq!(
        unsafe { descriptor().authorize_incoming.unwrap()(&request, peer) },
        -1
    );
    request.remote = abi::rptadv_peer_bytes_v1 {
        data: b"aabbccddeeff00112233445566778899".as_ptr(),
        length: 32,
    };
    for mode in 1..=2 {
        METADATA_MODE.store(mode, Ordering::Release);
        let foreign = crate::fixture::peer();
        let prior = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        assert_eq!(
            unsafe { descriptor().authorize_incoming.unwrap()(&request, foreign) },
            -1
        );
        assert_eq!(crate::fixture::PEER_DROPS.load(Ordering::Acquire), prior);
        assert_eq!(
            unsafe { descriptor().incoming.unwrap()(&request, foreign) },
            -1
        );
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire),
            prior + 1
        );
    }
    METADATA_MODE.store(0, Ordering::Release);
    assert_eq!(
        unsafe { descriptor().incoming.unwrap()(&request, peer) },
        -1
    );
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Acquire),
        before + 3
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(
        crate::fixture::PEER_DROPS.load(Ordering::Acquire),
        before + 3
    );
}

#[test]
fn pad_test_dispatch_queues_local_telemetry() {
    use rpt_advanced_core::runtime::dtmf::{DigitOutcome, PadTestResult};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    assert!(
        selected()
            .unwrap()
            .0
            .execute_digit(
                LOCAL.into(),
                DigitOutcome::PadTest(PadTestResult {
                    digits: "*#A".into(),
                    overflowed: false
                })
            )
            .is_ok()
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn dtmf_macro_stopped_dispatch_queues_only_the_existing_source_reply() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();
    assert!(
        engine
            .execute_digit(LOCAL.into(), DigitOutcome::MacroStopped { reply: None })
            .is_ok()
    );
    assert!(
        engine
            .execute_digit("missing".into(), DigitOutcome::MacroStopped { reply: None })
            .is_err()
    );
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn dtmf_macro_pending_dial_checks_source_retirement_before_dial_and_after_answer() {
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize};
    static DIALS: AtomicUsize = AtomicUsize::new(0);
    static CANCEL_DURING: AtomicBool = AtomicBool::new(false);
    static RELOAD_DURING: AtomicBool = AtomicBool::new(false);
    static STILL_CURRENT: AtomicBool = AtomicBool::new(false);
    static CALLBACK_OK: AtomicBool = AtomicBool::new(true);
    static RELOAD_RESULT: Mutex<Option<Result<AdoptionOutcome, RuntimeError>>> = Mutex::new(None);
    static TARGET: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static TARGET_DROPS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn destroy(_: *mut c_void, peer: *mut c_void) {
        if !peer.is_null() && TARGET.load(Ordering::Acquire) == peer {
            TARGET_DROPS.fetch_add(1, Ordering::AcqRel);
        }
        // Forward the actual fixture destructor; the source has a separate lifetime.
        unsafe { crate::fixture::release_peer(peer) };
    }
    unsafe extern "C" fn dial(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        current: abi::rptadv_current_v1,
        context: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        DIALS.fetch_add(1, Ordering::AcqRel);
        if CANCEL_DURING.load(Ordering::Acquire) {
            let Ok((engine, _call)) = selected() else {
                CALLBACK_OK.store(false, Ordering::Release);
                return -1;
            };
            let clock = engine.clock();
            if engine
                .run(move |host| {
                    // Retiring the actual issuing connection revokes the copied
                    // action permit. A star is ordinary command input.
                    host.immediate(LOCAL, LinkEffect::Detach(vec!["2000".into()]), clock)
                })
                .is_err()
            {
                CALLBACK_OK.store(false, Ordering::Release);
                return -1;
            }
        }
        if RELOAD_DURING.load(Ordering::Acquire) {
            let Ok((engine, _call)) = selected() else {
                CALLBACK_OK.store(false, Ordering::Release);
                return -1;
            };
            // Keep stable resource identities while replacing the command generation.
            // Assert the real result outside this non-unwinding host callback.
            *RELOAD_RESULT
                .lock()
                .unwrap_or_else(|error| error.into_inner()) =
                Some(reload(&engine, macro_configuration()));
        }
        STILL_CURRENT.store(
            current.is_some_and(|callback| unsafe { callback(context) } != 0),
            Ordering::Release,
        );
        // A provider may answer despite cancellation; final publication must still reject it.
        let peer = crate::fixture::peer();
        TARGET.store(peer, Ordering::Release);
        unsafe { *output = peer };
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for phase in ["before_dial", "during_dial", "during_reload", "ordinary"] {
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
        DIALS.store(0, Ordering::Release);
        STILL_CURRENT.store(false, Ordering::Release);
        CANCEL_DURING.store(false, Ordering::Release);
        RELOAD_DURING.store(false, Ordering::Release);
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        let source = macro_configuration();
        let mut table = unsafe { crate::fixture::host_descriptor().read() };
        // The fake physical provider must actually run RX/TX adoption callbacks.
        table.context = ptr::without_provenance_mut(1);
        CALLBACK_OK.store(true, Ordering::Release);
        *RELOAD_RESULT
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        table.peer_dial = Some(dial);
        table.peer_destroy = Some(destroy);
        assert_eq!(unsafe { start_table(source, &table) }, 0);
        let (engine, call) = selected().unwrap();
        engine
            .execute(LOCAL.into(), operation(LinkAction::Transceive, "2000"))
            .unwrap();
        wait_for_ingress(&engine, "2000");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let clock = engine.clock();
            engine.pump(clock, false).unwrap();
            if engine
                .run(move |host| {
                    Ok(host
                        .runtime
                        .status(clock.now_ms)
                        .iter()
                        .any(|(name, state)| name == LOCAL && state.retiring.is_none()))
                })
                .unwrap()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "physical callbacks did not acknowledge the peer mix generation"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let reply = engine
            .run(|host| Ok(host.fixture_peer_reply(LOCAL, "2000").unwrap()))
            .unwrap();
        DIALS.store(0, Ordering::Release);
        TARGET.store(ptr::null_mut(), Ordering::Release);
        TARGET_DROPS.store(0, Ordering::Release);
        STILL_CURRENT.store(false, Ordering::Release);
        CANCEL_DURING.store(
            matches!(phase, "during_dial" | "ordinary"),
            Ordering::Release,
        );
        RELOAD_DURING.store(phase == "during_reload", Ordering::Release);
        let effect = if phase == "ordinary" {
            engine
                .operation(LOCAL, operation(LinkAction::Transceive, "3000"))
                .unwrap()
        } else {
            dtmf_macro_connect_from(&engine, Some(reply))
        };
        let LinkEffect::Connect(attempt) = effect else {
            panic!("pending connect")
        };
        if phase == "before_dial" {
            let clock = engine.clock();
            engine
                .run(move |host| {
                    // Retiring the actual issuing connection revokes the copied
                    // action permit. A star is ordinary command input.
                    host.immediate(LOCAL, LinkEffect::Detach(vec!["2000".into()]), clock)
                })
                .unwrap();
        }
        let before = TARGET_DROPS.load(Ordering::Acquire);
        let result = engine.dial(
            LOCAL.into(),
            Dial::Connect(attempt),
            engine.revision.load(Ordering::Acquire),
        );
        let attached = engine
            .run(|host| {
                Ok(host
                    .runtime
                    .node(LOCAL)
                    .unwrap()
                    .links()
                    .manager()
                    .snapshot()
                    .iter()
                    .any(|peer| peer.name == "3000"))
            })
            .unwrap();
        let dials = DIALS.load(Ordering::Acquire);
        let still_current = STILL_CURRENT.load(Ordering::Acquire);
        let drops = TARGET_DROPS.load(Ordering::Acquire);
        let callback_ok = CALLBACK_OK.load(Ordering::Acquire);
        let reloaded = RELOAD_RESULT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(call);
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
        assert!(callback_ok, "source cancellation callback failed");
        assert_eq!(
            reloaded,
            (phase == "during_reload").then_some(Ok(AdoptionOutcome::Confirmed))
        );
        assert_eq!(attached, phase == "ordinary", "{phase}");
        assert_eq!(result.is_ok(), phase == "ordinary", "{phase}");
        assert_eq!(dials, usize::from(phase != "before_dial"));
        if matches!(phase, "during_dial" | "during_reload") {
            assert!(!still_current);
            assert_eq!(drops, before + 1);
        }
        if phase == "ordinary" {
            assert!(still_current);
        }
    }
}

fn macro_configuration() -> PreparedConfiguration {
    active_configuration(&[
        (
            LOCAL,
            Kind::Command,
            "",
            json!({"mappings":{"macro-connect":{"command":"*95","action":"call_macro","macro":"connect","protected":false}}}),
        ),
        (
            LOCAL,
            Kind::Macro,
            "connect",
            json!({"actions":[{"action":"connect","parameter":"3000"}]}),
        ),
    ])
}

fn dtmf_macro_connect(engine: &Arc<Engine>) -> LinkEffect {
    dtmf_macro_connect_from(engine, None)
}

fn dtmf_macro_connect_from(
    engine: &Arc<Engine>,
    source: Option<rpt_advanced_core::link::PeerReply>,
) -> LinkEffect {
    use rpt_advanced_core::runtime::dtmf::{DigitEvent, DigitOutcome};
    let clock = engine.clock();
    engine
        .run(move |host| {
            let mut outcome = None;
            for digit in "*95#".chars() {
                outcome = if let Some(reply) = &source {
                    host.runtime.node(LOCAL).unwrap().links().peer_digit(
                        "2000",
                        digit,
                        clock.now_ms,
                        reply.clone(),
                    )
                } else {
                    host.runtime.digit(
                        LOCAL,
                        DigitEvent::Digit {
                            digit,
                            now_ms: clock.now_ms,
                        },
                    )
                };
            }
            let Some(DigitOutcome::Mapped(command)) = outcome else {
                panic!("registered macro mapping")
            };
            host.runtime.dispatch_mapping(LOCAL, command, clock)?;
            let (effect, _receipt) = host
                .runtime
                .advance_commands(LOCAL, clock)?
                .expect("macro reserves a dial");
            Ok(effect)
        })
        .unwrap()
}

#[test]
fn dtmf_macro_stop_during_directory_lookup_prevents_provider_dial() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    type PendingStop = (Arc<Engine>, u64, mpsc::SyncSender<()>);
    static STOP_LOOKUP: Mutex<Option<PendingStop>> = Mutex::new(None);
    static STOP_OBSERVED: AtomicBool = AtomicBool::new(false);
    static DIALS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn record(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        let pending = STOP_LOOKUP
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some((engine, revision, request)) = pending else {
            return -1;
        };
        if request.send(()).is_err() {
            return -1;
        }
        // Stop closes admission before waiting for this command's held Call.
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.current(revision) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let stopped = !engine.current(revision);
        STOP_OBSERVED.store(stopped, Ordering::Release);
        let Some(sink) = sink.filter(|_| stopped) else {
            return -1;
        };
        let destination = "radio@fixture/3000,127.0.0.1";
        unsafe { sink(context, destination.as_ptr().cast(), destination.len()) };
        0
    }
    unsafe extern "C" fn dial(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        _: *const std::ffi::c_char,
        _: usize,
        _: usize,
        _: abi::rptadv_current_v1,
        _: *mut c_void,
        output: *mut *mut c_void,
    ) -> i32 {
        DIALS.fetch_add(1, Ordering::AcqRel);
        unsafe { *output = crate::fixture::peer() };
        0
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    STOP_OBSERVED.store(false, Ordering::Release);
    DIALS.store(0, Ordering::Release);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let source = macro_configuration();
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.directory_record = Some(record);
    table.peer_dial = Some(dial);
    assert_eq!(unsafe { start_table(source, &table) }, 0);
    let (engine, call) = selected().unwrap();
    let LinkEffect::Connect(attempt) = dtmf_macro_connect(&engine) else {
        panic!("pending macro connection")
    };
    let revision = engine.revision.load(Ordering::Acquire);
    let (request, receive) = mpsc::sync_channel(1);
    *STOP_LOOKUP.lock().unwrap() = Some((Arc::clone(&engine), revision, request));
    let stop = std::thread::spawn(move || {
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        crate::lifecycle::tests::fixture_stop()
    });
    let result = engine.dial(LOCAL.into(), Dial::Connect(attempt), revision);
    drop(call);
    assert_eq!(stop.join().unwrap(), 0);
    assert!(STOP_OBSERVED.load(Ordering::Acquire));
    assert_eq!(result, Err(RuntimeError::Rejected));
    assert_eq!(DIALS.load(Ordering::Acquire), 0);
}

#[test]
fn remote_candidate_dispatch_ignores_denied_command_but_keeps_accepted_effect() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();

    // An unconnected peer's candidate D command must not reject local digit handling.
    assert!(matches!(
        engine.digit_effect(
            LOCAL,
            DigitOutcome::RemoteCandidate(operation(LinkAction::Command, "2000"))
        ),
        Ok(LinkEffect::None)
    ));
    engine
        .execute(LOCAL.into(), operation(LinkAction::Transceive, "2000"))
        .unwrap();
    wait_for_ingress(&engine, "2000");
    assert!(matches!(
        engine.digit_effect(
            LOCAL,
            DigitOutcome::RemoteCandidate(operation(LinkAction::Command, "2000"))
        ),
        Ok(LinkEffect::SelectedRemote)
    ));

    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn regeneration_dispatch_obeys_node_configuration_and_reload() {
    use rpt_advanced_core::runtime::dtmf::DigitOutcome;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    for enabled in [false, true] {
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        assert_eq!(
            unsafe {
                start(active_configuration(&[(
                    LOCAL,
                    Kind::Command,
                    "",
                    json!({"regeneration":{"enabled":enabled}}),
                )]))
            },
            0
        );
        let (engine, call) = selected().unwrap();
        assert!(
            engine
                .execute_digit(LOCAL.into(), DigitOutcome::Regenerate('5'))
                .is_ok()
        );
        assert!(
            engine
                .execute_digit("missing".into(), DigitOutcome::Regenerate('5'))
                .is_err()
        );
        assert!(
            engine
                .run(|host| host.status_text(LOCAL))
                .unwrap()
                .contains("regeneration-dropped=0")
        );
        drop(call);
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    }
}

#[test]
fn non_rf_loop_rejection_is_returned_without_local_rf_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);

    assert_eq!(
        unsafe {
            descriptor().link_command.unwrap()(
                LOCAL.as_ptr().cast(),
                LOCAL.len(),
                c"1000".as_ptr(),
                4,
                1,
            )
        },
        -1
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn failed_start_drains_or_retains_the_executor_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let configuration = || active_configuration(&[]);

    crate::fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 1);
    assert_eq!(unsafe { start(idle_configuration()) }, 0);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    // A ticker startup error after an opened radio reaches the actual Engine drain.
    // A rejected drain retains the owner and successful retry relinquishes it.
    FAIL_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::CONTROL_STOP_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 1);
    assert!(ENGINE.lock().unwrap().is_some());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert!(ENGINE.lock().unwrap().is_none());
}

#[test]
fn helpers_reject_malformed_inputs_and_capture_bounded_wall_time() {
    let services = unsafe { HostServices::open(crate::fixture::host_descriptor()) }.unwrap();
    let origin = Instant::now();
    let before_epoch = UNIX_EPOCH.checked_sub(Duration::from_secs(1)).unwrap();
    let clock = capture_at(origin, before_epoch, services);
    assert_eq!(clock.wall_seconds, -1);
    assert!(clock.civil.is_none());

    crate::fixture::LOCAL_TIME_RESULT.store(1, Ordering::Release);
    let clock = capture_at(origin, UNIX_EPOCH + Duration::from_secs(1), services);
    assert_eq!(clock.wall_seconds, 1);
    assert!(clock.civil.is_none());

    assert_eq!(unsafe { input(ptr::null(), 0) }, Ok(""));
    assert!(unsafe { input(ptr::null(), 1) }.is_err());
    let invalid = [0xff_u8];
    assert!(unsafe { input(invalid.as_ptr().cast(), invalid.len()) }.is_err());
    assert!(
        unsafe {
            incoming_identity(&crate::fixture::incoming_request(
                ptr::null(),
                1,
                c"2".as_ptr(),
                1,
                ptr::null(),
                0,
            ))
        }
        .is_err()
    );
}

#[test]
fn incoming_identity_rejects_foreign_headers_and_limits_before_borrowing_spans() {
    assert!(unsafe { incoming_identity(ptr::null()) }.is_err());
    for mode in 0..=5 {
        let mut request = crate::fixture::incoming_request(
            c"1000".as_ptr(),
            4,
            c"2000".as_ptr(),
            4,
            c"127.0.0.1".as_ptr(),
            9,
        );
        match mode {
            0 => request.struct_size = 0,
            1 => request.abi_version = 0,
            2 => request.local.length = 33,
            3 => request.remote.length = 33,
            4 => request.source.length = 65,
            _ => request.protocol = 99,
        }
        assert!(
            unsafe { incoming_identity(&request) }.is_err(),
            "case {mode}"
        );
    }
}

#[test]
fn phone_local_binding_request_preserves_configured_names_and_the_63_byte_limit() {
    let long = "a".repeat(63);
    for local in ["local-repeater", long.as_str(), "rÃƒÆ’Ã‚Â©pÃƒÆ’Ã‚Â©teur"] {
        let mut request = crate::fixture::incoming_request(
            local.as_ptr().cast(),
            local.len(),
            c"aabbccddeeff00112233445566778899".as_ptr(),
            32,
            ptr::null(),
            0,
        );
        request.protocol = 3;
        let (_, copied, _, _) =
            unsafe { incoming_identity(&request) }.expect("valid phone local binding rejected");
        assert_eq!(copied, local);
    }
    for local in ["", "bad name", "bad[node]", &"a".repeat(64)] {
        let mut request = crate::fixture::incoming_request(
            local.as_ptr().cast(),
            local.len(),
            c"aabbccddeeff00112233445566778899".as_ptr(),
            32,
            ptr::null(),
            0,
        );
        request.protocol = 3;
        assert!(unsafe { incoming_identity(&request) }.is_err());
    }
    let request = crate::fixture::incoming_request(
        long.as_ptr().cast(),
        long.len(),
        c"2000".as_ptr(),
        4,
        ptr::null(),
        0,
    );
    assert!(
        unsafe { incoming_identity(&request) }.is_err(),
        "ASL's existing wire bound is unchanged"
    );
}

#[test]
fn serialized_engine_rejects_stale_missing_and_unaccepted_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration(&[]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let (engine, call) = selected().unwrap();

    engine.reloading.store(true, Ordering::Release);
    assert_eq!(engine.enter().err(), Some(RuntimeError::Rejected));
    assert_eq!(engine.run(|_| Ok(1)).unwrap_err(), RuntimeError::Rejected);
    assert_eq!(engine.run_inner(true, |_| Ok(2)).unwrap(), 2);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire)));
    engine.reloading.store(false, Ordering::Release);

    engine.accepting.store(false, Ordering::Release);
    assert_eq!(engine.enter().err(), Some(RuntimeError::Rejected));
    assert_eq!(engine.run(|_| Ok(3)).unwrap_err(), RuntimeError::Rejected);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire)));
    engine.accepting.store(true, Ordering::Release);

    let host = engine.host.lock().unwrap().take();
    assert_eq!(engine.run(|_| Ok(4)).unwrap_err(), RuntimeError::Rejected);
    *engine.host.lock().unwrap() = host;

    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(engine.run(|_| Ok(5)).unwrap_err(), RuntimeError::Rejected);
    assert!(engine.lost_digits.load(Ordering::Acquire));

    crate::fixture::CONTROL_REVISION.store(
        ptr::from_ref(engine.revision.as_ref()).cast_mut(),
        Ordering::Release,
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(3, Ordering::Release);
    assert_eq!(engine.run(|_| Ok(7)).unwrap_err(), RuntimeError::Rejected);
    assert!(!engine.current(engine.revision.load(Ordering::Acquire) - 1));
    assert!(engine.current(engine.revision.load(Ordering::Acquire)));

    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn dial_retry_background_and_remote_command_paths_are_serialized() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    let configuration = active_configuration(&[]);
    assert_eq!(unsafe { start(configuration) }, 0);
    let (engine, call) = selected().unwrap();

    engine
        .execute(LOCAL.into(), operation(LinkAction::Transceive, "2000"))
        .unwrap();
    wait_for_ingress(&engine, "2000");
    let selected = engine
        .operation(LOCAL, operation(LinkAction::Command, "2000"))
        .unwrap();
    engine
        .effect(
            LOCAL.into(),
            selected,
            engine.revision.load(Ordering::Acquire),
        )
        .unwrap();
    let mut forwarded = operation(LinkAction::Command, "2000");
    forwarded.digit = Some('5');
    let effect = engine.operation(LOCAL, forwarded).unwrap();
    engine
        .effect(
            LOCAL.into(),
            effect,
            engine.revision.load(Ordering::Acquire),
        )
        .unwrap();
    engine
        .execute(LOCAL.into(), operation(LinkAction::Disconnect, "2000"))
        .unwrap();

    let revision = engine.revision.load(Ordering::Acquire);
    assert_eq!(
        engine.effect(LOCAL.into(), LinkEffect::None, revision - 1),
        Err(RuntimeError::Rejected)
    );
    let LinkEffect::Connect(stale) = engine
        .operation(LOCAL, operation(LinkAction::Transceive, "2001"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    engine.revision.fetch_add(1, Ordering::AcqRel);
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Connect(stale), revision),
        Err(RuntimeError::Rejected)
    );

    let LinkEffect::Connect(permanent) = engine
        .operation(LOCAL, operation(LinkAction::PermanentTransceive, "3000"))
        .unwrap()
    else {
        panic!("permanent operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    let revision = engine.revision.load(Ordering::Acquire);
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Connect(permanent), revision),
        Err(RuntimeError::Rejected)
    );
    let mut retry_clock = engine.clock();
    retry_clock.now_ms = u64::MAX;
    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    assert_eq!(engine.pump(retry_clock, true), Err(RuntimeError::Rejected));
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node(LOCAL).ok_or(RuntimeError::MissingNode)?;
            let work = node
                .work()
                .expect("configured node retains an active generation");
            node.links()
                .take_retry(retry_clock.now_ms, work)
                .ok_or(RuntimeError::Rejected)
        })
        .unwrap();
    engine
        .dial(LOCAL.into(), Dial::Retry(retry), revision)
        .unwrap();

    let LinkEffect::Connect(rejected) = engine
        .operation(LOCAL, operation(LinkAction::Transceive, "3500"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    crate::fixture::PEER_PREPARE_TEXT_RESULT.store(1, Ordering::Release);
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Connect(rejected), revision),
        Err(RuntimeError::Preparation)
    );

    crate::fixture::NOTICE_COUNT.store(0, Ordering::Release);
    engine
        .background(LOCAL.into(), LinkEffect::None, revision, true)
        .unwrap();
    engine
        .background(LOCAL.into(), LinkEffect::None, revision, false)
        .unwrap();
    assert_eq!(crate::fixture::NOTICE_COUNT.load(Ordering::Acquire), 1);

    let LinkEffect::Connect(attempt) = engine
        .operation(LOCAL, operation(LinkAction::Transceive, "4000"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_DELAY_MS.store(20, Ordering::Release);
    engine
        .background(LOCAL.into(), LinkEffect::Connect(attempt), revision, true)
        .unwrap();
    engine.pump(engine.clock(), false).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    engine.pump(engine.clock(), false).unwrap();
    assert_eq!(crate::fixture::NOTICE_COUNT.load(Ordering::Acquire), 2);

    let stale_revision = engine.revision.load(Ordering::Acquire);
    let LinkEffect::Connect(permanent) = engine
        .operation(LOCAL, operation(LinkAction::PermanentTransceive, "3600"))
        .unwrap()
    else {
        panic!("permanent operation must reserve a dial");
    };
    crate::fixture::PEER_DIAL_RESULT.store(1, Ordering::Release);
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Connect(permanent), stale_revision),
        Err(RuntimeError::Rejected)
    );
    let retry = engine
        .run(move |host| {
            let node = host.runtime.node(LOCAL).ok_or(RuntimeError::MissingNode)?;
            let work = node
                .work()
                .expect("configured node retains an active generation");
            node.links()
                .take_retry(u64::MAX, work)
                .ok_or(RuntimeError::Rejected)
        })
        .unwrap();
    engine.revision.fetch_add(1, Ordering::AcqRel);
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Retry(retry), stale_revision),
        Err(RuntimeError::Rejected)
    );

    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

#[test]
fn public_commands_cover_link_modes_status_digits_and_admission_failures() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = || active_configuration(&[battery()]);
    assert_eq!(unsafe { start(configuration()) }, 0);

    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    ptr::null(),
                    1,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().authorize_incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"missing".as_ptr(),
                    7,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        -1
    );
    let peer = crate::fixture::peer();
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    ptr::null(),
                    1,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                peer,
            )
        },
        1
    );
    unsafe { crate::fixture::release_peer(peer) };
    assert_eq!(
        unsafe {
            descriptor().incoming.unwrap()(
                &crate::fixture::incoming_request(
                    c"1000".as_ptr(),
                    4,
                    c"2000".as_ptr(),
                    4,
                    ptr::null(),
                    0,
                ),
                ptr::null_mut(),
            )
        },
        1
    );
    assert_eq!(
        unsafe {
            descriptor().link_status.unwrap()(
                LOCAL.as_ptr().cast(),
                LOCAL.len(),
                None,
                ptr::null_mut(),
            )
        },
        -1
    );
    let mut rejected = 0;
    assert_eq!(
        unsafe {
            descriptor().digit.unwrap()(LOCAL.as_ptr().cast(), LOCAL.len(), b'1', ptr::null_mut())
        },
        -1
    );
    assert_eq!(
        unsafe {
            descriptor().digit.unwrap()(LOCAL.as_ptr().cast(), LOCAL.len(), b'!', &mut rejected)
        },
        -1
    );
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    for (action, remote) in [(1, "2001"), (2, "2002"), (3, "2003")] {
        SKIP_TICKER_THREAD.store(true, Ordering::Release);
        assert_eq!(unsafe { start(configuration()) }, 0);
        assert_eq!(
            unsafe {
                descriptor().link_command.unwrap()(
                    LOCAL.as_ptr().cast(),
                    LOCAL.len(),
                    remote.as_ptr().cast(),
                    remote.len(),
                    action,
                )
            },
            0,
            "action {action} remote {remote}"
        );
        assert_eq!(
            unsafe {
                descriptor().link_command.unwrap()(
                    LOCAL.as_ptr().cast(),
                    LOCAL.len(),
                    remote.as_ptr().cast(),
                    remote.len(),
                    4,
                )
            },
            0
        );
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    }

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let mut completed = 99;
    for command in [b"*722#".as_slice(), b"*761#"] {
        for digit in command {
            completed = 99;
            assert_eq!(
                unsafe {
                    descriptor().digit.unwrap()(
                        LOCAL.as_ptr().cast(),
                        LOCAL.len(),
                        *digit,
                        &mut completed,
                    )
                },
                0
            );
            assert!(completed <= 1);
        }
        assert_eq!(completed, 1);
    }
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn lifecycle_thread_and_drain_failures_remain_recoverable() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let configuration = || active_configuration(&[]);

    FAIL_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 1);
    // The failed start may retain a pending account withdrawal. Drain it before
    // the next independent injected start failure rather than testing Busy.
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    FAIL_TICKER_THREAD.store(true, Ordering::Release);
    crate::fixture::CONTROL_STOP_RESULT.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 1);
    assert!(ENGINE.lock().unwrap().is_some());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    PANIC_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let (engine, call) = selected().unwrap();
    let stopper = std::thread::spawn(crate::lifecycle::tests::fixture_stop);
    std::thread::sleep(Duration::from_millis(2));
    drop(call);
    assert_eq!(stopper.join().unwrap(), 0);
    drop(engine);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let (engine, call) = selected().unwrap();
    let (release_job, finish_job) = mpsc::sync_channel::<()>(1);
    engine
        .jobs
        .lock()
        .unwrap()
        .push(std::thread::spawn(move || {
            let _ = finish_job.recv();
        }));
    drop(call);
    clear_scheduler_notices();
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    release_job.send(()).unwrap();
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(
        scheduler_notices(),
        ["controller stop pending: asynchronous jobs have not finished"]
    );

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let (engine, call) = selected().unwrap();
    engine.jobs.lock().unwrap().push(std::thread::spawn(|| {
        panic!("injected dial job failure");
    }));
    drop(call);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(idle_configuration()) }, 0);
    HOST_STOP_MODE.store(1, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    HOST_STOP_MODE.store(2, Ordering::Release);
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, -1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn stale_removed_node_and_failed_reload_preserve_lifecycle_ownership() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = || active_configuration(&[]);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let (engine, call) = selected().unwrap();
    let revision = engine.revision.load(Ordering::Acquire);
    let LinkEffect::Connect(attempt) = engine
        .operation(LOCAL, operation(LinkAction::Transceive, "2000"))
        .unwrap()
    else {
        panic!("connect operation must reserve a dial");
    };
    assert_eq!(
        reload(&engine, idle_configuration()),
        Ok(AdoptionOutcome::Confirmed)
    );
    assert_eq!(
        engine.dial(LOCAL.into(), Dial::Connect(attempt), revision),
        Err(RuntimeError::Rejected)
    );
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(configuration()) }, 0);
    let replacement = station::prepare(
        &[
            (LOCAL, "1000", json!({})),
            (SECOND_LOCAL, "2000", json!({})),
        ],
        &[
            radio("usb"),
            (
                SECOND_LOCAL,
                Kind::Interface,
                "",
                json!({"audio":{"device_identifier":"hw:second"}}),
            ),
        ],
    );
    let (engine, guard) = selected().unwrap();
    crate::fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
    let result = reload(&engine, replacement);
    let unconsumed = crate::fixture::RADIO_OPEN_RESULT.swap(0, Ordering::AcqRel);
    drop(guard);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(
        unconsumed, 0,
        "the provider must receive the attempted open"
    );
    assert_ne!(result, Ok(AdoptionOutcome::Confirmed));
}

#[test]
fn ticker_route_and_peer_digits_drive_serialized_background_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    crate::fixture::LOCAL_TIME_RESULT.store(6, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (wall_engine, wall_call) = selected().unwrap();
    wall_engine.reloading.store(true, Ordering::Release);
    std::thread::sleep(Duration::from_millis(35));
    wall_engine.reloading.store(false, Ordering::Release);
    drop(wall_call);
    drop(wall_engine);
    crate::fixture::LOCAL_TIME_RESULT.store(0, Ordering::Release);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);

    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let configuration = active_configuration(&[(
        LOCAL,
        Kind::PermanentGroup,
        "primary",
        json!({"remote_node":["2000"]}),
    )]);
    crate::fixture::RADIO_READY.store(1, Ordering::Release);
    assert_eq!(unsafe { start(configuration) }, 0);
    let (engine, call) = selected().unwrap();
    FAIL_DIAL_THREAD.store(true, Ordering::Release);
    engine.pump(engine.clock(), true).unwrap();
    wait_for_ingress(&engine, "2000");
    assert!(
        engine
            .run(|host| host.status_text(LOCAL))
            .unwrap()
            .contains("2000")
    );

    crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(b"*722");
    let deadline = Instant::now() + Duration::from_secs(1);
    while !crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(5));
    engine.pump(engine.clock(), false).unwrap();

    crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(b"*31000#");
    while !crate::fixture::PEER_DIGITS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(5));
    engine.pump(engine.clock(), false).unwrap();
    // A recognized self-link command queues its failure response without
    // declaring correctly received mapped digits lost.
    assert!(!engine.lost_digits.load(Ordering::Acquire));

    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    crate::fixture::RADIO_READY.store(0, Ordering::Release);
}

#[test]
fn registered_uuid_engine_uses_account_at_native_dial_and_preauthorization_boundary() {
    use std::sync::Mutex;
    static LOCAL: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    unsafe extern "C" fn capture(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        local: *const std::ffi::c_char,
        n: usize,
        _: usize,
        _: abi::rptadv_current_v1,
        _: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32 {
        *LOCAL.lock().unwrap() = unsafe { std::slice::from_raw_parts(local.cast(), n) }.to_vec();
        unsafe { *out = std::ptr::null_mut() };
        -1
    }
    const SLOT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_dial = Some(capture);
    for account in [Some(1000), None] {
        let configuration = if account.is_some() {
            active_configuration(&[])
        } else {
            station::prepare(
                &[(SLOT, "North", json!({}))],
                &[(
                    SLOT,
                    Kind::Interface,
                    "",
                    json!({"audio":{"device_identifier":"usb"}}),
                )],
            )
        };
        assert_eq!(unsafe { start_table(configuration, &table) }, 0);
        let (engine, call) = selected().unwrap();
        let proof = admission("2000", "127.0.0.1");
        assert_eq!(proof.is_ok(), account.is_some());
        if let Ok(proof) = proof {
            assert_eq!(
                unsafe { super::asl::ADMISSION.complete.unwrap()(proof, ptr::null_mut()) },
                0
            );
        }
        LOCAL.lock().unwrap().clear();
        let effect = engine
            .operation(SLOT, operation(LinkAction::Transceive, "3000"))
            .unwrap();
        assert!(matches!(&effect, LinkEffect::Connect(_)));
        let _ = engine.effect(SLOT.into(), effect, engine.revision.load(Ordering::Acquire));
        assert_eq!(
            LOCAL.lock().unwrap().as_slice(),
            if account.is_some() {
                b"1000".as_slice()
            } else {
                b""
            }
        );
        drop(call);
        assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn modular_radio_preparation_survives_serial_executor_migration_and_expires_with_its_ticket() {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
            ResourceRegistry, ResourceScope,
        },
    };
    use serde_json::json;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let site: ResourceId = "00000000-0000-4000-8000-000000000001".parse().unwrap();
    let port: ResourceId = "00000000-0000-4000-8000-000000000002".parse().unwrap();
    let interface = ResourceBinding {
        id: "00000000-0000-4000-8000-000000000003".parse().unwrap(),
        scope: ResourceScope::Port(port),
        kind: ResourceKind::Interface,
    };
    let identifier = ResourceBinding {
        id: "00000000-0000-4000-8000-000000000004".parse().unwrap(),
        scope: ResourceScope::Site(site),
        kind: ResourceKind::Identifier,
    };
    let registry = ResourceRegistry::with_members(
        site,
        &[(port, PortKey::new("back room"))],
        &[interface],
        &[(identifier, PortKey::new("station-id"))],
    )
    .unwrap();
    let rows: Vec<_> = registry
        .bindings()
        .map(|binding| {
            let mut row = json!({"id":binding.id.to_string(), "kind":binding.kind.as_str(),
            "configuration":{"name":"Site","inherit_from":null}});
            if binding.id == port {
                row["key"] = json!("back room");
                row["configuration"] = json!({"name":"Local","inherit_from":"site","enabled":true});
            }
            if binding == interface {
                row["configuration"] =
                    json!({"audio":{"device_identifier":"test-only missing device"}});
            }
            if binding == identifier {
                row["key"] = json!("station-id");
                row["configuration"] = json!({
                    "morse":{"text":"CQ TEST"}, "timing":{"interval_ms":10},
                    "regardless_of_activity":true
                });
            }
            (binding, serde_json::to_vec(&row).unwrap())
        })
        .collect();
    let capture: Vec<_> = rows
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let candidate = ModularCandidate::assemble(&registry, &capture).unwrap();
    let inputs = Arc::new(crate::management::ProfileInputs {
        messages: rpt_advanced_core::messages::InstalledMessageSources::capture(),
        interfaces: Default::default(),
    });
    let configuration =
        crate::management::PreparedConfiguration::prepare(&candidate, Some("UTC"), &inputs)
            .unwrap();

    let domain = configuration.runtime();
    {
        use rpt_advanced_core::config::resources::profiles::equipment::{
            BoundInterface, InterfaceCapability,
        };
        let station = domain.radio.port(port).unwrap();
        let declared = |id, property: &str, kind| {
            crate::site::radio::declared_pin_capability(
                station,
                &BoundInterface {
                    port: id,
                    key: PortKey::new("back room"),
                    property: property.into(),
                },
                kind,
            )
        };
        assert_eq!(
            declared(port, "/gpio/pins/2", ResourceKind::Input),
            Some(InterfaceCapability::Boolean)
        );
        assert_eq!(declared(port, "/gpio/pins/2", ResourceKind::Output), None);
        assert_eq!(declared(port, "/gpio/pins/3", ResourceKind::Input), None);
        assert_eq!(declared(site, "/gpio/pins/2", ResourceKind::Input), None);
        assert_eq!(declared(port, "/gpio/pins/02", ResourceKind::Input), None);
    }

    let node = domain.station_settings(port).unwrap();
    assert_eq!(node.channel, port.to_string());
    assert_eq!(
        node.language,
        domain.scopes[&ResourceScope::Port(port)].metadata.language
    );
    assert_eq!(
        node.hang_ms,
        u64::from(domain.radio.port(port).unwrap().signals.tx.hang_ms)
    );
    assert_eq!(
        node.dtmf_admin_timeout_ms,
        domain.scopes[&ResourceScope::Port(port)]
            .policy
            .admin_timeout_ms
    );
    assert!(domain.station_settings(site).is_err());
    let derived = &domain.scopes[&ResourceScope::Port(port)].media;
    let original = &derived.recipes[&("identifiers", "station-id".into())];
    let mut count = 0;
    let (mut controller, mut controls) = domain
        .prepare_station_controller(port, 7, |binding, name, recipe| {
            assert_eq!(binding, identifier);
            assert_eq!(name, "station-id");
            assert!(std::ptr::eq(recipe, original));
            count += 1;
            Ok(27)
        })
        .unwrap();
    assert_eq!(count, 1);
    controller.process_audio(false, false, &[], &mut [0.0; 960]);
    let attempt = controls
        .take_message_request()
        .expect("scheduled station ID reached the control owner");
    assert_eq!(attempt.attempt.generation, 7);
    assert_eq!(attempt.content_index, 27);
    assert!(
        domain
            .prepare_station_controller(site, 7, |_, _, _| unreachable!())
            .is_err()
    );

    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(idle_configuration()) }, 0);
    let (engine, call) = selected().unwrap();
    drop(call);
    let opens = crate::fixture::RADIO_OPENS.load(Ordering::Acquire);
    let stage =
        |engine: &Arc<Engine>| engine.prepare_radio_plans(Arc::clone(configuration.runtime()));
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert!(matches!(stage(&engine), Err(RuntimeError::Rejected)));
    crate::fixture::CONTROL_REVISION.store(
        ptr::from_ref(engine.revision.as_ref()).cast_mut(),
        Ordering::Release,
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(3, Ordering::Release);
    assert!(matches!(stage(&engine), Err(RuntimeError::Rejected)));
    let ticket = stage(&engine).unwrap();
    assert!(matches!(stage(&engine), Err(RuntimeError::Busy)));
    std::thread::scope(|threads| {
        let engine = engine.clone();
        let key = ticket.clone();
        threads
            .spawn(move || {
                engine.run(move |host| {
                    let plan = host.staged_radio(&key, port)?;
                    let station = &plan.station;
                    assert_eq!(station.port_id(), port);
                    assert!(station.inspect().is_ok());
                    let mut peer = plan.incoming.prepare_peer(8000, 160).unwrap();
                    let mut pcm = [100i16; 160];
                    peer.process(&mut pcm).unwrap();
                    assert_eq!(pcm, [100i16; 160]);
                    Ok(())
                })
            })
            .join()
            .unwrap()
            .unwrap();
    });
    drop(ticket);
    let replacement = stage(&engine).unwrap();
    let site: PreparedSite = engine
        .prepare_modular_sites(configuration.runtime(), &replacement)
        .unwrap();
    engine.begin_modular_sites(site).unwrap();
    let retired = engine
        .run(|host| {
            assert!(host.site_barrier(0)?);
            Ok(host.abort_sites())
        })
        .unwrap();
    drop(retired);
    assert_eq!(crate::fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    drop(replacement);
}

#[cfg(target_os = "linux")]
#[path = "phone_courtesy_tests.rs"]
mod phone_courtesy;

#[test]
fn admitted_dtmf_dial_survives_a_tick_sampled_before_its_submission() {
    use rpt_advanced_core::command_language::{MacroOutcome, prepare_runtime_action};
    use serde_json::json;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();
    let port = engine.run(|host| Ok(crate::host::tests::commands::install(
        host, LOCAL, "North", Some(1000),
        json!({"mappings":{"fixture-dial":{"action":"phone_dial","command":"*509","protected":false}}}),
        &[],
    ))).unwrap();

    let mut outcomes = Vec::new();
    for earlier_tick in [false, true] {
        let sampled = engine.clock();
        // Model the real serialized queue interleaving, with an actual later management timestamp.
        while engine.elapsed_ms() <= sampled.now_ms {
            std::thread::yield_now();
        }
        let admitted_at = engine.clock();
        let receipt = engine
            .run(move |host| {
                let action =
                    prepare_runtime_action(&json!({"action":"execute_dtmf","digits":"*50901816#"}))
                        .map_err(|_| RuntimeError::Rejected)?;
                match host
                    .runtime
                    .management_action(port, "test", &action, admitted_at)?
                {
                    (_, LinkEffect::CommandQueued(response)) => Ok(response),
                    _ => Err(RuntimeError::Rejected),
                }
            })
            .unwrap();
        engine
            .pump(
                if earlier_tick {
                    sampled
                } else {
                    engine.clock()
                },
                false,
            )
            .unwrap();
        let first = receipt.outcome();
        engine.pump(engine.clock(), false).unwrap();
        outcomes.push((first, receipt.outcome()));
    }
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    // This fixture has no registered phone account. The queued programme reaches the domain
    // rejection only if its DTMF was dispatched and its asynchronous receipt was settled.
    assert_eq!(outcomes[0], (None, Some(MacroOutcome::Rejected)));
    assert_eq!(
        outcomes[1], outcomes[0],
        "an earlier ticker sample must not discard an admitted dial"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn outgoing_progress_reader_observes_reload_before_core_publication() {
    use rpt_advanced_core::command_language::prepare_runtime_action;
    use serde_json::json;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(unsafe { start(active_configuration(&[])) }, 0);
    let (engine, call) = selected().unwrap();
    let at = engine.clock();
    let permit = engine
        .run(move |host| {
            let port = crate::host::tests::commands::install(
                host,
                LOCAL,
                "North",
                Some(1000),
                json!({}),
                &[],
            );
            let action =
                prepare_runtime_action(&json!({"action":"phone_dial","parameter":"01852"}))
                    .map_err(|_| RuntimeError::Rejected)?;
            match host
                .runtime
                .management_action(port, "operator", &action, at)?
            {
                (_, LinkEffect::PhoneDial(_, permit)) => Ok(permit),
                _ => Err(RuntimeError::Rejected),
            }
        })
        .unwrap();
    let revision = engine.revision.load(Ordering::Acquire);
    let observed = crate::host::fixture_cut_progress(&engine, revision, permit, || {
        engine.reloading.store(true, Ordering::Release);
    });
    engine.reloading.store(false, Ordering::Release);
    drop(call);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(observed.unwrap(), (true, true, false, false));
}
