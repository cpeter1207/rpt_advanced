//! Real site handoff and shutdown policy with delayed hardware acknowledgments.
use super::*;
use crate::{
    fixture::station::{self, LOCAL},
    management::PreparedConfiguration,
};
use rpt_advanced_core::config::resources::candidate::ResourceKind as Kind;
use serde_json::json;
use std::sync::{Weak, atomic::AtomicUsize};

struct Bank {
    generation: u64,
    mask: u32,
    high: u32,
    hold: usize,
    pending: Option<(bool, u32, u32)>,
}

static BANK: Mutex<Bank> = Mutex::new(Bank {
    generation: 7,
    mask: 0,
    high: 0,
    hold: 0,
    pending: None,
});
struct ObservedBank {
    owner: String,
    time: u64,
    quality: u32,
    snapshots: usize,
    writes: usize,
}
static OBSERVED: Mutex<Option<ObservedBank>> = Mutex::new(None);

/// Select the cached sample returned by the existing delayed physical bank and read its counters.
pub(crate) fn site_bank_sample(owner: &str, time: u64, quality: u32) -> (usize, usize) {
    let mut observed = OBSERVED.lock().unwrap();
    let state = observed.get_or_insert_with(|| ObservedBank {
        owner: owner.to_owned(),
        time,
        quality,
        snapshots: 0,
        writes: 0,
    });
    assert_eq!(state.owner, owner);
    state.time = time;
    state.quality = quality;
    (state.snapshots, state.writes)
}

static REJECT_AFTER: AtomicUsize = AtomicUsize::new(0);
static CLOSE_AFTER: Mutex<Option<(String, Weak<Engine>)>> = Mutex::new(None);

pub(crate) fn install_delayed_site_bank(host: &mut abi::rptadv_host_services_v11) {
    let mut bank = BANK.lock().unwrap();
    bank.generation = 7;
    bank.mask = 0;
    bank.high = 0;
    bank.hold = 0;
    bank.pending = None;
    host.radio_site_bind = Some(bind);
    host.radio_site_publish = Some(publish);
    host.radio_site_snapshot = Some(snapshot);
}
/// Return checked masks and whether a provider request is still waiting for service.
pub(crate) fn site_bank_electrical() -> (u32, u32, bool) {
    let bank = BANK.lock().unwrap();
    (bank.mask, bank.high, bank.pending.is_some())
}

pub(crate) fn site_bank_delay(hold: usize) -> u32 {
    let mut bank = BANK.lock().unwrap();
    bank.hold = hold;
    bank.high
}

fn apply(bank: &mut Bank, binding: bool, mask: u32, high: u32) {
    if binding {
        bank.generation += 1;
    }
    bank.mask = mask;
    bank.high = high;
}

unsafe extern "C" fn snapshot(
    _: *mut c_void,
    handle: *mut c_void,
    _: u32,
    result: *mut abi::UrpAstSiteSnapshot,
) -> i32 {
    let mut bank = BANK.lock().unwrap();
    if bank.hold != 0 {
        bank.hold -= 1;
    } else if let Some((binding, mask, high)) = bank.pending.take() {
        apply(&mut bank, binding, mask, high);
    }
    unsafe {
        (*result).generation = bank.generation;
        (*result).output_mask = 0xff;
        (*result).input_mask = 0xff;
        (*result).input_quality = 1;
        (*result).observed_at_valid = 1;
        if let Some(view) = OBSERVED.lock().unwrap().as_mut() {
            if crate::fixture::radio_name(handle) != view.owner {
                return -1;
            }
            view.snapshots += 1;
            (*result).observed_at_ms = view.time;
            (*result).snapshot_at_ms = view.time;
            (*result).input_quality = view.quality;
        }
        (*result).claimed_mask = bank.mask;
        (*result).requested_high_mask = bank.high;
        (*result).submitted_high_mask = bank.high;
        (*result).serviced_high_mask = bank.high;
        (*result).submitted_generation = bank.generation;
        (*result).serviced_generation = bank.generation;
    }
    0
}

unsafe extern "C" fn bind(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteBinding,
) -> i32 {
    let value = unsafe { &*value };
    let mut bank = BANK.lock().unwrap();
    assert_eq!(value.generation, bank.generation);
    if bank.hold != 0 {
        bank.pending = Some((true, value.output_mask, value.initial_high_mask));
    } else {
        apply(&mut bank, true, value.output_mask, value.initial_high_mask);
    }
    0
}

unsafe extern "C" fn publish(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteRequest,
) -> i32 {
    let value = unsafe { &*value };
    let mut bank = BANK.lock().unwrap();
    assert_eq!(value.generation, bank.generation);
    if let Some(view) = OBSERVED.lock().unwrap().as_mut() {
        view.writes += 1;
    }
    if bank.hold != 0 {
        bank.pending = Some((false, value.output_mask, value.high_mask));
    } else {
        apply(&mut bank, false, value.output_mask, value.high_mask);
    }
    0
}

