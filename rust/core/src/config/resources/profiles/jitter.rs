//! Shared source-clock-independent network playout timing.

use super::{
    CandidateWarning,
    normalize::{Fields, Layers, Path, Rule, TimingSource, timings},
};

const PATHS: [Path; 3] = [
    &["jitter", "timing", "initial_ms"],
    &["jitter", "timing", "minimum_ms"],
    &["jitter", "timing", "maximum_ms"],
];

pub(super) const FIELDS: Fields = &[(
    "timing",
    Rule::Object(&[
        ("minimum_ms", Rule::Unsigned(0, u32::MAX as u64)),
        ("initial_ms", Rule::Unsigned(0, u32::MAX as u64)),
        ("maximum_ms", Rule::Unsigned(0, u32::MAX as u64)),
    ]),
)];

/// A field in the common jitter timing tuple.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitterField {
    /// Target occupancy at startup.
    Initial,
    /// Lower delay bound during adaptation.
    Minimum,
    /// Upper delay bound during adaptation.
    Maximum,
}

/// Ordered jitter delays in milliseconds, before provider clock conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterTiming {
    /// Minimum adaptive delay.
    pub minimum_ms: u32,
    /// Initial playout delay.
    pub initial_ms: u32,
    /// Maximum adaptive delay.
    pub maximum_ms: u32,
}

/// Common playout timing with its field-by-field authored fallback chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JitterProfile {
    timing: JitterTiming,
    sources: [Vec<TimingSource>; 3],
    selected: [usize; 3],
}

impl JitterProfile {
    pub(super) fn resolve(layers: &Layers<'_>, warnings: &mut Vec<CandidateWarning>) -> Self {
        // Keep the initial setting first when otherwise equal alternatives exist.
        let (sources, selected) = timings(
            layers,
            PATHS,
            [40, 20, 100],
            |[initial, minimum, maximum]| minimum <= initial && initial <= maximum,
            warnings,
        );
        let [initial_ms, minimum_ms, maximum_ms] =
            std::array::from_fn(|i| sources[i][selected[i]].milliseconds);
        Self {
            timing: JitterTiming {
                minimum_ms,
                initial_ms,
                maximum_ms,
            },
            sources,
            selected,
        }
    }

    /// The tuple that passed common ordering checks.
    pub fn timing(&self) -> JitterTiming {
        self.timing
    }

    /// Ordered per-field candidates for provider checks, including the built-in
    /// final choice. Any provider-selected tuple must satisfy common ordering.
    pub fn sources(&self, field: JitterField) -> &[TimingSource] {
        &self.sources[field as usize]
    }

    /// The source of a field that passed ordering checks.
    pub fn selected_source(&self, field: JitterField) -> TimingSource {
        self.sources(field)[self.selected[field as usize]]
    }
}
