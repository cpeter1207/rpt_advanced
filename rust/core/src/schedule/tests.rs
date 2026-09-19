use super::{
    CivilTime, ScheduledAction, ScheduledEvent, ScheduledWindow, Weekday, end_time_valid,
    start_time_valid,
};
use std::str::FromStr;

#[test]
fn low_ascii_digits_are_rejected_without_panicking() {
    for text in ["daily -1:00", "daily 12:/0", "once /026-01-01 00:00"] {
        assert!(ScheduledEvent::from_str(text).is_err(), "{text}");
    }
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
fn events_parse_strictly_and_match_a_stable_civil_minute() {
    let daily = ScheduledEvent::from_str("daily 13:07").unwrap();
    let weekly = ScheduledEvent::from_str("weekly Wednesday 13:07").unwrap();
    let once = ScheduledEvent::from_str("once 2028-02-29 23:59").unwrap();
    let now = CivilTime::new(2026, 9, 9, Weekday::Wednesday, 13, 7).unwrap();

    assert_eq!(daily.occurrence(&now), daily.occurrence(&now));
    assert!(weekly.occurrence(&now).is_some());
    assert!(
        once.occurrence(&CivilTime::new(2028, 2, 29, Weekday::Tuesday, 23, 59).unwrap())
            .is_some()
    );
    for text in ["daily 24:00", "weekly Wed 13:07", "once 2027-02-29 12:00"] {
        assert!(ScheduledEvent::from_str(text).is_err());
    }
}

#[test]
fn windows_accept_midnight_only_as_an_exclusive_end() {
    assert!(start_time_valid("23:59"));
    assert!(!start_time_valid("24:00"));
    assert!(end_time_valid("24:00"));
    assert!(!end_time_valid("24:01"));

    let window = ScheduledWindow::parse(Some("Monday"), None, "23:00", "24:00").unwrap();
    assert!(window.matches(&CivilTime::new(2026, 9, 14, Weekday::Monday, 23, 59).unwrap()));
    assert_eq!(
        window.elapsed_after_end(
            &CivilTime::new(2026, 9, 14, Weekday::Monday, 23, 59).unwrap(),
            0
        ),
        None
    );
    let midnight = CivilTime::new(2026, 9, 15, Weekday::Tuesday, 0, 0).unwrap();
    assert!(!window.matches(&midnight));
    assert_eq!(window.elapsed_after_end(&midnight, 0), Some(0));

    assert!(ScheduledWindow::parse(None, None, "24:00", "12:00").is_err());
    assert!(ScheduledWindow::parse(None, None, "11:00", "11:00").is_err());
}

#[test]
fn midnight_end_elapsed_time_crosses_calendar_boundaries() {
    let cases = [
        (
            Some("Monday"),
            None,
            CivilTime::new(2026, 9, 15, Weekday::Tuesday, 0, 5).unwrap(),
        ),
        (
            None,
            Some("2026-04-30"),
            CivilTime::new(2026, 5, 1, Weekday::Friday, 0, 5).unwrap(),
        ),
        (
            None,
            Some("2026-12-31"),
            CivilTime::new(2027, 1, 1, Weekday::Friday, 0, 5).unwrap(),
        ),
        (
            None,
            Some("2028-02-29"),
            CivilTime::new(2028, 3, 1, Weekday::Wednesday, 0, 5).unwrap(),
        ),
    ];
    for (days, dates, current) in cases {
        let window = ScheduledWindow::parse(days, dates, "23:00", "24:00").unwrap();
        assert_eq!(window.elapsed_after_end(&current, 7), Some(307_000));
    }
}

#[test]
fn previous_calendar_day_handles_each_weekday_and_the_minimum_year() {
    use super::previous_civil_day;
    for (weekday, previous) in [
        (Weekday::Sunday, Weekday::Saturday),
        (Weekday::Monday, Weekday::Sunday),
        (Weekday::Tuesday, Weekday::Monday),
        (Weekday::Wednesday, Weekday::Tuesday),
        (Weekday::Thursday, Weekday::Wednesday),
        (Weekday::Friday, Weekday::Thursday),
        (Weekday::Saturday, Weekday::Friday),
    ] {
        let day = CivilTime::new(2026, 9, 15, weekday, 0, 0).unwrap();
        assert_eq!(
            previous_civil_day(day),
            Some(CivilTime::new(2026, 9, 14, previous, 23, 59).unwrap())
        );
    }
    let minimum = CivilTime::new(1, 1, 1, Weekday::Monday, 0, 0).unwrap();
    assert_eq!(previous_civil_day(minimum), None);
}

#[test]
fn civil_time_rejects_years_outside_the_c_parser_range() {
    assert!(CivilTime::new(9_999, 12, 31, Weekday::Thursday, 23, 59).is_ok());
    assert!(CivilTime::new(10_000, 1, 1, Weekday::Friday, 0, 0).is_err());
}

#[test]
fn windows_support_date_selectors_and_inclusive_start_exclusive_end() {
    let window =
        ScheduledWindow::parse(None, Some("2026-09-09, 2028-02-29"), "11:00", "12:00").unwrap();
    assert!(window.matches(&CivilTime::new(2026, 9, 9, Weekday::Wednesday, 11, 0).unwrap()));
    assert!(!window.matches(&CivilTime::new(2026, 9, 9, Weekday::Wednesday, 12, 0).unwrap()));
    assert!(ScheduledWindow::parse(Some("Monday"), Some("2026-09-09"), "11:00", "12:00").is_err());
}

#[test]
fn windows_accept_case_insensitive_wrapping_weekday_ranges_and_reject_malformed_selectors() {
    let window = ScheduledWindow::parse(Some(" Friday - Monday "), None, "00:00", "00:01").unwrap();
    for weekday in [
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
        Weekday::Monday,
    ] {
        assert!(window.matches(&CivilTime::new(2026, 9, 7, weekday, 0, 0).unwrap()));
    }
    for days in [
        "Mon",
        "Monday Tuesday",
        "Monday-",
        ",Monday",
        "Monday,,Friday",
    ] {
        assert!(ScheduledWindow::parse(Some(days), None, "11:00", "12:00").is_err());
    }
}

#[test]
fn events_and_windows_reject_non_gregorian_dates_and_malformed_civil_clocks() {
    for event in [
        "daily 7:05",
        "daily 00:60",
        "weekly Monday 24:00",
        "once 0000-01-01 12:00",
        "once 2100-02-29 12:00",
        "once 2026-01-01 12:00 UTC",
    ] {
        assert!(ScheduledEvent::from_str(event).is_err());
    }
    for (start, end) in [
        ("1:00", "12:00"),
        ("11:60", "12:00"),
        ("11:00", "24:01"),
        ("12:00", "12:00"),
        ("12:01", "12:00"),
    ] {
        assert!(ScheduledWindow::parse(None, None, start, end).is_err());
    }
}

#[test]
fn immutable_windows_validate_selectors_once_and_match_every_weekday_boundary() {
    for (name, weekday) in [
        ("Sunday", Weekday::Sunday),
        ("Monday", Weekday::Monday),
        ("Tuesday", Weekday::Tuesday),
        ("Wednesday", Weekday::Wednesday),
        ("Thursday", Weekday::Thursday),
        ("Friday", Weekday::Friday),
        ("Saturday", Weekday::Saturday),
    ] {
        let window = ScheduledWindow::parse(Some(name), None, "00:00", "23:59").unwrap();
        let start = CivilTime::new(2026, 9, 15, weekday, 0, 0).unwrap();
        let end = CivilTime::new(2026, 9, 15, weekday, 23, 59).unwrap();
        assert!(window.matches(&start));
        assert!(!window.matches(&end));
        assert_eq!(window.elapsed_after_end(&start, 0), None);
        assert_eq!(window.elapsed_after_end(&end, 59), Some(59000));
        assert_eq!(window.elapsed_after_end(&end, 60), None);
    }
    let dates = std::iter::repeat_n("2000-02-29", 64)
        .collect::<Vec<_>>()
        .join(",");
    assert!(ScheduledWindow::parse(None, Some(&dates), "00:00", "23:59").is_ok());
    assert!(
        ScheduledWindow::parse(None, Some(&format!("{dates},2000-02-29")), "00:00", "23:59")
            .is_err()
    );
    for text in [
        "",
        "weekly Monday",
        "once 2000-02-29",
        "once 2000/02-29 12:00",
        "once 2000-02/29 12:00",
        "once 2000-00-01 12:00",
        "once 2000-13-01 12:00",
        "once 2000-01-00 12:00",
    ] {
        assert!(ScheduledEvent::from_str(text).is_err(), "{text}");
    }
    assert!(ScheduledWindow::parse(Some("Monday-Tuesday-Friday"), None, "11:00", "12:00").is_err());
}
#[test]
fn civil_validation_and_occurrence_comparison_check_each_calendar_component() {
    assert!(CivilTime::new(2026, 9, 15, Weekday::Tuesday, 24, 0).is_err());
    assert!(CivilTime::new(2026, 9, 15, Weekday::Tuesday, 0, 60).is_err());
    for bad in [
        "daily 09;07",
        "once 2026/09-15 09:07",
        "once 2026-09/15 09:07",
        "once 2026-9-15 09:07",
    ] {
        assert!(bad.parse::<ScheduledEvent>().is_err());
    }
    let daily: ScheduledEvent = "daily 09:07".parse().unwrap();
    let weekly: ScheduledEvent = "weekly Tuesday 09:07".parse().unwrap();
    let once: ScheduledEvent = "once 2026-09-15 09:07".parse().unwrap();
    for (year, month, day, weekday, hour, minute, expected) in [
        (2025, 9, 15, Weekday::Tuesday, 9, 7, [true, true, false]),
        (2026, 8, 15, Weekday::Tuesday, 9, 7, [true, true, false]),
        (2026, 9, 14, Weekday::Tuesday, 9, 7, [true, true, false]),
        (2026, 9, 15, Weekday::Monday, 9, 7, [true, false, true]),
        (2026, 9, 15, Weekday::Tuesday, 8, 7, [false, false, false]),
        (2026, 9, 15, Weekday::Tuesday, 9, 8, [false, false, false]),
        (2026, 9, 15, Weekday::Tuesday, 9, 7, [true, true, true]),
    ] {
        let time = CivilTime::new(year, month, day, weekday, hour, minute).unwrap();
        for (event, expected) in [daily, weekly, once].iter().zip(expected) {
            assert_eq!(event.occurrence(&time).is_some(), expected);
        }
    }
    let dates = ScheduledWindow::parse(None, Some("2026-09-15"), "09:00", "10:00").unwrap();
    let weekday = ScheduledWindow::parse(Some("Tuesday"), None, "09:00", "10:00").unwrap();
    for (year, month, day, selected_day, date_match, day_match) in [
        (2025, 9, 15, Weekday::Tuesday, false, true),
        (2026, 8, 15, Weekday::Tuesday, false, true),
        (2026, 9, 14, Weekday::Tuesday, false, true),
        (2026, 9, 15, Weekday::Monday, true, false),
    ] {
        let time = CivilTime::new(year, month, day, selected_day, 9, 7).unwrap();
        assert_eq!(dates.matches(&time), date_match);
        assert_eq!(weekday.matches(&time), day_match);
    }
}