unsafe extern "C" fn submit(context: *mut c_void, task: abi::rptadv_control_task_v1) -> i32 {
    if REJECT_AFTER.fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
        remaining.checked_sub(1)
    }) == Ok(1)
    {
        return -1;
    }
    let result = unsafe { (*crate::fixture::control_descriptor()).submit.unwrap()(context, task) };
    let mut checkpoint = CLOSE_AFTER.lock().unwrap();
    if let Some((revision, observed)) = checkpoint.as_ref()
        && let Some(engine) = observed.upgrade()
        && engine.host.try_lock().is_ok_and(|host| {
            host.as_ref()
                .is_some_and(|host| host.modular_revision_matches(revision))
        })
    {
        engine.accepting.store(false, Ordering::Release);
        checkpoint.take();
    }
    result
}

fn site_config(relay: bool) -> PreparedConfiguration {
    let mut rows = vec![(
        LOCAL,
        Kind::Interface,
        "",
        json!({"audio":{"device_identifier":"3-1"},"gpio":{"wiring_profile":"dudeusb","pins":{"7":"out0"}}}),
    )];
    if relay {
        rows.push((LOCAL,Kind::Output,"relay",json!({"enabled":true,"source":{"type":"interface","node":"1000","property":"/gpio/pins/7"},"startup_state":"on","safe_state":"off"})));
    }
    station::prepare(&[(LOCAL, "1000", json!({}))], &rows)
}
fn start(configuration: PreparedConfiguration) -> Arc<Engine> {
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    *OBSERVED.lock().unwrap() = None;
    *BANK.lock().unwrap() = Bank {
        generation: 7,
        mask: 0,
        high: 0,
        hold: 0,
        pending: None,
    };
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let mut host = unsafe { crate::fixture::host_descriptor().read() };
    host.radio_site_bind = Some(bind);
    host.radio_site_publish = Some(publish);
    host.radio_site_snapshot = Some(snapshot);
    let mut control = unsafe { crate::fixture::control_descriptor().read() };
    control.submit = Some(submit);
    unsafe {
        crate::lifecycle::tests::fixture_engine(
            configuration,
            Box::leak(Box::new(host)),
            Box::leak(Box::new(control)),
        )
    }
    .unwrap()
}

#[test]
fn site_handoff_timeout_keeps_ownership_until_delayed_safe_service() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(site_config(true));
    engine
        .run(|host| {
            host.poll_sites(0);
            host.poll_sites(1);
            Ok(())
        })
        .unwrap();
    assert_eq!(BANK.lock().unwrap().high, 0x40);

    BANK.lock().unwrap().hold = 100;
    assert!(!engine.stop_sites_until(Instant::now()));
    assert_eq!(
        engine.wait_sites_until(false, Instant::now()),
        Err(RuntimeError::Busy)
    );
    assert_eq!(BANK.lock().unwrap().mask, 0x40);
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    assert!(engine.host.lock().unwrap().is_some());

    BANK.lock().unwrap().hold = 2;
    engine
        .wait_sites_until(false, Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(engine.stop_sites());
    let bank = BANK.lock().unwrap();
    assert_eq!(bank.mask, 0);
    assert_eq!(bank.high, 0);
    drop(bank);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn site_shutdown_retries_pending_safe_service_before_releasing_the_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(site_config(true));
    engine
        .run(|host| {
            host.poll_sites(0);
            host.poll_sites(1);
            Ok(())
        })
        .unwrap();
    BANK.lock().unwrap().hold = 3;
    assert!(engine.stop_sites_until(Instant::now() + Duration::from_secs(1)));
    assert_eq!(BANK.lock().unwrap().mask, 0);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn site_reload_publication_survives_concurrent_admission_close() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(site_config(false));
    let before = engine.revision.load(Ordering::Acquire);
    let revision = format!("edit-{before}");
    *CLOSE_AFTER.lock().unwrap() = Some((revision.clone(), Arc::downgrade(&engine)));
    let outcome = crate::lifecycle::tests::reload(&engine, site_config(true));
    // The core reached publication before admission closed. Final confirmation must
    // report the loss of admission and cannot undo the already-selected site.
    assert!(outcome.is_err());
    assert!(CLOSE_AFTER.lock().unwrap().is_none());
    assert!(
        engine
            .host
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .modular_revision_matches(&revision)
    );
    assert!(!engine.accepting.load(Ordering::Acquire));
    assert_eq!(engine.revision.load(Ordering::Acquire), before + 1);
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}

#[test]
fn site_cleanup_submission_failure_is_observable_without_changing_digit_policy() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let engine = start(site_config(false));
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
    REJECT_AFTER.store(1, Ordering::Release);
    let result = engine.site_control(|_| ());
    let lost_digits = engine.lost_digits.load(Ordering::Acquire);
    let mut status = String::new();
    engine.health.status(&mut status);
    let notices = crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clone();
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    assert_eq!(result, Err(RuntimeError::Rejected));
    assert!(!lost_digits);
    assert!(status.contains("submission_failures=1"), "{status}");
    assert_eq!(notices.len(), 1, "{notices:?}");
}

#[test]
fn rejected_final_host_stop_retains_host_after_site_cleanup() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(site_config(false));
    // Site cleanup must succeed before rejecting the final Host stop task.
    REJECT_AFTER.store(2, Ordering::Release);
    assert!(!engine.stop());
    assert!(engine.host.lock().unwrap().is_some());
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
}
