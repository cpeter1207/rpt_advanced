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
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
        },
        (),
        (),
    )
    .unwrap()
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation::new(
        crate::command::Command {
            action,
            node: remote.into(),
        },
        None,
    )
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
fn parrot_actions_require_a_valid_admin_unlock_and_use_live_typed_effects() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let mut links = links();
    links.configure_admin(
        &digest(b"1234", "c29tZXNhbHQ"),
        &digest(b"5678", "bG9ja3NhbHQ"),
        60_000,
    );
    let mut host = NodeHost::new(generation(1));
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '*',
                now_ms: 1
            })
            .is_none()
    );
    for digit in "8009999#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 2 })
                .is_none()
        );
    }
    for digit in "*804".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 3 })
                .is_none()
        );
    }
    for digit in "*8001234#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 4 })
                .is_none()
        );
    }
    let mut enabled = None;
    for digit in "*804".chars() {
        enabled = links
            .digit(DigitEvent::Digit { digit, now_ms: 5 })
            .or(enabled);
    }
    assert!(matches!(
        links.command(enabled.unwrap(), host.control().work().unwrap(), 6, false),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    links.confirm_parrot_command(true, 6);
    assert!(matches!(
        links.command(
            operation(LinkAction::ParrotDisable, ""),
            host.control().work().unwrap(),
            7,
            false,
        ),
        Err(AdmissionError::Denied)
    ));
    for digit in "*8015678#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 8 })
                .is_none()
        );
    }
    for digit in "*805".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 9 })
                .is_none()
        );
    }
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
fn reconfiguration_preserves_callback_selection_for_an_unchanged_group() {
    use super::super::link_schedule::RouteSpec;
    let routes = || {
        ["2000", "3000"]
            .into_iter()
            .enumerate()
            .map(|(priority, remote)| RouteSpec {
                local: "524950".into(),
                remote: remote.into(),
                permanent: true,
                group_label: Some("north".into()),
                group_name: Some("North network".into()),
                group_priority: Some(priority),
            })
            .collect()
    };
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(1, routes(), vec![], None).unwrap()),
    )
    .unwrap();
    let member = links.group_member("2000").unwrap();
    member
        .selection()
        .publish_desired(std::num::NonZeroUsize::new(1));
    assert_eq!(member.selection().callback_slot(true), 1);

    links.reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(2, routes(), vec![], None).unwrap()),
    );

    let retained = links.group_member("2000").unwrap();
    assert_eq!(retained.selection().active(), Some(retained.slot()));
}

#[test]
fn priority_status_distinguishes_unattempted_reachable_and_ended_members() {
    use super::super::link_schedule::RouteSpec;
    let schedule = LinkScheduler::new(
        1,
        vec![RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: Some("north".into()),
            group_name: Some("North".into()),
            group_priority: Some(0),
        }],
        vec![],
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

    assert!(!links.priority_groups()[0].unavailable);
    links.accept("2000", true).unwrap();
    assert!(!links.priority_groups()[0].unavailable);
    links.ended("2000");
    assert!(links.priority_groups()[0].unavailable);
}

