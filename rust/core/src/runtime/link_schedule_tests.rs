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
            end_inactivity_ms: 60000,
        }],
        None,
    )
    .unwrap()
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
        "same",
        "missing_primary",
        "missing_replacement",
        "temporary_primary",
        "different_local",
    ] {
        let mut routes = old.route_specs();
        let mut windows = old.window_specs();
        match invalid {
            "same" => windows[0].route = windows[0].replaced,
            "missing_primary" => windows[0].replaced = 2,
            "missing_replacement" => windows[0].route = 2,
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
