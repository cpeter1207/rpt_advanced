//! Controller-facing civil scheduling backed by the released schedule ABI.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fmt;
use std::ptr::{self, NonNull};
use std::str::FromStr;
use std::sync::OnceLock;

/// A schedule parsing or validation failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// The supplied value is outside the deliberately small schedule grammar.
    Invalid,
}

/// The mandatory shared schedule provider is absent or incompatible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleProviderError;

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
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::Sunday,
            1 => Self::Monday,
            2 => Self::Tuesday,
            3 => Self::Wednesday,
            4 => Self::Thursday,
            5 => Self::Friday,
            6 => Self::Saturday,
            _ => return None,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ScheduleCivil {
    year: u16,
    month: u8,
    day: u8,
    weekday: u8,
    hour: u8,
    minute: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ScheduleEventRecord {
    kind: u32,
    year: u16,
    month: u8,
    day: u8,
    weekday: u8,
    hour: u8,
    minute: u8,
}

type Boundary = unsafe extern "C" fn(
    *const c_void,
    *const ScheduleCivil,
    u8,
    *mut u32,
    *mut u64,
    *mut u64,
) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct ScheduleDescriptor {
    struct_size: u32,
    abi_version: u32,
    capability_name: *const c_char,
    civil_new: Option<unsafe extern "C" fn(u16, u8, u8, u8, u8, u8, *mut ScheduleCivil) -> i32>,
    event_parse: Option<unsafe extern "C" fn(*const c_char, *mut ScheduleEventRecord) -> i32>,
    event_occurrence: Option<
        unsafe extern "C" fn(
            *const ScheduleEventRecord,
            *const ScheduleCivil,
            *mut u32,
            *mut u64,
        ) -> i32,
    >,
    start_time_valid: Option<unsafe extern "C" fn(*const c_char) -> u32>,
    end_time_valid: Option<unsafe extern "C" fn(*const c_char) -> u32>,
    weekday_selector_valid: Option<unsafe extern "C" fn(*const c_char) -> u32>,
    date_selector_valid: Option<unsafe extern "C" fn(*const c_char) -> u32>,
    window_create: Option<
        unsafe extern "C" fn(
            u32,
            *const c_char,
            u32,
            *const c_char,
            *const c_char,
            *const c_char,
            *mut *mut c_void,
        ) -> i32,
    >,
    window_destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    window_clone: Option<unsafe extern "C" fn(*const c_void, *mut *mut c_void) -> i32>,
    window_equal: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut u32) -> i32>,
    window_matches:
        Option<unsafe extern "C" fn(*const c_void, *const ScheduleCivil, *mut u32) -> i32>,
    window_overlaps: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut u32) -> i32>,
    window_elapsed_after_end: Option<
        unsafe extern "C" fn(*const c_void, *const ScheduleCivil, u8, *mut u32, *mut u64) -> i32,
    >,
    window_next_start: Option<Boundary>,
    window_next_end: Option<Boundary>,
}

// The released descriptor is immutable and remains valid for the DSO lifetime.
unsafe impl Sync for ScheduleDescriptor {}

unsafe extern "C" {
    fn rptadv_schedule_descriptor() -> *const ScheduleDescriptor;
}

static API: OnceLock<Result<&'static ScheduleDescriptor, ScheduleProviderError>> = OnceLock::new();

fn provider_api() -> Result<&'static ScheduleDescriptor, ScheduleProviderError> {
    *API.get_or_init(|| unsafe { validate_descriptor(rptadv_schedule_descriptor()) })
}

/// Validate the mandatory provider during product setup, even without schedules.
pub fn provider_ready() -> Result<(), ScheduleProviderError> {
    provider_api().map(|_| ())
}

fn input_status(status: i32) -> Result<(), ScheduleError> {
    match status {
        0 => Ok(()),
        -1 => Err(ScheduleError::Invalid),
        unexpected => panic!("schedule provider contract failure: {unexpected}"),
    }
}

