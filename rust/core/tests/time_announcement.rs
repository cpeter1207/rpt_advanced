use rpt_advanced_core::time::{TimeAnnouncement, TimeAnnouncementError, TimeFormat};

#[test]
fn formats_greeting_boundaries_in_both_clock_modes() {
    let cases = [
        (
            0,
            5,
            TimeFormat::TwelveHour,
            "Good Morning. The time is 12:05 AM.",
            "12:05 AM",
        ),
        (
            11,
            59,
            TimeFormat::TwelveHour,
            "Good Morning. The time is 11:59 AM.",
            "11:59 AM",
        ),
        (
            12,
            0,
            TimeFormat::TwelveHour,
            "Good Afternoon. The time is 12:00 PM.",
            "12:00 PM",
        ),
        (
            13,
            1,
            TimeFormat::TwelveHour,
            "Good Afternoon. The time is 1:01 PM.",
            "1:01 PM",
        ),
        (
            17,
            9,
            TimeFormat::TwentyFourHour,
            "Good Evening. The time is 17:09.",
            "17:09",
        ),
    ];

    for (hour, minute, format, speech, morse) in cases {
        let announcement = TimeAnnouncement::format(hour, minute, format).unwrap();
        assert_eq!(announcement.speech(), speech);
        assert_eq!(announcement.morse(), morse);
    }
}

#[test]
fn rejects_out_of_range_civil_time() {
    for (hour, minute) in [(-1, 2), (24, 2), (1, -1), (1, 60)] {
        assert_eq!(
            TimeAnnouncement::format(hour, minute, TimeFormat::TwelveHour),
            Err(TimeAnnouncementError::InvalidTime)
        );
    }
}

#[test]
fn rejects_unknown_clock_format() {
    assert_eq!(TimeFormat::try_from(12_u64), Ok(TimeFormat::TwelveHour));
    assert_eq!(TimeFormat::try_from(24_u64), Ok(TimeFormat::TwentyFourHour));
    assert_eq!(
        TimeFormat::try_from(13_u64),
        Err(TimeAnnouncementError::InvalidFormat)
    );
    assert_eq!(
        TimeAnnouncementError::InvalidFormat.to_string(),
        "invalid time announcement format"
    );
    assert_eq!(
        TimeAnnouncementError::InvalidTime.to_string(),
        "invalid civil time"
    );
}
