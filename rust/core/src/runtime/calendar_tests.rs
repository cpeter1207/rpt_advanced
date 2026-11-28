use super::*;
use crate::config::resources::{
    candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope},
    events::{Boundary, Event, MacroRef, Trigger, Warning},
};
use crate::schedule::recurrence::Recurrence;
use std::sync::Arc;
fn id(n: u8) -> ResourceId {
    format!("00000000-0000-4000-8000-{n:012}").parse().unwrap()
}
fn scope() -> ResourceScope {
    ResourceScope::Port(id(1))
}
fn clock(wall: i64, now: u64) -> RuntimeClock {
    RuntimeClock {
        wall_seconds: wall,
        wall_subsecond_ms: 0,
        now_ms: now,
        civil: None,
    }
}
fn calendar(s: &str) -> Trigger {
    Trigger::Calendar(Arc::new(Recurrence::compile(s, Some("UTC")).unwrap()))
}
fn spec(n: u8, number: Option<i128>, start: &str, end: Option<&str>, p: u64) -> Event {
    let binding = |i, kind| ResourceBinding {
        id: id(i),
        scope: scope(),
        kind,
    };
    let boundary = |s: &str| Boundary {
        at: if s == "controller_start" {
            Trigger::ControllerStart
        } else {
            calendar(s)
        },
        command: MacroRef {
            binding: binding(n + 100, ResourceKind::Macro),
            key: format!("macro{n}"),
        },
        politeness_minutes: p,
        warnings: vec![],
    };
    Event {
        binding: binding(n, ResourceKind::Event),
        owner: scope(),
        sequence_number: number,
        start: boundary(start),
        end: end.map(boundary),
        restart_recovery: false,
        replace_permanent: None,
    }
}
fn scheduler(events: Vec<Event>) -> EventScheduler {
    EventScheduler::from_events(1, scope(), events, None).unwrap()
}
fn idle() -> ReceiveActivity {
    ReceiveActivity {
        active: false,
        last_active_ms: None,
    }
}
fn busy(now: u64) -> ReceiveActivity {
    ReceiveActivity {
        active: true,
        last_active_ms: Some(now),
    }
}
const MID: &str = "every day at 0:00";
#[test]
fn keeps_same_instant_order_until_outer_macro_terminates_and_rebinds_once() {
    let events = vec![
        spec(2, Some(20), MID, None, 0),
        spec(3, Some(10), MID, None, 1),
    ];
    let mut s = scheduler(events.clone());
    s.activate(clock(0, 500000), busy(500000)).unwrap();
    assert!(s.offer().is_none());
    s.observe(clock(61, 560000), busy(560000)).unwrap();
    let first = s.offer().unwrap();
    assert_eq!(first.event, id(3));
    assert_eq!(s.offer().unwrap(), first); // queue full
    assert!(s.admission(&first, MacroAdmission::Queued));
    assert!(s.offer().is_none());
    assert!(s.begin(&first));
    let mut next = EventScheduler::from_events(2, scope(), events, Some(&s)).unwrap();
    assert!(!next.terminate(&first, MacroOutcome::Completed));
    assert!(next.offer().is_none());
    let moved = next.rebind(&first).unwrap();
    assert!(!next.begin(&moved));
    assert!(next.terminate(&moved, MacroOutcome::Completed));
    let second = next.offer().unwrap();
    assert_eq!(second.event, id(2));
    assert!(next.admission(&second, MacroAdmission::Queued));
    assert!(next.begin(&second));
    assert!(next.terminate(&second, MacroOutcome::Completed));
    next.observe(clock(0, 600000), idle()).unwrap();
    assert!(next.offer().is_none());
}
#[test]
fn cancels_an_admitted_unbegun_window_at_nominal_end_before_next_offer() {
    let mut s = scheduler(vec![spec(2, None, MID, Some("every day at 0:01"), 0)]);
    s.activate(clock(-120, 0), idle()).unwrap();
    s.observe(clock(12, 200000), idle()).unwrap();
    let start = s.offer().unwrap();
    assert!(s.admission(&start, MacroAdmission::Queued));
    assert_eq!(
        s.begin_at(&start, clock(60, 248000), idle()).unwrap(),
        BeginDecision::Retired
    );
    assert!(s.offer().is_none());
}
#[test]
fn startup_barrier_precedes_single_recovery_and_false_skips_the_entire_window() {
    let mut window = spec(2, Some(0), MID, Some("every day at 0:05"), 2);
    window.restart_recovery = true;
    let events = vec![
        window,
        spec(3, None, "controller_start", None, 0),
        spec(4, None, MID, Some("every day at 0:05"), 0),
    ];
    let mut s = scheduler(events.clone());
    s.activate(clock(30, 0), busy(0)).unwrap();
    let first = s.offer().unwrap();
    assert_eq!(first.event, id(3));
    assert!(s.admission(&first, MacroAdmission::Queued));
    assert!(s.begin(&first));
    assert!(s.terminate(&first, MacroOutcome::Completed));
    let recovered = s.offer().unwrap();
    assert_eq!(recovered.event, id(2));
    assert!(s.admission(&recovered, MacroAdmission::Queued));
    assert!(s.begin(&recovered));
    assert!(s.terminate(&recovered, MacroOutcome::Completed));
    assert!(s.offer().is_none());
    s.observe(clock(300, 270000), idle()).unwrap();
    let end = s.offer().unwrap();
    assert_eq!(end.event, id(2));
    assert!(s.admission(&end, MacroAdmission::Queued));
    assert!(s.begin(&end));
    assert!(s.terminate(&end, MacroOutcome::Completed));
    assert!(s.offer().is_none());
}
#[test]
fn warnings_keep_their_own_minute_and_missed_boundaries_do_not_catch_up() {
    let mut event = spec(2, None, MID, None, 2);
    let Trigger::Calendar(at) = calendar("every day at 0:01") else {
        unreachable!()
    };
    event.start.warnings.push(Warning {
        at,
        command: event.start.command.clone(),
    });
    let mut s = scheduler(vec![event]);
    s.activate(clock(-120, 0), idle()).unwrap();
    s.observe(clock(60, 180000), busy(180000)).unwrap();
    let warning = s.offer().unwrap();
    assert_eq!(warning.kind, EventWork::StartWarning(0));
    assert!(s.admission(&warning, MacroAdmission::Queued));
    assert!(s.begin(&warning));
    assert!(s.terminate(&warning, MacroOutcome::Completed));
    assert!(s.offer().is_none());
}

