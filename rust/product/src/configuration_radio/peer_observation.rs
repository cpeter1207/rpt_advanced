//! Provider-native aggregate measurements at the incoming link graph output.
use crate::abi;

/// Observation of a Product-owned link graph's actual output boundary.
pub(crate) struct ProviderOutput {
    pub(crate) rate: u32,
    pub(crate) state: ProviderOutputState,
}

pub(crate) enum ProviderOutputState {
    Observed(ProviderOutputMeasurement),
    RateMismatch,
    ObserverUnavailable,
    ReadFailed,
}

/// Lifetime counts and aggregate loudness, with no source sample timing.
#[derive(Debug, PartialEq)]
pub(crate) struct ProviderOutputMeasurement {
    pub(crate) loudness_epoch: u64,
    pub(crate) valid_samples: u64,
    pub(crate) processed_blocks: u64,
    pub(crate) bypassed_blocks: u64,
    pub(crate) failed_blocks: u64,
    pub(crate) integrated_lufs: Option<f64>,
    pub(crate) true_peak_dbtp: Option<f64>,
}

impl ProviderOutputMeasurement {
    pub(crate) fn from_wire(wire: &abi::UrpAstLinkObservation) -> Option<Self> {
        if wire.struct_size as usize != std::mem::size_of_val(wire)
            || wire.abi_version != 1
            || wire.loudness_valid > 1
            || wire.true_peak_valid > 1
        {
            return None;
        }
        Some(Self {
            loudness_epoch: wire.loudness_epoch,
            valid_samples: wire.loudness_sample_count,
            processed_blocks: wire.processed_blocks,
            bypassed_blocks: wire.bypassed_blocks,
            failed_blocks: wire.failed_blocks,
            integrated_lufs: (wire.loudness_valid == 1 && wire.integrated_lufs.is_finite())
                .then_some(wire.integrated_lufs),
            true_peak_dbtp: (wire.true_peak_valid == 1 && wire.true_peak_dbtp.is_finite())
                .then_some(wire.true_peak_dbtp),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_measurement_retains_epoch_and_independent_validity_flags() {
        // SAFETY: flat record of scalar values; the SDK accepts initialized storage.
        let mut wire: abi::UrpAstLinkObservation = unsafe { std::mem::zeroed() };
        wire.struct_size = std::mem::size_of_val(&wire) as u32;
        wire.abi_version = 1;
        wire.loudness_epoch = 8;
        wire.loudness_sample_count = 19_200;
        wire.processed_blocks = 20;
        wire.bypassed_blocks = 2;
        wire.failed_blocks = 1;
        wire.true_peak_valid = 1;
        wire.true_peak_dbtp = -12.25;
        let measured = ProviderOutputMeasurement::from_wire(&wire).unwrap();
        assert_eq!(
            measured,
            ProviderOutputMeasurement {
                loudness_epoch: 8,
                valid_samples: 19_200,
                processed_blocks: 20,
                bypassed_blocks: 2,
                failed_blocks: 1,
                integrated_lufs: None,
                true_peak_dbtp: Some(-12.25),
            }
        );
        wire.loudness_valid = 1;
        wire.integrated_lufs = -23.0;
        wire.true_peak_dbtp = f64::NEG_INFINITY;
        let measured = ProviderOutputMeasurement::from_wire(&wire).unwrap();
        assert_eq!(measured.integrated_lufs, Some(-23.0));
        assert_eq!(measured.true_peak_dbtp, None);
        wire.struct_size -= 1;
        assert!(ProviderOutputMeasurement::from_wire(&wire).is_none());
    }
}
