use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Resource(Arc<AtomicUsize>);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn generation(id: u64, drops: &Arc<AtomicUsize>) -> RuntimeGeneration<Resource, Resource> {
    RuntimeGeneration::prepare(
        id,
        GenerationSettings {
            node: "524950".into(),
            device: "radio0".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
        },
        Resource(drops.clone()),
        Resource(drops.clone()),
    )
    .unwrap()
}

#[test]
fn fixed_owned_audio_registration_allows_control_lookup_without_audio_refcounts() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut receive, mut transmit) = host.register_audio().unwrap();
    assert!(host.register_audio().is_none());
    let old = receive.acquire().unwrap();
    host.control().publish(generation(2, &drops), 1).unwrap();
    assert_eq!(old.id(), 1);
    assert_eq!(transmit.acquire().unwrap().id(), 2);
    drop(old);
    assert_eq!(receive.acquire().unwrap().id(), 2);
    host.control().mark_detached(1).unwrap();
    assert!(host.control().reclaim());
    host.control().stop(2).unwrap();
    host.control().mark_detached(2).unwrap();
    assert!(host.control().reclaim());
    drop(host);
    assert!(receive.acquire().is_none());
    assert!(transmit.acquire().is_none());
    assert_eq!(drops.load(Ordering::Relaxed), 4);
}

#[test]
fn dropping_a_host_never_force_frees_undetached_or_outstanding_resources() {
    for detached in [false, true] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut host = NodeHost::new(generation(1, &drops));
        let work = {
            let (mut control, _, _) = host.split();
            let work = control.work().unwrap();
            control.stop(0).unwrap();
            if detached {
                control.mark_detached(1).unwrap();
            }
            work
        };
        drop(host);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(!work.is_current());
        drop(work);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    {
        let (mut control, _, _) = host.split();
        control.stop(0).unwrap();
        control.mark_detached(1).unwrap();
    }
    drop(host);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn failed_candidate_retains_live_generation_and_releases_partial_resources() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (control, mut receive, _) = host.split();
    let candidate = RuntimeGeneration::prepare(
        2,
        GenerationSettings {
            node: "524950".into(),
            device: "radio0".into(),
            receive_maximum: 0,
            transmit_maximum: 960,
        },
        Resource(drops.clone()),
        Resource(drops.clone()),
    );
    assert!(candidate.is_err());
    assert_eq!(receive.acquire().unwrap().id(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert_eq!(control.status(0).active, Some(1));
}

#[test]
fn both_adoption_orders_block_reclamation_until_protected_owner_finishes() {
    for receive_first in [false, true] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut host = NodeHost::new(generation(1, &drops));
        let (mut control, mut receive, mut transmit) = host.split();
        let old_work = control.work().unwrap();
        let old_receive = receive.acquire().unwrap();
        control.publish(generation(2, &drops), 100).unwrap();
        assert!(!old_work.is_current());
        assert_eq!(control.status(150).state, LifecycleState::AdoptionPending);
        assert!(!control.reclaim());
        drop(old_receive);
        if receive_first {
            drop(receive.acquire().unwrap());
        } else {
            drop(transmit.acquire().unwrap());
        }
        assert_eq!(control.status(175).state, LifecycleState::AdoptionPending);
        drop(receive.acquire().unwrap());
        drop(transmit.acquire().unwrap());
        assert_eq!(control.status(200).state, LifecycleState::RetirementPending);
        control.mark_detached(1).unwrap();
        assert!(!control.reclaim());
        drop(old_work);
        assert!(control.reclaim());
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_eq!(control.status(200).state, LifecycleState::Running);
    }
}

#[test]
fn stalled_owner_is_reported_and_second_retirement_is_rejected() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(10, &drops));
    let (mut control, mut receive, mut transmit) = host.split();
    let stalled = receive.acquire().unwrap();
    control.publish(generation(11, &drops), 100).unwrap();
    drop(transmit.acquire().unwrap());
    control.mark_detached(10).unwrap();
    let status = control.status(5100);
    assert_eq!(
        (status.active, status.retiring, status.age_ms),
        (Some(11), Some(10), 5000)
    );
    assert_eq!(
        (
            status.receive_adopted,
            status.transmit_adopted,
            status.protected_owners
        ),
        (10, 11, 1)
    );
    assert_eq!(
        control.publish(generation(12, &drops), 5200),
        Err(LifecycleError::RetirementPending)
    );
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert!(!control.reclaim());
    drop(stalled);
    drop(receive.acquire().unwrap());
    assert!(control.reclaim());
}