#[test]
fn early_polite_offer_expires_on_new_activity_before_admission() {
    let mut s = scheduler(vec![spec(2, None, MID, None, 2)]);
    s.activate(clock(-180, 0), idle()).unwrap();
    s.observe(clock(0, 180000), idle()).unwrap();
    assert!(s.offer().is_some());
    s.observe(clock(10, 190000), busy(190000)).unwrap();
    assert!(s.offer().is_none());
    s.observe(
        clock(60, 240000),
        ReceiveActivity {
            active: false,
            last_active_ms: Some(190000),
        },
    )
    .unwrap();
    assert!(s.offer().is_none());
    s.observe(clock(30, 300000), busy(300000)).unwrap();
    assert!(s.offer().is_some());
}
#[test]
fn provider_actual_instant_deduplicates_historical_subminute_zone_boundaries() {
    let mut s = scheduler(vec![spec(
        2,
        None,
        "every day at 00:05 in Africa/Monrovia",
        None,
        0,
    )]);
    let due = 63506970; // 1972-01-06T00:49:30Z
    s.activate(clock(due - 10, 0), idle()).unwrap();
    assert!(s.offer().is_none());
    s.observe(clock(due + 5, 15000), idle()).unwrap();
    let t = s.offer().unwrap();
    assert_eq!(t.nominal, due);
    assert!(s.admission(&t, MacroAdmission::Queued));
    assert!(s.begin(&t));
    assert!(s.terminate(&t, MacroOutcome::Completed));
    s.observe(clock(due + 34, 44000), idle()).unwrap();
    assert!(s.offer().is_none());
}
#[test]
fn rollback_before_recovered_window_suspends_queued_start() {
    let mut e = spec(2, None, MID, Some("every day at 0:05"), 0);
    e.restart_recovery = true;
    let mut s = scheduler(vec![e]);
    s.activate(clock(80, 0), idle()).unwrap();
    let t = s.offer().unwrap();
    assert!(s.admission(&t, MacroAdmission::Queued));
    assert_eq!(
        s.begin_at(&t, clock(-60, 1000), idle()).unwrap(),
        BeginDecision::Wait
    );
    assert_eq!(
        s.begin_at(&t, clock(60, 2000), idle()).unwrap(),
        BeginDecision::Begin
    );
}

