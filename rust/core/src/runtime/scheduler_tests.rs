use super::*;
use crate::schedule::{CivilTime, Weekday};

fn civil(minute: u8) -> CivilTime {
    CivilTime::new(2026, 9, 15, Weekday::Tuesday, 12, minute).unwrap()
}
fn event(name: &str, minute: u8) -> EventSpec {
    EventSpec {
        local: "524950".into(),
        name: name.into(),
        trigger: format!("daily 12:{minute:02}").parse().unwrap(),
        media: false,
    }
}
fn message(_: &EventSpec, _: &CivilTime) -> Result<DispatchContent, SchedulerError> {
    Ok(DispatchContent {
        message: Some("test".into()),
        operation: None,
    })
}

#[test]
fn media_occurrences_keep_original_deadlines_through_replacement_and_reload() {
    let spec = EventSpec {
        trigger: "every 1 minute".parse().unwrap(),
        media: true,
        ..event("clock", 0)
    };
    let mut scheduler = EventScheduler::new(1, vec![spec.clone()], None).unwrap();
    let first = scheduler
        .next(100, -50, civil(0), message)
        .unwrap()
        .unwrap();
    let obsolete = scheduler
        .next(160, 59_950, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!(
        obsolete.civil_time(),
        civil(0),
        "the older action reservation must settle first"
    );
    assert!(obsolete.occurrence().unwrap().cancelled());
    assert_eq!(obsolete.due_ms(), -50);
    assert!(scheduler.message_queued(&obsolete));
    assert!(scheduler.complete(&obsolete));
    assert!(!scheduler.current(&first));
    let latest = scheduler
        .next(161, 60_950, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!(latest.civil_time(), civil(1));
    assert_eq!(latest.due_ms(), 59_950, "retry cannot grant another budget");
    let mut reloaded = EventScheduler::new(2, vec![spec], Some(&scheduler)).unwrap();
    assert!(!reloaded.current(&latest));
    let retained = reloaded
        .next(162, 61_950, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!(retained.due_ms(), 59_950);
    assert!(reloaded.message_queued(&retained));
    assert!(reloaded.complete(&retained));
    assert!(
        reloaded
            .next(163, 62_950, civil(1), message)
            .unwrap()
            .is_none()
    );
}

#[test]
fn media_clock_steps_do_not_repeat_already_consumed_civil_minutes() {
    let spec = EventSpec {
        trigger: "every 1 minute".parse().unwrap(),
        media: true,
        ..event("clock", 0)
    };
    let mut scheduler = EventScheduler::new(1, vec![spec], None).unwrap();
    for (wall, minute, due) in [
        (100, 58, true),
        (160, 59, true),
        (220, 0, false),
        (280, 58, false),
        (340, 59, false),
    ] {
        let next = scheduler
            .next(wall, i128::from(wall) * 1000, civil(minute), message)
            .unwrap();
        assert_eq!(next.is_some(), due, "repeated civil minute12:{minute:02}");
        if let Some(dispatch) = next {
            assert!(scheduler.message_queued(&dispatch));
            assert!(scheduler.complete(&dispatch));
        }
    }
    let tomorrow = CivilTime::new(2026, 9, 16, Weekday::Wednesday, 12, 0).unwrap();
    assert!(
        scheduler
            .next(86_500, 86_500_000, tomorrow, message)
            .unwrap()
            .is_some()
    );
}

#[test]
fn periodic_triggers_become_pending_on_clock_boundaries() {
    for (at, hour, minute, due) in [
        ("every 1 minute", 12, 7, true),
        ("every 15 minutes", 12, 14, false),
        ("every 15 minutes", 12, 15, true),
        ("every 15 minutes", 12, 16, false),
        ("every 60 minutes", 12, 0, true),
        ("every 60 minutes", 12, 1, false),
        ("every 2 hours", 12, 0, true),
        ("every 2 hours", 13, 0, false),
        ("every 24 hours", 0, 0, true),
        ("every 24 hours", 1, 0, false),
    ] {
        let spec = EventSpec {
            local: "524950".into(),
            name: "clock_message".into(),
            trigger: at.parse().expect("periodic clock trigger should be valid"),
            media: false,
        };
        let mut scheduler = EventScheduler::new(1, vec![spec], None).unwrap();
        let local = CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, minute).unwrap();
        assert_eq!(
            scheduler
                .next(100, 100000, local, message)
                .unwrap()
                .is_some(),
            due,
            "{at} at {hour:02}:{minute:02}"
        );
    }
}

#[test]
fn periodic_occurrences_keep_one_reservation_across_repeated_ticks_and_reload() {
    let spec = EventSpec {
        local: "524950".into(),
        name: "quarter_hour".into(),
        trigger: "every 15 minutes".parse().unwrap(),
        media: false,
    };
    let mut scheduler = EventScheduler::new(1, vec![spec.clone()], None).unwrap();
    let first = scheduler
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    assert!(scheduler.message_queued(&first));
    assert!(scheduler.complete(&first));
    assert!(
        scheduler
            .next(101, 101000, civil(0), message)
            .unwrap()
            .is_none()
    );
    assert!(
        scheduler
            .next(99, 99000, civil(0), message)
            .unwrap()
            .is_none()
    );
    assert!(
        scheduler
            .next(102, 102000, civil(14), message)
            .unwrap()
            .is_none()
    );
    let next = scheduler
        .next(103, 103000, civil(15), message)
        .unwrap()
        .unwrap();
    assert_eq!(next.name(), "quarter_hour");
    assert!(scheduler.message_queued(&next));
    assert!(scheduler.complete(&next));

    let mut reloaded = EventScheduler::new(2, vec![spec], Some(&scheduler)).unwrap();
    assert!(
        reloaded
            .next(104, 104000, civil(15), message)
            .unwrap()
            .is_none()
    );
    assert!(
        reloaded
            .next(105, 105000, civil(29), message)
            .unwrap()
            .is_none()
    );
    assert!(
        reloaded
            .next(106, 106000, civil(30), message)
            .unwrap()
            .is_some()
    );
}

#[test]
fn same_revision_dispatch_from_another_scheduler_cannot_match_a_different_event() {
    let mut first = EventScheduler::new(1, vec![event("first", 0)], None).unwrap();
    let mut second = EventScheduler::new(1, vec![event("second", 0)], None).unwrap();
    let first_token = first.next(100, 100000, civil(0), message).unwrap().unwrap();
    let second_token = second
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    assert!(!first.current(&second_token));
    assert!(!second.current(&first_token));
    assert!(first.current(&first_token));
    assert!(second.current(&second_token));
}

#[test]
fn pending_event_retries_without_duplicate_admission_and_other_nodes_can_reuse_names() {
    let first = event("same", 0);
    let mut other = first.clone();
    other.local = "2000".into();
    let mut scheduler = EventScheduler::new(1, vec![first, other], None).unwrap();
    let pending = scheduler
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    assert!(scheduler.message_queued(&pending));
    let retry = scheduler
        .next(101, 101000, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!(retry.local(), pending.local());
    assert_eq!(retry.civil_time(), pending.civil_time());
    assert!(scheduler.is_message_queued(&retry));
    assert!(scheduler.complete(&retry));
    let second = scheduler
        .next(102, 102000, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!(second.local(), "2000");
    assert!(scheduler.message_queued(&second));
    assert!(scheduler.complete(&second));
}

#[test]
fn constructor_rejects_invalid_identity_duplicates_and_nonincreasing_generation() {
    assert!(EventScheduler::new(0, vec![], None).is_err());
    let mut old = EventScheduler::new(1, vec![event("test", 0)], None).unwrap();
    assert!(EventScheduler::new(1, vec![], Some(&old)).is_err());
    for bad in ["local", "name", "duplicate"] {
        let mut events = vec![event("test", 0)];
        match bad {
            "local" => events[0].local.clear(),
            "name" => events[0].name.clear(),
            _ => events.push(events[0].clone()),
        }
        assert!(EventScheduler::new(2, events, Some(&old)).is_err(), "{bad}");
    }
    let dispatch = old.next(100, 100000, civil(0), message).unwrap().unwrap();
    let mut next = EventScheduler::new(2, vec![event("test", 0)], Some(&old)).unwrap();
    assert!(!next.message_queued(&dispatch));
}

#[test]
fn same_minute_events_snapshot_before_slow_dispatch_and_keep_config_order() {
    let mut scheduler = EventScheduler::new(
        1,
        vec![event("first", 0), event("second", 0), event("third", 1)],
        None,
    )
    .unwrap();
    let first = scheduler
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    assert_eq!(first.name(), "first");
    assert!(!scheduler.complete(&first));
    assert!(scheduler.message_queued(&first));
    assert!(scheduler.complete(&first));
    let second = scheduler
        .next(160, 160000, civil(1), message)
        .unwrap()
        .unwrap();
    assert_eq!((second.name(), second.civil_time()), ("second", civil(0)));
    assert!(scheduler.message_queued(&second));
    assert!(scheduler.complete(&second));
    let third = scheduler
        .next(90, 90000, civil(0), message)
        .unwrap()
        .unwrap();
    assert_eq!((third.name(), third.civil_time()), ("third", civil(1)));
    assert!(scheduler.message_queued(&third));
    assert!(scheduler.complete(&third));
    assert!(
        scheduler
            .next(90, 90000, civil(0), message)
            .unwrap()
            .is_none()
    );
}

#[test]
fn reload_rejects_stale_dispatch_and_preserves_matching_occurrences() {
    let specs = vec![event("first", 0), event("second", 0)];
    let mut previous = EventScheduler::new(1, specs.clone(), None).unwrap();
    let first = previous
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    previous.message_queued(&first);
    previous.complete(&first);
    let second = previous
        .next(100, 100000, civil(0), message)
        .unwrap()
        .unwrap();
    let mut next = EventScheduler::new(2, specs, Some(&previous)).unwrap();
    assert!(!next.complete(&second));
    let current = next.next(160, 160000, civil(1), message).unwrap().unwrap();
    assert_eq!(current.name(), "second");
    next.message_queued(&current);
    assert!(next.complete(&current));
    assert!(next.next(160, 160000, civil(1), message).unwrap().is_none());
}

#[test]
fn impossible_render_consumes_only_failed_occurrence_and_macro_needs_no_message() {
    for (reserve, failed_minute) in [(false, 0), (true, 0), (true, 1)] {
        let first = EventSpec {
            trigger: "every 1 minute".parse().unwrap(),
            media: true,
            ..event("first", 0)
        };
        let mut scheduler = EventScheduler::new(1, vec![first, event("second", 0)], None).unwrap();
        if reserve {
            let first = scheduler
                .next(100, 100000, civil(0), message)
                .unwrap()
                .unwrap();
            assert_eq!(first.name(), "first");
        }
        assert!(
            scheduler
                .next(160, 160000, civil(failed_minute), |_, _| {
                    Err(SchedulerError::Render)
                })
                .is_err()
        );
        let second = scheduler
            .next(160, 160000, civil(failed_minute), |_, _| {
                Ok(DispatchContent {
                    message: None,
                    operation: None,
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(second.name(), "second");
        assert!(scheduler.complete(&second));
        assert!(!scheduler.complete(&second));
        let successor = scheduler
            .next(160, 160000, civil(failed_minute), message)
            .unwrap();
        assert_eq!(successor.is_some(), failed_minute == 1);
        if let Some(successor) = successor {
            assert_eq!(successor.name(), "first");
            assert_eq!(successor.civil_time(), civil(1));
            assert!(scheduler.message_queued(&successor));
            assert!(scheduler.complete(&successor));
        }
        assert!(
            scheduler
                .next(160, 160000, civil(failed_minute), message)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn reload_settles_an_already_queued_message_without_repeating_it() {
    let specs = vec![event("first", 0)];
    let mut old = EventScheduler::new(1, specs.clone(), None).unwrap();
    let dispatch = old.next(100, 100000, civil(0), message).unwrap().unwrap();
    assert!(old.message_queued(&dispatch));
    let mut replacement = EventScheduler::new(2, specs, Some(&old)).unwrap();
    assert!(!replacement.complete(&dispatch));
    assert!(
        replacement
            .next(100, 100000, civil(0), message)
            .unwrap()
            .is_none()
    );
}
