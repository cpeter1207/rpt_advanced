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
    fn read(
        v: &Inputs<'_>,
        stage: &str,
        band: &str,
        default: Self,
        source: &mut dyn FnMut(&[&str], ResourceBinding),
    ) -> Self {
        let mut number = |path: &[&str], fallback: &[&str], default: f64| {
            let selected = |p: &[&str]| {
                v.values(p)
                    .find_map(|(value, b)| Some((value.as_f64()?, b)))
            };
            if let Some((value, binding)) = selected(path).or_else(|| selected(fallback)) {
                source(path, binding);
                value
            } else {
                default
            }
        };
        let mut direct =
            |key, default| number(&[stage, band, key], &[stage, "defaults", key], default);
        let threshold_dbfs = direct("threshold_dbfs", default.threshold_dbfs);
        let ratio = direct("ratio", default.ratio);
        let makeup_gain_db = direct("makeup_gain_db", default.makeup_gain_db);
        let knee_db = direct("knee_db", default.knee_db);
        let mut timing = |key, default| {
            number(
                &[stage, band, "timing", key],
                &[stage, "defaults", "timing", key],
                default,
            )
        };
        Self {
            threshold_dbfs,
            ratio,
            makeup_gain_db,
            knee_db,
            attack_ms: timing("attack_ms", default.attack_ms),
            release_ms: timing("release_ms", default.release_ms),
        }
    }
}
impl Compressor {
    pub(super) fn read(
        v: &Inputs<'_>,
        w: &mut Vec<CandidateWarning>,
        source: &mut dyn FnMut(&[&str], ResourceBinding),
    ) -> Self {
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
        let [full, low, mid, high] = ["defaults", "low", "mid", "high"]
            .map(|band| Band::read(v, "compressor", band, base, source));
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
    pub(super) fn read(
        v: &Inputs<'_>,
        w: &mut Vec<CandidateWarning>,
        source: &mut dyn FnMut(&[&str], ResourceBinding),
    ) -> Self {
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
                source,
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

impl Band {
    fn public_fields(&self, makeup: bool, knee: bool) -> serde_json::Value {
        let mut band = serde_json::json!({"threshold_dbfs":self.threshold_dbfs,"ratio":self.ratio,
            "timing":{"attack_ms":self.attack_ms,"release_ms":self.release_ms}});
        if makeup {
            band["makeup_gain_db"] = self.makeup_gain_db.into();
        }
        if knee {
            band["knee_db"] = self.knee_db.into();
        }
        band
    }
}
impl Compressor {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({"enabled":self.enabled,
            "crossover":{"low_hz":self.low_crossover_hz,"high_hz":self.high_crossover_hz},
            "defaults":self.full.public_fields(true,false),"low":self.low.public_fields(true,true),
            "mid":self.mid.public_fields(true,true),"high":self.high.public_fields(true,true),"sidechain":{"highpass_hz":self.sidechain_highpass_hz,"lowpass_hz":self.sidechain_lowpass_hz}})
    }
}

impl Limiter {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({"enabled":self.enabled,
            "crossover":{"low_hz":self.low_crossover_hz,"high_hz":self.high_crossover_hz},
            "defaults":self.full.public_fields(false,true),"low":self.low.public_fields(false,true),
            "mid":self.mid.public_fields(false,true),"high":self.high.public_fields(false,true)})
    }
}
