//! Copy resolved host requirements across the dynamically loaded product boundary.

use crate::providers::ProductInspection;
use std::{
    ffi::{c_char, c_void},
    fmt,
};

/// Safely printable product configuration diagnostic.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError(pub(crate) String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}

#[derive(Debug, Default)]
struct Inspection {
    diagnostics: Vec<String>,
    invalid_record: bool,
}

/// Validate without opening device providers and return the product's warnings.
pub fn check_config(text: &str) -> Result<Vec<String>, ConfigError> {
    let product = ProductInspection::open().map_err(|e| ConfigError(e.to_string()))?;
    let api = product
        .managed_inspection()
        .map_err(|e| ConfigError(e.to_string()))?;
    let mut result = Inspection::default();
    let context = std::ptr::from_mut(&mut result).cast();
    // SAFETY: the DSO, input span and diagnostics owner live through the synchronous validation.
    let code = unsafe {
        api.inspect.unwrap()(
            text.as_ptr().cast(),
            text.len(),
            None,
            std::ptr::null_mut(),
            Some(copy_diagnostic),
            context,
        )
    };
    finish_inspection(result, code).map(|result| result.diagnostics)
}

fn finish_inspection(mut output: Inspection, result: i32) -> Result<Inspection, ConfigError> {
    if result != 0 || output.invalid_record {
        return Err(ConfigError(output.diagnostics.pop().unwrap_or_else(|| {
            "product configuration inspection failed".into()
        })));
    }
    Ok(output)
}

unsafe fn text<'a>(pointer: *const c_char, length: usize) -> Result<&'a str, ()> {
    if length == 0 {
        return Ok("");
    }
    if pointer.is_null() || length > isize::MAX as usize {
        return Err(());
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer.cast(), length) })
        .map_err(|_| ())
}

unsafe extern "C" fn copy_diagnostic(context: *mut c_void, pointer: *const c_char, length: usize) {
    let output = unsafe { &mut *context.cast::<Inspection>() };
    match unsafe { text(pointer, length) } {
        Ok(text) => output.diagnostics.push(text.to_owned()),
        Err(()) => output.invalid_record = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_without_a_diagnostic_reports_a_safe_fallback() {
        assert_eq!(
            finish_inspection(Inspection::default(), -1).unwrap_err(),
            ConfigError("product configuration inspection failed".into())
        );
        assert_eq!(
            finish_inspection(
                Inspection {
                    invalid_record: true,
                    ..Inspection::default()
                },
                0
            )
            .unwrap_err(),
            ConfigError("product configuration inspection failed".into())
        );
    }
}
