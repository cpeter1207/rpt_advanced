//! Decode owned JSON values with unambiguous object membership.

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;

mod borrowed;

pub use borrowed::{BorrowedChildren, BorrowedKey, BorrowedValue, decode_borrowed};

/// JSON syntax or structure error with an input location and no supplied text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// One-based source line.
    pub line: usize,
    /// Source column reported by the decoder.
    pub column: usize,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid JSON or duplicate object key at line {}, column {}",
            self.line, self.column,
        )
    }
}

impl std::error::Error for DecodeError {}

/// Decode exactly one JSON value, preserving actual token types and member names.
///
/// Duplicate member names are rejected at every depth.
pub fn decode_value(input: &[u8]) -> Result<Value, DecodeError> {
    decode_subtree(input, raw_root(input)?, 1)
}

fn raw_root(input: &[u8]) -> Result<&RawValue, DecodeError> {
    serde_json::from_slice(input).map_err(|error| DecodeError {
        line: error.line(),
        column: error.column(),
    })
}

// Serde checks this limit while building values. Borrowed raw values skip that
// recursive descent, so keep the same bound when expanding their containers.
const MAX_CONTAINER_DEPTH: usize = 127;

enum RawChildren<'a, K> {
    Object(RawObject<'a, K>),
    Array(Vec<&'a RawValue>),
    Scalar,
}

fn raw_children<'a, K: Deserialize<'a> + Ord>(
    input: &[u8],
    value: &'a RawValue,
    depth: usize,
) -> Result<RawChildren<'a, K>, DecodeError> {
    let raw = value.get();
    let parse_error = |error: serde_json::Error| error_at(input, raw, error.line(), error.column());

    // Dispatch from the actual token to preserve literal member names that
    // happen to match a Serde-internal arbitrary-precision token.
    match raw.as_bytes()[0] {
        b'{' | b'[' if depth > MAX_CONTAINER_DEPTH => Err(error_at(input, raw, 1, 1)),
        b'{' => serde_json::from_str(raw)
            .map(RawChildren::Object)
            .map_err(parse_error),
        b'[' => serde_json::from_str(raw)
            .map(RawChildren::Array)
            .map_err(parse_error),
        _ => Ok(RawChildren::Scalar),
    }
}

fn decode_subtree(input: &[u8], value: &RawValue, depth: usize) -> Result<Value, DecodeError> {
    match raw_children::<String>(input, value, depth)? {
        RawChildren::Object(RawObject(fields)) => fields
            .into_iter()
            .map(|(key, child)| Ok((key, decode_subtree(input, child, depth + 1)?)))
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
        RawChildren::Array(items) => items
            .into_iter()
            .map(|child| decode_subtree(input, child, depth + 1))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        RawChildren::Scalar => serde_json::from_str(value.get())
            .map_err(|error| error_at(input, value.get(), error.line(), error.column())),
    }
}

fn error_at(input: &[u8], raw: &str, line: usize, column: usize) -> DecodeError {
    // RawValue borrows its text from the input, including nested values.
    let offset = raw.as_ptr().addr() - input.as_ptr().addr();
    let prefix = &input[..offset];
    let column = if line == 1 {
        column
            + prefix
                .iter()
                .rev()
                .take_while(|&&byte| byte != b'\n')
                .count()
    } else {
        column
    };
    DecodeError {
        line: line + prefix.iter().filter(|&&byte| byte == b'\n').count(),
        column,
    }
}

struct RawObject<'a, K>(BTreeMap<K, &'a RawValue>);

impl<'de, K: Deserialize<'de> + Ord> Deserialize<'de> for RawObject<'de, K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawObjectVisitor(std::marker::PhantomData))
    }
}

struct RawObjectVisitor<K>(std::marker::PhantomData<K>);

impl<'de, K: Deserialize<'de> + Ord> Visitor<'de> for RawObjectVisitor<K> {
    type Value = RawObject<'de, K>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = BTreeMap::new();
        while let Some(key) = map.next_key::<K>()? {
            match fields.entry(key) {
                Entry::Occupied(_) => return Err(de::Error::custom("duplicate JSON object key")),
                Entry::Vacant(entry) => entry.insert(map.next_value::<&RawValue>()?),
            };
        }
        Ok(RawObject(fields))
    }
}
