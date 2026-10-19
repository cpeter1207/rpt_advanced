//! Hardware-boundary fixtures keep queueing, lifecycle, and generation fencing real.
use super::*;
use crate::site::worker::{DeviceWorker, Outcome};
use rpt_advanced_core::runtime::{GenerationSettings, GenerationWork, NodeHost, RuntimeGeneration};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) static BLOCK_GPIO: AtomicBool = AtomicBool::new(false);
pub(super) static GPIO_ENTERED: AtomicBool = AtomicBool::new(false);

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "owner did not complete bounded fixture work"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
struct ReleaseInput;
impl Drop for ReleaseInput {
    fn drop(&mut self) {
        BLOCK_GPIO.store(false, Ordering::Release);
    }
}
fn generation(id: u64) -> RuntimeGeneration<(), ()> {
    RuntimeGeneration::prepare(
        id,
        GenerationSettings {
            node: "1000".into(),
            device: "fixture".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
        },
        (),
        (),
    )
    .unwrap()
}
fn permit(host: &mut NodeHost<(), ()>) -> GenerationWork {
    host.control().work().unwrap()
}
fn read_result(worker: &mut DeviceWorker) -> crate::site::worker::Reply {
    let mut reply = None;
    until(|| {
        reply = worker.poll();
        reply.is_some()
    });
    reply.unwrap()
}
fn close(worker: &mut DeviceWorker) {
    worker.pause(true);
    until(|| worker.barrier().is_some());
    assert_eq!(worker.barrier(), Some(Ok(())));
    assert!(worker.finish());
}
fn host_cleanup(host: &mut NodeHost<(), ()>, generation: u64) {
    host.control().stop(0).unwrap();
    host.control().mark_detached(generation).unwrap();
    assert!(host.reclaim());
}

#[test]
fn site_worker_slow_device_isolated_and_repeated_polls_bounded() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    BLOCK_GPIO.store(true, Ordering::Release);
    GPIO_ENTERED.store(false, Ordering::Release);
    let direction = Direction::Input { active_high: true };
    let mut host = NodeHost::new(generation(1));
    let mut slow = DeviceWorker::start(
        backends(),
        vec![(settings(SiteBackend::Gpiod), direction)],
        Instant::now(),
    )
    .unwrap();
    let mut fast = DeviceWorker::start(
        backends(),
        vec![(settings(SiteBackend::Hwmon), direction)],
        Instant::now(),
    )
    .unwrap();
    let release = ReleaseInput;
    assert!(slow.submit(0, None, permit(&mut host)));
    until(|| GPIO_ENTERED.load(Ordering::Acquire));
    for _ in 0..1000 {
        assert!(!slow.submit(0, None, permit(&mut host)));
    }
    assert!(fast.submit(0, None, permit(&mut host)));
    assert_eq!(
        read_result(&mut fast).outcome,
        Ok(Outcome::Reading(Sample::Number(12.5)))
    );
    assert!(slow.poll().is_none());
    drop(release);
    assert_eq!(
        read_result(&mut slow).outcome,
        Ok(Outcome::Reading(Sample::Digital(true)))
    );
    close(&mut slow);
    close(&mut fast);
    host_cleanup(&mut host, 1);
}

#[test]
fn site_worker_fence_cancels_old_queued_writes_before_handoff() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    BLOCK_GPIO.store(true, Ordering::Release);
    GPIO_ENTERED.store(false, Ordering::Release);
    let mut host = NodeHost::new(generation(1));
    let mut output = settings(SiteBackend::Gpiod);
    output.channel = "8".into();
    let mut worker = DeviceWorker::start(
        backends(),
        vec![
            (
                settings(SiteBackend::Gpiod),
                Direction::Input { active_high: true },
            ),
            (
                output,
                Direction::Output {
                    active_high: true,
                    startup_on: false,
                    safe_on: false,
                },
            ),
        ],
        Instant::now(),
    )
    .unwrap();
    let release = ReleaseInput;
    assert!(worker.submit(0, None, permit(&mut host)));
    until(|| GPIO_ENTERED.load(Ordering::Acquire));
    assert!(worker.submit(1, Some(1.0), permit(&mut host)));
    worker.pause(false);
    assert!(!worker.submit(1, Some(0.0), permit(&mut host)));
    assert_eq!(worker.barrier(), None); // Physical read is still using the old owner.
    drop(release);
    until(|| worker.barrier().is_some());
    while worker.poll().is_some() {}
    assert!(PROBE.lock().unwrap().written.is_empty());
    host.control().publish(generation(2), 1).unwrap();
    host.control().mark_detached(1).unwrap();
    assert!(host.reclaim()); // No lifetime-held or undrained old work permit.
    assert!(worker.resume());
    assert!(worker.submit(1, Some(0.0), permit(&mut host)));
    assert_eq!(
        read_result(&mut worker).outcome,
        Ok(Outcome::Submitted { pwm: false })
    );
    assert_eq!(PROBE.lock().unwrap().written.clone(), vec![0.0]);
    close(&mut worker);
    host_cleanup(&mut host, 2);
}

#[test]
fn site_worker_safe_failure_retains_owner_and_unchanged_resume_does_not_reopen() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *PROBE.lock().unwrap() = Probe::default();
    let mut host = NodeHost::new(generation(1));
    let mut worker = DeviceWorker::start(
        backends(),
        vec![(
            settings(SiteBackend::Gpiod),
            Direction::Output {
                active_high: true,
                startup_on: false,
                safe_on: false,
            },
        )],
        Instant::now(),
    )
    .unwrap();
    assert!(worker.submit(0, Some(1.0), permit(&mut host)));
    assert_eq!(
        read_result(&mut worker).outcome,
        Ok(Outcome::Submitted { pwm: false })
    );
    worker.pause(false);
    until(|| worker.barrier().is_some());
    assert_eq!(PROBE.lock().unwrap().written.clone(), vec![1.0]);
    assert!(worker.resume());
    assert!(worker.submit(0, Some(0.0), permit(&mut host)));
    assert!(read_result(&mut worker).outcome.is_ok());
    assert_eq!(PROBE.lock().unwrap().opened.len(), 1);
    PROBE.lock().unwrap().failure = true;
    worker.pause(true);
    until(|| worker.barrier().is_some());
    assert_eq!(worker.barrier(), Some(Err(ReadingQuality::Fault)));
    assert!(!worker.finish());
    assert_eq!(PROBE.lock().unwrap().closed, 0);
    PROBE.lock().unwrap().failure = false;
    close(&mut worker);
    assert_eq!(PROBE.lock().unwrap().closed, 1);
    assert_eq!(PROBE.lock().unwrap().written.clone(), vec![1.0, 0.0, 0.0]);
    host_cleanup(&mut host, 1);
}
