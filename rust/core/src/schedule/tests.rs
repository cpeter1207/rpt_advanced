use super::{
    CivilTime, ScheduledAction, ScheduledEvent, ScheduledWindow, Weekday, date_selector_valid,
    end_time_valid, start_time_valid, weekday_selector_valid,
};
use std::str::FromStr;

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
    bad.capability_name = c"other.schedule".as_ptr();
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    bad = *descriptor;
    bad.window_create = None;
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
    bad = *descriptor;
    bad.event_occurrence = None;
    assert!(unsafe { super::validate_descriptor(&bad) }.is_err());
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
    let local = CivilTime::new(2028, 2, 29, Weekday::Tuesday, 23, 59).unwrap();
    assert_eq!(local.components(), (2028, 2, 29, Weekday::Tuesday, 23, 59));
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
