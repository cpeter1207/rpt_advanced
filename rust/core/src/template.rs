//! Strict, bounded scheduled-message templates.

use std::ffi::{CStr, c_char};
use std::fmt;
use std::sync::OnceLock;

/// Maximum rendered payload size, excluding C's equivalent trailing NUL byte.
const MAX_OUTPUT_BYTES: usize = 127;

/// Maximum replacement widths guaranteed by the current controller inputs.
const WEEKDAY_MAX: usize = 32;
const DATE_MAX: usize = 10;
const TIME_MAX: usize = 8;
const GREETING_MAX: usize = 14;
const LINK_STATUS_MAX: usize = 106;
const NODE_NAME_MAX: usize = 63;

/// A validated scheduled-message template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageTemplate(String);

/// Values available to a scheduled-message template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateValues<'a> {
    /// Local weekday name.
    pub day_of_week: &'a str,
    /// Local date text.
    pub date: &'a str,
    /// Local clock text.
    pub time: &'a str,
    /// Time-of-day greeting.
    pub greeting: &'a str,
    /// Current direct-link status summary.
    pub link_status: &'a str,
    /// Local node identifier.
    pub node: &'a str,
    /// Local station callsign.
    pub callsign: &'a str,
}

/// A scheduled-message template validation or rendering failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateError {
    /// A substitution is malformed or is not one of the documented names.
    InvalidSyntax,
    /// The complete rendered payload would exceed 127 bytes.
    OutputTooLong,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSyntax => "invalid message template syntax",
            Self::OutputTooLong => "message template output is too long",
        })
    }
}

impl std::error::Error for TemplateError {}

/// The required shared template provider is absent or incompatible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateProviderError;

#[repr(C)]
#[derive(Clone, Copy)]
struct Span {
    data: *const u8,
    length: usize,
}

impl Span {
    fn of(text: &str) -> Self {
        Self {
            data: text.as_ptr(),
            length: text.len(),
        }
    }
}

#[repr(C)]
struct KeyLimit {
    name: Span,
    maximum: usize,
}

#[repr(C)]
struct KeyValue {
    name: Span,
    value: Span,
}

type Validate = unsafe extern "C" fn(Span, *const KeyLimit, usize, usize) -> i32;
type Render = unsafe extern "C" fn(Span, *const KeyValue, usize, *mut u8, usize, *mut usize) -> i32;

#[repr(C)]
struct Descriptor {
    struct_size: u32,
    abi_version: u32,
    capability_name: *const c_char,
    validate: Option<Validate>,
    render: Option<Render>,
}

#[derive(Clone, Copy)]
struct TemplateApi {
    validate: Validate,
    render: Render,
}

unsafe extern "C" {
    fn rptadv_template_descriptor() -> *const Descriptor;
}

static API: OnceLock<Result<TemplateApi, TemplateProviderError>> = OnceLock::new();

fn provider_api() -> Result<TemplateApi, TemplateProviderError> {
    *API.get_or_init(|| unsafe { validate_descriptor(rptadv_template_descriptor()) })
}

/// Check the size prefix before reading the complete immutable table.
///
/// # Safety
/// `pointer` must address a readable ABI prefix and, if its size suffices,
/// a complete process-lifetime descriptor.
unsafe fn validate_descriptor(
    pointer: *const Descriptor,
) -> Result<TemplateApi, TemplateProviderError> {
    if pointer.is_null() {
        return Err(TemplateProviderError);
    }
    let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read() };
    if size < std::mem::size_of::<Descriptor>() as u32 {
        return Err(TemplateProviderError);
    }
    let abi = unsafe { std::ptr::addr_of!((*pointer).abi_version).read() };
    if abi != 1 {
        return Err(TemplateProviderError);
    }
    let descriptor = unsafe { &*pointer };
    if descriptor.capability_name.is_null()
        || unsafe { CStr::from_ptr(descriptor.capability_name) } != c"rptadv.template"
    {
        return Err(TemplateProviderError);
    }
    Ok(TemplateApi {
        validate: descriptor.validate.ok_or(TemplateProviderError)?,
        render: descriptor.render.ok_or(TemplateProviderError)?,
    })
}

fn status_result(status: i32) -> Result<(), TemplateError> {
    match status {
        0 => Ok(()),
        2 => Err(TemplateError::OutputTooLong),
        // The ABI's invalid-argument status is unreachable for valid Rust inputs.
        // Any unexpected failure remains fail-closed under the existing API.
        _ => Err(TemplateError::InvalidSyntax),
    }
}

impl MessageTemplate {
    /// Validate the mandatory stateless descriptor before loading configuration.
    pub fn provider_ready() -> Result<(), TemplateProviderError> {
        provider_api().map(|_| ())
    }

