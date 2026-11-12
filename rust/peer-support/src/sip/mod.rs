//! Experimental bounded SIP wire parsing, shared by hosts but not a provider.
//!
//! RFC 3261 sections 7, 18.3 and 25 define framing. Admission limits below
//! are implementation policy, not protocol maxima. All views borrow the admitted
//! wire buffer; retained transactions must acquire their own storage reservation.

mod headers;
mod uri;
pub use headers::{Header, NameAddr, Values};
pub use uri::{Host, RouteError, Scheme, SipUri};

/// Maximum admitted wire-buffer size, independent of parser workspace.
pub const MAX_MESSAGE: usize = 32_768;
/// Maximum start-line and header section, including its empty terminating line.
pub const MAX_HEADERS: usize = 16_384;
/// Maximum binary body size.
pub const MAX_BODY: usize = 16_384;
/// Maximum number of logical fields; continuation lines do not add fields.
pub const MAX_FIELDS: usize = 128;
/// Maximum unfolded field value and reusable caller scratch size.
pub const MAX_FIELD: usize = 4096;

/// Transport-specific message framing rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Datagram boundaries supply an absent Content-Length.
    Udp,
    /// Content-Length is mandatory and following messages remain unconsumed.
    Tcp,
}

/// Redacted parse errors never contain message or credential bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// More stream bytes are needed; never returned for UDP.
    Incomplete,
    /// A datagram ended before its declared or syntactic boundary.
    Truncated,
    /// Invalid syntax in the interpreted message, field, or URI.
    Syntax,
    /// A fixed admission bound would be exceeded.
    Limit,
    /// Repeated singleton fields, compact aliases, or parameter names.
    Duplicate,
    /// A decimal value is absent, signed, malformed, or overflowing.
    InvalidLength,
    /// Stream framing requires a Content-Length field.
    MissingLength,
    /// URI belongs to a scheme other than SIP or SIPS.
    UnsupportedScheme,
}

#[derive(Clone, Copy, Default)]
struct Field {
    name_start: u16,
    name_end: u16,
    value_start: u16,
    value_end: u16,
}

/// Fixed span index; no owned strings, per-field allocations or global state.
pub struct Parser {
    fields: [Field; MAX_FIELDS],
    count: usize,
    start: usize,
    first_space: usize,
    second_space: usize,
    line_end: usize,
    response: bool,
    status: u16,
    header_end: usize,
    length: Option<usize>,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            fields: [Field::default(); MAX_FIELDS],
            count: 0,
            start: 0,
            first_space: 0,
            second_space: 0,
            line_end: 0,
            response: false,
            status: 0,
            header_end: 0,
            length: None,
        }
    }
}

/// A validated SIP start-line; unknown method tokens are preserved verbatim.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartLine<'a> {
    /// Request method and uninterpreted request-target, without percent decoding.
    Request {
        /// Case-sensitive method token.
        method: &'a [u8],
        /// Request-target; SIP URI interpretation is a separate operation.
        target: &'a [u8],
    },
    /// Response status and optional human-readable reason bytes.
    Response {
        /// Three-digit response status.
        status: u16,
        /// Reason phrase, which may be empty and is not a security decision.
        reason: &'a [u8],
    },
}

/// Immutable message view. Dropping it releases the parser and wire borrow.
pub struct Message<'a> {
    parser: &'a Parser,
    wire: &'a [u8],
    body_end: usize,
    consumed: usize,
    discarded: usize,
}

impl<'a> Message<'a> {
    /// Binary body, exactly the declared or datagram-implied byte count.
    pub fn body(&self) -> &'a [u8] {
        &self.wire[self.parser.header_end..self.body_end]
    }

    /// Datagram bytes consumed, or stream bytes preceding the next message.
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    /// Extra UDP suffix bytes discarded under RFC 3261 section 18.3.
    pub fn discarded(&self) -> usize {
        self.discarded
    }

    /// Validated start-line with original method, target and reason bytes.
    pub fn start_line(&self) -> StartLine<'a> {
        let p = self.parser;
        if p.response {
            StartLine::Response {
                status: p.status,
                reason: &self.wire[p.second_space + 1..p.line_end],
            }
        } else {
            StartLine::Request {
                method: &self.wire[p.start..p.first_space],
                target: &self.wire[p.first_space + 1..p.second_space],
            }
        }
    }
}