/// Check the size prefix before reading the complete immutable ABI table.
///
/// # Safety
/// `pointer` must address a readable prefix and, if its size suffices, a
/// complete process-lifetime descriptor.
unsafe fn validate_descriptor<'a>(
    pointer: *const ScheduleDescriptor,
) -> Result<&'a ScheduleDescriptor, ScheduleProviderError> {
    if pointer.is_null() {
        return Err(ScheduleProviderError);
    }
    let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read() };
    if size < std::mem::size_of::<ScheduleDescriptor>() as u32 {
        return Err(ScheduleProviderError);
    }
    let abi = unsafe { std::ptr::addr_of!((*pointer).abi_version).read() };
    if abi != 1 {
        return Err(ScheduleProviderError);
    }
    let descriptor = unsafe { &*pointer };
    if descriptor.capability_name.is_null()
        || unsafe { CStr::from_ptr(descriptor.capability_name) } != c"rptadv.schedule"
        || descriptor.civil_new.is_none()
        || descriptor.event_parse.is_none()
        || descriptor.event_occurrence.is_none()
        || descriptor.start_time_valid.is_none()
        || descriptor.end_time_valid.is_none()
        || descriptor.weekday_selector_valid.is_none()
        || descriptor.date_selector_valid.is_none()
        || descriptor.window_create.is_none()
        || descriptor.window_destroy.is_none()
        || descriptor.window_clone.is_none()
        || descriptor.window_equal.is_none()
        || descriptor.window_matches.is_none()
        || descriptor.window_overlaps.is_none()
        || descriptor.window_elapsed_after_end.is_none()
        || descriptor.window_next_start.is_none()
        || descriptor.window_next_end.is_none()
    {
        return Err(ScheduleProviderError);
    }
    Ok(descriptor)
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
        let api = provider_api().map_err(|_| ScheduleError::Invalid)?;
        let mut record = ScheduleCivil::default();
        let status = unsafe {
            api.civil_new.expect("validated schedule ABI")(
                year,
                month,
                day,
                weekday as u8,
                hour,
                minute,
                &mut record,
            )
        };
        input_status(status)?;
        Ok(Self {
            year: record.year,
            month: record.month,
            day: record.day,
            weekday: Weekday::from_index(record.weekday).ok_or(ScheduleError::Invalid)?,
            hour: record.hour,
            minute: record.minute,
        })
    }

    fn record(&self) -> ScheduleCivil {
        ScheduleCivil {
            year: self.year,
            month: self.month,
            day: self.day,
            weekday: self.weekday as u8,
            hour: self.hour,
            minute: self.minute,
        }
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
        let api = provider_api().expect("schedule provider validated at setup");
        let record = match *self {
            Self::Daily { hour, minute } => ScheduleEventRecord {
                kind: 1,
                hour,
                minute,
                ..ScheduleEventRecord::default()
            },
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => ScheduleEventRecord {
                kind: 2,
                weekday: weekday as u8,
                hour,
                minute,
                ..ScheduleEventRecord::default()
            },
            Self::Once {
                year,
                month,
                day,
                hour,
                minute,
            } => ScheduleEventRecord {
                kind: 3,
                year,
                month,
                day,
                hour,
                minute,
                ..ScheduleEventRecord::default()
            },
        };
        let mut present = 0;
        let mut occurrence = 0;
        let status = unsafe {
            api.event_occurrence.expect("validated schedule ABI")(
                &record,
                &local.record(),
                &mut present,
                &mut occurrence,
            )
        };
        // Public enum variants permit invalid fields; the old matcher returned None.
        if status == -1 {
            return None;
        }
        assert_eq!(status, 0, "schedule provider event contract failure");
        (present != 0).then_some(occurrence)
    }
}

impl FromStr for ScheduledEvent {
    type Err = ScheduleError;

