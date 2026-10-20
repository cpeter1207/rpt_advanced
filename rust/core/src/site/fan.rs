//! Cooling demand and faults without claiming unverified hardware state.

use super::SiteError;
use crate::config::{FanMode, FanSettings, NodeId};
use std::collections::BTreeMap;

/// Actual backend availability/capability, not inferred from a configured name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FanOutput {
    /// The configured output can currently accept a command.
    pub available: bool,
    /// Hardware provides duty control; software GPIO PWM is forbidden.
    pub pwm: bool,
}

/// Requested cooling and source/control/feedback health.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanSnapshot {
    /// Normalized requested duty, 0..1; not verified physical operation.
    pub duty: f64,
    /// Source, capability, output, or tachometer fault.
    pub fault: bool,
    /// Monotonic end of transmitter run-on.
    pub run_on_until_ms: Option<u64>,
}

/// Control-owned cooling policy; manual demand cannot reduce protection.
#[derive(Clone)]
pub struct FanState {
    settings: FanSettings,
    keyed: bool,
    run_on_until: Option<u64>,
    temperature_on: bool,
    manual: Option<f64>,
    started_at: Option<u64>,
}
impl FanState {
    /// Start without assuming earlier transmitter activity or fan motion.
    pub fn new(settings: FanSettings) -> Self {
        Self {
            settings,
            keyed: false,
            run_on_until: None,
            temperature_on: false,
            manual: None,
            started_at: None,
        }
    }
    /// Set additional manual cooling demand, or return to automatic operation.
    pub fn manual(&mut self, duty: Option<f64>) -> Result<(), SiteError> {
        if duty.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(SiteError::InvalidValue);
        }
        self.manual = duty;
        Ok(())
    }
    fn temperature_demand(&mut self, temperature: Option<f64>) -> (f64, bool) {
        if self.settings.mode == FanMode::Ptt {
            return (0.0, false);
        }
        let Some(temperature) = temperature.filter(|value| value.is_finite()) else {
            return (1.0, true);
        };
        let curve = &self.settings.temperature_curve;
        if !curve.is_empty() {
            let duty = curve
                .windows(2)
                .find(|pair| temperature < pair[1].0)
                .map(|pair| {
                    let weight =
                        ((temperature - pair[0].0) / (pair[1].0 - pair[0].0)).clamp(0.0, 1.0);
                    pair[0].1 + weight * (pair[1].1 - pair[0].1)
                })
                .unwrap_or(curve.last().expect("nonempty curve").1);
            return (duty, false);
        }
        if temperature
            >= self
                .settings
                .on_temperature
                .expect("validated temperature threshold")
        {
            self.temperature_on = true;
        } else if temperature
            <= self
                .settings
                .off_temperature
                .expect("validated temperature threshold")
        {
            self.temperature_on = false;
        }
        (if self.temperature_on { 1.0 } else { 0.0 }, false)
    }
    /// Advance monotonic demand from current, valid engineering values. Missing
    /// temperature requests full cooling; missing control remains a visible fault.
    pub fn advance(
        &mut self,
        now_ms: u64,
        ptt: &BTreeMap<NodeId, bool>,
        temperature: Option<f64>,
        rpm: Option<f64>,
        output: FanOutput,
    ) -> FanSnapshot {
        if !self.settings.enabled {
            return FanSnapshot {
                duty: 0.0,
                fault: false,
                run_on_until_ms: None,
            };
        }
        let keyed = self.settings.mode != FanMode::Temperature
            && self
                .settings
                .ptt_nodes
                .iter()
                .any(|node| ptt.get(node).copied().unwrap_or(false));
        if self.keyed && !keyed {
            self.run_on_until = Some(now_ms.saturating_add(self.settings.run_on_ms));
        }
        self.keyed = keyed;
        if self.run_on_until.is_some_and(|deadline| now_ms >= deadline) {
            self.run_on_until = None;
        }
        let ptt_demand: f64 = if keyed || self.run_on_until.is_some() {
            1.0
        } else {
            0.0
        };
        let (thermal, thermal_fault) = self.temperature_demand(temperature);
        let duty = ptt_demand.max(thermal).max(self.manual.unwrap_or(0.0));
        if duty > 0.0 {
            self.started_at.get_or_insert(now_ms);
        } else {
            self.started_at = None;
        }
        let tach_fault = !self.settings.rpm_sensor.is_empty()
            && self
                .started_at
                .is_some_and(|start| now_ms.saturating_sub(start) >= self.settings.spinup_ms)
            && !rpm.is_some_and(|value| {
                value.is_finite()
                    && value >= self.settings.minimum_rpm.expect("validated RPM threshold")
            });
        FanSnapshot {
            duty,
            fault: thermal_fault
                || !output.available
                || (!output.pwm && !self.settings.temperature_curve.is_empty())
                || tach_fault,
            run_on_until_ms: self.run_on_until,
        }
    }
}

#[cfg(test)]
#[path = "fan_tests.rs"]
mod tests;
