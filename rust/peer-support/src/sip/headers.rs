//! Field-aware borrowed views; unknown fields and authentication are not lists.
use super::{MAX_FIELD, Message, ParseError, decimal, token, trim, unfolded};

/// A field in its original wire order, including repeated compact aliases.
#[derive(Clone, Copy)]
pub struct Header<'a> {
    name: &'a [u8],
    value: &'a [u8],
}

fn canonical(name: &[u8]) -> &[u8] {
    if name.len() == 1 {
        match name[0].to_ascii_lowercase() {
            b'i' => b"Call-ID",
            b'm' => b"Contact",
            b'e' => b"Content-Encoding",
            b'l' => b"Content-Length",
            b'c' => b"Content-Type",
            b'f' => b"From",
            b's' => b"Subject",
            b'k' => b"Supported",
            b't' => b"To",
            b'v' => b"Via",
            _ => name,
        }
    } else {
        name
    }
}

impl Header<'_> {
    /// Compare a full or RFC 3261 compact field name without allocation.
    pub fn is(&self, name: &[u8]) -> bool {
        canonical(self.name).eq_ignore_ascii_case(canonical(name))
    }

    /// Original field-name bytes, excluding whitespace before the colon.
    pub fn name(&self) -> &[u8] {
        self.name
    }

    /// Original field value including folding, but excluding the final CRLF.
    pub fn raw_value(&self) -> &[u8] {
        self.value
    }

    /// Unfold into reusable caller storage and remove boundary SWS.
    ///
    /// Parsing already validated the bound. Bytes including quoted-pair NUL
    /// and percent escapes remain uninterpreted; no global UTF-8 conversion.
    pub fn unfold<'s>(&self, scratch: &'s mut [u8; MAX_FIELD]) -> &'s [u8] {
        let mut count = 0;
        for byte in unfolded(self.value) {
            scratch[count] = byte;
            count += 1;
        }
        trim(&scratch[..count])
    }

    /// Iterate grammar-defined list values, never arbitrary comma-delimited text.
    ///
    /// Authentication challenges, Call-ID and unknown extensions stay opaque.
    /// Nested comments are recognized only in fields whose grammar permits them.
    /// This is lexical separation, not complete validation of every field type.
    pub fn values<'s>(&self, scratch: &'s mut [u8; MAX_FIELD]) -> Values<'s> {
        let list = [
            b"Contact".as_slice(),
            b"Via",
            b"Route",
            b"Record-Route",
            b"Allow",
            b"Supported",
            b"Require",
            b"Proxy-Require",
            b"Unsupported",
            b"Accept",
            b"Accept-Encoding",
            b"Accept-Language",
            b"Content-Encoding",
            b"Warning",
            b"Error-Info",
            b"Alert-Info",
            b"Call-Info",
            b"In-Reply-To",
        ]
        .iter()
        .any(|name| self.is(name));
        let comments = [b"Server".as_slice(), b"User-Agent", b"Retry-After"]
            .iter()
            .any(|name| self.is(name));
        Values {
            remaining: self.unfold(scratch),
            list,
            comments,
        }
    }
}

impl<'a> Message<'a> {
    /// Borrow every logical field in original wire order.
    pub fn headers(&self) -> impl Iterator<Item = Header<'a>> + '_ {
        self.parser.fields[..self.parser.count]
            .iter()
            .map(|field| Header {
                name: &self.wire[field.name_start as usize..field.name_end as usize],
                value: &self.wire[field.value_start as usize..field.value_end as usize],
            })
    }