#[test]
fn unconfigured_schedule_and_closed_or_stale_admission_do_not_reserve_work() {
    let mut links = links();
    let mut host = NodeHost::new(generation(1));
    links.tick(
        CivilTime::new(2026, 9, 15, crate::schedule::Weekday::Tuesday, 12, 0).unwrap(),
        0,
        0,
        |_| None,
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
        local.finish_connect_at(attempt, true, 0, None, |_| None),
        Err(AdmissionError::Stale)
    );
    let mut local = links();
    let schedule = LinkScheduler::new(
        1,
        vec![super::super::link_schedule::RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: None,
            group_name: None,
            group_priority: None,
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
        local.finish_connect_at(attempt, true, 0, None, |_| None),
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
        links.finish_connect_at(attempt, true, 2, None, |_| None),
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
        links.finish_connect_at(attempt, true, 3, None, |_| None),
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
        links.finish_connect_at(attempt, false, 10, None, |_| None),
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
fn topology_blocked_automatic_route_retries_only_after_explicit_evidence_change() {
    use super::super::link_schedule::RouteSpec;
    let schedule = |generation| {
        LinkScheduler::new(
            generation,
            vec![RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            }],
            vec![],
            None,
        )
        .unwrap()
    };
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule(1)),
    )
    .unwrap();
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.peer_text("2000", b"L T3000", false, 0, 0),
        Ok(LinkEffect::None)
    ));
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("configured dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, None, |_| None),
        Err(AdmissionError::Loop)
    );
    let status = links
        .manager()
        .snapshot()
        .into_iter()
        .find(|status| status.name == "3000")
        .unwrap();
    assert!(status.retrying && status.topology_blocked);
    assert_eq!(status.due_ms, None);
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );

    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectAll, ""),
            control.work().unwrap(),
            1,
            false
        ),
        Ok(LinkEffect::None)
    ));
    let retry = links.take_retry(1, control.work().unwrap()).unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 1),
        Err(AdmissionError::Loop)
    );

    links.reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule(2)),
    );
    let retry = links.take_retry(2, control.work().unwrap()).unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 2),
        Err(AdmissionError::Loop)
    );

    links.accept("4000", true).unwrap();
    links.peer_text("4000", b"L T5000", false, 0, 3).unwrap();
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );
    links.ended("2000");
    links.reclaimed("2000", 4);
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );

    links.accept("2000", true).unwrap();
    links.peer_text("2000", b"L T6000", false, 0, 5).unwrap();
    let retry = links.take_retry(5, control.work().unwrap()).unwrap();
    assert_eq!(links.finish_retry(retry, false, 5), Ok(false));
    let status = links
        .manager()
        .snapshot()
        .into_iter()
        .find(|status| status.name == "3000")
        .unwrap();
    assert!(!status.topology_blocked);
    assert_eq!(status.due_ms, Some(1005));
}

#[test]
fn group_member_is_not_topology_blocked_but_other_routes_are() {
    use super::super::link_schedule::RouteSpec;
    use crate::schedule::Weekday;
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let schedule = LinkScheduler::new(
        1,
        vec![
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: true,
                group_label: Some("network".into()),
                group_name: Some("Network".into()),
                group_priority: Some(0),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "4000".into(),
                permanent: true,
                group_label: Some("network".into()),
                group_name: Some("Network".into()),
                group_priority: Some(1),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "5000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ],
        vec![],
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
    links.accept("2000", true).unwrap();
    links
        .peer_text("2000", b"L T3000,T4000,T5000", false, 0, 1)
        .unwrap();
    let civil = CivilTime::new(2026, 9, 15, Weekday::Tuesday, 11, 0).unwrap();
    links.tick(civil, 0, 1, |_| None);

    let LinkEffect::Connect(group) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("group member dial")
    };
    assert_eq!(group.remote(), "3000");
    assert_eq!(
        links.finish_connect_at(group, true, 2, None, |_| None),
        Ok(true)
    );
    assert!(matches!(
        links.peer_text("3000", b"L T2000", false, 0, 2),
        Ok(LinkEffect::None)
    ));
    links.ended("3000");
    links.reclaimed("3000", 3);
    let retry = links.take_retry(3, control.work().unwrap()).unwrap();
    assert_eq!(links.finish_retry(retry, true, 4), Ok(true));
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "3000" && !peer.topology_blocked)
    );

    let LinkEffect::Connect(group) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("second group member dial")
    };
    assert_eq!(group.remote(), "4000");
    assert_eq!(
        links.finish_connect_at(group, true, 3, None, |_| None),
        Ok(true)
    );

    let LinkEffect::Connect(other) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("other permanent dial")
    };
    assert_eq!(other.remote(), "5000");
    assert_eq!(
        links.finish_connect_at(other, true, 4, None, |_| None),
        Err(AdmissionError::Loop)
    );
}

