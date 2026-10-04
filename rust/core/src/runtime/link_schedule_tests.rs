use super::*;
use crate::schedule::Weekday;
fn civil(hour: u8, minute: u8) -> CivilTime {
    CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, minute).unwrap()
}
fn scheduler() -> LinkScheduler {
    LinkScheduler::new(
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
            end_inactivity_ms: 60000,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        }],
        None,
    )
    .unwrap()
}

#[test]
fn separate_nonoverlapping_windows_may_share_a_scheduled_peer() {
    let routes = vec![
        RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: Some("blind-hams".into()),
            group_name: None,
            group_priority: Some(0),
        },
        RouteSpec {
            local: "524950".into(),
            remote: "3000".into(),
            permanent: false,
            group_label: Some("schedule:morning".into()),
            group_name: Some("Morning".into()),
            group_priority: Some(0),
        },
        RouteSpec {
            local: "524950".into(),
            remote: "3000".into(),
            permanent: false,
            group_label: Some("schedule:evening".into()),
            group_name: Some("Evening".into()),
            group_priority: Some(0),
        },
    ];
    let windows = vec![
        ReplacementSpec {
            routes: vec![1],
            replaced: vec![0],
            window: ScheduledWindow::parse(Some("Monday-Friday"), None, "09:30", "11:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        },
        ReplacementSpec {
            routes: vec![2],
            replaced: vec![0],
            window: ScheduledWindow::parse(Some("Monday"), None, "20:00", "21:30").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        },
    ];

    assert!(LinkScheduler::new(1, routes.clone(), windows.clone(), None).is_ok());

    let mut overlapping = windows;
    overlapping[1].window =
        ScheduledWindow::parse(Some("Monday-Friday"), None, "10:00", "11:30").unwrap();
    assert!(LinkScheduler::new(1, routes, overlapping, None).is_err());
}

fn warning_scheduler(start: &[u64], end: &[u64]) -> LinkScheduler {
    let mut schedule = scheduler();
    schedule.windows[0].spec.warning_before_start_ms = start.to_vec();
    schedule.windows[0].spec.warning_before_end_ms = end.to_vec();
    schedule.windows[0].spec.warning_message_id = Some("scheduled-link-change".into());
    schedule
}

fn tick_warnings(
    schedule: &mut LinkScheduler,
    time: CivilTime,
    second: u8,
    now_ms: u64,
    activity: impl FnMut(&str) -> Option<u64>,
    active: impl FnMut(&str) -> bool,
) -> Option<super::ScheduleWarning> {
    schedule.tick_with_warnings(
        time,
        second,
        now_ms,
        activity,
        |_| false,
        WarningGate {
            source_active: active,
            capacity: true,
        },
    )
}

#[test]
fn schedule_warning_start_leads_are_ordered_and_not_repeated() {
    let mut schedule = warning_scheduler(&[120_000, 60_000], &[]);
    let first = tick_warnings(&mut schedule, civil(11, 58), 0, 1_000, |_| None, |_| false);
    assert_eq!(first.unwrap().remaining_ms, 120_000);
    let second = tick_warnings(&mut schedule, civil(11, 59), 0, 61_000, |_| None, |_| false);
    assert_eq!(second.unwrap().remaining_ms, 60_000);
    assert!(
        tick_warnings(
            &mut schedule,
            civil(11, 59),
            30,
            91_000,
            |_| None,
            |_| false
        )
        .is_none()
    );
    assert!(tick_warnings(&mut schedule, civil(12, 0), 0, 121_000, |_| None, |_| false).is_none());
}

#[test]
fn schedule_warning_end_uses_expected_inactivity_deadline() {
    let mut schedule = warning_scheduler(&[], &[60_000]);
    assert!(tick_warnings(&mut schedule, civil(12, 59), 0, 1_000, |_| None, |_| false).is_none());
    let due = tick_warnings(&mut schedule, civil(13, 0), 0, 61_000, |_| None, |_| false).unwrap();
    assert_eq!(due.remaining_ms, 60_000);
    assert!(tick_warnings(&mut schedule, civil(13, 1), 0, 121_000, |_| None, |_| false).is_none());
}

#[test]
fn schedule_warning_skips_active_input_and_reenables_after_inactivity_reset() {
    let mut schedule = warning_scheduler(&[], &[60_000]);
    schedule.tick(civil(12, 59), 0, 1_000, |_| None, |_| false);
    assert!(tick_warnings(&mut schedule, civil(13, 0), 0, 61_000, |_| None, |_| true).is_none());
    let due_after_reset = tick_warnings(
        &mut schedule,
        civil(13, 0),
        10,
        71_000,
        |_| Some(71_000),
        |_| false,
    )
    .unwrap();
    assert_eq!(due_after_reset.remaining_ms, 60_000);
}

#[test]
fn schedule_start_warning_is_skipped_during_activity_without_later_deferral() {
    let mut schedule = warning_scheduler(&[60_000], &[]);
    assert!(tick_warnings(&mut schedule, civil(11, 59), 0, 1_000, |_| None, |_| true,).is_none());
    assert!(
        tick_warnings(
            &mut schedule,
            civil(11, 59),
            30,
            31_000,
            |_| None,
            |_| false,
        )
        .is_none()
    );
}

#[test]
fn schedule_warning_is_skipped_at_or_after_deadline() {
    let mut starts = warning_scheduler(&[1], &[]);
    assert!(tick_warnings(&mut starts, civil(12, 0), 0, 1_000, |_| None, |_| false).is_none());
    let mut ends = warning_scheduler(&[], &[1]);
    ends.tick(civil(12, 59), 0, 1_000, |_| None, |_| false);
    assert!(tick_warnings(&mut ends, civil(13, 1), 0, 121_000, |_| None, |_| false).is_none());
}

#[test]
fn schedule_warning_waits_for_bounded_status_queue_capacity() {
    let mut schedule = warning_scheduler(&[60_000], &[]);
    assert!(
        schedule
            .tick_with_warnings(
                civil(11, 59),
                0,
                1_000,
                |_| None,
                |_| false,
                WarningGate {
                    source_active: |_: &str| false,
                    capacity: false,
                }
            )
            .is_none()
    );
    assert_eq!(
        schedule
            .tick_with_warnings(
                civil(11, 59),
                1,
                2_000,
                |_| None,
                |_| false,
                WarningGate {
                    source_active: |_: &str| false,
                    capacity: true,
                }
            )
            .unwrap()
            .remaining_ms,
        59_000
    );
}

#[test]
fn end_warning_can_target_a_future_window_with_and_without_inactivity() {
    let mut before_window = warning_scheduler(&[], &[u64::MAX]);
    assert!(tick_warnings(&mut before_window, civil(10, 0), 0, 0, |_| None, |_| false,).is_some());

    let mut immediate = warning_scheduler(&[], &[60_000]);
    immediate.windows[0].spec.end_inactivity_ms = 0;
    assert!(tick_warnings(&mut immediate, civil(12, 59), 0, 1_000, |_| None, |_| false,).is_some());
}

#[test]
fn due_warning_is_selected_once_across_multiple_windows() {
    let old = warning_scheduler(&[60_000], &[]);
    let mut windows = old.window_specs();
    windows.push(windows[0].clone());
    let mut schedule =
        LinkScheduler::new(2, old.route_specs(), windows, None).expect("two valid windows");

    assert!(tick_warnings(&mut schedule, civil(11, 59), 0, 1_000, |_| None, |_| false,).is_some());
}

#[test]
fn reload_matching_checks_replacement_route_identity_and_members() {
    let old = warning_scheduler(&[120_000], &[60_000]);
    let add_primary = |routes: &mut Vec<RouteSpec>| {
        routes.push(RouteSpec {
            local: "524950".into(),
            remote: "4000".into(),
            permanent: true,
            group_label: None,
            group_name: None,
            group_priority: None,
        });
    };

    let mut routes = old.route_specs();
    routes[1].remote = "4000".into();
    assert!(LinkScheduler::new(2, routes, old.window_specs(), Some(&old)).is_ok());

    let mut routes = old.route_specs();
    add_primary(&mut routes);
    let mut windows = old.window_specs();
    windows[0].replaced.push(2);
    assert!(LinkScheduler::new(2, routes, windows, Some(&old)).is_ok());

    let mut routes = old.route_specs();
    add_primary(&mut routes);
    let mut windows = old.window_specs();
    windows[0].replaced[0] = 2;
    assert!(LinkScheduler::new(2, routes, windows, Some(&old)).is_ok());
}

#[test]
fn quiet_window_exit_and_cold_start_after_grace_restore_primary_immediately() {
    let mut cold = scheduler();
    cold.tick(civil(13, 2), 0, 1, |_| None, |_| false);
    assert_eq!(cold.next_operation().unwrap().remote(), "2000");
    let mut active = scheduler();
    active.tick(civil(12, 59), 0, 1, |_| None, |_| false);
    let attach = active.next_operation().unwrap();
    assert_eq!(attach.remote(), "3000");
    assert!(active.complete(&attach, true));
    active.tick(civil(13, 0), 0, 2, |_| None, |_| true);
    let detach = active.next_operation().unwrap();
    assert_eq!(detach.remote(), "3000");
    assert_eq!(detach.action(), LinkTransition::Detach);
}

#[test]
fn same_revision_reservation_from_another_route_cannot_consume_current_intent() {
    let mut first = scheduler();
    let mut routes = first.route_specs();
    routes[0].remote = "4000".into();
    let mut second = LinkScheduler::new(1, routes, first.window_specs(), None).unwrap();
    let first_token = first.next_operation().unwrap();
    let second_token = second.next_operation().unwrap();
    assert!(!first.complete(&second_token, true));
    assert!(!second.complete(&first_token, true));
    assert!(first.complete(&first_token, true));
    assert!(second.complete(&second_token, true));
}

#[test]
fn different_local_routes_do_not_inherit_ownership_and_future_activity_waits() {
    let mut old = scheduler();
    old.tick(civil(11, 0), 0, 1, |_| None, |_| false);
    let attach = old.next_operation().unwrap();
    assert!(old.complete(&attach, true));
    let mut routes = old.route_specs();
    for route in &mut routes {
        route.local = "other".into();
    }
    let mut next = LinkScheduler::new(2, routes, old.window_specs(), Some(&old)).unwrap();
    assert_eq!(old.removed_routes(&next).len(), 1);
    assert_eq!(next.next_operation().unwrap().local(), "other");
    next.pause("other");
    assert!(next.next_operation().is_none());
    let mut schedule = scheduler();
    schedule.tick(civil(12, 59), 0, 100, |_| Some(200), |_| false);
    let replacement = schedule.next_operation().unwrap();
    assert!(schedule.complete(&replacement, true));
    schedule.tick(civil(13, 0), 0, 101, |_| Some(200), |_| true);
    assert!(schedule.next_operation().is_none());
    schedule.tick(civil(13, 1), 0, 60200, |_| Some(200), |_| true);
    assert_eq!(
        schedule.next_operation().unwrap().action(),
        LinkTransition::Detach
    );
}
#[test]
fn withdraw_before_attach_and_revalidate_each_reservation() {
    let mut schedule = scheduler();
    schedule.tick(civil(11, 0), 0, 1, |_| None, |_| false);
    let permanent = schedule.next_operation().unwrap();
    assert_eq!(
        (permanent.remote(), permanent.action()),
        ("2000", LinkTransition::Attach)
    );
    assert!(schedule.complete(&permanent, true));
    schedule.tick(civil(12, 0), 0, 2, |_| None, |_| true);
    let withdraw = schedule.next_operation().unwrap();
    assert_eq!(
        (withdraw.remote(), withdraw.action()),
        ("2000", LinkTransition::Detach)
    );
    assert!(schedule.next_operation().is_none());
    assert!(schedule.complete(&withdraw, true));
    let attach = schedule.next_operation().unwrap();
    assert_eq!(attach.remote(), "3000");
    schedule.pause("524950");
    assert!(!schedule.current(&attach));
    schedule.resume("524950");
    let replacement = schedule.next_operation().unwrap();
    assert!(!schedule.complete(&attach, true));
    assert!(schedule.current(&replacement));
    assert!(schedule.complete(&replacement, true));
}
#[test]
fn cold_start_grace_and_real_receive_activity_have_distinct_deadlines() {
    for activity in [0, 99000] {
        let mut schedule = scheduler();
        schedule.tick(
            civil(13, 0),
            30,
            100000,
            |_| (activity != 0).then_some(activity),
            |_| false,
        );
        let replacement = schedule.next_operation().unwrap();
        assert_eq!(replacement.remote(), "3000");
        schedule.complete(&replacement, true);
        schedule.tick(
            civil(13, 1),
            0,
            130000,
            |_| (activity != 0).then_some(activity),
            |_| true,
        );
        let next = schedule.next_operation();
        if activity == 0 {
            assert_eq!(next.unwrap().action(), LinkTransition::Detach);
        } else {
            assert!(next.is_none());
            schedule.tick(
                civil(13, 1),
                30,
                159000,
                |_| (activity != 0).then_some(activity),
                |_| true,
            );
            assert_eq!(
                schedule.next_operation().unwrap().action(),
                LinkTransition::Detach
            );
        }
    }
}

#[test]
fn activity_at_monotonic_zero_is_observed_and_survives_reload() {
    let mut never_observed = scheduler();
    never_observed.tick(civil(12, 59), 0, 0, |_| None, |_| false);
    let attach = never_observed.next_operation().unwrap();
    assert!(never_observed.complete(&attach, true));
    never_observed.tick(civil(13, 0), 0, 1, |_| None, |_| true);
    assert_eq!(never_observed.windows[0].last_activity, None);
    assert_eq!(
        never_observed.next_operation().unwrap().action(),
        LinkTransition::Detach
    );

    let mut observed = scheduler();
    observed.tick(civil(12, 59), 0, 0, |_| Some(0), |_| false);
    let attach = observed.next_operation().unwrap();
    assert!(observed.complete(&attach, true));
    observed.tick(civil(13, 0), 0, 1, |_| Some(0), |_| true);
    assert_eq!(observed.windows[0].last_activity, Some(0));
    assert!(observed.next_operation().is_none());

    let mut reloaded = LinkScheduler::new(
        2,
        observed.route_specs(),
        observed.window_specs(),
        Some(&observed),
    )
    .unwrap();
    assert_eq!(reloaded.windows[0].last_activity, Some(0));
    reloaded.tick(civil(13, 0), 0, 59_999, |_| Some(0), |_| true);
    assert!(reloaded.next_operation().is_none());
    reloaded.tick(civil(13, 1), 0, 60_000, |_| Some(0), |_| true);
    assert_eq!(
        reloaded.next_operation().unwrap().action(),
        LinkTransition::Detach
    );
}
#[test]
fn reload_keeps_activity_but_invalidates_old_inflight_dials() {
    let mut old = scheduler();
    old.tick(civil(12, 59), 0, 100000, |_| Some(100000), |_| false);
    let pending = old.next_operation().unwrap();
    let mut replacement =
        LinkScheduler::new(2, old.route_specs(), old.window_specs(), Some(&old)).unwrap();
    assert!(!replacement.current(&pending));
    replacement.tick(civil(13, 0), 30, 120000, |_| None, |_| false);
    assert_eq!(replacement.next_operation().unwrap().remote(), "3000");
}

#[test]
fn receive_activity_replaces_cold_start_estimate() {
    let mut schedule = scheduler();
    schedule.tick(civil(13, 0), 30, 100000, |_| None, |_| false);
    let replacement = schedule.next_operation().unwrap();
    schedule.complete(&replacement, true);
    schedule.tick(civil(13, 0), 50, 120000, |_| Some(120000), |_| true);
    schedule.tick(civil(13, 1), 0, 130000, |_| Some(120000), |_| true);
    assert!(schedule.next_operation().is_none());
    schedule.tick(civil(13, 1), 50, 180000, |_| Some(120000), |_| true);
    assert_eq!(
        schedule.next_operation().unwrap().action(),
        LinkTransition::Detach
    );
}

#[test]
fn candidate_validation_rejects_each_invalid_endpoint_and_window_relationship() {
    let old = scheduler();
    assert!(LinkScheduler::new(0, vec![], vec![], None).is_err());
    assert!(LinkScheduler::new(1, old.route_specs(), old.window_specs(), Some(&old)).is_err());
    for invalid in ["local", "remote", "self", "duplicate"] {
        let mut routes = old.route_specs();
        match invalid {
            "local" => routes[0].local.clear(),
            "remote" => routes[0].remote.clear(),
            "self" => routes[0].remote = routes[0].local.clone(),
            _ => routes.push(routes[0].clone()),
        }
        assert!(
            LinkScheduler::new(2, routes, old.window_specs(), Some(&old)).is_err(),
            "{invalid}"
        );
    }
    for invalid in [
        "empty_routes",
        "duplicate_routes",
        "empty_replaced",
        "duplicate_replaced",
        "same",
        "missing_primary",
        "missing_replacement",
        "temporary_primary",
        "different_local",
    ] {
        let mut routes = old.route_specs();
        let mut windows = old.window_specs();
        match invalid {
            "empty_routes" => windows[0].routes.clear(),
            "duplicate_routes" => {
                let route = windows[0].routes[0];
                windows[0].routes.push(route);
            }
            "empty_replaced" => windows[0].replaced.clear(),
            "duplicate_replaced" => windows[0].replaced.push(0),
            "same" => windows[0].routes[0] = windows[0].replaced[0],
            "missing_primary" => windows[0].replaced = vec![2],
            "missing_replacement" => windows[0].routes[0] = 2,
            "temporary_primary" => routes[0].permanent = false,
            _ => routes[1].local = "other".into(),
        }
        assert!(
            LinkScheduler::new(2, routes, windows, Some(&old)).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn candidate_validation_requires_scheduled_lists_to_be_one_ordered_group() {
    for invalid in [
        "missing_group",
        "wrong_priority",
        "different_name",
        "different_group",
        "different_local",
    ] {
        let mut routes = vec![
            RouteSpec {
                local: "524950".into(),
                remote: "2000".into(),
                permanent: true,
                group_label: Some("primary".into()),
                group_name: None,
                group_priority: Some(0),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
                group_label: Some("schedule:net".into()),
                group_name: Some("Net".into()),
                group_priority: Some(0),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "4000".into(),
                permanent: false,
                group_label: Some("schedule:net".into()),
                group_name: Some("Net".into()),
                group_priority: Some(1),
            },
        ];
        match invalid {
            "missing_group" => routes[1].group_label = None,
            "wrong_priority" => routes[2].group_priority = Some(2),
            "different_name" => routes[2].group_name = Some("Other".into()),
            "different_group" => routes[2].group_label = Some("schedule:other".into()),
            _ => routes[2].local = "other".into(),
        }
        let windows = vec![ReplacementSpec {
            routes: vec![1, 2],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        }];
        assert!(
            LinkScheduler::new(1, routes, windows, None).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn route_removal_and_lost_ownership_only_reissue_exact_configured_intent() {
    let mut schedule = scheduler();
    schedule.tick(civil(11, 0), 0, 0, |_| None, |_| false);
    let attach = schedule.next_operation().unwrap();
    assert_eq!(attach.local(), "524950");
    assert!(schedule.complete(&attach, true));
    assert!(schedule.next_operation().is_none());
    let unchanged = LinkScheduler::new(
        2,
        schedule.route_specs(),
        schedule.window_specs(),
        Some(&schedule),
    )
    .unwrap();
    assert!(schedule.removed_routes(&unchanged).is_empty());
    let removed = LinkScheduler::new(2, vec![], vec![], Some(&schedule)).unwrap();
    assert_eq!(
        schedule.removed_routes(&removed),
        [schedule.route_specs()[0].clone()]
    );
    schedule.tick(civil(11, 1), 0, 1, |_| None, |_| false);
    let redial = schedule.next_operation().unwrap();
    assert_eq!(redial.remote(), "2000");
    assert!(schedule.complete(&redial, false));
    assert!(schedule.next_operation().is_some());
}

#[test]
fn permanent_group_members_are_desired_and_retried_independently() {
    let mut routes = (0..3)
        .map(|priority| RouteSpec {
            local: "524950".into(),
            remote: format!("{}", 2000 + priority),
            permanent: true,
            group_label: Some("network".into()),
            group_name: Some("Network".into()),
            group_priority: Some(priority),
        })
        .collect::<Vec<_>>();
    let mut schedule = LinkScheduler::new(1, std::mem::take(&mut routes), vec![], None).unwrap();
    schedule.tick(civil(11, 0), 0, 0, |_| None, |_| false);
    let reservations = (0..3)
        .map(|_| schedule.next_operation().unwrap())
        .collect::<Vec<_>>();
    for reservation in reservations.into_iter().rev() {
        assert!(schedule.complete(&reservation, true));
    }
    assert!(schedule.next_operation().is_none());

    schedule.tick(civil(11, 1), 0, 1, |_| None, |route| route.remote != "2001");
    let retry = schedule.next_operation().unwrap();
    assert_eq!(retry.remote(), "2001");
    assert!(schedule.complete(&retry, true));
    assert!(schedule.next_operation().is_none());
}

#[test]
fn scheduled_replacement_suppresses_and_restores_every_group_member() {
    let mut routes = (0..3)
        .map(|priority| RouteSpec {
            local: "524950".into(),
            remote: format!("{}", 2000 + priority),
            permanent: true,
            group_label: Some("network".into()),
            group_name: Some("Network".into()),
            group_priority: Some(priority),
        })
        .collect::<Vec<_>>();
    routes.push(RouteSpec {
        local: "524950".into(),
        remote: "3000".into(),
        permanent: false,
        group_label: None,
        group_name: None,
        group_priority: None,
    });
    let mut schedule = LinkScheduler::new(
        1,
        routes,
        vec![ReplacementSpec {
            routes: vec![3],
            replaced: vec![0, 1, 2],
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        }],
        None,
    )
    .unwrap();
    schedule.tick(civil(11, 59), 0, 0, |_| None, |_| false);
    for remote in ["2000", "2001", "2002"] {
        let attach = schedule.next_operation().unwrap();
        assert_eq!(attach.remote(), remote);
        assert!(schedule.complete(&attach, true));
    }
    assert!(schedule.next_operation().is_none());

    schedule.tick(civil(12, 0), 0, 1, |_| None, |_| true);
    for remote in ["2000", "2001", "2002"] {
        let detach = schedule.next_operation().unwrap();
        assert_eq!(detach.action(), LinkTransition::Detach);
        assert_eq!(detach.remote(), remote);
        assert!(schedule.complete(&detach, true));
    }
    let replacement = schedule.next_operation().unwrap();
    assert_eq!(replacement.action(), LinkTransition::Attach);
    assert_eq!(replacement.remote(), "3000");
    assert!(schedule.complete(&replacement, true));

    schedule.tick(civil(13, 0), 0, 2, |_| None, |_| true);
    let detach = schedule.next_operation().unwrap();
    assert_eq!(detach.action(), LinkTransition::Detach);
    assert_eq!(detach.remote(), "3000");
    assert!(schedule.complete(&detach, true));
    for remote in ["2000", "2001", "2002"] {
        let restore = schedule.next_operation().unwrap();
        assert_eq!(restore.action(), LinkTransition::Attach);
        assert_eq!(restore.remote(), remote);
        assert!(schedule.complete(&restore, true));
    }
}

#[test]
fn changed_windows_preserve_activity_but_not_previous_window_state() {
    let mut old = scheduler();
    old.tick(civil(12, 0), 0, 1000, |_| Some(900), |_| false);
    for change in ["time", "inactivity", "replacement", "primary"] {
        let mut routes = old.route_specs();
        let mut windows = old.window_specs();
        match change {
            "time" => {
                windows[0].window = ScheduledWindow::parse(None, None, "10:00", "11:00").unwrap()
            }
            "inactivity" => windows[0].end_inactivity_ms = 30000,
            "replacement" => routes[1].remote = "4000".into(),
            _ => routes[0].remote = "4000".into(),
        }
        let mut next = LinkScheduler::new(2, routes, windows, Some(&old)).unwrap();
        assert_eq!(next.windows[0].last_activity, Some(900));
        assert!(!next.windows[0].initialized);
        next.tick(civil(13, 0), 0, 1000, |_| None, |_| false);
        assert_eq!(
            next.next_operation().unwrap().remote(),
            if change == "replacement" {
                "4000"
            } else {
                "3000"
            }
        );
    }
    let empty = LinkScheduler::new(1, vec![], vec![], None).unwrap();
    let fresh = LinkScheduler::new(2, old.route_specs(), old.window_specs(), Some(&empty)).unwrap();
    assert_eq!(fresh.windows[0].last_activity, None);
}

#[test]
fn changed_warning_configuration_does_not_reuse_a_previous_window() {
    for change in ["start", "end", "message"] {
        let mut old = warning_scheduler(&[120_000], &[60_000]);
        old.tick(civil(11, 0), 0, 1, |_| None, |_| false);
        let mut window = old.window_specs();
        match change {
            "start" => window[0].warning_before_start_ms.push(30_000),
            "end" => window[0].warning_before_end_ms.push(30_000),
            _ => window[0].warning_message_id = Some("another-message".into()),
        }
        let next = LinkScheduler::new(2, old.route_specs(), window, Some(&old)).unwrap();
        assert!(!next.windows[0].initialized, "{change}");
        assert_eq!(next.windows[0].start_warned, Vec::<u64>::new());
        assert_eq!(next.windows[0].end_warned, Vec::<u64>::new());
    }
}
