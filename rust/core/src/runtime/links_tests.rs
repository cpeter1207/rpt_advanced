use super::*;
use crate::runtime::{GenerationSettings, NodeHost, RuntimeGeneration};
fn generation(id: u64) -> RuntimeGeneration<(), ()> {
    RuntimeGeneration::prepare(
        id,
        GenerationSettings {
            node: "524950".into(),
            device: "radio0".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
        },
        (),
        (),
    )
    .unwrap()
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation {
        command: crate::command::Command {
            action,
            node: remote.into(),
        },
        digit: None,
    }
}
fn links() -> NodeLinkControl {
    NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        None,
    )
    .unwrap()
}

#[test]
fn reconnecting_an_empty_configured_schedule_has_no_detach_effect() {
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(1, vec![], vec![], None).unwrap()),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectAll, ""),
            host.control().work().unwrap(),
            0,
            false
        ),
        Ok(LinkEffect::None)
    ));
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    host.reclaim();
}

#[test]
fn unconfigured_schedule_and_closed_or_stale_admission_do_not_reserve_work() {
    let mut links = links();
    let mut host = NodeHost::new(generation(1));
    links.tick(
        CivilTime::new(2026, 9, 15, crate::schedule::Weekday::Tuesday, 12, 0).unwrap(),
        0,
        0,
        |_| 0,
    );
    assert!(
        links
            .next_scheduled(host.control().work().unwrap())
            .is_none()
    );
    for action in [LinkAction::DisconnectAll, LinkAction::ReconnectAll] {
        assert!(
            links
                .command(
                    operation(action, ""),
                    host.control().work().unwrap(),
                    0,
                    false
                )
                .is_ok()
        );
    }
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true
        ),
        Err(AdmissionError::Loop)
    ));
    let stale = host.control().work().unwrap();
    let retry_stale = host.control().work().unwrap();
    host.control().publish(generation(2), 0).unwrap();
    assert!(links.next_scheduled(stale).is_none());
    assert!(links.take_retry(0, retry_stale).is_none());
    links.close();
    assert!(
        links
            .next_scheduled(host.control().work().unwrap())
            .is_none()
    );
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    host.control().mark_detached(2).unwrap();
    host.reclaim();
}
#[test]
fn copied_peer_text_updates_topology_relays_advice_and_detaches_loops() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    assert_eq!(links.due_topology(0).len(), 2);
    assert!(links.due_topology(1).is_empty());
    let LinkEffect::Text(messages) = links
        .peer_text("2000", b"K? * 2000 0 0", true, 1000, 4000)
        .unwrap()
    else {
        panic!("key advice")
    };
    assert_eq!(messages[0], ("2000".into(), "K 2000 524950 1 3".into()));
    assert_eq!(messages[1], ("3000".into(), "K? * 2000 0 0".into()));
    assert!(matches!(
        links.peer_text("2000", b"L T4000", false, 0, 5000),
        Ok(LinkEffect::None)
    ));
    assert!(links.manager().reaches("4000", false));
    assert_eq!(links.due_topology(5000).len(), 2);
    assert!(matches!(
        links.peer_text("2000", b"L T3000", false, 0, 6000),
        Ok(LinkEffect::Detach(peers)) if peers == ["2000"]
    ));
    assert_eq!(
        links.peer_text("2000", b"L T5000", false, 0, 7000).err(),
        Some(AdmissionError::Invalid)
    );
    assert!(
        links
            .due_topology(7000)
            .iter()
            .all(|(name, _)| name == "3000")
    );
}

