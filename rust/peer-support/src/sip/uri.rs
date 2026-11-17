//! SIP URI syntax from RFC 3261 section 25 with RFC 5954 IPv6 correction.
use super::{MAX_FIELD, ParseError, decimal, token};
use std::net::{Ipv4Addr, Ipv6Addr};

/// URI security scheme; syntax recognition never implies supported routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Ordinary SIP URI; transport policy is checked separately.
    Sip,
    /// Secure SIP URI, never silently changed to SIP.
    Sips,
}

/// Parsed host, without DNS resolution or URI normalization.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Host<'a> {
    /// ASCII domain name with original case and optional final dot.
    Name(&'a [u8]),
    /// Numeric IPv4 address.
    Ipv4(Ipv4Addr),
    /// Numeric IPv6 address, originally enclosed in brackets.
    Ipv6(Ipv6Addr),
}

/// Explicit supported-profile rejection, distinct from malformed URI syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// Secure SIP routing is not implemented by this unencrypted profile.
    SecureRouting,
    /// URI-requested headers may not inject routing, authentication or body data.
    UriHeaders,
    /// Only UDP and TCP transport are admitted by this narrow profile.
    Transport,
    /// Port zero is not a remote service destination.
    Port,
}

/// Borrowed SIP URI components. No decoding, logging or credential retention.
pub struct SipUri<'a> {
    scheme: Scheme,
    user: Option<&'a [u8]>,
    password: Option<&'a [u8]>,
    host: Host<'a>,
    port: Option<u16>,
    parameters: &'a [u8],
    headers: Option<&'a [u8]>,
}

impl<'a> SipUri<'a> {
    /// Validate bounded SIP/SIPS syntax without network or policy side effects.
    pub fn parse(value: &'a [u8]) -> Result<Self, ParseError> {
        if value.len() > MAX_FIELD {
            return Err(ParseError::Limit);
        }
        let colon = value
            .iter()
            .position(|b| *b == b':')
            .ok_or(ParseError::Syntax)?;
        let scheme = if value[..colon].eq_ignore_ascii_case(b"sip") {
            Scheme::Sip
        } else if value[..colon].eq_ignore_ascii_case(b"sips") {
            Scheme::Sips
        } else {
            return Err(ParseError::UnsupportedScheme);
        };
        let mut rest = &value[colon + 1..];
        let mut user = None;
        let mut password = None;
        // Userinfo admits literal ';', '?' and ','; find '@' before host suffixes.
        if let Some(at) = rest.iter().position(|b| *b == b'@') {
            let info = &rest[..at];
            let separator = info.iter().position(|b| *b == b':').unwrap_or(info.len());
            let name = &info[..separator];
            if name.is_empty() || !component(name, b"&=+$,;?/") {
                return Err(ParseError::Syntax);
            }
            user = Some(name);
            if separator < info.len() {
                let secret = &info[separator + 1..];
                if !component(secret, b"&=+$,") {
                    return Err(ParseError::Syntax);
                }
                password = Some(secret);
            }
            rest = &rest[at + 1..];
        }
        let end = rest
            .iter()
            .position(|b| matches!(b, b';' | b'?'))
            .unwrap_or(rest.len());
        let (host, port) = host_port(&rest[..end])?;
        rest = &rest[end..];
        let question = rest.iter().position(|b| *b == b'?').unwrap_or(rest.len());
        let parameters = &rest[..question];
        validate_uri_parameters(parameters)?;
        let headers = if question < rest.len() {
            let headers = &rest[question + 1..];
            for header in headers.split(|b| *b == b'&') {
                let equal = header
                    .iter()
                    .position(|b| *b == b'=')
                    .ok_or(ParseError::Syntax)?;
                if equal == 0
                    || !component(&header[..equal], b"[]/?:+$")
                    || !component(&header[equal + 1..], b"[]/?:+$")
                {
                    return Err(ParseError::Syntax);
                }
            }
            Some(headers)
        } else {
            None
        };
        Ok(Self {
            scheme,
            user,
            password,
            host,
            port,
            parameters,
            headers,
        })
    }