#[test]
fn inhibited_ticks_cannot_fire_startup_and_reactivation_cannot_replay_it() {
    let mut s = scheduler(vec![spec(2, None, "controller_start", None, 0)]);
    s.observe(clock(0, 0), idle()).unwrap();
    assert!(s.offer().is_none());
    s.activate(clock(90, 90000), idle()).unwrap();
    let t = s.offer().unwrap();
    assert_eq!(t.nominal, 90);
    assert!(s.admission(&t, MacroAdmission::Queued));
    assert!(s.begin(&t));
    assert!(s.terminate(&t, MacroOutcome::Completed));
    s.activate(clock(150, 150000), idle()).unwrap();
    assert!(s.offer().is_none());
}

fn finish(s: &mut EventScheduler) -> EventTicket {
    let t = s.offer().unwrap();
    assert!(s.admission(&t, MacroAdmission::Queued));
    assert!(s.begin(&t));
    assert!(s.terminate(&t, MacroOutcome::Completed));
    t
}
#[test]
fn mixed_numbered_ties_and_unnumbered_ends_preserve_the_same_instant_chain() {
    let opening = "every day at 0:01";
    let mut warning = spec(7, None, "every day at 22:00", None, 0);
    let Trigger::Calendar(at) = calendar(opening) else {
        unreachable!()
    };
    warning.start.warnings.push(Warning {
        at,
        command: warning.start.command.clone(),
    });
    let mut s = scheduler(vec![
        spec(2, Some(-2), opening, None, 0),
        spec(3, Some(3), opening, None, 0),
        spec(4, None, MID, Some(opening), 0),
        spec(5, Some(3), MID, Some(opening), 0),
        spec(6, None, opening, None, 0),
        warning,
    ]);
    s.activate(clock(-120, 0), idle()).unwrap();
    s.observe(clock(0, 120000), idle()).unwrap();
    finish(&mut s);
    finish(&mut s);
    s.observe(clock(60, 180000), idle()).unwrap();
    let order = (0..6).map(|_| finish(&mut s).event).collect::<Vec<_>>();
    assert_eq!(order, [4, 2, 5, 3, 7, 6].map(id));
}
#[test]
fn ignores_overlapping_starts_and_unmatched_ends_across_midnight() {
    let mut s = scheduler(vec![spec(
        2,
        None,
        "every day at 23:50, 23:55",
        Some("every day at 0:10, 0:20"),
        0,
    )]);
    s.activate(clock(23 * 3600, 0), idle()).unwrap();
    s.observe(clock(23 * 3600 + 50 * 60, 3000000), idle())
        .unwrap();
    assert_eq!(finish(&mut s).kind, EventWork::Start);
    s.observe(clock(23 * 3600 + 55 * 60, 3300000), idle())
        .unwrap();
    assert!(s.offer().is_none());
    s.observe(clock(86400 + 10 * 60, 4800000), idle()).unwrap();
    assert_eq!(finish(&mut s).kind, EventWork::End);
    s.observe(clock(86400 + 20 * 60, 5400000), idle()).unwrap();
    assert!(s.offer().is_none());
}
#[test]
fn fresh_run_recovers_original_window_and_a_reload_does_not_replay_lifecycle() {
    let mut window = spec(
        2,
        None,
        "every day at 23:50, 23:55",
        Some("every day at 0:10"),
        1,
    );
    window.restart_recovery = true;
    let startup = spec(4, Some(0), "controller_start", None, 0);
    for _ in 0..2 {
        let mut s = scheduler(vec![window.clone(), startup.clone()]);
        s.activate(clock(86400, 10), busy(10)).unwrap();
        assert_eq!(finish(&mut s).event, id(4));
        let t = finish(&mut s);
        assert_eq!(t.nominal, 23 * 3600 + 50 * 60);
        assert_eq!(t.origin, EventOrigin::Recovery);
        assert_eq!(t.observed, clock(86400, 10));
        let mut added = startup.clone();
        added.binding.id = id(8);
        let mut next = EventScheduler::from_events(
            2,
            scope(),
            vec![added, window.clone(), startup.clone()],
            Some(&s),
        )
        .unwrap();
        next.activate(clock(86460, 60010), busy(60010)).unwrap();
        assert!(next.offer().is_none());
    }
    let mut ended = scheduler(vec![window]);
    ended.activate(clock(86400 + 600, 0), idle()).unwrap();
    assert!(ended.offer().is_none());
}
#[test]
fn forced_politeness_survives_skipped_checks_and_semantically_equal_reload() {
    let mut event = spec(2, None, MID, None, 2);
    let mut s = scheduler(vec![event.clone()]);
    s.activate(clock(12, 100000), busy(100000)).unwrap();
    event.start.at = calendar("EVERY DAY AT 00:00");
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&s)).unwrap();
    next.observe(clock(10 * 3600, 207999), busy(207999))
        .unwrap();
    assert!(next.offer().is_none());
    next.observe(clock(1, 208000), busy(208000)).unwrap();
    assert_eq!(finish(&mut next).nominal, 0);
    next.observe(clock(20, 210000), busy(210000)).unwrap();
    assert!(next.offer().is_none());
}
#[test]
fn a_different_instant_warning_does_not_inherit_pending_start_politeness() {
    let mut event = spec(2, None, MID, None, 2);
    let Trigger::Calendar(at) = calendar("every day at 0:01") else {
        unreachable!()
    };
    event.start.warnings.push(Warning {
        at,
        command: event.start.command.clone(),
    });
    let mut s = scheduler(vec![event]);
    s.activate(clock(0, 0), busy(0)).unwrap();
    s.observe(clock(60, 60000), busy(60000)).unwrap();
    assert_eq!(finish(&mut s).kind, EventWork::StartWarning(0));
    s.observe(clock(120, 120000), busy(120000)).unwrap();
    assert_eq!(finish(&mut s).kind, EventWork::Start);
}