    /// Read a singleton, rejecting repeated full/compact aliases even if equal.
    pub fn unique(&self, name: &[u8]) -> Result<Option<Header<'a>>, ParseError> {
        let mut matching = self.headers().filter(|header| header.is(name));
        let first = matching.next();
        if matching.next().is_some() {
            return Err(ParseError::Duplicate);
        }
        Ok(first)
    }

    /// Read the bounded CSeq number and case-sensitive method token.
    pub fn cseq<'s>(
        &self,
        scratch: &'s mut [u8; MAX_FIELD],
    ) -> Result<Option<(u32, &'s [u8])>, ParseError> {
        let Some(header) = self.unique(b"CSeq")? else {
            return Ok(None);
        };
        let value = header.unfold(scratch);
        let separator = value
            .iter()
            .position(|b| matches!(b, b' ' | b'\t'))
            .ok_or(ParseError::Syntax)?;
        let number = decimal(value[..separator].iter().copied(), 2_147_483_647)?;
        let method = trim(&value[separator..]);
        if method.is_empty() || !method.iter().copied().all(token) {
            return Err(ParseError::Syntax);
        }
        Ok(Some((number as u32, method)))
    }

    /// Read Call-ID's word grammar; quotes and parentheses have no grouping role.
    pub fn call_id<'s>(
        &self,
        scratch: &'s mut [u8; MAX_FIELD],
    ) -> Result<Option<&'s [u8]>, ParseError> {
        let Some(header) = self.unique(b"Call-ID")? else {
            return Ok(None);
        };
        let value = header.unfold(scratch);
        let mut words = value.split(|b| *b == b'@');
        let valid = |word: &[u8]| {
            !word.is_empty()
                && word
                    .iter()
                    .all(|b| token(*b) || b"()<>:\\\"/[]?{}".contains(b))
        };
        if !valid(words.next().unwrap_or_default())
            || words.next().is_some_and(|word| !valid(word))
            || words.next().is_some()
        {
            return Err(ParseError::Syntax);
        }
        Ok(Some(value))
    }
}

/// Borrowed field-aware values; the first syntax error terminates iteration.
pub struct Values<'a> {
    remaining: &'a [u8],
    list: bool,
    comments: bool,
}

impl<'a> Iterator for Values<'a> {
    type Item = Result<&'a [u8], ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        let input = self.remaining;
        if input.is_empty() {
            return None;
        }
        self.remaining = &[];
        if !self.list && !self.comments {
            return Some(Ok(input));
        }
        match value_end(input, self.list, self.comments) {
            Ok(end) => {
                let value = trim(&input[..end]);
                if value.is_empty() {
                    return Some(Err(ParseError::Syntax));
                }
                if end < input.len() {
                    let rest = trim(&input[end + 1..]);
                    if rest.is_empty() {
                        return Some(Err(ParseError::Syntax));
                    }
                    self.remaining = rest;
                }
                Some(Ok(value))
            }
            Err(error) => Some(Err(error)),
        }
    }
}

fn value_end(input: &[u8], list: bool, comments: bool) -> Result<usize, ParseError> {
    let mut quote = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut angle = false;
    for (at, byte) in input.iter().copied().enumerate() {
        if escaped {
            if byte > 0x7f || matches!(byte, b'\r' | b'\n') {
                return Err(ParseError::Syntax);
            }
            escaped = false;
            continue;
        }
        if byte < 0x20 && byte != b'\t' || byte == 0x7f {
            return Err(ParseError::Syntax);
        }
        if quote || depth != 0 {
            match byte {
                b'\\' => escaped = true,
                b'"' if quote => quote = false,
                b'(' if depth != 0 => depth += 1,
                b')' if depth != 0 => depth -= 1,
                _ => {}
            }
        } else {
            match byte {
                b'"' if !angle => quote = true,
                b'(' if comments && !angle => depth = 1,
                b')' if comments && !angle => return Err(ParseError::Syntax),
                b'<' if !angle => angle = true,
                b'>' if angle => angle = false,
                b'<' | b'>' => return Err(ParseError::Syntax),
                b',' if list && !angle => return Ok(at),
                _ => {}
            }
        }
    }
    if quote || escaped || depth != 0 || angle {
        return Err(ParseError::Syntax);
    }
    Ok(input.len())
}