#[test]
fn paired_call_keeps_one_generation_across_publication() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, mut receive, mut transmit) = host.split();
    let mut pair = receive.acquire_pair(&mut transmit).unwrap();
    control.publish(generation(2, &drops), 0).unwrap();
    assert_eq!(pair.id(), 1);
    let _ = pair.receive();
    let _ = pair.transmit();
    assert_eq!(control.status(0).protected_owners, 2);
    drop(pair);
    assert_eq!(receive.acquire_pair(&mut transmit).unwrap().id(), 2);
}

#[test]
fn unload_gates_new_callbacks_and_waits_for_work_and_detachment() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, mut receive, mut transmit) = host.split();
    let work = control.work().unwrap();
    let callback = transmit.acquire().unwrap();
    control.stop(10).unwrap();
    assert!(receive.acquire().is_none());
    assert!(control.work().is_none());
    assert!(!work.is_current());
    control.mark_detached(1).unwrap();
    assert!(!control.reclaim());
    drop(work);
    assert!(!control.reclaim());
    drop(callback);
    assert!(control.reclaim());
    assert_eq!(control.status(100).state, LifecycleState::Stopped);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn generation_ids_never_repeat_and_failed_publish_does_not_invalidate_work() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(2, &drops));
    let (mut control, _, _) = host.split();
    let work = control.work().unwrap();
    assert_eq!(
        control.publish(generation(2, &drops), 1),
        Err(LifecycleError::InvalidGeneration)
    );
    assert!(work.is_current());
}

#[test]
fn device_open_failure_restores_old_generation_or_leaves_rf_safe() {
    struct Device {
        opens: Vec<String>,
        restore: bool,
        closed: usize,
    }
    impl DeviceHandoff for Device {
        fn quiesce(&mut self) -> bool {
            true
        }
        fn close(&mut self) {
            self.closed += 1;
        }
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.opens.push(settings.device.clone());
            settings.device == "radio0" && self.restore
        }
    }
    for restore in [false, true] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut host = NodeHost::new(generation(1, &drops));
        let (mut control, mut receive, _) = host.split();
        let candidate = RuntimeGeneration::prepare(
            2,
            GenerationSettings {
                node: "524950".into(),
                device: "radio1".into(),
                receive_maximum: 960,
                transmit_maximum: 960,
            },
            Resource(drops.clone()),
            Resource(drops.clone()),
        )
        .unwrap();
        let mut device = Device {
            opens: Vec::new(),
            restore,
            closed: 0,
        };
        assert_eq!(
            control.handoff(candidate, &mut device, 10),
            Err(if restore {
                LifecycleError::HandoffRestored
            } else {
                LifecycleError::HandoffFailed
            })
        );
        assert_eq!(device.opens, ["radio1", "radio0"]);
        assert_eq!(device.closed, 1);
        assert_eq!(receive.acquire().map(|g| g.id()), restore.then_some(1));
    }
}

#[test]
fn callback_thread_keeps_old_state_alive_until_returning() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, mut receive, mut transmit) = host.split();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let mut guard = receive.acquire().unwrap();
            entered_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            assert_eq!(guard.id(), 1);
            assert_eq!(guard.state().0.load(Ordering::Relaxed), 0);
        });
        entered_rx.recv().unwrap();
        control.publish(generation(2, &drops), 1).unwrap();
        control.mark_detached(1).unwrap();
        drop(transmit.acquire().unwrap());
        assert!(!control.reclaim());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        finish_tx.send(()).unwrap();
    });
    // Stalled receive has not adopted generation 2: teardown can still safely release both.
    control.stop(2).unwrap();
    control.mark_detached(2).unwrap();
    assert!(control.reclaim());
    assert_eq!(drops.load(Ordering::Relaxed), 4);
}

#[test]
fn tagged_control_work_drops_stale_payload_without_applying_policy() {
    use crate::control::ControlTask;
    let drops = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, _, _) = host.split();
    let task_calls = calls.clone();
    let task = ControlTask::tagged(control.work().unwrap(), move || {
        task_calls.fetch_add(1, Ordering::Relaxed);
    });
    control.publish(generation(2, &drops), 1).unwrap();
    task.run();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(control.status(1).outstanding_work, 0);
    let task_calls = calls.clone();
    ControlTask::tagged(control.work().unwrap(), move || {
        task_calls.fetch_add(1, Ordering::Relaxed);
    })
    .run();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn ordinary_reload_cannot_claim_a_different_device() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, mut receive, _) = host.split();
    let replacement = RuntimeGeneration::prepare(
        2,
        GenerationSettings {
            node: "524950".into(),
            device: "radio1".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
        },
        Resource(drops.clone()),
        Resource(drops.clone()),
    )
    .unwrap();
    assert_eq!(
        control.publish(replacement, 1),
        Err(LifecycleError::HandoffRequired)
    );
    assert_eq!(receive.acquire().unwrap().id(), 1);
}

