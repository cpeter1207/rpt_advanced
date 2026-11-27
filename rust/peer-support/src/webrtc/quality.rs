//! RF observations associated with a receiver's actual capture window.

use super::channel::{self, Coordinate, MAX_TIMING_BYTES, TimeBasis};
use serde::{Deserialize, Serialize};

/// One signal quantity, identified independently of availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantity {
    Rssi,
    NoiseFloor,
    Snr,
}

/// Units of a receiver indicator without absolute RF calibration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndicatorUnit {
    AdcCounts,
    Relative,
}

/// Receiver measurement. Digital audio levels are not RF measurements.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Measurement {
    CalibratedRssi {
        dbm: f64,
        calibration_id: String,
        uncertainty_db: f64,
    },
    UncalibratedRssi {
        value: f64,
        unit: IndicatorUnit,
    },
    NoiseFloor {
        dbm: f64,
        calibration_id: String,
        uncertainty_db: Option<f64>,
    },
    Snr {
        db: f64,
        uncertainty_db: Option<f64>,
    },
    Unavailable {
        quantity: Quantity,
    },
}

impl Measurement {
    /// Identity for replacing an older observation of the same quantity.
    pub fn quantity(&self) -> Quantity {
        match self {
            Self::CalibratedRssi { .. } | Self::UncalibratedRssi { .. } => Quantity::Rssi,
            Self::NoiseFloor { .. } => Quantity::NoiseFloor,
            Self::Snr { .. } => Quantity::Snr,
            Self::Unavailable { quantity } => *quantity,
        }
    }
}

/// A measured clock coordinate tied to the sample counter of this receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureTimestamp {
    pub sample: Coordinate,
    pub time_basis: TimeBasis,
    pub time_ns: Coordinate,
    pub uncertainty_ns: Coordinate,
}

/// A single RF observation. Noise and SNR may use independent capture windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverQuality {
    /// Opaque media binding already assigned by the session owner.
    pub binding: String,
    pub epoch: Coordinate,
    pub source_generation: Coordinate,
    pub clock_epoch: Coordinate,
    /// Inclusive start sample in this source generation.
    pub start: Coordinate,
    /// Exclusive end sample in this source generation.
    pub end: Coordinate,
    pub rate_hz: u32,
    /// Absent when this sampling clock has no reported timestamp mapping.
    pub timestamp: Option<CaptureTimestamp>,
    pub measurement: Measurement,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame<T> {
    ReceiverQuality(T),
}

impl ReceiverQuality {
    /// Decode one record within the existing timing-channel budget.
    /// Failure carries no supplied values.
    pub fn decode(input: &[u8]) -> Option<Self> {
        let Frame::ReceiverQuality(record): Frame<Self> = channel::decode(input, MAX_TIMING_BYTES)?;
        record.valid().then_some(record)
    }

    /// Encode one observation for the timing channel.
    pub fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        channel::encode(&Frame::ReceiverQuality(self), MAX_TIMING_BYTES)
    }

    fn valid(&self) -> bool {
        if self.binding.trim().is_empty() || self.start.0 >= self.end.0 || self.rate_hz == 0 {
            return false;
        }
        match &self.measurement {
            Measurement::CalibratedRssi {
                dbm,
                calibration_id,
                uncertainty_db,
            } => calibrated(*dbm, calibration_id) && valid_uncertainty(*uncertainty_db),
            Measurement::NoiseFloor {
                dbm,
                calibration_id,
                uncertainty_db,
            } => calibrated(*dbm, calibration_id) && uncertainty_db.is_none_or(valid_uncertainty),
            Measurement::Snr { db, uncertainty_db } => {
                db.is_finite() && uncertainty_db.is_none_or(valid_uncertainty)
            }
            Measurement::UncalibratedRssi { value, .. } => value.is_finite(),
            Measurement::Unavailable { .. } => true,
        }
    }

    /// Match against the receiver identity already authorized by the owner.
    /// These coordinates do not authenticate the channel or establish clock comparability.
    pub fn matches_source(
        &self,
        binding: &str,
        epoch: Coordinate,
        source_generation: Coordinate,
        clock_epoch: Coordinate,
    ) -> bool {
        self.binding == binding
            && self.epoch == epoch
            && self.source_generation == source_generation
            && self.clock_epoch == clock_epoch
    }
}