#[test]
fn administrative_cancellation_and_foreign_scheduled_dials_do_not_change_links() {
    let mut host = NodeHost::new(generation(1));
    let mut local = links();
    let LinkEffect::Connect(attempt) = local
        .command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true,
        )
        .unwrap()
    else {
        panic!("administrative dial")
    };
    local.cancel_connect(attempt);
    assert!(local.manager().snapshot().is_empty());
    let LinkEffect::Connect(attempt) = local
        .command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true,
        )
        .unwrap()
    else {
        panic!("administrative dial")
    };
    local.close();
    assert_eq!(
        local.finish_connect_at(attempt, true, 0, None, |_| 0),
        Err(AdmissionError::Stale)
    );
    let mut local = links();
    let schedule = LinkScheduler::new(
        1,
        vec![super::super::link_schedule::RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
        }],
        vec![],
        None,
    )
    .unwrap();
    let mut scheduled = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let LinkEffect::Connect(attempt) = scheduled
        .next_scheduled(host.control().work().unwrap())
        .unwrap()
    else {
        panic!("scheduled dial")
    };
    assert_eq!(
        local.finish_connect_at(attempt, true, 0, None, |_| 0),
        Err(AdmissionError::Stale)
    );
}
#[test]
fn slow_connect_revalidates_generation_and_new_loop_before_publication() {
    let mut host = NodeHost::new(generation(1));
    let (mut control, _, _) = host.split();
    let mut links = links();
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::Transceive, "2000"),
            control.work().unwrap(),
            0,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    control.publish(generation(2), 1).unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 2, None, |_| 0),
        Err(AdmissionError::Stale)
    );
    assert!(!links.manager.reaches("2000", true));
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::Transceive, "3000"),
            control.work().unwrap(),
            2,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    links.accept("3000", true).unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 3, None, |_| 0),
        Err(AdmissionError::Loop)
    );
}
#[test]
fn permanent_failure_retains_retry_and_operator_disconnect_cancels_it() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::PermanentTransceive, "2000"),
            control.work().unwrap(),
            0,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, false, 10, None, |_| 0),
        Ok(false)
    );
    assert!(links.manager.reaches("2000", true));
    let retry = links.take_retry(10, control.work().unwrap()).unwrap();
    links
        .command(
            operation(LinkAction::DisconnectPermanent, "2000"),
            control.work().unwrap(),
            11,
            false,
        )
        .unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 12),
        Err(AdmissionError::Stale)
    );
    assert!(!links.manager.reaches("2000", true));
}
#[test]
fn incoming_and_remote_selection_use_current_deny_first_policy() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    assert_eq!(links.accept("2000", false), Err(AdmissionError::Denied));
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            true
        ),
        Ok(LinkEffect::SelectedRemote)
    ));
    links.reconfigure(
        AccessPolicy::new("", "2000").unwrap(),
        DtmfCommandMap::standard(),
        None,
    );
    assert_eq!(
        links
            .command(
                operation(LinkAction::Command, "2000"),
                control.work().unwrap(),
                0,
                true
            )
            .err(),
        Some(AdmissionError::Denied)
    );
}

#[test]
fn configured_dial_rechecks_window_boundary_and_requires_clock_only_for_windows() {
    use super::super::link_schedule::{ReplacementSpec, RouteSpec};
    use crate::schedule::{ScheduledWindow, Weekday};
    let clock = |hour| CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
    for window in [false, true] {
        let mut host = NodeHost::new(generation(1));
        let (control, _, _) = host.split();
        let routes = vec![
            RouteSpec {
                local: "524950".into(),
                remote: "2000".into(),
                permanent: true,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
            },
        ];
        let windows = if window {
            vec![ReplacementSpec {
                route: 1,
                replaced: 0,
                window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
                end_inactivity_ms: 0,
            }]
        } else {
            vec![]
        };
        let schedule = LinkScheduler::new(1, routes, windows, None).unwrap();
        let mut links = NodeLinkControl::new(
            "524950",
            AccessPolicy::new("", "").unwrap(),
            DtmfCommandMap::standard(),
            Some(schedule),
        )
        .unwrap();
        links.tick(clock(11), 0, 1, |_| 0);
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("dial")
        };
        let result =
            links.finish_connect_at(attempt, true, 2, window.then_some((clock(12), 0)), |_| 0);
        assert_eq!(
            result,
            if window {
                Err(AdmissionError::Stale)
            } else {
                Ok(true)
            }
        );
        assert_eq!(links.manager.reaches("2000", true), !window);
    }
}

