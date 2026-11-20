//! Fixed three-band processing controls, including the shared band defaults.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Layout {
    ThreeBand,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
struct Band {
    threshold_dbfs: f64,
    ratio: f64,
    makeup_gain_db: f64,
    knee_db: f64,
    attack_ms: f64,
    release_ms: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Compressor {
    pub(super) enabled: bool,
    layout: Layout,
    low_crossover_hz: f64,
    high_crossover_hz: f64,
    full: Band,
    low: Band,
    mid: Band,
    high: Band,
    sidechain_highpass_hz: f64,
    sidechain_lowpass_hz: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Limiter {
    pub(super) enabled: bool,
    layout: Layout,
    low_crossover_hz: f64,
    high_crossover_hz: f64,
    full: Band,
    low: Band,
    mid: Band,
    high: Band,
}
impl Band {
    fn read(v: &Inputs<'_>, stage: &str, band: &str, default: Self) -> Self {
        let direct = |key, default| {
            v.number(
                &[stage, band, key],
                v.number(&[stage, "defaults", key], default),
            )
        };
        let timing = |key, default| {
            v.number(
                &[stage, band, "timing", key],
                v.number(&[stage, "defaults", "timing", key], default),
            )
        };
        Self {
            threshold_dbfs: direct("threshold_dbfs", default.threshold_dbfs),
            ratio: direct("ratio", default.ratio),
            makeup_gain_db: direct("makeup_gain_db", default.makeup_gain_db),
            knee_db: direct("knee_db", default.knee_db),
            attack_ms: timing("attack_ms", default.attack_ms),
            release_ms: timing("release_ms", default.release_ms),
        }
    }
}
impl Compressor {
    pub(super) fn read(v: &Inputs<'_>, w: &mut Vec<CandidateWarning>) -> Self {
        let [low_crossover_hz, high_crossover_hz] = v.ordered(
            &["compressor", "crossover", "low_hz"],
            &["compressor", "crossover", "high_hz"],
            [500.0, 2000.0],
            w,
        );
        let [sidechain_highpass_hz, sidechain_lowpass_hz] = v.ordered(
            &["compressor", "sidechain", "highpass_hz"],
            &["compressor", "sidechain", "lowpass_hz"],
            [800.0, 1500.0],
            w,
        );
        let base = Band {
            threshold_dbfs: -6.0,
            ratio: 2.0,
            makeup_gain_db: 0.0,
            knee_db: 9.0,
            attack_ms: 75.0,
            release_ms: 300.0,
        };
        let [full, low, mid, high] =
            ["defaults", "low", "mid", "high"].map(|band| Band::read(v, "compressor", band, base));
        Self {
            enabled: v.flag(&["compressor", "enabled"], false),
            layout: Layout::ThreeBand,
            low_crossover_hz,
            high_crossover_hz,
            full,
            low,
            mid,
            high,
            sidechain_highpass_hz,
            sidechain_lowpass_hz,
        }
    }
}
impl Limiter {
    pub(super) fn read(v: &Inputs<'_>, w: &mut Vec<CandidateWarning>) -> Self {
        let [low_crossover_hz, high_crossover_hz] = v.ordered(
            &["multiband_limiter", "crossover", "low_hz"],
            &["multiband_limiter", "crossover", "high_hz"],
            [500.0, 2000.0],
            w,
        );
        let [full, low, mid, high] = [
            ("defaults", 20.0, 0.0, 1.0, 50.0),
            ("low", 10.0, 6.0, 50.0, 250.0),
            ("mid", 10.0, 6.0, 10.0, 100.0),
            ("high", 20.0, 6.0, 0.5, 25.0),
        ]
        .map(|(name, ratio, knee_db, attack_ms, release_ms)| {
            Band::read(
                v,
                "multiband_limiter",
                name,
                Band {
                    threshold_dbfs: -1.5,
                    ratio,
                    makeup_gain_db: 0.0,
                    knee_db,
                    attack_ms,
                    release_ms,
                },
            )
        });
        Self {
            enabled: v.flag(&["multiband_limiter", "enabled"], false),
            layout: Layout::ThreeBand,
            low_crossover_hz,
            high_crossover_hz,
            full,
            low,
            mid,
            high,
        }
    }
}
