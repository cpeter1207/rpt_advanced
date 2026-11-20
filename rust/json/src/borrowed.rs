use super::{DecodeError, RawChildren, RawObject, error_at, raw_children, raw_root};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::{borrow::Borrow, collections::BTreeMap};
use zeroize::Zeroize;

/// A JSON node whose original token still borrows the input.
///
/// Scalar values remain encoded. The caller can inspect structure without
/// creating owned copies of private scalar data.
pub struct BorrowedValue<'a> {
    raw: &'a RawValue,
    /// Parsed containers and undecoded scalar leaves.
    pub children: BorrowedChildren<'a>,
}

impl BorrowedValue<'_> {
    /// Return the original JSON token or complete container.
    pub fn raw(&self) -> &str {
        self.raw.get()
    }
}

/// Structure of a borrowed JSON node.
pub enum BorrowedChildren<'a> {
    /// Object with decoded, unambiguous member names.
    Object(BTreeMap<BorrowedKey, BorrowedValue<'a>>),
    /// Array in authored order.
    Array(Vec<BorrowedValue<'a>>),
    /// String, number, boolean or null stored as borrowed input.
    Scalar,
}

/// Decoded member name with owned storage erased on drop.
///
/// A caller may discover that a whole subtree is private after parsing its
/// structure. Its decoded member names are protected until that decision.
#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub struct BorrowedKey(String);

impl BorrowedKey {
    /// View the decoded name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for BorrowedKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for BorrowedKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl Drop for BorrowedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Inspect exactly one JSON value, rejecting duplicate members and retaining
/// scalar tokens as borrowed input.
pub fn decode_borrowed(input: &[u8]) -> Result<BorrowedValue<'_>, DecodeError> {
    borrow_subtree(input, raw_root(input)?, 1)
}

fn borrow_subtree<'a>(
    input: &[u8],
    raw: &'a RawValue,
    depth: usize,
) -> Result<BorrowedValue<'a>, DecodeError> {
    let children = match raw_children::<BorrowedKey>(input, raw, depth)? {
        RawChildren::Object(RawObject(fields)) => BorrowedChildren::Object(
            fields
                .into_iter()
                .map(|(key, child)| Ok((key, borrow_subtree(input, child, depth + 1)?)))
                .collect::<Result<_, _>>()?,
        ),
        RawChildren::Array(items) => BorrowedChildren::Array(
            items
                .into_iter()
                .map(|child| borrow_subtree(input, child, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        RawChildren::Scalar => {
            // RawValue validates escape syntax without checking surrogate pairs.
            // Check them without decoding private text into an owned string.
            let token = raw.get();
            if token.starts_with('"') && !valid_surrogate_pairs(token) {
                return Err(error_at(input, token, 1, 1));
            }
            BorrowedChildren::Scalar
        }
    };
    Ok(BorrowedValue { raw, children })
}

fn valid_surrogate_pairs(mut token: &str) -> bool {
    while let Some((_, tail)) = token.split_once('\\') {
        let Some(hex) = tail.strip_prefix('u') else {
            token = &tail[1..];
            continue;
        };
        // RawValue already checked that Unicode escapes have four hex digits.
        let (unit, rest) = hex.split_at(4);
        let unit = u16::from_str_radix(unit, 16).expect("validated Unicode escape");
        token = rest;
        if (0xdc00..=0xdfff).contains(&unit) {
            return false;
        }
        if (0xd800..=0xdbff).contains(&unit) {
            let Some(low_escape) = rest.strip_prefix("\\u") else {
                return false;
            };
            let (low, rest) = low_escape.split_at(4);
            let low = u16::from_str_radix(low, 16).expect("validated Unicode escape");
            if !(0xdc00..=0xdfff).contains(&low) {
                return false;
            }
            token = rest;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{BorrowedChildren, decode_borrowed};
    use crate::decode_value;
    use serde_json::json;

    #[test]
    fn checks_surrogate_pairs_without_decoding_borrowed_strings() {
        for (token, expected) in [
            (r#""\uD83D\uDE00""#, Some("😀")),
            (r#""\\uD83D\\uDE00""#, Some(r"\uD83D\uDE00")),
            (r#""synthetic-marker\uDE00""#, None),
            (r#""synthetic-marker\uD83D""#, None),
            (r#""synthetic-marker\uD83D\u0041""#, None),
        ] {
            let request = format!(r#"{{"value":[{token}]}}"#);
            let input = request.as_bytes();
            if let Some(decoded) = expected {
                let root = decode_borrowed(input).unwrap();
                let BorrowedChildren::Object(fields) = root.children else {
                    panic!("expected object");
                };
                let BorrowedChildren::Array(items) = &fields["value"].children else {
                    panic!("expected array");
                };
                assert!(matches!(items[0].children, BorrowedChildren::Scalar));
                assert_eq!(items[0].raw(), token);
                assert_eq!(decode_value(input).unwrap(), json!({"value": [decoded]}));
            } else {
                let error = decode_borrowed(input).err().expect("invalid surrogate");
                assert!(decode_value(input).is_err());
                assert!(!format!("{error:?} {error}").contains("synthetic-marker"));
            }
        }
    }
}