    /// Parse a template and reject malformed, unknown, or conservatively oversized output.
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let api = provider_api().map_err(|_| TemplateError::InvalidSyntax)?;
        let limits = [
            KeyLimit {
                name: Span::of("day_of_week"),
                maximum: WEEKDAY_MAX,
            },
            KeyLimit {
                name: Span::of("date"),
                maximum: DATE_MAX,
            },
            KeyLimit {
                name: Span::of("time"),
                maximum: TIME_MAX,
            },
            KeyLimit {
                name: Span::of("greeting"),
                maximum: GREETING_MAX,
            },
            KeyLimit {
                name: Span::of("link_status"),
                maximum: LINK_STATUS_MAX,
            },
            KeyLimit {
                name: Span::of("node"),
                maximum: NODE_NAME_MAX,
            },
            KeyLimit {
                name: Span::of("callsign"),
                maximum: NODE_NAME_MAX,
            },
        ];
        status_result(unsafe {
            (api.validate)(
                Span::of(text),
                limits.as_ptr(),
                limits.len(),
                MAX_OUTPUT_BYTES,
            )
        })?;
        Ok(Self(text.to_owned()))
    }

    /// Render the template completely, returning an error instead of truncated output.
    pub fn render(&self, values: &TemplateValues<'_>) -> Result<String, TemplateError> {
        let api = provider_api().map_err(|_| TemplateError::InvalidSyntax)?;
        let substitutions = [
            KeyValue {
                name: Span::of("day_of_week"),
                value: Span::of(values.day_of_week),
            },
            KeyValue {
                name: Span::of("date"),
                value: Span::of(values.date),
            },
            KeyValue {
                name: Span::of("time"),
                value: Span::of(values.time),
            },
            KeyValue {
                name: Span::of("greeting"),
                value: Span::of(values.greeting),
            },
            KeyValue {
                name: Span::of("link_status"),
                value: Span::of(values.link_status),
            },
            KeyValue {
                name: Span::of("node"),
                value: Span::of(values.node),
            },
            KeyValue {
                name: Span::of("callsign"),
                value: Span::of(values.callsign),
            },
        ];
        let mut output = [0u8; MAX_OUTPUT_BYTES];
        let mut written = 0;
        status_result(unsafe {
            (api.render)(
                Span::of(&self.0),
                substitutions.as_ptr(),
                substitutions.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut written,
            )
        })?;
        let bytes = output.get(..written).ok_or(TemplateError::InvalidSyntax)?;
        let text = std::str::from_utf8(bytes).map_err(|_| TemplateError::InvalidSyntax)?;
        Ok(text.to_owned())
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    unsafe extern "C" fn validate(_: Span, _: *const KeyLimit, _: usize, _: usize) -> i32 {
        0
    }
    unsafe extern "C" fn render(
        _: Span,
        _: *const KeyValue,
        _: usize,
        _: *mut u8,
        _: usize,
        _: *mut usize,
    ) -> i32 {
        0
    }

    #[test]
    fn descriptor_rejects_short_wrong_or_missing_fields() {
        let mut descriptor = Descriptor {
            struct_size: std::mem::size_of::<Descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.template".as_ptr(),
            validate: Some(validate),
            render: Some(render),
        };
        assert!(unsafe { validate_descriptor(&descriptor) }.is_ok());
        assert!(unsafe { validate_descriptor(std::ptr::null()) }.is_err());
        descriptor.struct_size -= 1;
        assert!(unsafe { validate_descriptor(&descriptor) }.is_err());
        descriptor.struct_size += 1;
        descriptor.abi_version = 2;
        assert!(unsafe { validate_descriptor(&descriptor) }.is_err());
        descriptor.abi_version = 1;
        descriptor.capability_name = std::ptr::null();
        assert!(unsafe { validate_descriptor(&descriptor) }.is_err());
        descriptor.capability_name = c"other".as_ptr();
        assert!(unsafe { validate_descriptor(&descriptor) }.is_err());
        descriptor.capability_name = c"rptadv.template".as_ptr();
        descriptor.render = None;
        assert!(unsafe { validate_descriptor(&descriptor) }.is_err());
    }

    #[test]
    fn status_translation_preserves_two_public_outcomes() {
        assert_eq!(status_result(0), Ok(()));
        assert_eq!(status_result(1), Err(TemplateError::InvalidSyntax));
        assert_eq!(status_result(2), Err(TemplateError::OutputTooLong));
        assert_eq!(status_result(3), Err(TemplateError::InvalidSyntax));
        assert_eq!(status_result(99), Err(TemplateError::InvalidSyntax));
    }
}
