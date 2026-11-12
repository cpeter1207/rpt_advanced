//! Real site handoff and shutdown policy with delayed hardware acknowledgments.
use super::*;
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
static FOLLOW_PANIC: Mutex<Option<(Weak<Engine>, Arc<std::sync::atomic::AtomicBool>)>> =
    Mutex::new(None);
static FATAL_BEFORE_RUN: Mutex<Option<Weak<Engine>>> = Mutex::new(None);
static SUBMISSIONS: AtomicUsize = AtomicUsize::new(0);
static REJECT_AFTER: AtomicUsize = AtomicUsize::new(0);
static CLOSE_AFTER: Mutex<Option<(usize, Weak<Engine>)>> = Mutex::new(None);

fn apply(bank: &mut Bank, binding: bool, mask: u32, high: u32) {
    if binding {
        bank.generation += 1;
    }
    bank.mask = mask;
    bank.high = high;
}

unsafe extern "C" fn snapshot(
    _: *mut c_void,
    _: *mut c_void,
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
    if bank.hold != 0 {
        bank.pending = Some((false, value.output_mask, value.high_mask));
    } else {
        apply(&mut bank, false, value.output_mask, value.high_mask);
    }
    0
}

unsafe extern "C" fn submit(context: *mut c_void, task: abi::rptadv_control_task_v1) -> i32 {
    SUBMISSIONS.fetch_add(1, Ordering::AcqRel);
    if REJECT_AFTER.fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
        remaining.checked_sub(1)
    }) == Ok(1)
    {
        return -1;
    }
    let close = {
        let mut pending = CLOSE_AFTER.lock().unwrap();
        if let Some((remaining, _)) = pending.as_mut() {
            *remaining -= 1;
        }
        if pending
            .as_ref()
            .is_some_and(|(remaining, _)| *remaining == 0)
        {
            pending.take().and_then(|(_, engine)| engine.upgrade())
        } else {
            None
        }
    };
    if let Some(engine) = FATAL_BEFORE_RUN
        .lock()
        .unwrap()
        .take()
        .and_then(|value| value.upgrade())
    {
        engine.reloading.store(true, Ordering::Release);
        engine.reload_status.store(-3, Ordering::Release);
    }
    let result = unsafe { (*crate::fixture::control_descriptor()).submit.unwrap()(context, task) };
    let follower = FOLLOW_PANIC.lock().unwrap().take();
    if let Some((engine, executed)) = follower {
        if let Some(engine) = engine.upgrade() {
            let _ = engine.run_inner(true, move |_| {
                executed.store(true, Ordering::Release);
                Ok(())
            });
        }
    }

    if let Some(engine) = close {
        engine.accepting.store(false, Ordering::Release);
    }
    result
}

fn start(configuration: &str) -> Arc<Engine> {
    assert_eq!(rptadv_product_stop(), 0);
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
    assert_eq!(
        unsafe {
            rptadv_product_start(
                Box::leak(Box::new(host)),
                Box::leak(Box::new(control)),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                configuration.as_ptr().cast(),
                configuration.len(),
            )
        },
        0
    );
    let (engine, call) = selected().unwrap();
    drop(call);
    engine
}