impl Parser {
    /// Parse one admitted message without allocation. TCP may be incomplete.
    pub fn parse<'a>(
        &'a mut self,
        wire: &'a [u8],
        transport: Transport,
    ) -> Result<Message<'a>, ParseError> {
        self.count = 0;
        self.length = None;
        if wire.len() > MAX_MESSAGE {
            return Err(ParseError::Limit);
        }
        if let Err(error) = self.parse_head(wire) {
            return Err(
                if error == ParseError::Incomplete && transport == Transport::Udp {
                    ParseError::Truncated
                } else {
                    error
                },
            );
        }
        let length = match (self.length, transport) {
            (Some(length), _) => length,
            (None, Transport::Udp) => wire.len() - self.header_end,
            (None, Transport::Tcp) => return Err(ParseError::MissingLength),
        };
        if length > MAX_BODY {
            return Err(ParseError::Limit);
        }
        let body_end = self.header_end + length;
        if body_end > wire.len() {
            return Err(match transport {
                Transport::Udp => ParseError::Truncated,
                Transport::Tcp => ParseError::Incomplete,
            });
        }
        let consumed = match transport {
            Transport::Udp => wire.len(),
            Transport::Tcp => body_end,
        };
        Ok(self.view(wire, body_end, consumed))
    }

    fn view<'a>(&'a self, wire: &'a [u8], body_end: usize, consumed: usize) -> Message<'a> {
        Message {
            parser: self,
            wire,
            body_end,
            consumed,
            discarded: consumed - body_end,
        }
    }

    fn parse_head(&mut self, wire: &[u8]) -> Result<(), ParseError> {
        self.count = 0;
        self.length = None;
        let mut begin = 0;
        loop {
            let end = line_end(wire, begin)?;
            if end != begin {
                self.parse_start(wire, begin, end)?;
                begin = end + 2;
                break;
            }
            begin = end + 2;
        }
        loop {
            let end = line_end(wire, begin)?;
            if end == begin {
                self.header_end = end + 2;
                break;
            }
            if matches!(wire[begin], b' ' | b'\t') {
                if self.count == 0 {
                    return Err(ParseError::Syntax);
                }
                self.fields[self.count - 1].value_end = end as u16;
            } else {
                if self.count == MAX_FIELDS {
                    return Err(ParseError::Limit);
                }
                let colon = wire[begin..end]
                    .iter()
                    .position(|b| *b == b':')
                    .map(|n| begin + n)
                    .ok_or(ParseError::Syntax)?;
                let name = trim(&wire[begin..colon]);
                if name.is_empty() || !name.iter().copied().all(token) {
                    return Err(ParseError::Syntax);
                }
                self.fields[self.count] = Field {
                    name_start: begin as u16,
                    name_end: (begin + name.len()) as u16,
                    value_start: (colon + 1) as u16,
                    value_end: end as u16,
                };
                self.count += 1;
            }
            begin = end + 2;
        }
        for field in &self.fields[..self.count] {
            let value = &wire[field.value_start as usize..field.value_end as usize];
            if unfolded(value).count() > MAX_FIELD {
                return Err(ParseError::Limit);
            }
            let name = &wire[field.name_start as usize..field.name_end as usize];
            if name.eq_ignore_ascii_case(b"Content-Length") || name.eq_ignore_ascii_case(b"l") {
                if self.length.is_some() {
                    return Err(ParseError::Duplicate);
                }
                self.length = Some(decimal(unfolded(value), MAX_BODY)?);
            }
        }
        Ok(())
    }

    fn parse_start(&mut self, wire: &[u8], begin: usize, end: usize) -> Result<(), ParseError> {
        let line = &wire[begin..end];
        let first = line
            .iter()
            .position(|b| *b == b' ')
            .ok_or(ParseError::Syntax)?;
        let second = line[first + 1..]
            .iter()
            .position(|b| *b == b' ')
            .map(|n| first + 1 + n)
            .ok_or(ParseError::Syntax)?;
        self.response = line[..first].eq_ignore_ascii_case(b"SIP/2.0");
        if self.response {
            let code = &line[first + 1..second];
            if code.len() != 3 || !code.iter().all(u8::is_ascii_digit) {
                return Err(ParseError::Syntax);
            }
            self.status = (code[0] - b'0') as u16 * 100
                + (code[1] - b'0') as u16 * 10
                + (code[2] - b'0') as u16;
            reason_phrase(&line[second + 1..])?;
        } else if first == 0
            || !line[..first].iter().copied().all(token)
            || second == first + 1
            || line[first + 1..second]
                .iter()
                .any(|b| !(0x21..=0x7e).contains(b))
            || !line[second + 1..].eq_ignore_ascii_case(b"SIP/2.0")
        {
            return Err(ParseError::Syntax);
        }
        self.start = begin;
        self.first_space = begin + first;
        self.second_space = begin + second;
        self.line_end = end;
        Ok(())
    }
}