#[test]
fn dial_modes_failed_temporary_calls_and_telemetry_preserve_command_semantics() {
    for (action, mode, permanent) in [
        (LinkAction::Monitor, Mode::MONITOR, false),
        (LinkAction::LocalMonitor, Mode::LOCAL_MONITOR, false),
        (LinkAction::Transceive, Mode::TRANSCEIVE, false),
        (LinkAction::PermanentMonitor, Mode::MONITOR, true),
        (LinkAction::PermanentLocalMonitor, Mode::LOCAL_MONITOR, true),
        (LinkAction::PermanentTransceive, Mode::TRANSCEIVE, true),
    ] {
        let mut host = NodeHost::new(generation(1));
        let (control, _, _) = host.split();
        let mut links = links();
        let LinkEffect::Connect(attempt) = links
            .command(operation(action, "2000"), control.work().unwrap(), 0, false)
            .unwrap()
        else {
            panic!("dial")
        };
        assert_eq!(
            (attempt.remote(), attempt.mode(), attempt.permanent()),
            ("2000", mode, permanent)
        );
        assert!(attempt.current());
        assert_eq!(
            links.finish_connect_at(attempt, false, 1, None, |_| 0),
            Ok(false)
        );
        assert_eq!(links.manager().owns_permanent("2000"), permanent);
    }
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    for action in [
        LinkAction::Status,
        LinkAction::FullStatus,
        LinkAction::LastKeyed,
        LinkAction::Time,
    ] {
        assert!(
            matches!(links.command(operation(action, ""), control.work().unwrap(), 0, false), Ok(LinkEffect::Telemetry(found)) if found == action)
        );
    }
    assert!(
        matches!(links.command(operation(LinkAction::Disconnect, "2000"), control.work().unwrap(), 0, false), Ok(LinkEffect::Detach(names)) if names.is_empty())
    );
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "524950"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Loop)
    ));
    links.policy = AccessPolicy::new("", "2000").unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "2000"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Denied)
    ));
}

#[test]
fn remote_selection_forwards_digits_and_temporary_disconnect_preserves_permanents() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    links.accept("2000", true).unwrap();
    links
        .manager
        .attach("3000", Mode::TRANSCEIVE, true)
        .unwrap();
    assert!(links.peer_digit("missing", '*', 0).is_none());
    assert!(links.peer_digit("2000", '*', 0).is_none());
    assert!(matches!(
        links.command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Denied)
    ));
    links
        .command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            true,
        )
        .unwrap();
    let forwarded = links
        .digit(DigitEvent::Digit {
            digit: '5',
            now_ms: 1,
        })
        .unwrap();
    assert!(
        matches!(links.command(forwarded, control.work().unwrap(), 1, false), Ok(LinkEffect::RemoteDigit { remote, digit: '5' }) if remote == "2000")
    );
    assert!(
        matches!(links.command(operation(LinkAction::DisconnectNonPermanentAll, ""), control.work().unwrap(), 2, false), Ok(LinkEffect::Detach(names)) if names == ["2000"])
    );
    assert!(links.manager().owns_permanent("3000"));
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '6',
                now_ms: 3
            })
            .is_none()
    );
}

#[test]
fn copied_peer_negotiation_disconnect_and_closed_admission_have_no_inline_effects() {
    let mut host = NodeHost::new(generation(1));
    let (mut control, _, _) = host.split();
    let mut links = links();
    links.accept("2000", true).unwrap();
    for bytes in [b"!NEWKEY1!".as_slice(), b"!IAXKEY!"] {
        assert!(matches!(
            links.peer_text("2000", bytes, false, 0, 0),
            Ok(LinkEffect::None)
        ));
    }
    assert!(matches!(
        links.peer_text("2000", b"bad", false, 0, 0),
        Err(AdmissionError::Invalid)
    ));
    assert!(
        matches!(links.peer_text("2000", b"!!DISCONNECT!!", false, 0, 0), Ok(LinkEffect::Detach(names)) if names == ["2000"])
    );
    links.reclaimed("2000", 0);
    let stale = control.work().unwrap();
    control.publish(generation(2), 0).unwrap();
    assert!(matches!(
        links.command(operation(LinkAction::Status, ""), stale, 0, false),
        Err(AdmissionError::Stale)
    ));
    links.close();
    assert_eq!(links.accept("2000", true), Err(AdmissionError::Stale));
    assert!(matches!(
        links.peer_text("2000", b"L", false, 0, 0),
        Err(AdmissionError::Stale)
    ));
    assert!(links.take_retry(0, control.work().unwrap()).is_none());
    assert!(links.next_scheduled(control.work().unwrap()).is_none());
    assert!(matches!(
        links.command(
            operation(LinkAction::Status, ""),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Stale)
    ));
}

