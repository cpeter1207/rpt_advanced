//! Three independent source roles. USBRadioPlus owns graph construction and metering.
mod dynamics;
pub(super) mod fields;
mod groups;
use super::super::normalize::warn;
use super::inputs::Inputs;
use super::{CandidateWarning, NormalizedProfileResource, ResourceKind};
use dynamics::{Compressor, Limiter};
use groups::*;
use serde::Serialize;

/// Independently present local, incoming-link, and transmit processing.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProcessingRoles {
    /// RX-local role, independently present when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<ProcessingProfile>,
    /// Per-peer incoming audio role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<ProcessingProfile>,
    /// Voice and telemetry role preceding TX conditioning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_telemetry: Option<ProcessingProfile>,
}

/// Resolved semantic role, serializable to the provider's typed settings contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProcessingProfile {
    /// Master optional processing enable; a disabled role retains its own metering.
    pub enabled: bool,
    rnnoise_enabled: bool,
    input_gain_db: f64,
    output_gain_db: f64,
    stage_order: Vec<Stage>,
    equalizer: Equalizer,
    expander: Expander,
    agc: Agc,
    deesser: Deesser,
    compressor: Compressor,
    limiter: Limiter,
    peak_limiter: PeakLimiter,
    #[serde(skip_serializing_if = "Option::is_none")]
    receive: Option<Receive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transmit_tail: Option<TransmitTail>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Equalizer,
    Expander,
    Agc,
    Deesser,
    Compressor,
    Limiter,
}
impl Stage {
    const STANDARD: [Self; 6] = [
        Self::Equalizer,
        Self::Expander,
        Self::Agc,
        Self::Deesser,
        Self::Compressor,
        Self::Limiter,
    ];
    fn from_public(s: &str) -> Option<Self> {
        Some(match s {
            "equalizer" => Self::Equalizer,
            "expander" => Self::Expander,
            "agc" => Self::Agc,
            "deesser" => Self::Deesser,
            "compressor" => Self::Compressor,
            "multiband_limiter" => Self::Limiter,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Receive {
    bandpass_enabled: bool,
    bandpass_highpass_hz: f64,
    bandpass_lowpass_hz: f64,
    pl_filter: PlFilter,
    notch_width_hz: f64,
    highpass_hz: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlFilter {
    Disabled,
    DecodedToneNotch,
    HighPass,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
struct TransmitTail {
    bandpass_enabled: bool,
    bandpass_highpass_hz: f64,
    bandpass_lowpass_hz: f64,
}

pub(super) fn resolve(
    kind: ResourceKind,
    layers: [Option<&NormalizedProfileResource>; 2],
    captured: [bool; 2],
    w: &mut Vec<CandidateWarning>,
) -> Option<ProcessingProfile> {
    let v = Inputs(std::array::from_fn(|i| {
        let resource = layers[i]?;
        if !captured[i] {
            return None;
        }
        let body = if kind == ResourceKind::PeerAudio {
            &resource.configuration
        } else {
            resource.configuration.get("processing")?.as_object()?
        };
        Some((body, resource.binding))
    }));
    if v.0.iter().all(Option::is_none) {
        return None;
    }
    let local = kind == ResourceKind::RxAudio;
    let voice = kind == ResourceKind::TxAudio;
    let equalizer = Equalizer::read(&v);
    let mut expander = Expander::read(&v);
    [
        expander.sidechain_highpass_hz,
        expander.sidechain_lowpass_hz,
    ] = v.ordered(
        &["expander", "sidechain", "highpass_hz"],
        &["expander", "sidechain", "lowpass_hz"],
        [800.0, 1500.0],
        w,
    );
    let agc = Agc::read(&v);
    let deesser = Deesser::read(&v);
    let compressor = Compressor::read(&v, w);
    let limiter = Limiter::read(&v, w);
    let enabled = [
        equalizer.enabled,
        expander.enabled,
        agc.enabled,
        deesser.enabled,
        compressor.enabled,
        limiter.enabled,
    ];
    let stage_order = v
        .values(&["stage_order"])
        .find_map(|(value, b)| {
            let order: Vec<_> = value
                .as_array()
                .expect("normalized order")
                .iter()
                .map(|s| Stage::from_public(s.as_str().unwrap()).unwrap())
                .collect();
            if Stage::STANDARD
                .iter()
                .zip(enabled)
                .all(|(stage, active)| !active || order.contains(stage))
            {
                Some(order)
            } else {
                warn(w, b, "stage_order");
                None
            }
        })
        .unwrap_or_else(|| Stage::STANDARD.to_vec());
    let receive = local.then(|| {
        let [bandpass_highpass_hz, bandpass_lowpass_hz] = v.ordered(
            &["bandpass", "highpass_hz"],
            &["bandpass", "lowpass_hz"],
            [20.0, 5000.0],
            w,
        );
        let pl_filter = match v
            .values(&["ctcss_filter", "mode"])
            .next()
            .and_then(|(v, _)| v.as_str())
        {
            Some("disabled") => PlFilter::Disabled,
            Some("notch") => PlFilter::DecodedToneNotch,
            _ => PlFilter::HighPass,
        };
        Receive {
            bandpass_enabled: v.flag(&["bandpass", "enabled"], true),
            bandpass_highpass_hz,
            bandpass_lowpass_hz,
            pl_filter,
            notch_width_hz: 10.0,
            highpass_hz: v.number(&["ctcss_filter", "highpass_hz"], 300.0),
        }
    });
    let transmit_tail = voice.then(|| TransmitTail {
        bandpass_enabled: v.flag(&["bandpass", "enabled"], false),
        bandpass_highpass_hz: v.number(&["bandpass", "highpass_hz"], 0.0),
        bandpass_lowpass_hz: v.number(&["bandpass", "lowpass_hz"], 8000.0),
    });
    Some(ProcessingProfile {
        enabled: v.flag(&["enabled"], true),
        input_gain_db: v.number(&["input_gain_db"], if voice { 6.0 } else { 0.0 }),
        output_gain_db: v.number(&["output_gain_db"], if voice { 0.0 } else { -6.2 }),
        rnnoise_enabled: local && v.flag(&["rnnoise", "enabled"], false),
        stage_order,
        equalizer,
        expander,
        agc,
        deesser,
        compressor,
        limiter,
        peak_limiter: PeakLimiter::read(&v),
        receive,
        transmit_tail,
    })
}
