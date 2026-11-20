//! Decode owned JSON values with unambiguous object membership.

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;

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
    let root: &RawValue = serde_json::from_slice(input).map_err(|error| DecodeError {
        line: error.line(),
        column: error.column(),
    })?;
    decode_subtree(input, root, 1)
}

// Serde checks this limit while building values. Borrowed raw values skip that
// recursive descent, so keep the same bound when expanding their containers.
const MAX_CONTAINER_DEPTH: usize = 127;

fn decode_subtree(input: &[u8], value: &RawValue, depth: usize) -> Result<Value, DecodeError> {
    let raw = value.get();
    let parse_error = |error: serde_json::Error| error_at(input, raw, error.line(), error.column());

    // Dispatch from the actual token. Serde's arbitrary-precision visitor
    // represents a large number as a map internally, but object names here
    // may contain any string, including the name used for that internal token.
    match raw.as_bytes()[0] {
        b'{' | b'[' if depth > MAX_CONTAINER_DEPTH => Err(error_at(input, raw, 1, 1)),
        b'{' => {
            let RawObject(fields) = serde_json::from_str(raw).map_err(parse_error)?;
            fields
                .into_iter()
                .map(|(key, value)| Ok((key, decode_subtree(input, value, depth + 1)?)))
                .collect::<Result<Map<_, _>, _>>()
                .map(Value::Object)
        }
        b'[' => {
            let items: Vec<&RawValue> = serde_json::from_str(raw).map_err(parse_error)?;
            items
                .into_iter()
                .map(|value| decode_subtree(input, value, depth + 1))
                .collect::<Result<_, _>>()
                .map(Value::Array)
        }
        _ => serde_json::from_str(raw).map_err(parse_error),
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

struct RawObject<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for RawObject<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawObjectVisitor)
    }
}

struct RawObjectVisitor;

impl<'de> Visitor<'de> for RawObjectVisitor {
    type Value = RawObject<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            match fields.entry(key) {
                Entry::Occupied(_) => return Err(de::Error::custom("duplicate JSON object key")),
                Entry::Vacant(entry) => entry.insert(map.next_value::<&RawValue>()?),
            };
        }
        Ok(RawObject(fields))
    }
}
