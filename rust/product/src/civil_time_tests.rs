//! ADR 0017 civil-clock contracts, using isolated libc timezone conversions.

use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::{
        link_schedule::{LinkScheduler, LinkTransition, ReplacementSpec, RouteSpec},
        scheduler::{DispatchContent, EventScheduler, EventSpec},
    },
    schedule::{CivilTime, ScheduledWindow, Weekday},
};
use std::{collections::BTreeSet, process::Command as ProcessCommand};

const TIMEZONE: &str = "CST6CDT,M3.2.0/2,M11.1.0/2";
const CHILD: &str = "RPTADV_CIVIL_TIME_CHILD";

// Each test starts its own executable with TZ already set, avoiding process-global
// timezone mutation and races with the rest of the parallel product test suite.
fn in_timezone(name: &str, test: impl FnOnce()) {
    if std::env::var(CHILD).as_deref() == Ok(name) {
        assert_eq!(std::env::var("TZ").unwrap(), TIMEZONE);
        test();
        return;
    }
    let output = ProcessCommand::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("civil_time_tests::{name}"),
            "--nocapture",
        ])
        .env("TZ", TIMEZONE)
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "timezone child failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "the exact child test must have executed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[derive(Clone, Copy)]
struct Stamp {
    wall: i64,
    civil: CivilTime,
    second: u8,
}

fn stamp(wall: i64, date: (u16, u8, u8), hour: u8, minute: u8, isdst: i32) -> Stamp {
    let timestamp: libc::time_t = wall;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    assert!(!unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null());
    assert_eq!(
        (
            local.tm_year + 1900,
            local.tm_mon + 1,
            local.tm_mday,
            local.tm_wday,
            local.tm_hour,
            local.tm_min,
            local.tm_sec,
            local.tm_isdst,
        ),
        (
            i32::from(date.0),
            i32::from(date.1),
            i32::from(date.2),
            0,
            i32::from(hour),
            i32::from(minute),
            0,
            isdst,
        ),
        "UTC timestamp {wall} must produce the hand-checked Sunday civil fixture"
    );
    Stamp {
        wall,
        civil: CivilTime::new(
            (local.tm_year + 1900).try_into().unwrap(),
            (local.tm_mon + 1).try_into().unwrap(),
            local.tm_mday.try_into().unwrap(),
            Weekday::Sunday,
            local.tm_hour.try_into().unwrap(),
            local.tm_min.try_into().unwrap(),
        )
        .unwrap(),
        second: local.tm_sec.try_into().unwrap(),
    }
}

fn fall(wall: i64, hour: u8, minute: u8, isdst: i32) -> Stamp {
    stamp(wall, (2026, 11, 1), hour, minute, isdst)
}

fn spring(wall: i64, hour: u8, minute: u8, isdst: i32) -> Stamp {
    stamp(wall, (2026, 3, 8), hour, minute, isdst)
}

fn event(name: &str, trigger: &str) -> EventSpec {
    EventSpec {
        local: "524950".into(),
        name: name.into(),
        trigger: trigger.parse().unwrap(),
    }
}

fn fired(scheduler: &mut EventScheduler, time: Stamp) -> Vec<String> {
    let mut names = Vec::new();
    while let Some(dispatch) = scheduler
        .next(time.wall, time.civil, |_, _| {
            Ok(DispatchContent {
                message: None,
                operation: None,
            })
        })
        .unwrap()
    {
        assert!(names.len() < 16, "one captured minute must settle finitely");
        assert_eq!(dispatch.civil_time(), time.civil);
        names.push(dispatch.name().to_owned());
        assert!(scheduler.complete(&dispatch));
    }
    names
}

// Real scheduler reservations drive an exact owned-route snapshot. Only the
// external transport's successful attach/detach side effect is represented here.
struct Links {
    scheduler: LinkScheduler,
    owned: BTreeSet<String>,
}

impl Links {
    fn new(start: &str, end: &str, inactivity: u64) -> Self {
        let routes = [("2000", true), ("3000", false)]
            .into_iter()
            .map(|(remote, permanent)| RouteSpec {
                local: "524950".into(),
                remote: remote.into(),
                permanent,
                group_label: None,
                group_name: None,
                group_priority: None,
            })
            .collect();
        let window = ReplacementSpec {
            routes: vec![1],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, start, end).unwrap(),
            end_inactivity_ms: inactivity,
            warning_before_start_ms: vec![],
            warning_before_end_ms: vec![],
            warning_message_id: None,
        };
        Self {
            scheduler: LinkScheduler::new(1, routes, vec![window], None).unwrap(),
            owned: BTreeSet::new(),
        }
    }

    fn tick(&mut self, time: Stamp, now_ms: u64, activity: Option<u64>) -> Vec<String> {
        self.scheduler.tick(
            time.civil,
            time.second,
            now_ms,
            |_| activity,
            |route| self.owned.contains(&route.remote),
        );
        let mut effects = Vec::new();
        while let Some(operation) = self.scheduler.next_operation() {
            assert!(effects.len() < 4, "route transitions must settle finitely");
            assert!(self.scheduler.current(&operation));
            let remote = operation.remote().to_owned();
            let action = match operation.action() {
                LinkTransition::Attach => {
                    assert!(self.owned.insert(remote.clone()));
                    "attach"
                }
                LinkTransition::Detach => {
                    assert!(self.owned.remove(&remote));
                    "detach"
                }
            };
            effects.push(format!("{action} {remote}"));
            assert!(self.scheduler.complete(&operation, true));
        }
        effects
    }
}

