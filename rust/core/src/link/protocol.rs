//! Strict bounded parsing of the external ASL text protocol.
use super::identity;

/// One validated linked-node advertisement entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    /// ASL route marker: T, R, C, or L.
    pub mode: char,
    /// Remote identity, independent of direct IAX numeric admission.
    pub node: String,
}

/// A complete recognized ASL control text message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Protocol {
    /// Complete replacement topology.
    Topology(Vec<Route>),
    /// Legacy redundant key-state handshake request.
    NewKey,
    /// Initial key-state handshake request.
    NewKey1,
    /// Broadcast keyed-source query.
    Query {
        /// Requesting node.
        requester: String,
    },
    /// Directed keyed-source reply.
    Key {
        /// Reply destination.
        destination: String,
        /// Reporting node.
        source: String,
        /// Current carrier state.
        keyed: bool,
        /// Seconds since the last carrier edge.
        age_seconds: u64,
    },
    /// Addressed AllStarLink remote-control digit.
    RemoteDigit {
        /// Node that should receive the digit.
        destination: String,
        /// Node that transmitted the digit.
        source: String,
        /// Sender's remote-DTMF sequence number.
        sequence: i32,
        /// The completed DTMF symbol.
        digit: char,
    },
    /// Supported IAX key capability request.
    IaxKey,
    /// Explicit remote teardown.
    Disconnect,
}

impl Protocol {
    /// Parse the entire payload, allowing exactly one optional trailing NUL.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        if bytes.len() > 10002 || bytes.contains(&0) {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        match text {
            "!NEWKEY!" => return Some(Self::NewKey),
            "!NEWKEY1!" => return Some(Self::NewKey1),
            "!IAXKEY!" => return Some(Self::IaxKey),
            "!!DISCONNECT!!" => return Some(Self::Disconnect),
            "L" | "L " => return Some(Self::Topology(Vec::new())),
            _ => {}
        }
        if let Some(payload) = text.strip_prefix("L ") {
            let mut routes = Vec::new();
            for token in payload.split(',') {
                let mode = *token.as_bytes().first()?;
                let name = token.get(1..)?;
                if !b"TRCL".contains(&mode)
                    || name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    return None;
                }
                routes.push(Route {
                    mode: mode as char,
                    node: name.to_owned(),
                });
            }
            return Some(Self::Topology(routes));
        }
        // All five required fields below are nonempty after this filtering.
        let mut tokens = text.split(' ').filter(|token| !token.is_empty());
        let [kind, destination, source, keyed, age] = [
            tokens.next()?,
            tokens.next()?,
            tokens.next()?,
            tokens.next()?,
            tokens.next()?,
        ];
        if tokens.next().is_some() || !identity(source) {
            return None;
        }
        match kind {
            "K?" if destination == "*" && keyed == "0" && age == "0" => Some(Self::Query {
                requester: source.to_owned(),
            }),
            "K" if identity(destination)
                && matches!(keyed, "0" | "1")
                && age.bytes().all(|b| b.is_ascii_digit()) =>
            {
                Some(Self::Key {
                    destination: destination.to_owned(),
                    source: source.to_owned(),
                    keyed: keyed == "1",
                    age_seconds: age.parse().ok()?,
                })
            }
            "D" if identity(destination)
                && keyed.parse::<i32>().is_ok()
                && age.len() == 1
                && b"0123456789*#ABCD".contains(&age.as_bytes()[0]) =>
            {
                Some(Self::RemoteDigit {
                    destination: destination.to_owned(),
                    source: source.to_owned(),
                    sequence: keyed.parse().ok()?,
                    digit: age.as_bytes()[0] as char,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Protocol;

    #[test]
    fn distinguishes_newkey1_from_the_legacy_newkey_handshake() {
        assert_eq!(Protocol::parse(b"!NEWKEY!"), Some(Protocol::NewKey));
        assert_eq!(Protocol::parse(b"!NEWKEY1!"), Some(Protocol::NewKey1));
    }

    #[test]
    fn parses_addressed_asl_remote_dtmf_text() {
        assert_eq!(
            Protocol::parse(b"D 1000 2000 17 5"),
            Some(Protocol::RemoteDigit {
                destination: "1000".into(),
                source: "2000".into(),
                sequence: 17,
                digit: '5',
            })
        );
    }

    #[test]
    fn rejects_malformed_asl_remote_dtmf_text() {
        for input in [
            &b"D 1000 2000 x 5"[..],
            b"D bad! 2000 1 5",
            b"D 1000 2000 1 E",
            b"D 1000 2000 1 55",
            b"D 1000 2000 1 5 extra",
        ] {
            assert_eq!(Protocol::parse(input), None, "{input:?}");
        }
    }
}