#[test]
fn timeout_reports_once_per_pending_stage_without_force_freeing() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut control, mut receive, mut transmit) = host.split();
    control.publish(generation(2, &drops), 100).unwrap();
    assert!(!control.report_timeout(109, 10));
    assert!(control.report_timeout(110, 10));
    assert!(!control.report_timeout(200, 10));
    drop(receive.acquire().unwrap());
    drop(transmit.acquire().unwrap());
    assert!(control.report_timeout(210, 10));
    assert_eq!(control.status(210).quiescence_timeouts, 2);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(!control.reclaim());
}

#[test]
fn handoff_rejects_every_precondition_without_closing_the_live_lease() {
    struct Lease {
        quiesced: bool,
        closes: usize,
    }
    impl DeviceHandoff for Lease {
        fn quiesce(&mut self) -> bool {
            self.quiesced
        }
        fn close(&mut self) {
            self.closes += 1;
        }
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
    }
    for case in [
        "quiescence",
        "work",
        "hazard",
        "id",
        "node",
        "retired",
        "stopped",
        "success",
    ] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut host = NodeHost::new(generation(1, &drops));
        let (mut control, mut receive, mut transmit) = host.split();
        let mut device = Lease {
            quiesced: case != "quiescence",
            closes: 0,
        };
        let mut settings = GenerationSettings {
            node: "524950".into(),
            device: "radio1".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
        };
        if case == "node" {
            settings.node = "different".into();
        }
        let replacement = RuntimeGeneration::prepare(
            if case == "id" { 1 } else { 3 },
            settings,
            Resource(drops.clone()),
            Resource(drops.clone()),
        )
        .unwrap();
        let work = (case == "work").then(|| control.work().unwrap());
        let hazard = (case == "hazard").then(|| receive.acquire().unwrap());
        if case == "retired" {
            control.publish(generation(2, &drops), 0).unwrap();
        }
        if case == "stopped" {
            control.stop(0).unwrap();
        }
        assert_eq!(
            control.handoff(replacement, &mut device, 1),
            match case {
                "id" | "node" => Err(LifecycleError::InvalidGeneration),
                "retired" => Err(LifecycleError::RetirementPending),
                "stopped" => Err(LifecycleError::Stopped),
                "success" => Ok(()),
                _ => Err(LifecycleError::CallbacksActive),
            },
            "{case}"
        );
        assert_eq!(device.closes, usize::from(case == "success"));
        drop(hazard);
        drop(work);
        if case == "success" {
            assert_eq!(receive.acquire().unwrap().settings().device, "radio1");
            assert_eq!(transmit.acquire().unwrap().settings().device, "radio1");
            assert!(control.reclaim());
        }
        control.stop(2).unwrap();
        for id in [1, 2, 3] {
            let _ = control.mark_detached(id);
        }
        control.reclaim();
    }
}

#[test]
fn audio_owner_pairs_cannot_mix_hosts_or_acquire_after_stop() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut first = NodeHost::new(generation(1, &drops));
    let mut second = NodeHost::new(generation(1, &drops));
    {
        let (mut control, mut rx, mut tx) = first.split();
        let (_, _, mut other_tx) = second.split();
        assert!(rx.acquire_pair(&mut other_tx).is_none());
        assert_eq!(rx.acquire().unwrap().settings().node, "524950");
        assert_eq!(tx.acquire().unwrap().state().0.load(Ordering::Relaxed), 0);
        control.stop(0).unwrap();
        assert!(rx.acquire_pair(&mut tx).is_none());
        control.mark_detached(1).unwrap();
        control.reclaim();
    }
    second.control().stop(0).unwrap();
    second.control().mark_detached(1).unwrap();
    second.reclaim();
    let mut owned = NodeHost::new(generation(1, &drops));
    let (mut rx, _) = owned.register_audio().unwrap();
    let mut third = NodeHost::new(generation(1, &drops));
    let (_, mut tx) = third.register_audio().unwrap();
    assert!(rx.acquire_pair(&mut tx).is_none());
    for host in [&mut owned, &mut third] {
        host.control().stop(0).unwrap();
        host.control().mark_detached(1).unwrap();
        host.reclaim();
    }
}

