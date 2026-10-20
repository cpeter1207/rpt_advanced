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
    let result = unsafe { (*crate::fixture::control_descriptor()).submit.unwrap()(context, task) };
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
    assert_eq!(engine.reload_configuration(candidate), Ok(()));
    assert!(!engine.accepting.load(Ordering::Acquire));
    assert_eq!(engine.revision.load(Ordering::Acquire), before + 1);
    assert_eq!(rptadv_product_stop(), 0);
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