    /// Explicit syntax scheme; no automatic secure-to-cleartext conversion.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }
    /// Original escaped user bytes, never percent decoded.
    pub fn user(&self) -> Option<&'a [u8]> {
        self.user
    }
    /// Original escaped password bytes; callers must not log this value.
    pub fn password(&self) -> Option<&'a [u8]> {
        self.password
    }
    /// Host parsed without resolving it or changing ownership of routing policy.
    pub fn host(&self) -> Host<'a> {
        self.host
    }
    /// Explicit checked 16-bit service port, or absent for provider defaults.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    /// Uninterpreted URI header syntax; never emitted as request headers here.
    pub fn headers(&self) -> Option<&'a [u8]> {
        self.headers
    }

    /// Find a unique parameter case-insensitively, equating percent escapes
    /// only for non-reserved bytes as specified by RFC 3261 section 19.1.4.
    /// Outer `None` means absent; inner `None` means a valueless flag.
    pub fn parameter(&self, name: &[u8]) -> Option<Option<&'a [u8]>> {
        parameters(self.parameters)
            .find(|(key, _)| equivalent(key, name))
            .map(|(_, value)| value)
    }

    /// Check only the narrow cleartext profile, never authorize a call or route.
    /// URI headers are deliberately unsupported, including encoded CR/LF names.
    pub fn require_cleartext_routing(&self) -> Result<(), RouteError> {
        if self.scheme == Scheme::Sips {
            return Err(RouteError::SecureRouting);
        }
        if self.headers.is_some() {
            return Err(RouteError::UriHeaders);
        }
        if let Some(Some(transport)) = self.parameter(b"transport") {
            if !equivalent(transport, b"udp") && !equivalent(transport, b"tcp") {
                return Err(RouteError::Transport);
            }
        }
        if self.port == Some(0) {
            return Err(RouteError::Port);
        }
        Ok(())
    }
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte)
}

fn component(mut value: &[u8], extra: &[u8]) -> bool {
    while let Some(byte) = value.first() {
        if *byte == b'%' {
            if value.len() < 3 || !value[1..3].iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            value = &value[3..];
        } else {
            if !unreserved(*byte) && !extra.contains(byte) {
                return false;
            }
            value = &value[1..];
        }
    }
    true
}

pub(super) fn host(value: &[u8]) -> Result<Host<'_>, ParseError> {
    if value.starts_with(b"[") && value.ends_with(b"]") {
        let ip = std::str::from_utf8(&value[1..value.len() - 1]).map_err(|_| ParseError::Syntax)?;
        return ip
            .parse::<Ipv6Addr>()
            .map(Host::Ipv6)
            .map_err(|_| ParseError::Syntax);
    }
    // RFC 3261's standalone IPv4 grammar admits decimal leading zeros.
    // Embedded IPv4 in IPv6 instead follows the RFC 3986 grammar via Ipv6Addr.
    if value.iter().all(|b| b.is_ascii_digit() || *b == b'.') {
        let mut octets = [0; 4];
        let mut parts = value.split(|b| *b == b'.');
        for octet in &mut octets {
            let part = parts.next().ok_or(ParseError::Syntax)?;
            if part.is_empty() || part.len() > 3 {
                return Err(ParseError::Syntax);
            }
            *octet = decimal(part.iter().copied(), 255)? as u8;
        }
        if parts.next().is_some() {
            return Err(ParseError::Syntax);
        }
        return Ok(Host::Ipv4(Ipv4Addr::from(octets)));
    }
    let name = value.strip_suffix(b".").unwrap_or(value);
    let mut labels = name.split(|b| *b == b'.').peekable();
    while let Some(label) = labels.next() {
        if !label.first().is_some_and(u8::is_ascii_alphanumeric)
            || !label.last().is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            || (labels.peek().is_none() && !label[0].is_ascii_alphabetic())
        {
            return Err(ParseError::Syntax);
        }
    }
    Ok(Host::Name(value))
}