#[test]
fn generation_rejects_unusable_identity_and_independent_callback_bounds() {
    for (id, node, device, receive_maximum, transmit_maximum) in [
        (0, "node", "radio", 1, 1),
        (1, "", "radio", 1, 1),
        (1, "node\0", "radio", 1, 1),
        (1, "node", "", 1, 1),
        (1, "node", "radio\0", 1, 1),
        (1, "node", "radio", 0, 1),
        (1, "node", "radio", 1, 0),
    ] {
        assert!(matches!(
            RuntimeGeneration::prepare(
                id,
                GenerationSettings {
                    node: node.into(),
                    device: device.into(),
                    receive_maximum,
                    transmit_maximum
                },
                (),
                ()
            ),
            Err(LifecycleError::InvalidGeneration)
        ));
    }
}

#[test]
fn lifecycle_task_runs_without_a_generation_dependency() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = runs.clone();
    let task = crate::control::ControlTask::lifecycle(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(runs.load(Ordering::Relaxed), 0);
    task.run();
    assert_eq!(runs.load(Ordering::Relaxed), 1);
}

#[test]
fn stopped_unregistered_host_can_be_reclaimed_repeatedly_without_owners() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    assert!(host.reclaim());
    assert!(!host.reclaim());
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn concurrent_callback_gating_only_exposes_the_protected_generation_or_shortfall() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut rx, _tx) = host.register_audio().unwrap();
    let start = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let ready = Arc::clone(&start);
        let reader = scope.spawn(move || {
            ready.wait();
            for _ in 0..200_000 {
                if let Some(guard) = rx.acquire() {
                    assert_eq!(guard.id(), 1);
                }
            }
        });
        start.wait();
        for _ in 0..200_000 {
            host.control().gate_callbacks();
            host.control().restore_callbacks();
        }
        reader.join().unwrap();
    });
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    assert!(host.reclaim());
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn stopping_reclaims_unprotected_current_before_protected_retired_and_never_frees_a_live_guard() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = NodeHost::new(generation(1, &drops));
    let (mut rx, _tx) = host.register_audio().unwrap();
    let guard = rx.acquire().unwrap();
    host.control().publish(generation(2, &drops), 1).unwrap();
    host.control().stop(2).unwrap();
    host.control().mark_detached(1).unwrap();
    host.control().mark_detached(2).unwrap();
    assert!(host.reclaim());
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert_eq!(host.control().status(2).state, LifecycleState::Stopping);
    assert_eq!(host.control().status(2).active, None);
    assert_eq!(guard.id(), 1);
    // Abnormal host destruction must retain the detached-but-still-protected old state.
    drop(host);
    assert_eq!(guard.settings().node, "524950");
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    drop(guard);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn stopped_generation_transfer_rejects_invalid_candidates_and_restores_only_live_callbacks() {
    for case in ["stopped", "retired", "same_id", "settings", "success"] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut host = NodeHost::new(generation(1, &drops));
        let (mut control, mut rx, _) = host.split();
        if case == "stopped" {
            control.stop(0).unwrap();
        }
        if case == "retired" {
            control.publish(generation(2, &drops), 0).unwrap();
        }
        let mut candidate = generation(if case == "same_id" { 1 } else { 3 }, &drops);
        if case == "settings" {
            candidate.settings.receive_maximum += 1;
        }
        let mut transferred = false;
        let result = control.publish_transferring(candidate, 1, |_, _, _, _| transferred = true);
        assert_eq!(
            result,
            match case {
                "stopped" => Err(LifecycleError::Stopped),
                "retired" => Err(LifecycleError::RetirementPending),
                "same_id" | "settings" => Err(LifecycleError::InvalidGeneration),
                _ => Ok(()),
            }
        );
        assert_eq!(transferred, case == "success");
        control.gate_callbacks();
        assert!(rx.acquire().is_none());
        control.restore_callbacks();
        assert_eq!(rx.acquire().is_some(), case != "stopped");
        for id in [1, 2, 3] {
            let _ = control.mark_detached(id);
        }
        control.reclaim();
        control.stop(2).unwrap();
        control.restore_callbacks();
        assert!(rx.acquire().is_none());
        assert_eq!(
            control.publish(generation(4, &drops), 3),
            Err(LifecycleError::Stopped)
        );
        control.reclaim();
    }
}
