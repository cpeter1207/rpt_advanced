//! Matched ABI-1 recurrence capability for generation preparation and scheduling.
use super::{ScheduleDescriptor, rptadv_schedule_descriptor};
use libloading::Library;
use std::{
    ffi::{CStr, c_char, c_void},
    fmt,
    ptr::{self, NonNull},
    sync::OnceLock,
};

/// Inclusive lower bound of a provider Unix-second query.
pub const MIN_SECOND: i64 = -62_135_510_400;
/// Exclusive upper bound of a provider Unix-second query.
pub const END_SECOND: i64 = 253_402_128_000;

/// Bounded, value-free provider diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Missing or incompatible recurrence descriptor or an invalid provider result.
    Provider,
    /// Invalid or overlong source text.
    Text,
    /// The expression needs a usable default timezone.
    Zone,
    /// Invalid calendar expression.
    Parse,
    /// Instant or date outside the supported range.
    Range,
    /// The provider exhausted its bounded search work.
    Limit,
}

fn status(code: i32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        -2 => Err(Error::Text),
        -3 => Err(Error::Zone),
        -4 => Err(Error::Parse),
        -5 => Err(Error::Range),
        -6 => Err(Error::Limit),
        _ => Err(Error::Provider),
    }
}

type Handle = *mut c_void;
type Query = unsafe extern "C" fn(*const c_void, i64, *mut i64) -> i32;
type Create = unsafe extern "C" fn(*const u8, u32, *const u8, u32, *mut Handle) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    size: u32,
    version: u32,
    name: *const c_char,
    min_second: i64,
    end_second: i64,
    create: Option<Create>,
    destroy: Option<unsafe extern "C" fn(Handle)>,
    clone_handle: Option<unsafe extern "C" fn(*const c_void, *mut Handle) -> i32>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut u32) -> i32>,
    zone: Option<unsafe extern "C" fn(*const c_void) -> *const c_char>,
    match_minute: Option<Query>,
    next_after: Option<Query>,
    previous_before: Option<Query>,
}
// The provider publishes one immutable static descriptor, shared by every reader.
unsafe impl Sync for Descriptor {}

struct Api {
    descriptor: &'static Descriptor,
    _library: Library,
}

unsafe fn validate(pointer: *const Descriptor) -> Result<&'static Descriptor, Error> {
    // Read only the prefix until the provider proves the complete structure is available.
    if pointer.is_null()
        || unsafe { ptr::addr_of!((*pointer).size).read() } < size_of::<Descriptor>() as u32
        || unsafe { ptr::addr_of!((*pointer).version).read() } != 1
    {
        return Err(Error::Provider);
    }
    let d = unsafe { &*pointer };
    if d.name.is_null()
        || unsafe { CStr::from_ptr(d.name) } != c"rptadv.recurrence"
        || d.min_second != MIN_SECOND
        || d.end_second != END_SECOND
        || d.create.is_none()
        || d.destroy.is_none()
        || d.clone_handle.is_none()
        || d.equal.is_none()
        || d.zone.is_none()
        || d.match_minute.is_none()
        || d.next_after.is_none()
        || d.previous_before.is_none()
    {
        return Err(Error::Provider);
    }
    Ok(d)
}

fn api() -> Result<&'static Descriptor, Error> {
    static API: OnceLock<Result<Api, Error>> = OnceLock::new();
    let provider = API
        .get_or_init(|| unsafe {
            super::provider_ready().map_err(|_| Error::Provider)?;
            let library = Library::new("librptadvschedule.so.1").map_err(|_| Error::Provider)?;
            let base = library
                .get::<unsafe extern "C" fn() -> *const ScheduleDescriptor>(
                    b"rptadv_schedule_descriptor\0",
                )
                .map_err(|_| Error::Provider)?;
            if base() != rptadv_schedule_descriptor() {
                return Err(Error::Provider);
            }
            let getter = library
                .get::<unsafe extern "C" fn() -> *const Descriptor>(
                    b"rptadv_recurrence_descriptor\0",
                )
                .map_err(|_| Error::Provider)?;
            Ok(Api {
                descriptor: validate(getter())?,
                _library: library,
            })
        })
        .as_ref()
        .map_err(|e| *e)?;
    Ok(provider.descriptor)
}

/// Verify the recurrence capability before publishing a JSON generation.
pub fn provider_ready() -> Result<(), Error> {
    api().map(|_| ())
}

/// An immutable compiled expression with its authored text and actual bound zone.
pub struct Recurrence {
    handle: NonNull<c_void>,
    descriptor: &'static Descriptor,
    source: String,
}