#[test]
fn terminal_admission_failure_consumes_without_locking_the_batch() {
    let mut s = scheduler(vec![
        spec(2, Some(0), MID, None, 0),
        spec(3, Some(1), MID, None, 0),
    ]);
    s.activate(clock(10, 0), idle()).unwrap();
    let t = s.offer().unwrap();
    assert!(s.admission(&t, MacroAdmission::Busy));
    assert_eq!(s.offer().unwrap(), t);
    assert!(s.admission(&t, MacroAdmission::Rejected));
    assert!(!s.admission(&t, MacroAdmission::Queued));
    assert_eq!(finish(&mut s).event, id(3));
}

#[test]
fn uses_provider_gap_fold_and_dateline_policy_for_actual_instants() {
    for (expression, now, due) in [
        ("every day at 02:30 in America/New_York", 1710055825, None), // 2024-03-10T07:30:25Z
        (
            "every day at 02:30, 03:30 in America/New_York",
            1710055825,
            Some(1710055800),
        ), // 2024-03-10T07:30:25Z
        (
            "every day at 01:30 in America/New_York",
            1730611825,
            Some(1730611800),
        ), // 2024-11-03T05:30:25Z
        ("every day at 01:30 in America/New_York", 1730615425, None), // 2024-11-03T06:30:25Z
        (
            "every day at 02:15 in Australia/Lord_Howe",
            1728143125,
            None,
        ), // 2024-10-05T15:45:25Z
        ("on 2011-12-30 at 09:00 in Pacific/Apia", 1325271625, None), // 2011-12-30T19:00:25Z
    ] {
        let mut s = scheduler(vec![spec(2, None, expression, None, 0)]);
        s.activate(clock(now, 0), idle()).unwrap();
        assert_eq!(s.offer().map(|t| t.nominal), due);
    }
}

