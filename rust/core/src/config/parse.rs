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

/// Parse a nonempty comma-separated list of positive decimal durations.
pub(crate) fn positive_milliseconds(text: &str) -> Option<Vec<u64>> {
    let values = text
        .split(',')
        .map(|value| unsigned(value.trim(), 1, u64::MAX))
        .collect::<Option<Vec<_>>>()?;
    (!values.is_empty()).then_some(values)
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

/// Parse a supported CTCSS frequency as tenths of a hertz.
pub(crate) fn ctcss_tone_tenths_hz(text: &str) -> Option<u16> {
    const TONES: [u16; 38] = [
        670, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188,
        1230, 1273, 1318, 1365, 1413, 1462, 1514, 1567, 1622, 1679, 1738, 1799, 1862, 1928, 2035,
        2107, 2181, 2257, 2336, 2418, 2503,
    ];
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    if fraction.len() != 1 {
        return None;
    }
    let whole = unsigned(whole, 0, u16::MAX as u64 / 10)?;
    let fraction = unsigned(fraction, 0, 9)?;
    let tenths = (whole * 10 + fraction) as u16;
    TONES.contains(&tenths).then_some(tenths)
}

/// Parse a nonempty unique list of supported comma-separated CTCSS tones.
pub(crate) fn ctcss_tones(text: &str) -> Option<Vec<u16>> {
    let tones = text
        .split(',')
        .map(|tone| ctcss_tone_tenths_hz(tone.trim()))
        .collect::<Option<Vec<_>>>()?;
    (tones
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == tones.len())
    .then_some(tones)
}

/// Parse a three-digit octal DCS code and its N/I polarity.
pub(crate) fn dcs_code(text: &str) -> Option<(u16, bool)> {
    let bytes = text.as_bytes();
    if bytes.len() != 4 || !bytes[..3].iter().all(|byte| matches!(byte, b'0'..=b'7')) {
        return None;
    }
    let value = u16::from(bytes[0] - b'0') * 64
        + u16::from(bytes[1] - b'0') * 8
        + u16::from(bytes[2] - b'0');
    let inverted = match bytes[3] {
        b'N' | b'n' => false,
        b'I' | b'i' => true,
        _ => return None,
    };
    Some((value, inverted))
}