#[test]
fn priority_group_status_reports_only_callback_active_peer_and_complete_outage() {
    use super::super::link_schedule::RouteSpec;
    let schedule = || {
        LinkScheduler::new(
            1,
            ["2000", "3000"]
                .into_iter()
                .enumerate()
                .map(|(priority, remote)| RouteSpec {
                    local: "524950".into(),
                    remote: remote.into(),
                    permanent: true,
                    group_label: Some("network".into()),
                    group_name: Some("Blind Hams Network".into()),
                    group_priority: Some(priority),
                })
                .collect(),
            vec![],
            None,
        )
        .unwrap()
    };
    let mut unavailable = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule()),
    )
    .unwrap();
    assert!(!unavailable.priority_groups()[0].unavailable);
    for remote in ["2000", "3000"] {
        unavailable
            .manager
            .retain_retry_member(remote, Mode::MONITOR, 0, unavailable.group_member(remote))
            .unwrap();
    }
    let status = unavailable.priority_groups();
    assert!(status[0].unavailable);
    assert_eq!(status[0].name, "Blind Hams Network");

    let mut selected = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule()),
    )
    .unwrap();
    let group = selected.group_member("2000").unwrap();
    selected
        .manager
        .attach_member("2000", Mode::TRANSCEIVE, true, Some(group.clone()))
        .unwrap();
    assert_eq!(group.selection().callback_slot(true), 1);
    let status = selected.priority_groups();
    assert_eq!(status[0].selected.as_deref(), Some("2000"));
    assert!(!status[0].unavailable);
}

#[test]
fn callback_group_selection_republishes_topology_immediately() {
    use super::super::link_schedule::RouteSpec;
    use crate::schedule::Weekday;

    let routes = [("3000", 0), ("4000", 1)]
        .into_iter()
        .map(|(remote, priority)| RouteSpec {
            local: "524950".into(),
            remote: remote.into(),
            permanent: true,
            group_label: Some("network".into()),
            group_name: Some("Network".into()),
            group_priority: Some(priority),
        })
        .collect();
    let schedule = LinkScheduler::new(1, routes, vec![], None).unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    links.tick(
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 11, 0).unwrap(),
        0,
        0,
        |_| None,
    );
    for _ in 0..2 {
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("permanent group dial")
        };
        assert!(
            links
                .finish_connect_at(attempt, true, 1, None, |_| None)
                .unwrap()
        );
    }

    let member = links.group_member("3000").unwrap();
    assert!(
        links
            .due_topology(2)
            .iter()
            .any(|(peer, text)| peer == "4000" && text == "L L3000")
    );

    assert_eq!(member.selection().callback_slot(true), 1);
    assert!(
        links
            .due_topology(3)
            .iter()
            .any(|(peer, text)| peer == "4000" && text == "L T3000")
    );

    let modes = links
        .manager()
        .snapshot()
        .into_iter()
        .map(|peer| (peer.name, peer.mode.transmits()))
        .collect::<Vec<_>>();
    assert_eq!(modes, [("3000".into(), true), ("4000".into(), false)]);
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
                group_label: None,
                group_name: None,
                group_priority: None,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ];
        let windows = if window {
            vec![ReplacementSpec {
                routes: vec![1],
                replaced: vec![0],
                window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
                end_inactivity_ms: 0,
                warning_before_start_ms: Vec::new(),
                warning_before_end_ms: Vec::new(),
                warning_message_id: None,
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
        links.tick(clock(11), 0, 1, |_| None);
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("dial")
        };
        let result =
            links.finish_connect_at(attempt, true, 2, window.then_some((clock(12), 0)), |_| None);
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
            links.finish_connect_at(attempt, false, 1, None, |_| None),
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
                group_label: None,
                group_name: None,
                group_priority: None,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ],
        vec![ReplacementSpec {
            routes: vec![1],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
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
    links.tick(civil(11), 0, 0, |_| None);
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
            links.finish_connect_at(attempt, true, 0, clock, |_| None),
            Err(AdmissionError::Stale)
        );
    }
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    links.policy = AccessPolicy::new("", "2000").unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| None),
        Err(AdmissionError::Denied)
    );
    links.policy = AccessPolicy::new("", "").unwrap();
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| None),
        Ok(true)
    );
    links.tick(civil(12), 0, 1, |_| None);
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
        links.finish_connect_at(attempt, true, 1, Some((civil(12), 0)), |_| None),
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
    links.tick(civil(13), 0, 3, |_| None);
    assert!(
        matches!(links.command(operation(LinkAction::ReconnectAll, ""), control.work().unwrap(), 3, false), Ok(LinkEffect::Detach(names)) if names == ["3000"])
    );
    assert!(matches!(
        links.next_scheduled(control.work().unwrap()),
        Some(LinkEffect::Connect(_))
    ));
}