#[test]
fn permanently_full_repeating_stream_retains_only_its_oldest_reservation() {
    let mut s = scheduler(vec![spec(
        2,
        None,
        "every 1 min from 00:00 to 23:59",
        None,
        0,
    )]);
    s.activate(clock(0, 0), idle()).unwrap();
    let first = s.offer().unwrap();
    for n in 1..=180 {
        s.observe(clock(n * 60, n as u64 * 60000), idle()).unwrap();
        assert_eq!(s.offer().unwrap(), first);
    }
    assert_eq!(finish(&mut s), first);
    assert!(s.offer().is_none());
    s.observe(clock(181 * 60, 181 * 60000), idle()).unwrap();
    assert_eq!(finish(&mut s).nominal, 181 * 60);
}

#[test]
fn distinct_startup_subsecond_and_invalid_wall_capture_do_not_mix_batches() {
    let mut s = scheduler(vec![
        spec(2, Some(0), "controller_start", None, 0),
        spec(3, Some(9), MID, None, 0),
    ]);
    let mut tick = clock(0, 100);
    tick.wall_subsecond_ms = 500;
    s.activate(tick, idle()).unwrap();
    assert_eq!(finish(&mut s).event, id(3));
    let t = s.offer().unwrap();
    assert_eq!(t.nominal_subsecond_ms, 500);
    assert!(s.admission(&t, MacroAdmission::Queued));
    assert_eq!(
        s.begin_at(&t, clock(i64::MAX, 200), idle()),
        Err(SchedulerError::Calendar(CalendarError::Range))
    );
    assert_eq!(
        s.begin_at(&t, clock(-1, 201), idle()),
        Err(SchedulerError::Clock)
    );
    assert_eq!(
        s.begin_at(&t, clock(1, 500), idle()),
        Ok(BeginDecision::Begin)
    );
}

