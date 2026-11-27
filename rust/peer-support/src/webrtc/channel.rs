//! Bounded messages on the negotiated controller data channels.

use super::{MediaAnchor, ReceiveLease};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{ops::Range, time::Instant};

const MAX_CONTROL_BYTES: usize = 4096;
pub(super) const MAX_TIMING_BYTES: usize = 512;

/// An unsigned clock coordinate that round-trips through browser JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coordinate(pub u64);

impl Serialize for Coordinate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Coordinate {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        let text = String::deserialize(decoder)?;
        let invalid = || serde::de::Error::custom("invalid unsigned clock coordinate");
        if text.is_empty()
            || text.len() > 20
            || !text.bytes().all(|byte| byte.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return Err(invalid());
        }
        text.parse().map(Self).map_err(|_| invalid())
    }
}

/// Scope of a measured nanosecond coordinate in a media anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBasis {
    SessionMonotonic,
    Tai,
}

/// Verified RTP and codec-input coordinates for one rendered media path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub binding: String,
    pub ssrc: u32,
    pub epoch: Coordinate,
    pub source_generation: Coordinate,
    pub source_sample: Coordinate,
    pub rtp_timestamp: Coordinate,
    pub time_basis: Option<TimeBasis>,
    pub time_ns: Option<Coordinate>,
    pub uncertainty_ns: Option<Coordinate>,
}

impl Reference {
    pub fn positions(&self) -> MediaAnchor {
        MediaAnchor {
            source_generation: self.source_generation.0,
            media_epoch: self.epoch.0,
            source_sample: self.source_sample.0,
            rtp_timestamp: self.rtp_timestamp.0,
        }
    }
}

/// Reliable ordered frame. Receipt alone never changes station policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Ready {
        binding: String,
        ice_generation: Coordinate,
    },
    RequestTalk {
        ice_generation: Coordinate,
    },
    RequestObservedTalk {
        ice_generation: Coordinate,
    },
    ObservedRtp {
        reference: Reference,
        received_at_ns: Coordinate,
    },
    TalkGranted {
        grant: Coordinate,
        epoch: Coordinate,
    },
    ReleaseTalk {
        grant: Coordinate,
        epoch: Coordinate,
    },
    Anchor {
        reference: Reference,
    },
}

impl Control {
    pub fn decode(input: &[u8]) -> Option<Self> {
        decode(input, MAX_CONTROL_BYTES)
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        encode(self, MAX_CONTROL_BYTES)
    }
}

/// A sample range received on the expiring timing channel.
/// Use a verified local deadline when converting it to an admitted lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub grant: Coordinate,
    pub epoch: Coordinate,
    pub sequence: Coordinate,
    pub start: Coordinate,
    pub end: Coordinate,
}

impl Lease {
    pub fn decode(input: &[u8]) -> Option<Self> {
        decode(input, MAX_TIMING_BYTES)
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        encode(self, MAX_TIMING_BYTES)
    }

    pub fn with_local_expiry(self, expires_at: Instant) -> Option<ReceiveLease> {
        let samples: Range<u64> = self.start.0..self.end.0;
        (!samples.is_empty()).then_some(ReceiveLease {
            grant: self.grant.0,
            media_epoch: self.epoch.0,
            sequence: self.sequence.0,
            samples,
            expires_at,
        })
    }
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(input: &[u8], bound: usize) -> Option<T> {
    if input.is_empty() || input.len() > bound {
        return None;
    }
    let value = rptadv_json::decode_value(input).ok()?;
    serde_json::from_value(value).ok()
}

pub(super) fn encode<T: Serialize>(value: &T, bound: usize) -> Option<Vec<u8>> {
    let bytes = serde_json::to_vec(value).ok()?;
    (bytes.len() <= bound).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_messages_preserve_big_coordinates_and_reject_ambiguous_leases() {
        let lease = Lease {
            grant: Coordinate(1),
            epoch: Coordinate(2),
            sequence: Coordinate(u64::MAX),
            start: Coordinate(u64::MAX - 960),
            end: Coordinate(u64::MAX),
        };
        let bytes = lease.encode().unwrap();
        assert_eq!(Lease::decode(&bytes), Some(lease));
        assert!(Control::decode(&bytes).is_none());
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"sequence\":\"18446744073709551615\""));
        assert!(Lease::decode(text.replace("\"1\"", "\"01\"").as_bytes()).is_none());
        assert!(Lease::decode(text.replace("\"1\"", "1").as_bytes()).is_none());
        assert!(Lease::decode(text.replace("\"1\"", "\"1\",\"grant\":\"1\"").as_bytes()).is_none());
        let expired = Instant::now();
        assert_eq!(
            lease.with_local_expiry(expired).unwrap().expires_at,
            expired
        );
        assert!(
            Lease {
                start: lease.end,
                ..lease
            }
            .with_local_expiry(expired)
            .is_none()
        );

        let request = Control::RequestTalk {
            ice_generation: Coordinate(2),
        };
        assert_eq!(Control::decode(&request.encode().unwrap()), Some(request));
        assert!(Control::decode(br#"{"type":"request_talk"}"#).is_none());
        assert!(Control::decode(br#"{"type":"request_talk","type":"request_talk"}"#).is_none());
        assert!(Control::decode(&[b' '; MAX_CONTROL_BYTES + 1]).is_none());
    }
}