fn calibrated(dbm: f64, calibration_id: &str) -> bool {
    dbm.is_finite() && !calibration_id.trim().is_empty()
}

fn valid_uncertainty(db: f64) -> bool {
    db.is_finite() && db >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rssi() -> ReceiverQuality {
        ReceiverQuality {
            binding: "receiver-1".into(),
            epoch: Coordinate(2),
            source_generation: Coordinate(3),
            clock_epoch: Coordinate(4),
            start: Coordinate(u64::MAX - 960),
            end: Coordinate(u64::MAX),
            rate_hz: 48_000,
            timestamp: Some(CaptureTimestamp {
                sample: Coordinate(u64::MAX),
                time_basis: TimeBasis::SessionMonotonic,
                time_ns: Coordinate(9_007_199_254_740_993),
                uncertainty_ns: Coordinate(250_000),
            }),
            measurement: Measurement::CalibratedRssi {
                dbm: -103.25,
                calibration_id: "antenna-input-2".into(),
                uncertainty_db: 1.5,
            },
        }
    }

    #[test]
    fn preserves_exact_sample_and_timestamp_identity() {
        let record = rssi();
        let bytes = record.encode().unwrap();
        assert_eq!(ReceiverQuality::decode(&bytes), Some(record.clone()));
        let wire = String::from_utf8(bytes).unwrap();
        assert!(wire.contains(r#""type":"receiver_quality""#));
        assert!(wire.contains(r#""end":"18446744073709551615""#));
        assert!(wire.contains(r#""time_ns":"9007199254740993""#));

        let expected = ("receiver-1", Coordinate(2), Coordinate(3), Coordinate(4));
        assert!(record.matches_source(expected.0, expected.1, expected.2, expected.3));
        assert!(!record.matches_source("other", expected.1, expected.2, expected.3));
        assert!(!record.matches_source(expected.0, Coordinate(1), expected.2, expected.3));
        assert!(!record.matches_source(expected.0, expected.1, Coordinate(1), expected.3));
        assert!(!record.matches_source(expected.0, expected.1, expected.2, Coordinate(1)));
    }

    #[test]
    fn keeps_uncalibrated_and_absent_readings_distinct() {
        for measurement in [
            Measurement::UncalibratedRssi {
                value: 427.0,
                unit: IndicatorUnit::AdcCounts,
            },
            Measurement::NoiseFloor {
                dbm: -125.3,
                calibration_id: "antenna-input-2".into(),
                uncertainty_db: None,
            },
            Measurement::Snr {
                db: 14.4,
                uncertainty_db: Some(0.125),
            },
            Measurement::Unavailable {
                quantity: Quantity::NoiseFloor,
            },
        ] {
            let record = ReceiverQuality {
                start: Coordinate(133),
                end: Coordinate(266),
                timestamp: None,
                measurement,
                ..rssi()
            };
            let bytes = record.encode().unwrap();
            assert_eq!(ReceiverQuality::decode(&bytes), Some(record));
        }
    }

    #[test]
    fn refuses_ambiguous_or_unbounded_records() {
        let record = rssi();
        let wire = String::from_utf8(record.encode().unwrap()).unwrap();
        for malformed in [
            wire.replace(r#""epoch":"2""#, r#""epoch":"2","epoch":"2""#),
            wire.replace(r#""epoch":"2""#, r#""epoch":2"#),
            wire.replace(r#""dbm":-103.25"#, r#""dbm":-103.25,"\u0064bm":-103.25"#),
            wire.replace(r#""uncertainty_db":1.5"#, r#""uncertainty_db":-0.5"#),
            wire.replace("antenna-input-2", ""),
            wire.replace(r#""rate_hz":48000"#, r#""rate_hz":0"#),
            wire.replace("18446744073709550655", "18446744073709551615"),
        ] {
            assert_ne!(wire, malformed);
            assert!(ReceiverQuality::decode(malformed.as_bytes()).is_none());
        }
        assert!(ReceiverQuality::decode(&[b' '; MAX_TIMING_BYTES + 1]).is_none());

        let mut invalid = record;
        invalid.measurement = Measurement::Snr {
            db: f64::NAN,
            uncertainty_db: None,
        };
        assert!(invalid.encode().is_none());
    }
}