fn line_end(wire: &[u8], begin: usize) -> Result<usize, ParseError> {
    let mut at = begin;
    while at < wire.len().min(MAX_HEADERS) {
        match wire[at] {
            b'\r' => match wire.get(at + 1) {
                Some(b'\n') => {
                    return if at + 2 <= MAX_HEADERS {
                        Ok(at)
                    } else {
                        Err(ParseError::Limit)
                    };
                }
                Some(_) => return Err(ParseError::Syntax),
                None => break,
            },
            b'\n' => return Err(ParseError::Syntax),
            _ => at += 1,
        }
    }
    Err(if wire.len() >= MAX_HEADERS {
        ParseError::Limit
    } else {
        ParseError::Incomplete
    })
}

fn trim(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        value = &value[1..];
    }
    while value.last().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte)
}

// Each CRLF followed by WSP becomes one SP. Ordinary and escaped bytes survive.
fn unfolded(mut value: &[u8]) -> impl Iterator<Item = u8> + '_ {
    std::iter::from_fn(move || {
        let byte = *value.first()?;
        value = &value[1..];
        if byte == b'\r' {
            value = &value[1..];
            while value.first().is_some_and(|b| matches!(b, b' ' | b'\t')) {
                value = &value[1..];
            }
            Some(b' ')
        } else {
            Some(byte)
        }
    })
}

fn decimal(value: impl Iterator<Item = u8>, maximum: usize) -> Result<usize, ParseError> {
    let mut result = 0usize;
    let mut started = false;
    let mut ended = false;
    for byte in value {
        if matches!(byte, b' ' | b'\t') {
            ended |= started;
        } else {
            if !byte.is_ascii_digit() || ended {
                return Err(ParseError::InvalidLength);
            }
            result = result
                .checked_mul(10)
                .and_then(|n| n.checked_add((byte - b'0') as usize))
                .ok_or(ParseError::InvalidLength)?;
            started = true;
        }
    }
    if !started {
        return Err(ParseError::InvalidLength);
    }
    if result > maximum {
        return Err(ParseError::Limit);
    }
    Ok(result)
}

