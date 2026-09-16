//! Deterministic parsers and matchers for local civil-time scheduling.

use std::str::FromStr;

/// A schedule parsing or validation failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// The supplied value is outside the deliberately small schedule grammar.
    Invalid,
}

/// Sunday-zero local weekdays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Weekday {
    /// Sunday.
    Sunday = 0,
    /// Monday.
    Monday = 1,
    /// Tuesday.
    Tuesday = 2,
    /// Wednesday.
    Wednesday = 3,
    /// Thursday.
    Thursday = 4,
    /// Friday.
    Friday = 5,
    /// Saturday.
    Saturday = 6,
}

impl Weekday {
    fn parse(text: &str) -> Result<Self, ScheduleError> {
        if text.eq_ignore_ascii_case("sunday") {
            Ok(Self::Sunday)
        } else if text.eq_ignore_ascii_case("monday") {
            Ok(Self::Monday)
        } else if text.eq_ignore_ascii_case("tuesday") {
            Ok(Self::Tuesday)
        } else if text.eq_ignore_ascii_case("wednesday") {
            Ok(Self::Wednesday)
        } else if text.eq_ignore_ascii_case("thursday") {
            Ok(Self::Thursday)
        } else if text.eq_ignore_ascii_case("friday") {
            Ok(Self::Friday)
        } else if text.eq_ignore_ascii_case("saturday") {
            Ok(Self::Saturday)
        } else {
            Err(ScheduleError::Invalid)
        }
    }
}

/// One validated local civil minute, independent of a host time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    year: u16,
    month: u8,
    day: u8,
    weekday: Weekday,
    hour: u8,
    minute: u8,
}

impl CivilTime {
    /// Read the validated calendar fields without reparsing or changing civil-time semantics.
    pub const fn components(self) -> (u16, u8, u8, Weekday, u8, u8) {
        (
            self.year,
            self.month,
            self.day,
            self.weekday,
            self.hour,
            self.minute,
        )
    }
    /// Construct one validated local civil minute.
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        weekday: Weekday,
        hour: u8,
        minute: u8,
    ) -> Result<Self, ScheduleError> {
        let date = CivilDate { year, month, day };
        if !date.valid() || hour >= 24 || minute >= 60 {
            return Err(ScheduleError::Invalid);
        }
        Ok(Self {
            year,
            month,
            day,
            weekday,
            hour,
            minute,
        })
    }

    fn minute_of_day(self) -> u16 {
        u16::from(self.hour) * 60 + u16::from(self.minute)
    }

    fn occurrence(self) -> u64 {
        ((((u64::from(self.year) * 13 + u64::from(self.month)) * 32 + u64::from(self.day)) * 24
            + u64::from(self.hour))
            * 60)
            + u64::from(self.minute)
    }
}

/// Operations permitted in a named scheduler macro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduledAction {
    /// Create one nonpermanent direct link.
    Connect,
    /// Disconnect one selected direct link.
    Disconnect,
    /// Disconnect every direct link and pause retained retries.
    DisconnectAll,
    /// Resume every retained retry.
    ReconnectAll,
}

impl FromStr for ScheduledAction {
    type Err = ScheduleError;

    /// Parse one exact documented controller operation.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "connect" => Ok(Self::Connect),
            "disconnect" => Ok(Self::Disconnect),
            "disconnect_all" => Ok(Self::DisconnectAll),
            "reconnect_all" => Ok(Self::ReconnectAll),
            _ => Err(ScheduleError::Invalid),
        }
    }
}

/// A zero-time local civil trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduledEvent {
    /// Every local day at one minute.
    Daily {
        /// Local hour.
        hour: u8,
        /// Local minute.
        minute: u8,
    },
    /// One local weekday at one minute.
    Weekly {
        /// Selected weekday.
        weekday: Weekday,
        /// Local hour.
        hour: u8,
        /// Local minute.
        minute: u8,
    },
    /// One local Gregorian date at one minute.
    Once {
        /// Gregorian year.
        year: u16,
        /// One-based Gregorian month.
        month: u8,
        /// One-based Gregorian day.
        day: u8,
        /// Local hour.
        hour: u8,
        /// Local minute.
        minute: u8,
    },
}

impl ScheduledEvent {
    /// Return this event's local calendar-minute occurrence when it is due.
    pub fn occurrence(&self, local: &CivilTime) -> Option<u64> {
        let matches = match *self {
            Self::Daily { hour, minute } => local.hour == hour && local.minute == minute,
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => local.weekday == weekday && local.hour == hour && local.minute == minute,
            Self::Once {
                year,
                month,
                day,
                hour,
                minute,
            } => {
                local.year == year
                    && local.month == month
                    && local.day == day
                    && local.hour == hour
                    && local.minute == minute
            }
        };
        matches.then(|| local.occurrence())
    }
}

impl FromStr for ScheduledEvent {
    type Err = ScheduleError;

    /// Parse one exact daily, weekly, or one-time local event trigger.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(clock) = text.strip_prefix("daily ") {
            let (hour, minute) = parse_clock(clock)?;
            return Ok(Self::Daily { hour, minute });
        }
        if let Some(rest) = text.strip_prefix("weekly ") {
            let (weekday, clock) = rest.split_once(' ').ok_or(ScheduleError::Invalid)?;
            let (hour, minute) = parse_clock(clock)?;
            return Ok(Self::Weekly {
                weekday: Weekday::parse(weekday)?,
                hour,
                minute,
            });
        }
        if let Some(rest) = text.strip_prefix("once ") {
            let (date, clock) = rest.split_once(' ').ok_or(ScheduleError::Invalid)?;
            let date = CivilDate::parse(date)?;
            let (hour, minute) = parse_clock(clock)?;
            return Ok(Self::Once {
                year: date.year,
                month: date.month,
                day: date.day,
                hour,
                minute,
            });
        }
        Err(ScheduleError::Invalid)
    }
}