#[test]
fn site_handoff_timeout_keeps_ownership_until_delayed_safe_service() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(
        "[1000]\nradio_channel=usb\nduplex=full\n\
         [output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\n\
         line=7\nstartup_state=on\nsafe_state=off\n",
    );
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
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn site_shutdown_retries_pending_safe_service_before_releasing_the_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start(
        "[1000]\nradio_channel=usb\nduplex=full\n\
         [output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\n\
         line=7\nstartup_state=on\nsafe_state=off\n",
    );
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
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn site_reload_publication_survives_concurrent_admission_close() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    let candidate = document("[1000]\nradio_channel=usb\nduplex=full\nhangtime_ms=123\n").unwrap();
    let before = engine.revision.load(Ordering::Acquire);
    // Basis, begin, barrier and publication are separate serialized tasks.
    *CLOSE_AFTER.lock().unwrap() = Some((4, Arc::downgrade(&engine)));
    assert_eq!(
        engine.reload_configuration(candidate),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -2);
    assert!(!engine.accepting.load(Ordering::Acquire));
    assert_eq!(engine.revision.load(Ordering::Acquire), before + 1);
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn site_cleanup_submission_failure_is_observable_without_changing_digit_policy() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clear();
    REJECT_AFTER.store(1, Ordering::Release);
    let result = engine.site_control(|_| ());
    let lost_digits = engine.lost_digits.load(Ordering::Acquire);
    let mut status = String::new();
    engine.health.status(&mut status);
    let notices = crate::fixture::SCHEDULER_NOTICES.lock().unwrap().clone();
    assert_eq!(rptadv_product_stop(), 0);
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
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    // Site cleanup must succeed before rejecting the final Host stop task.
    REJECT_AFTER.store(2, Ordering::Release);
    assert!(!engine.stop());
    assert!(engine.host.lock().unwrap().is_some());
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_terminal_cleanup_failure_preserves_published_candidate_and_diagnosis() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    let next = "[1000]\nradio_channel=usb\nduplex=full\nhangtime_ms=123\n";
    let before = engine.run(|host| Ok(host.runtime.revision())).unwrap();
    // Basis, begin, barrier, publish, then rejected retirement collection.
    REJECT_AFTER.store(5, Ordering::Release);
    assert_eq!(
        engine.reload_configuration(document(next).unwrap()),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -2);
    assert_eq!(
        engine.run(|host| Ok(host.runtime.revision())).unwrap(),
        before + 1
    );
    assert_eq!(unsafe { rptadv_product_reload_poll() }, -2);
    assert_eq!(unsafe { rptadv_product_reload_poll() }, -2);
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_terminal_unconfirmed_control_reply_is_fatal_and_keeps_admission_closed() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    REJECT_AFTER.store(4, Ordering::Release);
    assert!(
        engine
            .reload_configuration(
                document("[1000]\nradio_channel=usb\nduplex=full\nhangtime_ms=123\n").unwrap()
            )
            .is_err()
    );
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -3);
    assert!(engine.reloading.load(Ordering::Acquire));
    assert_eq!(engine.run(|_| Ok(())), Err(RuntimeError::Rejected));
    assert_eq!(unsafe { rptadv_product_reload_poll() }, -3);
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_rejected_parse_latches_the_new_terminal_status() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    engine.reload_status.store(-2, Ordering::Release);
    assert_eq!(unsafe { rptadv_product_reload(std::ptr::null(), 1) }, -1);
    assert_eq!(unsafe { rptadv_product_reload_poll() }, -1);
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_early_mutating_handoff_uncertainty_never_claims_rejection() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    REJECT_AFTER.store(2, Ordering::Release);
    assert!(
        engine
            .reload_configuration(
                document("[1000]\nradio_channel=usb\nduplex=full\nhangtime_ms=123\n").unwrap()
            )
            .is_err()
    );
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -3);
    assert!(engine.reloading.load(Ordering::Acquire));
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_fatal_cleanup_never_resumes_or_reopens_admission() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    engine.reload_status.store(-1, Ordering::Release);
    engine.reloading.store(true, Ordering::Release);
    let before = SUBMISSIONS.load(Ordering::Acquire);
    REJECT_AFTER.store(1, Ordering::Release);
    assert!(
        engine
            .finish_reload(Err(RuntimeError::Preparation))
            .is_err()
    );
    assert_eq!(SUBMISSIONS.load(Ordering::Acquire), before + 1);
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -3);
    assert!(engine.reloading.load(Ordering::Acquire));
    assert_eq!(rptadv_product_stop(), 0);
}

#[test]
fn reload_fatal_ticker_turn_does_not_pump_retained_candidate() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    engine.reloading.store(true, Ordering::Release);
    engine.reload_status.store(-3, Ordering::Release);
    let before = SUBMISSIONS.load(Ordering::Acquire);
    let turns = TICKER_TURNS.load(Ordering::Acquire);
    SKIP_TICKER_THREAD.store(false, Ordering::Release);
    engine.start_ticker().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while TICKER_TURNS.load(Ordering::Acquire) == turns {
        assert!(Instant::now() < deadline, "ticker did not execute its turn");
        std::thread::sleep(Duration::from_millis(1));
    }
    let submissions = SUBMISSIONS.load(Ordering::Acquire);
    assert_eq!(rptadv_product_stop(), 0);
    assert_eq!(
        submissions, before,
        "fatal ticker must not service retained runtime state"
    );
}

#[test]
fn reload_queued_operation_checks_fatal_fence_before_execution() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let target = Arc::clone(&executed);
    // Inject the fatal disposition after enqueue but before the real executor
    // invokes the task callback, not before run_inner captures its admission.
    *FATAL_BEFORE_RUN.lock().unwrap() = Some(Arc::downgrade(&engine));
    let result = engine.run_inner(true, move |_| {
        target.store(true, Ordering::Release);
        Ok(())
    });
    let shutdown = rptadv_product_stop();
    assert_eq!(result, Err(RuntimeError::Rejected));
    assert!(!executed.load(Ordering::Acquire));
    assert_eq!(shutdown, 0, "dedicated site shutdown remains admissible");
}