const LIMIT_EXPRESSION: &str = "every month on the 31st at 09:00 except 9998-05-01 during feb";
const FEB9_NOON: i64 = 1770638400;
fn add_warning(event: &mut Event, at: &str) {
    let Trigger::Calendar(at) = calendar(at) else {
        unreachable!()
    };
    event.start.warnings.push(Warning {
        at,
        command: event.start.command.clone(),
    });
}
#[test]
fn review_r1_warning_edit_preserves_completed_start() {
    let mut event = spec(2, None, MID, None, 0);
    let mut prev = scheduler(vec![event.clone()]);
    prev.activate(clock(5, 0), idle()).unwrap();
    assert_eq!(finish(&mut prev).kind, EventWork::Start);
    add_warning(&mut event, "every day at 23:00");
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&prev)).unwrap();
    next.observe(clock(20, 15000), idle()).unwrap();
    assert!(next.offer().is_none(), "unchanged start already completed");
    next.observe(clock(23 * 3600, 82800000), idle()).unwrap();
    assert_eq!(finish(&mut next).kind, EventWork::StartWarning(0));
}
#[test]
fn review_r1_warning_edit_preserves_live_pair_hold_and_end() {
    let mut event = spec(2, None, MID, Some("every day at 01:00"), 0);
    event.replace_permanent = Some((
        ResourceBinding {
            id: id(90),
            scope: scope(),
            kind: ResourceKind::PermanentGroup,
        },
        crate::config::resources::PortKey::new("net"),
    ));
    let mut prev = scheduler(vec![event.clone()]);
    prev.activate(clock(-120, 0), idle()).unwrap();
    prev.observe(clock(5, 125000), idle()).unwrap();
    assert_eq!(finish(&mut prev).kind, EventWork::Start);
    add_warning(&mut event, "every day at 23:00");
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&prev)).unwrap();
    next.observe(clock(1800, 1920000), idle()).unwrap();
    assert!(
        next.calendar.as_ref().unwrap().events[0]
            .held
            .contains(&3600),
        "begun window still owns group"
    );
    next.observe(clock(3600, 3720000), idle()).unwrap();
    assert_eq!(finish(&mut next).kind, EventWork::End);
    assert!(next.calendar.as_ref().unwrap().events[0].held.is_empty());
}
#[test]
fn review_r1_warning_edit_retires_only_changed_continuation() {
    let mut e = spec(2, None, MID, None, 1);
    add_warning(&mut e, "every day at 00:01");
    let mut prev = scheduler(vec![e.clone()]);
    prev.activate(clock(0, 0), busy(0)).unwrap();
    prev.observe(clock(60, 60000), busy(60000)).unwrap();
    let start = prev.offer().unwrap();
    assert!(prev.admission(&start, MacroAdmission::Queued));
    let warning = prev.offer().unwrap();
    assert!(prev.admission(&warning, MacroAdmission::Queued));
    e.start.warnings.clear();
    add_warning(&mut e, "every day at 00:02");
    let mut next = EventScheduler::from_events(2, scope(), vec![e], Some(&prev)).unwrap();
    assert!(next.rebind(&warning).is_none());
    let retained = next
        .rebind(&start)
        .expect("boundary reservation survives an independent warning edit");
    assert_eq!(
        next.begin_at(&retained, clock(65, 65000), idle()),
        Ok(BeginDecision::Begin)
    );
    assert!(next.terminate(&retained, MacroOutcome::Completed));
    next.observe(clock(120, 120000), idle()).unwrap();
    assert_eq!(finish(&mut next).kind, EventWork::StartWarning(0));
}
#[test]
fn review_r1_optional_cache_limit_cannot_starve_startup() {
    let Trigger::Calendar(real) = calendar(LIMIT_EXPRESSION) else {
        unreachable!()
    };
    assert_eq!(real.match_minute(FEB9_NOON), Ok(None));
    assert_eq!(real.next_after(FEB9_NOON), Err(CalendarError::Limit));
    let mut s = scheduler(vec![
        spec(2, None, LIMIT_EXPRESSION, None, 0),
        spec(3, None, "controller_start", None, 0),
    ]);
    s.activate(clock(FEB9_NOON, 0), idle()).unwrap();
    assert_eq!(finish(&mut s).event, id(3));
    assert!(s.offer().is_none());
    assert_eq!(
        s.calendar_issues().collect::<Vec<_>>(),
        vec![(id(2), CalendarError::Limit)]
    );
}
#[test]
fn review_r1_optional_cache_limit_cannot_starve_monotonic_deadline() {
    let e = spec(2, None, "every day at 12:00", None, 1);
    let mut prev = scheduler(vec![e.clone()]);
    prev.activate(clock(FEB9_NOON, 0), busy(0)).unwrap();
    assert!(prev.offer().is_none());
    let mut next = EventScheduler::from_events(
        2,
        scope(),
        vec![spec(3, None, LIMIT_EXPRESSION, None, 0), e],
        Some(&prev),
    )
    .unwrap();
    next.observe(clock(FEB9_NOON + 60, 60000), busy(60000))
        .unwrap();
    let forced = finish(&mut next);
    assert_eq!((forced.event, forced.nominal), (id(2), FEB9_NOON));
}
#[test]
fn review_r1_required_pair_limit_cannot_authorize_uncertain_start_or_block_other_events() {
    let uncertain = spec(2, None, "every day at 12:00", Some(LIMIT_EXPRESSION), 0);
    let mut s = scheduler(vec![
        uncertain,
        spec(3, None, "every day at 12:00", None, 0),
    ]);
    s.activate(clock(FEB9_NOON - 60, 0), idle()).unwrap();
    s.observe(clock(FEB9_NOON, 60000), idle()).unwrap();
    assert_eq!(finish(&mut s).event, id(3));
    assert!(s.offer().is_none());
    assert_eq!(
        s.calendar_issues().collect::<Vec<_>>(),
        vec![(id(2), CalendarError::Limit)]
    );
}
#[test]
fn review_r1_expired_new_pair_cannot_resurrect_after_rollback() {
    let start = 63_506_970;
    let mut s = scheduler(vec![spec(
        2,
        None,
        "every day at 00:05 in Africa/Monrovia",
        Some("every day at 00:50 in UTC"),
        0,
    )]);
    s.activate(clock(start - 10, 0), idle()).unwrap();
    s.observe(clock(start + 35, 45000), idle()).unwrap();
    assert!(
        s.offer().is_none(),
        "end passed before first observation of the start"
    );
    s.observe(clock(start + 29, 46000), idle()).unwrap();
    assert!(s.offer().is_none());
    s.observe(clock(start + 36, 47000), idle()).unwrap();
    assert!(s.offer().is_none());
}

