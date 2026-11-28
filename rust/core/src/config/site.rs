//! Station equipment identities and calibrated settings.
#[path = "site_policy.rs"]
mod policy;
pub use policy::{AlarmCondition, AlarmSettings, FanMode, FanSettings, SiteAction};
pub(crate) use policy::{curve_valid, parse_action};

/// Selected hardware owner; there is no automatic backend fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SiteBackend {
    /// The radio's existing CM119 HID owner.
    Cm119,
    /// The radio's existing parallel-port owner.
    Parallel,
    /// A Linux GPIO character device, through libgpiod.
    Gpiod,
    /// A Linux hwmon device, through libsensors.
    Hwmon,
    /// A kernel IIO channel.
    Iio,
}

/// Stable configured endpoint. Runtime capability resolution must also reject
/// aliases of an already-owned physical output before acquiring it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteEndpoint {
    /// Owner/backend selected by the instance.
    pub backend: SiteBackend,
    /// Stable device identity, never an enumeration position.
    pub device: String,
    /// Channel or digital-line identity within the device.
    pub channel: String,
}

/// Engineering-unit conversion after the adapter has normalized raw units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Calibration {
    /// Preserve adapter source units.
    None,
    /// Linear mapping between two distinct, finite source points.
    TwoPoint {
        /// First source-unit reference.
        input_low: f64,
        /// Engineering value at the first reference.
        value_low: f64,
        /// Second source-unit reference.
        input_high: f64,
        /// Engineering value at the second reference.
        value_high: f64,
    },
}

impl Calibration {
    /// Build a calibration only when both spans and their ratio remain finite.
    pub(crate) fn two_point(
        input_low: f64,
        input_high: f64,
        value_low: f64,
        value_high: f64,
    ) -> Option<Self> {
        let input_span = input_high - input_low;
        let value_span = value_high - value_low;
        (input_span != 0.0
            && input_span.is_finite()
            && value_span.is_finite()
            && (value_span / input_span).is_finite())
        .then_some(Calibration::TwoPoint {
            input_low,
            input_high,
            value_low,
            value_high,
        })
    }

    /// Apply calibration once; nonfinite results are acquisition faults.
    pub fn apply(self, value: f64) -> Option<f64> {
        let result = match self {
            Self::None => value,
            Self::TwoPoint {
                input_low,
                value_low,
                input_high,
                value_high,
            } => {
                value_low
                    + (value - input_low) / (input_high - input_low) * (value_high - value_low)
            }
        };
        result.is_finite().then_some(result)
    }
}

/// Resolved analog source and display policy; precision never rounds stored PCM-independent readings.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorSettings {
    /// Node-local stable name, shared with digital inputs.
    pub name: String,
    /// Whether acquisition may run after capability validation.
    pub enabled: bool,
    /// Instance-only source identity.
    pub endpoint: Option<SiteEndpoint>,
    /// Cached property of the already owning radio, mutually exclusive with an external endpoint.
    pub interface: Option<super::resources::profiles::equipment::BoundInterface>,
    /// Optional localized label; an empty value uses the stable name.
    pub label_message_id: String,
    /// Engineering unit used in reports.
    pub unit: String,
    /// Display decimal places, not measurement precision.
    pub precision: u8,
    /// Positive acquisition interval.
    pub poll_interval_ms: u64,
    /// Age at which a successful observation stops being current.
    pub stale_after_ms: u64,
    /// Exponential smoothing time constant; zero disables smoothing.
    pub smoothing_ms: u64,
    /// Conversion of already-normalized source units.
    pub calibration: Calibration,
    /// Optional canonical positive decimal query selector.
    pub dtmf_id: Option<String>,
}

/// Resolved site digital input, independent of radio COR/CTCSS debounce.
#[derive(Clone, Debug, PartialEq)]
pub struct InputSettings {
    /// Node-local stable name, shared with sensors.
    pub name: String,
    /// Whether this input may be acquired.
    pub enabled: bool,
    /// Instance-only source identity.
    pub endpoint: Option<SiteEndpoint>,
    /// Cached property of the already owning radio, mutually exclusive with an external endpoint.
    pub interface: Option<super::resources::profiles::equipment::BoundInterface>,
    /// Electrical high means logical active when true.
    pub active_high: bool,
    /// Time the changed state must remain stable.
    pub debounce_ms: u64,
    /// Read-only query selector.
    pub dtmf_id: Option<String>,
    /// Ordered action references on a debounced active edge.
    pub on_active: Vec<SiteAction>,
    /// Ordered action references on a debounced inactive edge.
    pub on_inactive: Vec<SiteAction>,
}

/// Resolved logical output; hardware readback is not implied by a requested state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSettings {
    /// Node-local stable name.
    pub name: String,
    /// Whether this output may be acquired.
    pub enabled: bool,
    /// Instance-only owned endpoint.
    pub endpoint: Option<SiteEndpoint>,
    /// Cached property of the already owning radio, mutually exclusive with an external endpoint.
    pub interface: Option<super::resources::profiles::equipment::BoundInterface>,
    /// Electrical polarity of logical on.
    pub active_high: bool,
    /// Initial logical demand.
    pub startup_on: bool,
    /// Logical demand on failure and shutdown.
    pub safe_on: bool,
    /// Default retriggerable pulse duration.
    pub pulse_ms: u64,
}

/// Ordered named site instances; defaults never create an instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedSiteSettings {
    /// Analog measurements in configuration order.
    pub sensors: Vec<SensorSettings>,
    /// Site digital inputs in configuration order.
    pub inputs: Vec<InputSettings>,
    /// Site digital outputs in configuration order.
    pub outputs: Vec<OutputSettings>,
    /// Threshold/health policies in configuration order.
    pub alarms: Vec<AlarmSettings>,
    /// Cooling policies in configuration order.
    pub fans: Vec<FanSettings>,
}

fn finite(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}
