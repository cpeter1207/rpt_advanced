//! Bounded scalar parsing for protocol settings.
/// Parse an unsigned decimal value with inclusive bounds.
pub(crate) fn unsigned(text: &str, min: u64, max: u64) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    (min..=max).contains(&value).then_some(value)
}

/// Parse the explicit, locale-independent yes/no switches.
pub(crate) fn boolean(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