#[test]
fn review_r1_adding_a_warning_in_the_same_minute_does_not_share_start_high_water() {
    let mut event = spec(2, None, MID, None, 0);
    let mut old = scheduler(vec![event.clone()]);
    old.activate(clock(5, 5000), idle()).unwrap();
    assert_eq!(finish(&mut old).kind, EventWork::Start);
    add_warning(&mut event, MID);
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&old)).unwrap();
    next.observe(clock(20, 20000), idle()).unwrap();
    assert_eq!(finish(&mut next).kind, EventWork::StartWarning(0));
    assert!(next.offer().is_none());
}
#[test]
fn review_r1_reordered_unchanged_warning_transfers_its_real_queued_reservation() {
    let mut event = spec(2, None, "every day at 01:00", None, 0);
    add_warning(&mut event, MID);
    let mut old = scheduler(vec![event.clone()]);
    old.activate(clock(5, 5000), idle()).unwrap();
    let queued = old.offer().unwrap();
    assert!(old.admission(&queued, MacroAdmission::Queued));
    add_warning(&mut event, "every day at 23:00");
    event.start.warnings.swap(0, 1);
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&old)).unwrap();
    let moved = next.rebind(&queued).unwrap();
    assert_eq!(moved.kind, EventWork::StartWarning(1));
    assert_eq!(
        next.begin_at(&moved, clock(10, 10000), idle()),
        Ok(BeginDecision::Begin)
    );
    assert!(next.terminate(&moved, MacroOutcome::Completed));
    assert!(next.offer().is_none());
}

#[test]
fn review_r1_changed_warning_macro_retires_without_replaying_its_observed_occurrence() {
    let mut event = spec(2, None, "every day at 01:00", None, 0);
    add_warning(&mut event, MID);
    let mut old = scheduler(vec![event.clone()]);
    old.activate(clock(5, 5000), idle()).unwrap();
    let queued = old.offer().unwrap();
    assert!(old.admission(&queued, MacroAdmission::Queued));
    event.start.warnings[0].command = spec(3, None, MID, None, 0).start.command;
    let mut next = EventScheduler::from_events(2, scope(), vec![event], Some(&old)).unwrap();
    assert!(next.rebind(&queued).is_none());
    next.observe(clock(20, 20000), idle()).unwrap();
    assert!(
        next.offer().is_none(),
        "same warning occurrence cannot run a replacement macro"
    );
}

