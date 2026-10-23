use super::{
    CivilTime, EventTrigger, ScheduledAction, ScheduledEvent, ScheduledWindow, Weekday,
    date_selector_valid, end_time_valid, start_time_valid, weekday_selector_valid,
};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTROYS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn event_trigger_selects_existing_and_stepped_clocks_with_optional_weekdays() {
    for (at, days, date, weekday, hour, minute, due) in [
        ("daily 12:15", None, 20, Weekday::Tuesday, 12, 15, true),
        (
            "weekly Tuesday 12:15",
            None,
            20,
            Weekday::Tuesday,
            12,
            15,
            true,
        ),
        (
            "weekly Monday 12:15",
            None,
            20,
            Weekday::Tuesday,
            12,
            15,
            false,
        ),
        (
            "once 2026-10-20 12:15",
            None,
            20,
            Weekday::Tuesday,
            12,
            15,
            true,
        ),
        (
            "once 2026-10-19 12:15",
            None,
            20,
            Weekday::Tuesday,
            12,
            15,
            false,
        ),
        (
            "every 1 minute",
            Some("Monday-Friday"),
            20,
            Weekday::Tuesday,
            12,
            7,
            true,
        ),
        (
            "every 1 minute",
            Some("Monday-Friday"),
            24,
            Weekday::Saturday,
            12,
            7,
            false,
        ),
        (
            "daily 12:15",
            Some("Monday,Wednesday"),
            20,
            Weekday::Tuesday,
            12,
            15,
            false,
        ),
        (
            "daily 12:15",
            Some("Monday,Wednesday"),
            21,
            Weekday::Wednesday,
            12,
            15,
            true,
        ),
        ("every 15 minute", None, 20, Weekday::Tuesday, 12, 15, true),
        ("every 1 hours", None, 20, Weekday::Tuesday, 12, 0, true),
        ("every 24 hour", None, 20, Weekday::Tuesday, 0, 0, true),
        ("every 7 minutes", None, 20, Weekday::Tuesday, 12, 56, true),
        ("every 7 minutes", None, 20, Weekday::Tuesday, 13, 0, true),
    ] {
        let trigger = EventTrigger::parse(at, days).unwrap();
        let local = CivilTime::new(2026, 10, date, weekday, hour, minute).unwrap();
        assert_eq!(trigger.occurrence(&local).is_some(), due, "{at}, {days:?}");
        assert_eq!(trigger, trigger.clone());
    }
}

#[test]
fn event_trigger_rejects_unsupported_steps_and_day_selectors() {
    for at in [
        "",
        "every",
        "every 0 minutes",
        "every 61 minutes",
        "every 0 hours",
        "every 25 hours",
        "every -1 minute",
        "every 1.5 hours",
        "every 256 minutes",
        "every 1 day",
        "every 1 minutes extra",
        "daily 24:00",
    ] {
        assert!(EventTrigger::parse(at, None).is_err(), "{at}");
    }
    for days in ["", "Funday", "Monday\0Tuesday"] {
        assert!(
            EventTrigger::parse("daily 12:00", Some(days)).is_err(),
            "{days}"
        );
    }
}

