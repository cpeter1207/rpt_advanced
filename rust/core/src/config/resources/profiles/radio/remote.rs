//! Explicit remote control policy; the radio engine owns hardware admission.
mod projection;
pub use projection::RemoteSettings;
pub(super) mod fields;
use super::super::normalize::warn;
use super::{CandidateWarning, RadioGeneral, ResourceBinding, inputs::Inputs};
pub(super) use fields::control;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

choices!(/// Remote control backend.
RemoteBackend{Hamlib=>"hamlib",ParallelChannel=>"parallel_channel",ParallelRtx=>"parallel_rtx"});
choices!(/// Remote operating authorization.
RemoteAccess{Open=>"open",Locked=>"locked",Disabled=>"disabled"});

/// Resolved control, RF limits and power. Arbitrary transport option values are private to the control owner.
#[derive(Clone, PartialEq, Eq)]
pub struct RemotePolicy {
    /// Admit remote operation when all prerequisites are configured.
    pub enabled: bool,
    /// Explicit selected control backend.
    pub backend: Option<RemoteBackend>,
    /// Optional Hamlib model name.
    pub hamlib_model: Option<String>,
    /// Effective native options, including the independent device as rig_pathname.
    pub hamlib_options: BTreeMap<String, String>,
    /// Hamlib transaction timeout.
    pub timeout_ms: u32,
    /// Finite transaction retries.
    pub retries: u32,
    /// Additional timeout retries.
    pub timeout_retries: u32,
    /// Monotonic rig poll period.
    pub poll_interval_ms: u64,
    /// Operating command admission policy.
    pub access: RemoteAccess,
    /// Inclusive permitted receive ranges in hertz.
    pub receive_ranges: Vec<[u64; 2]>,
    /// Inclusive permitted transmit ranges in hertz.
    pub transmit_ranges: Vec<[u64; 2]>,
    /// Explicitly receive-only ranges.
    pub receive_only_ranges: Vec<[u64; 2]>,
    /// Validated SDK modulation selectors, in configured order.
    pub modes: Vec<u32>,
    /// Optional established power ceiling in milliwatts.
    pub maximum_power_mw: Option<u32>,
    /// Low, medium and high power presets, in milliwatts.
    pub power_presets_mw: [Option<u32>; 3],
}
impl fmt::Debug for RemotePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemotePolicy")
            .field("enabled", &self.enabled)
            .field("backend", &self.backend)
            .field("transport_option_count", &self.hamlib_options.len())
            .finish_non_exhaustive()
    }
}
impl RemotePolicy {
    /// Public modulation names admitted by the RF limit profile, in authored order.
    pub fn mode_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.modes
            .iter()
            .map(|mode| fields::MODES[*mode as usize - 1])
    }
    /// Explicit public device, kept separate from the arbitrary public option keys.
    pub fn hamlib_device(&self) -> Option<&str> {
        self.hamlib_options.get("rig_pathname").map(String::as_str)
    }
    /// Effective public options. The device is the sole source of the reserved SDK pathname key.
    pub fn authored_hamlib_options(&self) -> impl Iterator<Item = (&String, &String)> {
        self.hamlib_options
            .iter()
            .filter(|(k, _)| k.as_str() != "rig_pathname")
    }
    pub(super) fn read(
        c: &Inputs<'_>,
        l: &Inputs<'_>,
        p: &Inputs<'_>,
        general: &RadioGeneral,
        memory_count: usize,
        w: &mut Vec<CandidateWarning>,
    ) -> Self {
        let backend = c
            .get(&["backend"])
            .and_then(Value::as_str)
            .map(RemoteBackend::read);
        let hamlib_model = c
            .get(&["hamlib", "model"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut hamlib_options = BTreeMap::new();
        for (opts, _) in c
            .values(&["hamlib", "options"])
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            for (k, v) in opts.as_object().expect("normalized options") {
                hamlib_options.insert(k.clone(), v.as_str().expect("normalized option").to_owned());
            }
        }
        if let Some(device) = c.get(&["hamlib", "device"]).and_then(Value::as_str) {
            hamlib_options.insert("rig_pathname".into(), device.to_owned());
        }
        let ranges = |name| {
            l.get(&[name])
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|v| {
                    let row = v.as_array().expect("normalized range");
                    [0, 1].map(|i| fields::scaled(&row[i], 6).expect("normalized hertz"))
                })
                .collect::<Vec<_>>()
        };
        let receive_ranges = ranges("rx_ranges_mhz");
        let modes = l
            .get(&["modes"])
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|v| {
                fields::MODES
                    .iter()
                    .position(|mode| Some(*mode) == v.as_str())
                    .expect("normalized modulation") as u32
                    + 1
            })
            .collect::<Vec<_>>();
        let maximum_power_mw = p
            .get(&["max_w"])
            .and_then(|n| fields::scaled(n, 3))
            .map(|n| n as u32);
        let power_presets_mw = ["low_w", "medium_w", "high_w"].map(|field| {
            p.values(&[field]).find_map(|(v, b)| {
                let value = fields::scaled(v, 3)? as u32;
                if maximum_power_mw.is_some_and(|max| value > max) {
                    warn(w, b, field);
                    None
                } else {
                    Some(value)
                }
            })
        });
        let complete = backend.is_some()
            && (backend != Some(RemoteBackend::Hamlib) || hamlib_model.is_some())
            && !modes.is_empty()
            && !receive_ranges.is_empty()
            && general
                .startup_memory
                .is_some_and(|index| index as usize <= memory_count);
        let enabled = c
            .values(&["enabled"])
            .find_map(|(v, b)| {
                let active = v.as_bool()?;
                if active && !complete {
                    warn(w, b, "enabled");
                    None
                } else {
                    Some(active)
                }
            })
            .unwrap_or(false);
        Self {
            enabled,
            backend,
            hamlib_model,
            hamlib_options,
            receive_ranges,
            transmit_ranges: ranges("tx_ranges_mhz"),
            receive_only_ranges: ranges("rx_only_ranges_mhz"),
            maximum_power_mw,
            power_presets_mw,
            modes,
            timeout_ms: c.uint(&["timing", "timeout_ms"], 500),
            retries: c.uint(&["retries"], 2),
            timeout_retries: c.uint(&["timeout_retries"], 0),
            poll_interval_ms: c
                .get(&["timing", "poll_interval_ms"])
                .and_then(Value::as_u64)
                .unwrap_or(500),
            access: RemoteAccess::read(c.text(&["access"], "locked")),
        }
    }
}