// RFC 3261 section 25 defines an octet grammar, not a decoded Unicode string.
// In particular UTF8-CONT alone and RFC 2279's historical widths are admitted.
// Preserve these display-only bytes; never interpret them as routing data.
fn reason_phrase(mut value: &[u8]) -> Result<(), ParseError> {
    while let Some(&byte) = value.first() {
        let count = match byte {
            b'%' => {
                if value.len() < 3 || !value[1..3].iter().all(u8::is_ascii_hexdigit) {
                    return Err(ParseError::Syntax);
                }
                3
            }
            0x80..=0xbf => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            0xf8..=0xfb => 5,
            0xfc..=0xfd => 6,
            _ if byte.is_ascii_alphanumeric() || b"-_.!~*'();/?:@&=+$, \t".contains(&byte) => 1,
            _ => return Err(ParseError::Syntax),
        };
        if byte >= 0xc0
            && (value.len() < count || !value[1..count].iter().all(|b| (0x80..=0xbf).contains(b)))
        {
            return Err(ParseError::Syntax);
        }
        value = &value[count..];
    }
    Ok(())
}

/// Incremental single-frame TCP owner, borrowing one admitted buffer generation.
///
/// `push` consumes only through this message, leaving pipeline bytes to the
/// caller. `reset` ends the generation; Rust borrows prevent reuse while a
/// message view is live. No retained raw pointer or caller-supplied cursor exists.
pub struct TcpDecoder<'a> {
    wire: &'a mut [u8; MAX_MESSAGE],
    parser: Parser,
    used: usize,
    line_start: usize,
    saw_start: bool,
    expected: Option<usize>,
    error: Option<ParseError>,
}

impl<'a> TcpDecoder<'a> {
    /// Lease caller-admitted storage without clearing or allocating a duplicate.
    pub fn new(wire: &'a mut [u8; MAX_MESSAGE]) -> Self {
        Self {
            wire,
            parser: Parser::default(),
            used: 0,
            line_start: 0,
            saw_start: false,
            expected: None,
            error: None,
        }
    }

    /// Consume at most one frame. An error stays terminal until explicit reset.
    pub fn push(&mut self, bytes: &[u8]) -> Result<usize, ParseError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = self.append(bytes);
        self.error = result.as_ref().err().copied();
        result
    }

    fn append(&mut self, bytes: &[u8]) -> Result<usize, ParseError> {
        let mut taken = 0;
        while self.expected.is_none() && taken < bytes.len() {
            if self.used == MAX_HEADERS {
                return Err(ParseError::Limit);
            }
            let byte = bytes[taken];
            if (self.used > 0 && self.wire[self.used - 1] == b'\r' && byte != b'\n')
                || (byte == b'\n' && (self.used == 0 || self.wire[self.used - 1] != b'\r'))
            {
                return Err(ParseError::Syntax);
            }
            self.wire[self.used] = byte;
            self.used += 1;
            taken += 1;
            if byte == b'\n' {
                if self.used - self.line_start == 2 {
                    if self.saw_start {
                        self.parser.parse_head(&self.wire[..self.used])?;
                        let length = self.parser.length.ok_or(ParseError::MissingLength)?;
                        self.expected = Some(self.used + length);
                    }
                } else {
                    self.saw_start = true;
                }
                self.line_start = self.used;
            }
        }
        if self.expected.is_none() && self.used == MAX_HEADERS {
            return Err(ParseError::Limit);
        }
        if let Some(expected) = self.expected {
            let count = (expected - self.used).min(bytes.len() - taken);
            self.wire[self.used..self.used + count].copy_from_slice(&bytes[taken..taken + count]);
            self.used += count;
            taken += count;
        }
        Ok(taken)
    }

    /// Borrow the completed message; invalid or partial messages have no view.
    pub fn message(&self) -> Option<Message<'_>> {
        if self.error.is_some() || self.expected != Some(self.used) {
            return None;
        }
        Some(
            self.parser
                .view(&self.wire[..self.used], self.used, self.used),
        )
    }

    /// End this buffer generation before accepting the next message.
    pub fn reset(&mut self) {
        self.used = 0;
        self.line_start = 0;
        self.saw_start = false;
        self.expected = None;
        self.error = None;
        self.parser.count = 0;
        self.parser.length = None;
    }
}

#[cfg(test)]
mod tests;