fn host_port(value: &[u8]) -> Result<(Host<'_>, Option<u16>), ParseError> {
    let end = if value.starts_with(b"[") {
        value
            .iter()
            .position(|b| *b == b']')
            .map(|n| n + 1)
            .ok_or(ParseError::Syntax)?
    } else {
        value.iter().position(|b| *b == b':').unwrap_or(value.len())
    };
    let host = host(&value[..end])?;
    let tail = &value[end..];
    let port = if tail.is_empty() {
        None
    } else {
        if tail[0] != b':' || tail.len() == 1 || !tail[1..].iter().all(u8::is_ascii_digit) {
            return Err(ParseError::Syntax);
        }
        Some(decimal(tail[1..].iter().copied(), 65_535)? as u16)
    };
    Ok((host, port))
}

fn parameters(value: &[u8]) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
    value.split(|b| *b == b';').skip(1).map(|parameter| {
        match parameter.iter().position(|b| *b == b'=') {
            Some(equal) => (&parameter[..equal], Some(&parameter[equal + 1..])),
            None => (parameter, None),
        }
    })
}

fn validate_uri_parameters(value: &[u8]) -> Result<(), ParseError> {
    for (index, (name, part)) in parameters(value).enumerate() {
        if name.is_empty() || !component(name, b"[]/:&+$") {
            return Err(ParseError::Syntax);
        }
        // Bounded by a 4096-byte URI, with no auxiliary collection or allocation.
        if parameters(value)
            .take(index)
            .any(|(prior, _)| equivalent(prior, name))
        {
            return Err(ParseError::Duplicate);
        }
        if equivalent(name, b"lr") {
            if part.is_some() {
                return Err(ParseError::Syntax);
            }
        } else if [b"transport".as_slice(), b"user", b"method"]
            .iter()
            .any(|known| equivalent(name, known))
        {
            if !part.is_some_and(|part| !part.is_empty() && part.iter().copied().all(token)) {
                return Err(ParseError::Syntax);
            }
        } else if equivalent(name, b"ttl") {
            let part = part.ok_or(ParseError::Syntax)?;
            if part.is_empty() || part.len() > 3 || !part.iter().all(u8::is_ascii_digit) {
                return Err(ParseError::Syntax);
            }
            decimal(part.iter().copied(), 255)?;
        } else if equivalent(name, b"maddr") {
            host(part.ok_or(ParseError::Syntax)?)?;
        } else if part.is_some_and(|part| part.is_empty() || !component(part, b"[]/:&+$")) {
            return Err(ParseError::Syntax);
        }
    }
    Ok(())
}

// Parameter comparison is single-pass decoding into tagged units, not text.
// An encoded reserved byte must remain distinct from both its literal form and
// a decoded percent followed by hex characters (e.g. %2f versus %252f).
fn equivalent(first: &[u8], second: &[u8]) -> bool {
    fn units(mut value: &[u8]) -> impl Iterator<Item = (u8, bool)> + '_ {
        std::iter::from_fn(move || {
            let byte = *value.first()?;
            value = &value[1..];
            if byte == b'%' && value.len() >= 2 {
                let hex = |byte: u8| (byte as char).to_digit(16);
                if let (Some(high), Some(low)) = (hex(value[0]), hex(value[1])) {
                    let decoded = (high * 16 + low) as u8;
                    value = &value[2..];
                    // RFC 3261 19.1.4 excludes RFC 2396's reserved set from
                    // equivalence. Non-reserved bytes, including %, are decoded
                    // once; comparison never interprets the resulting unit again.
                    return Some((
                        decoded.to_ascii_lowercase(),
                        b";/?:@&=+$,".contains(&decoded),
                    ));
                }
            }
            Some((byte.to_ascii_lowercase(), false))
        })
    }
    units(first).eq(units(second))
}