// ABI 1 permits concurrent reads. Drop requires unique ownership.
unsafe impl Send for Recurrence {}
unsafe impl Sync for Recurrence {}
impl fmt::Debug for Recurrence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Recurrence")
    }
}
impl Drop for Recurrence {
    fn drop(&mut self) {
        unsafe { self.descriptor.destroy.unwrap()(self.handle.as_ptr()) }
    }
}
impl PartialEq for Recurrence {
    fn eq(&self, other: &Self) -> bool {
        let mut equal = 0;
        unsafe {
            self.descriptor.equal.unwrap()(self.handle.as_ptr(), other.handle.as_ptr(), &mut equal)
                == 0
                && equal == 1
        }
    }
}
impl Eq for Recurrence {}
impl Recurrence {
    /// Compile once with a resolved IANA zone. An inline zone overrides this default.
    pub fn compile(source: &str, zone: Option<&str>) -> Result<Self, Error> {
        let descriptor = api()?;
        let (zone_ptr, zone_len) = zone.map_or((ptr::null(), 0), |s| (s.as_ptr(), s.len()));
        let mut handle = ptr::null_mut();
        status(unsafe {
            descriptor.create.unwrap()(
                source.as_ptr(),
                source.len().try_into().map_err(|_| Error::Text)?,
                zone_ptr,
                zone_len.try_into().map_err(|_| Error::Text)?,
                &mut handle,
            )
        })?;
        Ok(Self {
            handle: NonNull::new(handle).ok_or(Error::Provider)?,
            descriptor,
            source: source.to_owned(),
        })
    }
    /// Independently owned immutable provider clone.
    pub fn try_clone(&self) -> Result<Self, Error> {
        let mut handle = ptr::null_mut();
        status(unsafe {
            self.descriptor.clone_handle.unwrap()(self.handle.as_ptr(), &mut handle)
        })?;
        Ok(Self {
            handle: NonNull::new(handle).ok_or(Error::Provider)?,
            descriptor: self.descriptor,
            source: self.source.clone(),
        })
    }
    /// Original spelling retained for authoring.
    pub fn source(&self) -> &str {
        &self.source
    }
    /// Canonical timezone borrowed for the lifetime of this handle.
    pub fn effective_zone(&self) -> Result<&str, Error> {
        let z = unsafe { self.descriptor.zone.unwrap()(self.handle.as_ptr()) };
        if z.is_null() {
            return Err(Error::Provider);
        }
        unsafe { CStr::from_ptr(z) }
            .to_str()
            .map_err(|_| Error::Provider)
    }
    fn query(&self, op: Query, instant: i64) -> Result<Option<i64>, Error> {
        let mut out = 0;
        match unsafe { op(self.handle.as_ptr(), instant, &mut out) } {
            1 => Ok(Some(out)),
            code => status(code).map(|()| None),
        }
    }
    /// Match the supplied captured minute and return its actual UTC occurrence second.
    pub fn match_minute(&self, instant: i64) -> Result<Option<i64>, Error> {
        self.query(self.descriptor.match_minute.unwrap(), instant)
    }
    /// Find the next strictly later occurrence.
    pub fn next_after(&self, instant: i64) -> Result<Option<i64>, Error> {
        self.query(self.descriptor.next_after.unwrap(), instant)
    }
    /// Find the previous strictly earlier occurrence.
    pub fn previous_before(&self, instant: i64) -> Result<Option<i64>, Error> {
        self.query(self.descriptor.previous_before.unwrap(), instant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_real_matched_recurrence_descriptor() {
        assert_eq!(provider_ready(), Ok(()));
        let compiled = Recurrence::compile("EVERY DAY AT 0:00", Some("UTC")).unwrap();
        let copy = compiled.try_clone().unwrap();
        drop(compiled);
        assert_eq!(copy.source(), "EVERY DAY AT 0:00");
        assert_eq!(copy.effective_zone(), Ok("UTC"));
        assert_eq!(
            copy,
            Recurrence::compile("every day at 00:00 in UTC", None).unwrap()
        );
        assert_ne!(
            copy,
            Recurrence::compile("every day at 0:00", Some("America/Chicago")).unwrap()
        );
        assert_eq!(copy.match_minute(43), Ok(Some(0)));
        assert_eq!(copy.match_minute(60), Ok(None));
        assert_eq!(copy.previous_before(0), Ok(Some(-86400)));
        assert_eq!(copy.next_after(0), Ok(Some(86400)));
        assert_eq!(copy.next_after(i64::MAX), Err(Error::Range));
        assert_eq!(
            Recurrence::compile("every day at 0:00", None),
            Err(Error::Zone)
        );
        assert_eq!(
            Recurrence::compile("malformed", Some("UTC")),
            Err(Error::Parse)
        );
    }

    #[test]
    fn rejects_incompatible_recurrence_prefix_identity_range_and_operations() {
        let production = api().unwrap();
        assert!(unsafe { validate(ptr::null()) }.is_err());
        let short = 0_u32;
        assert!(unsafe { validate(ptr::from_ref(&short).cast()) }.is_err());
        let broken = [
            Descriptor {
                version: 2,
                ..*production
            },
            Descriptor {
                name: ptr::null(),
                ..*production
            },
            Descriptor {
                name: c"rptadv.schedule".as_ptr(),
                ..*production
            },
            Descriptor {
                end_second: 0,
                ..*production
            },
            Descriptor {
                create: None,
                ..*production
            },
            Descriptor {
                clone_handle: None,
                ..*production
            },
            Descriptor {
                destroy: None,
                ..*production
            },
            Descriptor {
                equal: None,
                ..*production
            },
            Descriptor {
                zone: None,
                ..*production
            },
            Descriptor {
                match_minute: None,
                ..*production
            },
            Descriptor {
                next_after: None,
                ..*production
            },
            Descriptor {
                previous_before: None,
                ..*production
            },
        ];
        for table in &broken {
            assert!(unsafe { validate(table) }.is_err());
        }
    }
}
