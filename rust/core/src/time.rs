//! Deterministic local-time announcement formatting.

use crate::messages::{Daypart, Message, MessageCatalog};
use std::fmt;

/// Supported clock-display modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeFormat {
    /// Twelve-hour clock with an AM or PM suffix.
    TwelveHour,
    /// Zero-padded twenty-four-hour clock.
    TwentyFourHour,
}

/// A complete speech announcement and its matching Morse clock text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeAnnouncement {
    text: String,
    speech: String,
    morse: String,
}

/// A time-announcement input failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeAnnouncementError {
    /// The numeric clock-format selector is neither 12 nor 24.
    InvalidFormat,
    /// The supplied hour or minute is outside civil-time bounds.
    InvalidTime,
}

impl fmt::Display for TimeAnnouncementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFormat => "invalid time announcement format",
            Self::InvalidTime => "invalid civil time",
        })
    }
}

impl std::error::Error for TimeAnnouncementError {}

impl TryFrom<u64> for TimeFormat {
    type Error = TimeAnnouncementError;

    /// Convert the configured numeric mode into a typed clock format.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            12 => Ok(Self::TwelveHour),
            24 => Ok(Self::TwentyFourHour),
            _ => Err(TimeAnnouncementError::InvalidFormat),
        }
    }
}

impl TimeAnnouncement {
    /// Format a supplied local civil time without reading a system clock.
    pub fn format(
        hour: i32,
        minute: i32,
        format: TimeFormat,
        catalog: &MessageCatalog,
    ) -> Result<Self, TimeAnnouncementError> {
        if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
            return Err(TimeAnnouncementError::InvalidTime);
        }

        let morse = match format {
            TimeFormat::TwelveHour => {
                let twelve_hour = match hour % 12 {
                    0 => 12,
                    value => value,
                };
                let suffix = if hour < 12 { "AM" } else { "PM" };
                format!("{twelve_hour}:{minute:02} {suffix}")
            }
            TimeFormat::TwentyFourHour => format!("{hour:02}:{minute:02}"),
        };
        let daypart = match hour {
            0..=11 => Daypart::Morning,
            12..=16 => Daypart::Afternoon,
            _ => Daypart::Evening,
        };
        let greeting = catalog
            .format(&Message::Greeting { daypart })
            .map_err(|_| TimeAnnouncementError::InvalidFormat)?;
        let forms = catalog
            .format(&Message::TimeAnnouncement {
                greeting_text: &greeting.text,
                greeting_tts: &greeting.tts,
                time: &morse,
            })
            .map_err(|_| TimeAnnouncementError::InvalidFormat)?;

        Ok(Self {
            text: forms.text,
            speech: forms.tts,
            morse: forms.morse,
        })
    }

    /// Borrow the text/status form of the announcement.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Borrow the complete spoken announcement.
    pub fn speech(&self) -> &str {
        &self.speech
    }

    /// Borrow the clock text intended for Morse playback.
    pub fn morse(&self) -> &str {
        &self.morse
    }
}