    /// Parse one exact daily, weekly, or one-time local event trigger.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let api = provider_api().map_err(|_| ScheduleError::Invalid)?;
        let text = CString::new(text).map_err(|_| ScheduleError::Invalid)?;
        let mut record = ScheduleEventRecord::default();
        let status =
            unsafe { api.event_parse.expect("validated schedule ABI")(text.as_ptr(), &mut record) };
        input_status(status)?;
        Ok(match record.kind {
            1 => Self::Daily {
                hour: record.hour,
                minute: record.minute,
            },
            2 => Self::Weekly {
                weekday: Weekday::from_index(record.weekday).ok_or(ScheduleError::Invalid)?,
                hour: record.hour,
                minute: record.minute,
            },
            3 => Self::Once {
                year: record.year,
                month: record.month,
                day: record.day,
                hour: record.hour,
                minute: record.minute,
            },
            _ => return Err(ScheduleError::Invalid),
        })
    }
}

/// A bounded same-date local civil-time window owned by the schedule provider.
pub struct ScheduledWindow {
    api: &'static ScheduleDescriptor,
    handle: NonNull<c_void>,
}

// The control owner transfers unique handles between threads and serializes
// all operations. Clones have independent handles; the DSO stays loaded.
unsafe impl Send for ScheduledWindow {}

impl Clone for ScheduledWindow {
    fn clone(&self) -> Self {
        let mut raw = ptr::null_mut();
        let status = unsafe {
            self.api.window_clone.expect("validated schedule ABI")(self.handle.as_ptr(), &mut raw)
        };
        assert_eq!(status, 0, "schedule provider rejected owned window clone");
        Self {
            api: self.api,
            handle: NonNull::new(raw).expect("schedule provider returned null clone"),
        }
    }
}

impl fmt::Debug for ScheduledWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScheduledWindow")
            .finish_non_exhaustive()
    }
}

impl PartialEq for ScheduledWindow {
    fn eq(&self, other: &Self) -> bool {
        let mut equal = 0;
        let status = unsafe {
            self.api.window_equal.expect("validated schedule ABI")(
                self.handle.as_ptr(),
                other.handle.as_ptr(),
                &mut equal,
            )
        };
        assert_eq!(
            status, 0,
            "schedule provider rejected owned window comparison"
        );
        equal != 0
    }
}

impl Eq for ScheduledWindow {}

impl Drop for ScheduledWindow {
    fn drop(&mut self) {
        unsafe { self.api.window_destroy.expect("validated schedule ABI")(self.handle.as_ptr()) };
    }
}

impl ScheduledWindow {
    /// Parse optional weekday/date selectors and a same-date local time range.
    pub fn parse(
        days: Option<&str>,
        dates: Option<&str>,
        start: &str,
        end: &str,
    ) -> Result<Self, ScheduleError> {
        let api = provider_api().map_err(|_| ScheduleError::Invalid)?;
        let days_text = days
            .map(CString::new)
            .transpose()
            .map_err(|_| ScheduleError::Invalid)?;
        let dates_text = dates
            .map(CString::new)
            .transpose()
            .map_err(|_| ScheduleError::Invalid)?;
        let start = CString::new(start).map_err(|_| ScheduleError::Invalid)?;
        let end = CString::new(end).map_err(|_| ScheduleError::Invalid)?;
        let mut raw = ptr::null_mut();
        let status = unsafe {
            api.window_create.expect("validated schedule ABI")(
                u32::from(days.is_some()),
                days_text.as_ref().map_or(ptr::null(), |text| text.as_ptr()),
                u32::from(dates.is_some()),
                dates_text
                    .as_ref()
                    .map_or(ptr::null(), |text| text.as_ptr()),
                start.as_ptr(),
                end.as_ptr(),
                &mut raw,
            )
        };
        if status != 0 {
            if let Some(handle) = NonNull::new(raw) {
                unsafe { api.window_destroy.expect("validated schedule ABI")(handle.as_ptr()) };
            }
        }
        input_status(status)?;
        Ok(Self {
            api,
            handle: NonNull::new(raw).expect("schedule provider returned null owned window"),
        })
    }