#[test]
fn reload_panicking_mutation_fences_executor_before_waiter_observes_reply() {
    assert_panic_fences_executor(false);
}

#[test]
fn reload_panicking_mutation_cleanup_fences_executor_before_waiter_observes_reply() {
    assert_panic_fences_executor(true);
}

fn assert_panic_fences_executor(cleanup: bool) {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    {
        let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
        engine.reloading.store(true, Ordering::Release);
        engine.reload_status.store(1, Ordering::Release);
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *FOLLOW_PANIC.lock().unwrap() = Some((Arc::downgrade(&engine), Arc::clone(&executed)));
        // The fixture dispatches a follower before submit returns, so the waiting
        // caller cannot latch a fatal disposition in time to protect it.
        let result: Result<(), RuntimeError> = if cleanup {
            engine.site_control(|_| panic!("injected mutating cleanup panic"))
        } else {
            engine.run_inner(true, |_| panic!("injected reload mutation panic"))
        };
        let status = engine.reload_status.load(Ordering::Acquire);
        let stopped = rptadv_product_stop();
        assert!(result.is_err());
        assert!(
            !executed.load(Ordering::Acquire),
            "queued follower ran after mutation panic"
        );
        assert_eq!(status, -3);
        assert_eq!(stopped, 0);
    }
}

#[test]
fn reload_barrier_panic_remains_fatal_through_caller_classification() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    PANIC_CONTROL_AFTER.store(3, Ordering::Release);
    let result = engine.reload_configuration(
        document("[1000]\nradio_channel=usb\nduplex=full\nhangtime_ms=123\n").unwrap(),
    );
    let status = engine.reload_status.load(Ordering::Acquire);
    let fenced = engine.reloading.load(Ordering::Acquire);
    let stopped = rptadv_product_stop();
    assert!(result.is_err());
    assert_eq!(status, -3);
    assert!(fenced);
    assert_eq!(stopped, 0);
}

#[test]
fn reload_finish_panic_remains_fatal_after_committed_cleanup() {
    assert_finish_panic_is_fatal(1);
}
#[test]
fn reload_finish_panic_remains_fatal_after_committed_resume() {
    assert_finish_panic_is_fatal(2);
}
#[test]
fn reload_finish_panic_remains_fatal_after_committed_notification() {
    assert_finish_panic_is_fatal(3);
}
fn assert_finish_panic_is_fatal(turn: u64) {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    engine.reloading.store(true, Ordering::Release);
    engine.reload_status.store(0, Ordering::Release);
    let before = SUBMISSIONS.load(Ordering::Acquire);
    PANIC_CONTROL_AFTER.store(turn, Ordering::Release);
    let result = engine.finish_reload(Ok(()));
    let submitted = SUBMISSIONS.load(Ordering::Acquire) - before;
    let status = engine.reload_status.load(Ordering::Acquire);
    let fenced = engine.reloading.load(Ordering::Acquire);
    let stopped = rptadv_product_stop();
    assert!(result.is_err());
    assert_eq!(status, -3);
    assert!(fenced);
    assert_eq!(
        submitted, turn as usize,
        "no continuation after fatal mutation"
    );
    assert_eq!(stopped, 0);
}

#[test]
fn reload_final_clear_race_keeps_authoritative_fatal_admission_closed() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let engine = start("[1000]\nradio_channel=usb\nduplex=full\n");
    engine.reloading.store(true, Ordering::Release);
    engine.reload_status.store(0, Ordering::Release);
    FATAL_BEFORE_FINISH_CLEAR.store(true, Ordering::Release);
    let _ = engine.finish_reload(Ok(()));
    assert_eq!(engine.reload_status.load(Ordering::Acquire), -3);
    let admitted = engine.enter().is_ok();
    let current = engine.current(engine.revision.load(Ordering::Acquire));
    let before = SUBMISSIONS.load(Ordering::Acquire);
    let turns = TICKER_TURNS.load(Ordering::Acquire);
    SKIP_TICKER_THREAD.store(false, Ordering::Release);
    engine.start_ticker().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while TICKER_TURNS.load(Ordering::Acquire) == turns {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let submissions = SUBMISSIONS.load(Ordering::Acquire);
    let stopped = rptadv_product_stop();
    assert!(
        !admitted,
        "stale boolean must not reopen external admission"
    );
    assert!(!current, "fatal epoch must never be current");
    assert_eq!(
        submissions, before,
        "fatal ticker must not schedule ordinary work"
    );
    assert_eq!(stopped, 0);
}