/// SIP name-address syntax; URI bytes and header parameters stay distinct.
/// URI syntax is validated separately before using the address as a target.
pub struct NameAddr<'a> {
    display: &'a [u8],
    uri: &'a [u8],
    parameters: &'a [u8],
}

impl<'a> NameAddr<'a> {
    /// Parse one already-unfolded value, not a comma-separated address list.
    pub fn parse(value: &'a [u8]) -> Result<Self, ParseError> {
        if value.len() > MAX_FIELD {
            return Err(ParseError::Limit);
        }
        let value = trim(value);
        let (display, rest) = if value.starts_with(b"\"") {
            let end = quoted_end(value)?;
            (&value[..end], trim(&value[end..]))
        } else if let Some(open) = value.iter().position(|b| *b == b'<') {
            let display = trim(&value[..open]);
            if !display
                .iter()
                .all(|b| token(*b) || matches!(b, b' ' | b'\t'))
            {
                return Err(ParseError::Syntax);
            }
            (display, &value[open..])
        } else {
            (&b""[..], value)
        };
        let (uri, parameters) = if rest.starts_with(b"<") {
            let close = rest
                .iter()
                .position(|b| *b == b'>')
                .ok_or(ParseError::Syntax)?;
            (&rest[1..close], trim(&rest[close + 1..]))
        } else {
            if !display.is_empty() {
                return Err(ParseError::Syntax);
            }
            let end = rest.iter().position(|b| *b == b';').unwrap_or(rest.len());
            let uri = trim(&rest[..end]);
            if uri.contains(&b',') || uri.contains(&b'?') {
                return Err(ParseError::Syntax);
            }
            (uri, &rest[end..])
        };
        if uri.is_empty() || uri.iter().any(|b| *b <= b' ' || b"<>\"".contains(b)) {
            return Err(ParseError::Syntax);
        }
        validate_parameters(parameters)?;
        Ok(Self {
            display,
            uri,
            parameters,
        })
    }

    /// Original display name, including quote/escape syntax when present.
    pub fn display_name(&self) -> &'a [u8] {
        self.display
    }
    /// URI span, never including header parameters outside the angle brackets.
    pub fn uri(&self) -> &'a [u8] {
        self.uri
    }
    /// Uninterpreted validated generic header-parameter suffix.
    pub fn parameters(&self) -> &'a [u8] {
        self.parameters
    }
}

fn quoted_end(value: &[u8]) -> Result<usize, ParseError> {
    let mut escaped = false;
    for (at, byte) in value.iter().copied().enumerate().skip(1) {
        if escaped {
            if byte > 0x7f || matches!(byte, b'\r' | b'\n') {
                return Err(ParseError::Syntax);
            }
            escaped = false;
        } else {
            match byte {
                b'\\' => escaped = true,
                b'"' => return Ok(at + 1),
                _ if byte < 0x20 && byte != b'\t' || byte == 0x7f => {
                    return Err(ParseError::Syntax);
                }
                _ => {}
            }
        }
    }
    Err(ParseError::Syntax)
}

fn validate_parameters(mut value: &[u8]) -> Result<(), ParseError> {
    while !value.is_empty() {
        if value[0] != b';' {
            return Err(ParseError::Syntax);
        }
        value = trim(&value[1..]);
        let count = value.iter().take_while(|b| token(**b)).count();
        if count == 0 {
            return Err(ParseError::Syntax);
        }
        value = trim(&value[count..]);
        if value.starts_with(b"=") {
            value = trim(&value[1..]);
            let count = if value.starts_with(b"\"") {
                quoted_end(value)?
            } else {
                value
                    .iter()
                    .take_while(|b| token(**b) || b"[]:".contains(b))
                    .count()
            };
            if count == 0 {
                return Err(ParseError::Syntax);
            }
            value = trim(&value[count..]);
        }
    }
    Ok(())
}