#[test]
fn retry_completion_rechecks_current_generation_policy_and_answer() {
    for outcome in ["answered", "unanswered", "denied", "stale", "closed"] {
        let mut host = NodeHost::new(generation(1));
        let (mut control, _, _) = host.split();
        let mut links = links();
        links
            .manager
            .retain_retry("2000", Mode::MONITOR, 0)
            .unwrap();
        let retry = links.take_retry(0, control.work().unwrap()).unwrap();
        assert_eq!((retry.remote(), retry.mode()), ("2000", Mode::MONITOR));
        assert!(retry.current());
        match outcome {
            "denied" => links.policy = AccessPolicy::new("", "2000").unwrap(),
            "stale" => {
                control.publish(generation(2), 1).unwrap();
                assert!(!retry.current());
            }
            "closed" => {
                links.close();
            }
            _ => {}
        }
        assert_eq!(
            links.finish_retry(retry, outcome != "unanswered", 2),
            match outcome {
                "answered" => Ok(true),
                "unanswered" => Ok(false),
                "denied" => Err(AdmissionError::Denied),
                _ => Err(AdmissionError::Stale),
            }
        );
    }
}

#[test]
fn scheduled_cancel_clock_failure_and_window_withdrawal_release_exact_reservations() {
    use super::super::link_schedule::{ReplacementSpec, RouteSpec};
    use crate::schedule::{ScheduledWindow, Weekday};
    let civil = |hour| CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
    let schedule = LinkScheduler::new(
        1,
        vec![
            RouteSpec {
                local: "524950".into(),
                remote: "2000".into(),
                permanent: true,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
            },
        ],
        vec![ReplacementSpec {
            route: 1,
            replaced: 0,
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
        }],
        None,
    )
    .unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    links.tick(civil(11), 0, 0, |_| 0);
    let LinkEffect::Connect(cancelled) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    links.cancel_connect(cancelled);
    for clock in [None, Some((civil(11), 60))] {
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("dial")
        };
        assert_eq!(
            links.finish_connect_at(attempt, true, 0, clock, |_| 0),
            Err(AdmissionError::Stale)
        );
    }
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    links.policy = AccessPolicy::new("", "2000").unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| 0),
        Err(AdmissionError::Denied)
    );
    links.policy = AccessPolicy::new("", "").unwrap();
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| 0),
        Ok(true)
    );
    links.tick(civil(12), 0, 1, |_| 0);
    assert!(
        matches!(links.next_scheduled(control.work().unwrap()), Some(LinkEffect::Detach(names)) if names == ["2000"])
    );
    links.reclaimed("2000", 1);
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        links.finish_connect_at(attempt, true, 1, Some((civil(12), 0)), |_| 0),
        Ok(true)
    );
    links
        .command(
            operation(LinkAction::DisconnectAll, ""),
            control.work().unwrap(),
            2,
            false,
        )
        .unwrap();
    links.reclaimed("3000", 2);
    links.tick(civil(13), 0, 3, |_| 0);
    assert!(
        matches!(links.command(operation(LinkAction::ReconnectAll, ""), control.work().unwrap(), 3, false), Ok(LinkEffect::Detach(names)) if names == ["3000"])
    );
    assert!(matches!(
        links.next_scheduled(control.work().unwrap()),
        Some(LinkEffect::Connect(_))
    ));
}
