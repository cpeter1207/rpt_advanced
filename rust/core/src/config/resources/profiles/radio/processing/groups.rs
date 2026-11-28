//! Typed settings passed to the provider after public field validation.
use super::*;
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct Equalizer {
    low_gain_db: f64,
    low_frequency_hz: f64,
    low_slope: f64,
    mid_gain_db: f64,
    mid_frequency_hz: f64,
    mid_width_octaves: f64,
    high_gain_db: f64,
    high_frequency_hz: f64,
    high_slope: f64,
    pub(super) enabled: bool,
}
impl Equalizer {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            low_gain_db: v.number(&["equalizer", "low", "gain_db"], 2.0),
            low_frequency_hz: v.number(&["equalizer", "low", "frequency_hz"], 500.0),
            low_slope: v.number(&["equalizer", "low", "slope"], 0.7),
            mid_gain_db: v.number(&["equalizer", "mid", "gain_db"], -0.5),
            mid_frequency_hz: v.number(&["equalizer", "mid", "frequency_hz"], 1000.0),
            mid_width_octaves: v.number(&["equalizer", "mid", "width_octaves"], 1.0),
            high_gain_db: v.number(&["equalizer", "high", "gain_db"], -1.0),
            high_frequency_hz: v.number(&["equalizer", "high", "frequency_hz"], 2000.0),
            high_slope: v.number(&["equalizer", "high", "slope"], 0.7),
            enabled: v.flag(&["equalizer", "enabled"], true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct Deesser {
    frequency_hz: f64,
    width_octaves: f64,
    threshold_dbfs: f64,
    ratio: f64,
    max_reduction_db: f64,
    attack_ms: f64,
    release_ms: f64,
    pub(super) enabled: bool,
}
impl Deesser {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            frequency_hz: v.number(&["deesser", "frequency_hz"], 4000.0),
            width_octaves: v.number(&["deesser", "width_octaves"], 1.0),
            threshold_dbfs: v.number(&["deesser", "threshold_dbfs"], -18.0),
            ratio: v.number(&["deesser", "ratio"], 3.0),
            max_reduction_db: v.number(&["deesser", "max_reduction_db"], 4.0),
            attack_ms: v.number(&["deesser", "timing", "attack_ms"], 2.0),
            release_ms: v.number(&["deesser", "timing", "release_ms"], 60.0),
            enabled: v.flag(&["deesser", "enabled"], false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct Agc {
    target_lufs: f64,
    max_gain_db: f64,
    max_attenuation_db: f64,
    loudness_window_ms: f64,
    gain_increase_db_per_second: f64,
    gain_decrease_db_per_second: f64,
    hold_ms: f64,
    activity_threshold_dbfs: f64,
    activity_hysteresis_db: f64,
    deadband_db: f64,
    pub(super) enabled: bool,
}
impl Agc {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            target_lufs: v.number(&["agc", "target_lufs"], -23.0),
            max_gain_db: v.number(&["agc", "max_gain_db"], 6.0),
            max_attenuation_db: v.number(&["agc", "max_attenuation_db"], 6.0),
            loudness_window_ms: v.number(&["agc", "timing", "loudness_window_ms"], 400.0),
            gain_increase_db_per_second: v
                .number(&["agc", "timing", "gain_increase_db_per_second"], 2.0),
            gain_decrease_db_per_second: v
                .number(&["agc", "timing", "gain_decrease_db_per_second"], 6.0),
            hold_ms: v.number(&["agc", "timing", "hold_ms"], 500.0),
            activity_threshold_dbfs: v.number(&["agc", "activity_threshold_dbfs"], -50.0),
            activity_hysteresis_db: v.number(&["agc", "activity_hysteresis_db"], 3.0),
            deadband_db: v.number(&["agc", "deadband_db"], 1.0),
            enabled: v.flag(&["agc", "enabled"], false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct Expander {
    threshold_dbfs: f64,
    ratio: f64,
    max_attenuation_db: f64,
    attack_ms: f64,
    release_ms: f64,
    pub(super) sidechain_highpass_hz: f64,
    pub(super) sidechain_lowpass_hz: f64,
    pub(super) enabled: bool,
}
impl Expander {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            threshold_dbfs: v.number(&["expander", "threshold_dbfs"], -55.0),
            ratio: v.number(&["expander", "ratio"], 1.5),
            max_attenuation_db: v.number(&["expander", "max_attenuation_db"], 9.0),
            attack_ms: v.number(&["expander", "timing", "attack_ms"], 10.0),
            release_ms: v.number(&["expander", "timing", "release_ms"], 250.0),
            sidechain_highpass_hz: v.number(&["expander", "sidechain", "highpass_hz"], 800.0),
            sidechain_lowpass_hz: v.number(&["expander", "sidechain", "lowpass_hz"], 1500.0),
            enabled: v.flag(&["expander", "enabled"], false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct PeakLimiter {
    ceiling_dbfs: f64,
    lookahead_ms: f64,
    attack_ms: f64,
    release_ms: f64,
    pub(super) enabled: bool,
    oversample: u32,
}
impl PeakLimiter {
    pub(super) fn read(v: &Inputs<'_>) -> Self {
        Self {
            ceiling_dbfs: v.number(&["peak_limiter", "ceiling_dbfs"], -3.0),
            lookahead_ms: v.number(&["peak_limiter", "timing", "lookahead_ms"], 5.0),
            attack_ms: v.number(&["peak_limiter", "timing", "attack_ms"], 1.0),
            release_ms: v.number(&["peak_limiter", "timing", "release_ms"], 100.0),
            enabled: v.flag(&["peak_limiter", "enabled"], false),
            oversample: v.uint(&["peak_limiter", "oversample"], 4),
        }
    }
}

impl Equalizer {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "low": {
                "gain_db": self.low_gain_db,
                "frequency_hz": self.low_frequency_hz,
                "slope": self.low_slope
            },
            "mid": {
                "gain_db": self.mid_gain_db,
                "frequency_hz": self.mid_frequency_hz,
                "width_octaves": self.mid_width_octaves
            },
            "high": {
                "gain_db": self.high_gain_db,
                "frequency_hz": self.high_frequency_hz,
                "slope": self.high_slope
            },
            "enabled": self.enabled
        })
    }
}

impl Deesser {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "frequency_hz": self.frequency_hz,
            "width_octaves": self.width_octaves,
            "threshold_dbfs": self.threshold_dbfs,
            "ratio": self.ratio,
            "max_reduction_db": self.max_reduction_db,
            "timing": {
                "attack_ms": self.attack_ms,
                "release_ms": self.release_ms
            },
            "enabled": self.enabled
        })
    }
}

impl Agc {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "target_lufs": self.target_lufs,
            "max_gain_db": self.max_gain_db,
            "max_attenuation_db": self.max_attenuation_db,
            "timing": {
                "loudness_window_ms": self.loudness_window_ms,
                "gain_increase_db_per_second": self.gain_increase_db_per_second,
                "gain_decrease_db_per_second": self.gain_decrease_db_per_second,
                "hold_ms": self.hold_ms
            },
            "activity_threshold_dbfs": self.activity_threshold_dbfs,
            "activity_hysteresis_db": self.activity_hysteresis_db,
            "deadband_db": self.deadband_db,
            "enabled": self.enabled
        })
    }
}

impl Expander {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "threshold_dbfs": self.threshold_dbfs,
            "ratio": self.ratio,
            "max_attenuation_db": self.max_attenuation_db,
            "timing": {
                "attack_ms": self.attack_ms,
                "release_ms": self.release_ms
            },
            "sidechain": {
                "highpass_hz": self.sidechain_highpass_hz,
                "lowpass_hz": self.sidechain_lowpass_hz
            },
            "enabled": self.enabled
        })
    }
}

impl PeakLimiter {
    pub(super) fn public_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "ceiling_dbfs": self.ceiling_dbfs,
            "oversample": self.oversample,
            "timing": {
                "lookahead_ms": self.lookahead_ms,
                "attack_ms": self.attack_ms,
                "release_ms": self.release_ms
            },
            "enabled": self.enabled
        })
    }
}