#[test]
fn fall_back_events_fire_once_per_repeated_civil_minute() {
    in_timezone(
        "fall_back_events_fire_once_per_repeated_civil_minute",
        || {
            // Break caught: including physical DST-hour identity in event occurrence keys.
            let mut scheduler = EventScheduler::new(
                1,
                vec![
                    event("daily", "daily 01:30"),
                    event("weekly", "weekly Sunday 01:30"),
                    event("once", "once 2026-11-01 01:30"),
                ],
                None,
            )
            .unwrap();
            // 06:30Z and 07:30Z are distinct physical occurrences of 01:30.
            assert_eq!(
                fired(&mut scheduler, fall(1_793_514_600, 1, 30, 1)),
                ["daily", "weekly", "once"]
            );
            for time in [
                fall(1_793_516_340, 1, 59, 1),
                fall(1_793_516_400, 1, 0, 0),
                fall(1_793_518_200, 1, 30, 0),
            ] {
                assert!(fired(&mut scheduler, time).is_empty());
            }
        },
    );
}

#[test]
fn spring_forward_skips_missing_event_minute_without_catch_up() {
    in_timezone(
        "spring_forward_skips_missing_event_minute_without_catch_up",
        || {
            // Break caught: replaying events between the previous and current civil minute.
            let mut scheduler = EventScheduler::new(
                1,
                vec![
                    event("daily-gap", "daily 02:30"),
                    event("weekly-gap", "weekly Sunday 02:30"),
                    event("once-gap", "once 2026-03-08 02:30"),
                    event("after-gap", "daily 03:00"),
                ],
                None,
            )
            .unwrap();
            assert!(fired(&mut scheduler, spring(1_772_956_740, 1, 59, 0)).is_empty());
            assert_eq!(
                fired(&mut scheduler, spring(1_772_956_800, 3, 0, 1)),
                ["after-gap"]
            );
            assert!(fired(&mut scheduler, spring(1_772_958_600, 3, 30, 1)).is_empty());
        },
    );
}

#[test]
fn partial_window_opens_and_closes_in_both_fall_back_hours() {
    in_timezone(
        "partial_window_opens_and_closes_in_both_fall_back_hours",
        || {
            // Break caught: de-duplicating window membership like a zero-time event.
            let mut links = Links::new("01:15", "01:45", 0);
            for (wall, minute, isdst, expected) in [
                (1_793_513_640, 14, 1, vec!["attach 2000"]),
                (1_793_513_700, 15, 1, vec!["detach 2000", "attach 3000"]),
                (1_793_515_500, 45, 1, vec!["detach 3000", "attach 2000"]),
                (1_793_517_240, 14, 0, vec![]),
                (1_793_517_300, 15, 0, vec!["detach 2000", "attach 3000"]),
                (1_793_519_100, 45, 0, vec!["detach 3000", "attach 2000"]),
            ] {
                assert_eq!(links.tick(fall(wall, 1, minute, isdst), 0, None), expected);
            }
        },
    );
}

#[test]
fn spring_forward_gap_window_never_opens() {
    in_timezone("spring_forward_gap_window_never_opens", || {
        // Break caught: applying a skipped window while catching up a civil jump.
        let mut links = Links::new("02:15", "02:45", 0);
        assert_eq!(
            links.tick(spring(1_772_956_740, 1, 59, 0), 0, None),
            ["attach 2000"]
        );
        assert!(
            links
                .tick(spring(1_772_956_800, 3, 0, 1), 1_000, None)
                .is_empty()
        );
        assert!(
            links
                .tick(spring(1_772_958_600, 3, 30, 1), 2_000, None)
                .is_empty()
        );
    });
}