    /// Test whether this window selects the supplied local civil minute.
    pub fn matches(&self, local: &CivilTime) -> bool {
        let mut matches = 0;
        let status = unsafe {
            self.api.window_matches.expect("validated schedule ABI")(
                self.handle.as_ptr(),
                &local.record(),
                &mut matches,
            )
        };
        assert_eq!(status, 0, "schedule provider rejected owned window match");
        matches != 0
    }

    /// Whether two bounded same-date windows can be active at the same local civil minute.
    pub fn overlaps(&self, other: &Self) -> bool {
        let mut overlaps = 0;
        let status = unsafe {
            self.api.window_overlaps.expect("validated schedule ABI")(
                self.handle.as_ptr(),
                other.handle.as_ptr(),
                &mut overlaps,
            )
        };
        assert_eq!(status, 0, "schedule provider rejected owned window overlap");
        overlaps != 0
    }

    /// Elapsed wall-clock milliseconds after this selected date's window end.
    pub fn elapsed_after_end(&self, local: &CivilTime, second: u8) -> Option<u64> {
        let mut present = 0;
        let mut elapsed = 0;
        let status = unsafe {
            self.api
                .window_elapsed_after_end
                .expect("validated schedule ABI")(
                self.handle.as_ptr(),
                &local.record(),
                second,
                &mut present,
                &mut elapsed,
            )
        };
        assert_eq!(
            status, 0,
            "schedule provider rejected owned window elapsed time"
        );
        (present != 0).then_some(elapsed)
    }

    /// Milliseconds until the next selected inclusive start and its date identity.
    pub(crate) fn next_start(&self, local: &CivilTime, second: u8) -> Option<(u64, u64)> {
        self.next_boundary(
            local,
            second,
            self.api.window_next_start.expect("validated schedule ABI"),
        )
    }

    /// Milliseconds until the next selected exclusive end and its date identity.
    pub(crate) fn next_end(&self, local: &CivilTime, second: u8) -> Option<(u64, u64)> {
        self.next_boundary(
            local,
            second,
            self.api.window_next_end.expect("validated schedule ABI"),
        )
    }

    fn next_boundary(
        &self,
        local: &CivilTime,
        second: u8,
        operation: Boundary,
    ) -> Option<(u64, u64)> {
        let mut present = 0;
        let mut identity = 0;
        let mut remaining = 0;
        let status = unsafe {
            operation(
                self.handle.as_ptr(),
                &local.record(),
                second,
                &mut present,
                &mut identity,
                &mut remaining,
            )
        };
        assert_eq!(
            status, 0,
            "schedule provider rejected owned window boundary"
        );
        (present != 0).then_some((identity, remaining))
    }
}

fn valid_selector(text: &str, operation: unsafe extern "C" fn(*const c_char) -> u32) -> bool {
    CString::new(text).is_ok_and(|text| unsafe { operation(text.as_ptr()) != 0 })
}

/// Check one inclusive start boundary.
pub(crate) fn start_time_valid(text: &str) -> bool {
    valid_selector(
        text,
        provider_api()
            .expect("schedule provider validated at setup")
            .start_time_valid
            .expect("validated schedule ABI"),
    )
}

/// Check one exclusive end boundary.
pub(crate) fn end_time_valid(text: &str) -> bool {
    valid_selector(
        text,
        provider_api()
            .expect("schedule provider validated at setup")
            .end_time_valid
            .expect("validated schedule ABI"),
    )
}

/// Check one optional weekday selector.
pub(crate) fn weekday_selector_valid(text: &str) -> bool {
    valid_selector(
        text,
        provider_api()
            .expect("schedule provider validated at setup")
            .weekday_selector_valid
            .expect("validated schedule ABI"),
    )
}

/// Check one optional bounded date selector.
pub(crate) fn date_selector_valid(text: &str) -> bool {
    valid_selector(
        text,
        provider_api()
            .expect("schedule provider validated at setup")
            .date_selector_valid
            .expect("validated schedule ABI"),
    )
}

#[cfg(test)]
#[path = "schedule/tests.rs"]
mod tests;