unsafe extern "C" fn destroy_fault_handle(_: *mut std::ffi::c_void) {
    DESTROYS.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn create_fault_handle(
    _: u32,
    _: *const std::ffi::c_char,
    _: u32,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
    out: *mut *mut std::ffi::c_void,
) -> i32 {
    unsafe { *out = std::ptr::NonNull::<u8>::dangling().cast().as_ptr() };
    -1
}

unsafe extern "C" fn fail_equal(
    _: *const std::ffi::c_void,
    _: *const std::ffi::c_void,
    _: *mut u32,
) -> i32 {
    -2
}

unsafe extern "C" fn fail_elapsed(
    _: *const std::ffi::c_void,
    _: *const super::ScheduleCivil,
    _: u8,
    _: *mut u32,
    _: *mut u64,
) -> i32 {
    -2
}

unsafe extern "C" fn fail_boundary(
    _: *const std::ffi::c_void,
    _: *const super::ScheduleCivil,
    _: u8,
    _: *mut u32,
    _: *mut u64,
    _: *mut u64,
) -> i32 {
    -2
}

#[test]
fn scheduled_actions_accept_only_the_controller_vocabulary() {
    let cases = [
        ("connect", ScheduledAction::Connect),
        ("disconnect", ScheduledAction::Disconnect),
        ("disconnect_all", ScheduledAction::DisconnectAll),
        ("reconnect_all", ScheduledAction::ReconnectAll),
    ];
    for (text, action) in cases {
        assert_eq!(ScheduledAction::from_str(text), Ok(action));
    }
    for text in ["", "/bin/sh", "connect; rm"] {
        assert!(ScheduledAction::from_str(text).is_err());
    }
}

#[test]
fn released_schedule_descriptor_is_required_before_configuration() {
    assert!(super::provider_ready().is_ok());
    assert!(unsafe { super::validate_descriptor(std::ptr::null()) }.is_err());
    let descriptor = unsafe { &*super::rptadv_schedule_descriptor() };
    let mut bad = *descriptor;
    bad.struct_size = 0;
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    bad = *descriptor;
    bad.abi_version = 2;
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    bad = *descriptor;
    bad.capability_name = std::ptr::null();
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    bad = *descriptor;
    bad.capability_name = c"other.schedule".as_ptr();
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    let missing: [fn(&mut super::ScheduleDescriptor); 16] = [
        |item| item.civil_new = None,
        |item| item.event_parse = None,
        |item| item.event_occurrence = None,
        |item| item.start_time_valid = None,
        |item| item.end_time_valid = None,
        |item| item.weekday_selector_valid = None,
        |item| item.date_selector_valid = None,
        |item| item.window_create = None,
        |item| item.window_destroy = None,
        |item| item.window_clone = None,
        |item| item.window_equal = None,
        |item| item.window_matches = None,
        |item| item.window_overlaps = None,
        |item| item.window_elapsed_after_end = None,
        |item| item.window_next_start = None,
        |item| item.window_next_end = None,
    ];
    for remove in missing {
        bad = *descriptor;
        remove(&mut bad);
        assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    }
}

#[test]
fn foreign_constructor_statuses_distinguish_invalid_input_from_contract_failure() {
    for (status, expected) in [(0, Ok(())), (-1, Err(super::ScheduleError::Invalid))] {
        assert_eq!(super::input_status(status), expected);
    }
    for status in [-2, 1] {
        assert!(std::panic::catch_unwind(|| super::input_status(status)).is_err());
    }
}

#[test]
fn released_records_keep_civil_components_and_public_invalid_events_absent() {
    for (index, weekday) in [
        Weekday::Sunday,
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(Weekday::from_index(index as u8), Some(weekday));
    }
    assert_eq!(Weekday::from_index(7), None);
    assert_eq!(
        ScheduledEvent::from_record(super::ScheduleEventRecord {
            kind: 99,
            ..Default::default()
        }),
        Err(super::ScheduleError::Invalid)
    );
    let local = CivilTime::new(2028, 2, 29, Weekday::Tuesday, 23, 59).unwrap();
    assert_eq!(local.components(), (2028, 2, 29, Weekday::Tuesday, 23, 59));
    assert_eq!(
        ScheduledEvent::Weekly {
            weekday: Weekday::Tuesday,
            hour: 23,
            minute: 59,
        }
        .occurrence(&local),
        Some(1_214_988_479)
    );
    assert_eq!(
        ScheduledEvent::from_str("once 2028-02-29 23:59")
            .unwrap()
            .occurrence(&local),
        Some(1_214_988_479)
    );
    assert_eq!(
        ScheduledEvent::Daily {
            hour: 24,
            minute: 0
        }
        .occurrence(&local),
        None
    );
    assert_eq!(
        ScheduledEvent::Once {
            year: 2028,
            month: 2,
            day: 30,
            hour: 23,
            minute: 59
        }
        .occurrence(&local),
        None
    );
    assert_eq!(
        CivilTime::new(0, 1, 1, Weekday::Sunday, 0, 0),
        Err(super::ScheduleError::Invalid)
    );
    assert_eq!(
        CivilTime::new(10_000, 1, 1, Weekday::Sunday, 0, 0),
        Err(super::ScheduleError::Invalid)
    );
}

#[test]
fn released_window_preserves_owned_equality_and_boundary_presence() {
    let source = ScheduledWindow::parse(None, Some("2028-02-29"), "23:59", "24:00").unwrap();
    assert_eq!(format!("{source:?}"), "ScheduledWindow { .. }");
    let cloned = source.clone();
    assert_eq!(source, cloned);
    drop(source);
    let local = CivilTime::new(2028, 2, 29, Weekday::Tuesday, 23, 59).unwrap();
    assert!(cloned.matches(&local));
    assert_eq!(cloned.next_end(&local, 59), Some((740_405, 1_000)));
    assert_eq!(cloned.next_end(&local, 60), None);
    assert_eq!(cloned.elapsed_after_end(&local, 60), None);
    assert_eq!(
        ScheduledWindow::parse(None, None, "23:59", "24:00").unwrap(),
        ScheduledWindow::parse(Some(""), Some(""), "23:59", "24:00").unwrap()
    );
}

#[test]
fn owned_window_contract_errors_remain_visible_and_failed_create_cleans_up() {
    let mut api = unsafe { *super::rptadv_schedule_descriptor() };
    api.window_create = Some(create_fault_handle);
    api.window_destroy = Some(destroy_fault_handle);
    let api = Box::leak(Box::new(api));
    DESTROYS.store(0, Ordering::SeqCst);
    assert!(ScheduledWindow::parse_with_api(api, None, None, "12:00", "13:00").is_err());
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);

    let mut api = *api;
    api.window_equal = Some(fail_equal);
    api.window_elapsed_after_end = Some(fail_elapsed);
    api.window_next_start = Some(fail_boundary);
    api.window_next_end = Some(fail_boundary);
    let api = Box::leak(Box::new(api));
    let window = ScheduledWindow {
        api,
        handle: std::ptr::NonNull::<u8>::dangling().cast(),
    };
    let local = CivilTime::new(2028, 2, 29, Weekday::Tuesday, 23, 59).unwrap();
    for case in 0..4 {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match case {
                0 => {
                    let _ = window == window;
                }
                1 => {
                    let _ = window.elapsed_after_end(&local, 0);
                }
                2 => {
                    let _ = window.next_start(&local, 0);
                }
                _ => {
                    let _ = window.next_end(&local, 0);
                }
            }))
            .is_err()
        );
    }
    drop(window);
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 2);
}

#[test]
fn released_parser_rejects_nul_and_preserves_clock_selector_boundaries() {
    assert!(!start_time_valid("24:00"));
    assert!(end_time_valid("24:00"));
    assert!(weekday_selector_valid("Sunday-Monday"));
    assert!(date_selector_valid("2028-02-29"));
    assert!(!date_selector_valid("2027-02-29"));
    assert!(!weekday_selector_valid("Sunday\0Monday"));
    assert!(ScheduledEvent::from_str("daily 12:00\0").is_err());
    assert!(ScheduledWindow::parse(Some("Monday\0"), None, "12:00", "13:00").is_err());
}