#[test]
fn wall_clock_steps_skip_events_and_immediately_follow_window_membership() {
    in_timezone(
        "wall_clock_steps_skip_events_and_immediately_follow_window_membership",
        || {
            // Break caught: catch-up events, lowering the event high-water mark, or
            // making current window membership depend on that event high-water mark.
            let mut events = EventScheduler::new(
                1,
                vec![
                    event("backward", "daily 01:15"),
                    event("skipped", "daily 01:30"),
                    event("landing", "daily 01:45"),
                    event("new-territory", "daily 01:46"),
                ],
                None,
            )
            .unwrap();
            let mut links = Links::new("01:15", "01:40", 0);
            for (index, (wall, minute, expected_events, expected_links)) in [
                (1_793_513_640, 14, vec![], vec!["attach 2000"]),
                (1_793_515_500, 45, vec!["landing"], vec![]),
                (
                    1_793_513_700,
                    15,
                    vec![],
                    vec!["detach 2000", "attach 3000"],
                ),
                (1_793_514_600, 30, vec![], vec![]),
                (
                    1_793_515_500,
                    45,
                    vec![],
                    vec!["detach 3000", "attach 2000"],
                ),
                (1_793_515_560, 46, vec!["new-territory"], vec![]),
            ]
            .into_iter()
            .enumerate()
            {
                let time = fall(wall, 1, minute, 1);
                assert_eq!(fired(&mut events, time), expected_events);
                assert_eq!(links.tick(time, index as u64 * 1_000, None), expected_links);
            }
        },
    );
}

#[test]
fn receive_inactivity_uses_monotonic_elapsed_time_despite_wall_steps() {
    in_timezone(
        "receive_inactivity_uses_monotonic_elapsed_time_despite_wall_steps",
        || {
            // Break caught: wall-clock elapsed arithmetic or treating activity at zero as absent.
            let mut links = Links::new("01:15", "01:45", 60_000);
            assert_eq!(
                links.tick(fall(1_793_514_600, 1, 30, 1), 0, Some(0)),
                ["attach 3000"]
            );
            // Jump beyond the end, then backwards while remaining outside the window.
            assert!(
                links
                    .tick(fall(1_793_521_800, 2, 30, 0), 1_000, Some(0))
                    .is_empty()
            );
            assert!(
                links
                    .tick(fall(1_793_515_500, 1, 45, 1), 59_999, Some(0))
                    .is_empty()
            );
            assert_eq!(
                links.tick(fall(1_793_521_740, 2, 29, 0), 60_000, Some(0)),
                ["detach 3000", "attach 2000"]
            );
        },
    );
}

#[test]
fn reload_preserves_event_identity_but_new_identity_and_instance_are_eligible() {
    in_timezone(
        "reload_preserves_event_identity_but_new_identity_and_instance_are_eligible",
        || {
            // Break caught: using rendered content as identity, ignoring an identity
            // component, or retaining a once event outside the running scheduler instance.
            let first = fall(1_793_514_600, 1, 30, 1);
            let repeated = fall(1_793_518_200, 1, 30, 0);
            for trigger in [
                "daily 01:30",
                "weekly Sunday 01:30",
                "once 2026-11-01 01:30",
            ] {
                let spec = event("identity", trigger);
                let mut previous = EventScheduler::new(1, vec![spec.clone()], None).unwrap();
                let original = previous
                    .next(first.wall, first.civil, |_, _| {
                        Ok(DispatchContent {
                            message: Some("original text".into()),
                            operation: Some(Command {
                                action: LinkAction::DisconnectAll,
                                node: String::new(),
                            }),
                        })
                    })
                    .unwrap()
                    .unwrap();
                assert!(previous.message_queued(&original));
                assert!(previous.complete(&original));
                let mut unchanged = EventScheduler::new(2, vec![spec], Some(&previous)).unwrap();
                // Changing the renderer's message and macro does not create an occurrence.
                assert!(
                    unchanged
                        .next(repeated.wall, repeated.civil, |_, _| Ok(DispatchContent {
                            message: Some("changed text".into()),
                            operation: Some(Command {
                                action: LinkAction::ReconnectAll,
                                node: String::new()
                            }),
                        }))
                        .unwrap()
                        .is_none()
                );
            }

            let original = event("identity", "once 2026-11-01 01:30");
            let mut previous = EventScheduler::new(1, vec![original.clone()], None).unwrap();
            assert_eq!(fired(&mut previous, first), ["identity"]);
            for (component, local, name, trigger, expected) in [
                (
                    "node",
                    "1000",
                    "identity",
                    "once 2026-11-01 01:30",
                    "identity",
                ),
                (
                    "label",
                    "524950",
                    "renamed",
                    "once 2026-11-01 01:30",
                    "renamed",
                ),
                ("trigger", "524950", "identity", "daily 01:30", "identity"),
            ] {
                let mut changed = event(name, trigger);
                changed.local = local.into();
                let mut replacement =
                    EventScheduler::new(2, vec![changed], Some(&previous)).unwrap();
                assert_eq!(fired(&mut replacement, repeated), [expected], "{component}");
            }
            let mut restarted = EventScheduler::new(1, vec![original], None).unwrap();
            assert_eq!(fired(&mut restarted, repeated), ["identity"]);
        },
    );
}
