use super::{
    messages::MessageCatalog,
    time::{TimeAnnouncement, TimeAnnouncementError, TimeFormat},
};

#[test]
fn announcement_accessors_expose_all_localized_forms() {
    let catalog =
        MessageCatalog::from_sources("en-US", include_str!("../../../messages/en-US.ftl"), None)
            .unwrap();
    let announcement = TimeAnnouncement::format(0, 5, TimeFormat::TwelveHour, &catalog).unwrap();
    assert!(announcement.text().contains("12:05 AM"));
    assert!(announcement.speech().contains("Good Morning"));
    assert_eq!(announcement.morse(), "12:05 AM");
}

#[test]
fn time_formatting_covers_clock_boundaries_and_readable_errors() {
    let catalog =
        MessageCatalog::from_sources("en-US", include_str!("../../../messages/en-US.ftl"), None)
            .unwrap();
    let midnight = TimeAnnouncement::format(0, 0, TimeFormat::TwentyFourHour, &catalog).unwrap();
    assert_eq!(midnight.morse(), "00:00");
    let afternoon = TimeAnnouncement::format(12, 0, TimeFormat::TwelveHour, &catalog).unwrap();
    assert!(afternoon.speech().contains("Good Afternoon"));
    let evening = TimeAnnouncement::format(17, 0, TimeFormat::TwelveHour, &catalog).unwrap();
    assert!(evening.speech().contains("Good Evening"));

    assert_eq!(TimeFormat::try_from(12), Ok(TimeFormat::TwelveHour));
    assert_eq!(TimeFormat::try_from(24), Ok(TimeFormat::TwentyFourHour));
    let format_error = TimeFormat::try_from(8).unwrap_err();
    assert_eq!(format_error.to_string(), "invalid time announcement format");
    let hour_error = TimeAnnouncement::format(-1, 0, TimeFormat::TwelveHour, &catalog).unwrap_err();
    assert_eq!(hour_error.to_string(), "invalid civil time");
    assert_eq!(
        TimeAnnouncement::format(10, 60, TimeFormat::TwentyFourHour, &catalog),
        Err(TimeAnnouncementError::InvalidTime)
    );
}