#[test]
fn adjacent_group_hold_follows_start_admission_and_politeness() {
    use crate::config::resources::PortKey;
    use crate::runtime::link_schedule::{LinkScheduler, LinkTransition, RouteSpec};

    #[derive(Clone, Copy, Debug)]
    enum Start {
        Rejected,
        Expired,
        BusyReceiver,
        Begun,
    }
    const CHANGE: &str = "every day at 0:05";
    for path in [
        Start::Rejected,
        Start::Expired,
        Start::BusyReceiver,
        Start::Begun,
    ] {
        let mut closing = spec(2, Some(10), MID, Some(CHANGE), 0);
        let mut next = spec(3, Some(20), CHANGE, Some("every day at 0:10"), 2);
        next.end.as_mut().unwrap().politeness_minutes = 0;
        add_warning(&mut next, CHANGE);
        next.end.as_mut().unwrap().warnings = std::mem::take(&mut next.start.warnings);
        let group = (
            ResourceBinding {
                id: id(90),
                scope: scope(),
                kind: ResourceKind::PermanentGroup,
            },
            PortKey::new("net"),
        );
        closing.replace_permanent = Some(group.clone());
        next.replace_permanent = Some(group);
        let profile = ScopedEvents {
            owner: scope(),
            port_key: Some(PortKey::new("station")),
            events: vec![closing, next],
            warnings: vec![],
            local: Default::default(),
        };
        let mut s = EventScheduler::from_profiles(1, &profile, None).unwrap();
        let mut links = LinkScheduler::new(
            1,
            vec![RouteSpec {
                local: "station".into(),
                remote: "506315".into(),
                permanent: true,
                group_label: Some("net".into()),
                group_name: None,
                group_priority: Some(0),
            }],
            vec![],
            None,
        )
        .unwrap();
        let old = links
            .next_operation()
            .expect("permanent route initially available");
        s.activate(clock(-180, 0), idle()).unwrap();
        s.observe(clock(0, 180_000), idle()).unwrap();
        assert_eq!(finish(&mut s).event, id(2));
        assert!(s.apply_group_holds("station", &mut links));
        assert!(!links.current(&old));
        assert!(links.next_operation().is_none());

        let at_change = clock(300, 480_000);
        s.observe(at_change, idle()).unwrap();
        let close = finish(&mut s);
        assert_eq!((close.event, close.kind), (id(2), EventWork::End));
        let warning = s.offer().unwrap();
        assert_eq!(
            (warning.event, warning.kind),
            (id(3), EventWork::EndWarning(0))
        );
        assert!(s.admission(&warning, MacroAdmission::Queued));
        assert_eq!(
            s.begin_at(&warning, at_change, idle()).unwrap(),
            BeginDecision::Begin
        );
        assert!(s.apply_group_holds("station", &mut links));
        assert!(
            links.next_operation().is_none(),
            "route remained suspended through the warning"
        );
        assert!(s.terminate(&warning, MacroOutcome::Completed));
        let start = s.offer().unwrap();
        assert_eq!((start.event, start.kind), (id(3), EventWork::Start));
        match path {
            Start::Rejected => assert!(s.admission(&start, MacroAdmission::Rejected)),
            Start::Expired => {
                assert!(s.admission(&start, MacroAdmission::Queued));
                assert_eq!(
                    s.begin_at(&start, clock(600, 780_000), idle()).unwrap(),
                    BeginDecision::Retired
                );
            }
            Start::BusyReceiver => {
                s.observe(clock(301, 481_000), busy(481_000)).unwrap();
                assert!(
                    s.offer().is_none(),
                    "new activity withdrew the polite offer"
                );
            }
            Start::Begun => {
                assert!(s.admission(&start, MacroAdmission::Queued));
                assert_eq!(
                    s.begin_at(&start, at_change, idle()).unwrap(),
                    BeginDecision::Begin
                );
                assert!(s.terminate(&start, MacroOutcome::Completed));
                assert!(s.apply_group_holds("station", &mut links));
                assert!(
                    links.next_operation().is_none(),
                    "begun window retains its normal hold"
                );
                s.observe(clock(600, 780_000), idle()).unwrap();
                assert_eq!(finish(&mut s).kind, EventWork::End);
            }
        }
        assert!(s.apply_group_holds("station", &mut links));
        let resumed = links
            .next_operation()
            .unwrap_or_else(|| panic!("route did not resume for {path:?}"));
        assert_eq!(
            (resumed.remote(), resumed.action()),
            ("506315", LinkTransition::Attach)
        );
        if matches!(path, Start::BusyReceiver) {
            s.observe(clock(420, 600_000), busy(600_000)).unwrap();
            assert_eq!(s.offer().unwrap(), start);
            assert!(s.apply_group_holds("station", &mut links));
            assert!(
                links.current(&resumed),
                "later eligibility alone cannot reacquire the abandoned hold"
            );
        }
    }
}

#[test]
fn calendar_observation_keeps_last_unobserved_minute_gap() {
    let mut s = scheduler(Vec::new());
    s.activate(clock(59, 0), idle()).unwrap();
    for (wall, now) in [(59, 1), (119, 2)] {
        s.observe(clock(wall, now), idle()).unwrap();
    }
    assert_eq!(
        serde_json::to_value(s.calendar_observation()).unwrap(),
        serde_json::json!({"previous_wall_seconds":59,"current_wall_seconds":119})
    );

    for (wall, now) in [(180, 3), (181, 4), (60, 5), (61, 6)] {
        s.observe(clock(wall, now), idle()).unwrap();
    }
    assert_eq!(
        serde_json::to_value(s.calendar_observation()).unwrap(),
        serde_json::json!({
            "previous_wall_seconds":60,
            "current_wall_seconds":61,
            "last_abnormal_gap":{"previous_wall_seconds":119,"current_wall_seconds":180}
        })
    );

    let mut next = EventScheduler::from_events(2, scope(), Vec::new(), Some(&s)).unwrap();
    next.observe(clock(359, 7), idle()).unwrap();
    assert_eq!(
        serde_json::to_value(next.calendar_observation()).unwrap(),
        serde_json::json!({"current_wall_seconds":359})
    );
}