/// A bounded same-date local civil-time window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledWindow {
    start_minute: u16,
    end_minute: u16,
    weekday_mask: u8,
    dates: Vec<CivilDate>,
}

impl ScheduledWindow {
    /// Elapsed wall-clock milliseconds after this selected date's window end.
    /// Returns `None` before the end, on another selected date, or for invalid seconds.
    pub fn elapsed_after_end(&self, local: &CivilTime, second: u8) -> Option<u64> {
        if second >= 60 || local.minute_of_day() < self.end_minute {
            return None;
        }
        let inside = CivilTime {
            hour: ((self.end_minute - 1) / 60) as u8,
            minute: ((self.end_minute - 1) % 60) as u8,
            ..*local
        };
        self.matches(&inside).then(|| {
            (u64::from(local.minute_of_day() - self.end_minute) * 60 + u64::from(second)) * 1000
        })
    }

    /// Parse optional weekday or date selectors and a same-date local time range.
    pub fn parse(
        days: Option<&str>,
        dates: Option<&str>,
        start: &str,
        end: &str,
    ) -> Result<Self, ScheduleError> {
        let start_minute = clock_minute(start)?;
        let end_minute = clock_minute(end)?;
        let weekday_mask = parse_weekdays(days.unwrap_or(""))?;
        let dates = parse_dates(dates.unwrap_or(""))?;
        if start_minute >= end_minute || (weekday_mask != 0 && !dates.is_empty()) {
            return Err(ScheduleError::Invalid);
        }
        Ok(Self {
            start_minute,
            end_minute,
            weekday_mask,
            dates,
        })
    }

    /// Test whether this window selects the supplied local civil minute.
    pub fn matches(&self, local: &CivilTime) -> bool {
        local.minute_of_day() >= self.start_minute
            && local.minute_of_day() < self.end_minute
            && (self.weekday_mask == 0 || self.weekday_mask & (1 << local.weekday as u8) != 0)
            && (self.dates.is_empty()
                || self.dates.iter().any(|date| {
                    date.year == local.year && date.month == local.month && date.day == local.day
                }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CivilDate {
    year: u16,
    month: u8,
    day: u8,
}

impl CivilDate {
    fn parse(text: &str) -> Result<Self, ScheduleError> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(ScheduleError::Invalid);
        }
        let date = Self {
            year: parse_digits(&bytes[0..4])? as u16,
            month: parse_digits(&bytes[5..7])? as u8,
            day: parse_digits(&bytes[8..10])? as u8,
        };
        date.valid().then_some(date).ok_or(ScheduleError::Invalid)
    }

    fn valid(&self) -> bool {
        (1..=9_999).contains(&self.year)
            && self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= month_days(self.year, self.month)
    }
}

fn parse_clock(text: &str) -> Result<(u8, u8), ScheduleError> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return Err(ScheduleError::Invalid);
    }
    let hour = parse_digits(&bytes[..2])? as u8;
    let minute = parse_digits(&bytes[3..])? as u8;
    if minute >= 60 || hour >= 24 {
        return Err(ScheduleError::Invalid);
    }
    Ok((hour, minute))
}

fn clock_minute(text: &str) -> Result<u16, ScheduleError> {
    let (hour, minute) = parse_clock(text)?;
    Ok(u16::from(hour) * 60 + u16::from(minute))
}

/// Check one inclusive start boundary.
pub(crate) fn start_time_valid(text: &str) -> bool {
    clock_minute(text).is_ok()
}

/// Check one exclusive end boundary.
pub(crate) fn end_time_valid(text: &str) -> bool {
    clock_minute(text).is_ok()
}

fn parse_digits(bytes: &[u8]) -> Result<u32, ScheduleError> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(byte - b'0'))
            .ok_or(ScheduleError::Invalid)
    })
}

fn parse_weekdays(text: &str) -> Result<u8, ScheduleError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    text.split(',').try_fold(0_u8, |mask, term| {
        let term = term.trim();
        if term.is_empty() {
            return Err(ScheduleError::Invalid);
        }
        let (first, last) = match term.split_once('-') {
            Some((first, last)) if !last.contains('-') => {
                (Weekday::parse(first.trim())?, Weekday::parse(last.trim())?)
            }
            Some(_) => return Err(ScheduleError::Invalid),
            None => {
                let weekday = Weekday::parse(term)?;
                (weekday, weekday)
            }
        };
        let mut updated = mask;
        let mut day = first as u8;
        loop {
            updated |= 1 << day;
            if day == last as u8 {
                return Ok(updated);
            }
            day = (day + 1) % 7;
        }
    })
}

/// Check one optional weekday selector.
pub(crate) fn weekday_selector_valid(text: &str) -> bool {
    parse_weekdays(text).is_ok()
}

fn parse_dates(text: &str) -> Result<Vec<CivilDate>, ScheduleError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let dates: Result<Vec<_>, _> = text
        .split(',')
        .map(|term| CivilDate::parse(term.trim()))
        .collect();
    let dates = dates?;
    (dates.len() <= 64)
        .then_some(dates)
        .ok_or(ScheduleError::Invalid)
}

/// Check one optional bounded date selector.
pub(crate) fn date_selector_valid(text: &str) -> bool {
    parse_dates(text).is_ok()
}

fn month_days(year: u16, month: u8) -> u8 {
    const DAYS: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    // CivilDate::valid checks the month range before calling this helper.
    if month == 2 && leap_year(year) {
        29
    } else {
        DAYS[usize::from(month - 1)]
    }
}

fn leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[cfg(test)]
#[path = "schedule/tests.rs"]
mod tests;
