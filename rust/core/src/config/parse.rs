//! Line-level configuration syntax.

/// A parsed configuration line before it is retained by the owned document.
pub(crate) enum ParsedLine<'a> {
    Empty,
    Section(&'a str),
    Option(&'a str, &'a str),
}

/// Parse one physical line using the configuration file's whitespace and comment rules.
pub(crate) fn parse_line(line: &str) -> Result<ParsedLine<'_>, ()> {
    let line = line.split(';').next().unwrap_or_default().trim();
    if line.is_empty() {
        return Ok(ParsedLine::Empty);
    }
    if let Some(section) = line.strip_prefix('[') {
        let Some(end) = section.find(']') else {
            return Err(());
        };
        let (name, trailing) = section.split_at(end);
        if name.trim().is_empty() || !trailing[1..].trim().is_empty() {
            return Err(());
        }
        return Ok(ParsedLine::Section(name.trim()));
    }
    let Some(equal) = line.find('=') else {
        return Err(());
    };
    let key = line[..equal].trim();
    if key.is_empty() {
        return Err(());
    }
    Ok(ParsedLine::Option(key, line[equal + 1..].trim()))
}

/// Parse an unsigned decimal value with inclusive bounds.
pub(crate) fn unsigned(text: &str, min: u64, max: u64) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    (min..=max).contains(&value).then_some(value)
}

/// Parse a signed decimal value with inclusive bounds.
pub(crate) fn signed(text: &str, min: i64, max: i64) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<i64>().ok()?;
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
