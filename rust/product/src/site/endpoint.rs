//! Serialized external site access; callers own scheduling and observation time.
use super::SiteBackends;
use crate::abi;
use crate::site::iio::IioReader;
use rpt_advanced_core::{
    config::{SiteBackend, SiteEndpoint},
    site::ReadingQuality,
};
use std::{ffi::c_void, mem::size_of, path::Path, ptr, sync::Arc};

/// Physical role and logical startup/safe policy selected before acquisition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Digital active polarity; analog inputs ignore this field.
    Input {
        /// Electrical high means logical active.
        active_high: bool,
    },
    /// Steady logical output, including kernel PWM duty for hwmon.
    Output {
        /// Electrical high means logical active.
        active_high: bool,
        /// Initial logical demand.
        startup_on: bool,
        /// Required retirement demand.
        safe_on: bool,
    },
}
/// Adapter-normalized measurement, with no timestamps or policy applied.
#[derive(Debug, PartialEq)]
pub enum Sample {
    /// Finite source-unit value; not engineering calibration.
    Number(f64),
    /// Logical input after polarity, applied once at its device boundary.
    Digital(bool),
}
/// A single endpoint used serially outside control and audio callbacks.
pub struct Endpoint {
    device: Device,
    direction: Direction,
    // Keep the executable descriptor code alive until Device's destruction.
    _backends: Arc<SiteBackends>,
}

enum Device {
    Gpiod(*mut c_void, abi::rptadv_gpiod_descriptor_v1),
    Hwmon(*mut c_void, abi::rptadv_hwmon_descriptor_v1),
    Iio(IioReader),
}

impl Direction {
    /// Role/polarity identify the physical request; startup/safe values are policy.
    pub fn same_io(self, other: Self) -> bool {
        self.output() == other.output() && self.active_high() == other.active_high()
    }
    fn output(self) -> bool {
        matches!(self, Self::Output { .. })
    }
    fn active_high(self) -> bool {
        match self {
            Self::Input { active_high } | Self::Output { active_high, .. } => active_high,
        }
    }
    fn levels(self) -> (bool, bool) {
        match self {
            Self::Input { .. } => (false, false),
            Self::Output {
                startup_on,
                safe_on,
                ..
            } => (startup_on, safe_on),
        }
    }
    fn percent(self, logical: f64) -> f64 {
        100.0
            * if self.active_high() {
                logical
            } else {
                1.0 - logical
            }
    }
}

fn status(code: i32) -> Result<(), ReadingQuality> {
    match code {
        0 => Ok(()),
        2 => Err(ReadingQuality::Unavailable),
        _ => Err(ReadingQuality::Fault),
    }
}

fn observation<T>(size: u32, version: u32, quality: u32) -> Result<(), ReadingQuality> {
    if size as usize != size_of::<T>() || version != 1 {
        return Err(ReadingQuality::Fault);
    }
    match quality {
        1 => Ok(()),
        0 => Err(ReadingQuality::Unavailable),
        _ => Err(ReadingQuality::Fault),
    }
}

impl Endpoint {
    /// Acquire only the requested endpoint from already-retained providers.
    pub fn open(
        backends: Arc<SiteBackends>,
        settings: &SiteEndpoint,
        direction: Direction,
    ) -> Result<Self, ReadingQuality> {
        let (startup, safe) = direction.levels();
        let mut context = ptr::null_mut();
        let device = match settings.backend {
            SiteBackend::Gpiod => {
                let table = backends
                    .gpiod
                    .as_ref()
                    .ok_or(ReadingQuality::Unavailable)?
                    .table;
                let endpoint = abi::rptadv_gpiod_endpoint_v1 {
                    struct_size: size_of::<abi::rptadv_gpiod_endpoint_v1>() as u32,
                    abi_version: 1,
                    device: settings.device.as_ptr(),
                    device_length: settings.device.len(),
                    line: settings
                        .channel
                        .parse()
                        .map_err(|_| ReadingQuality::Fault)?,
                    output: u32::from(direction.output()),
                    active_low: u32::from(!direction.active_high()),
                    startup_active: u32::from(startup),
                    safe_active: u32::from(safe),
                };
                status(unsafe { table.open.unwrap()(&endpoint, &mut context) })?;
                Device::Gpiod(context, table)
            }
            SiteBackend::Hwmon => {
                let table = backends
                    .hwmon
                    .as_ref()
                    .ok_or(ReadingQuality::Unavailable)?
                    .table;
                let endpoint = abi::rptadv_hwmon_endpoint_v1 {
                    struct_size: size_of::<abi::rptadv_hwmon_endpoint_v1>() as u32,
                    abi_version: 1,
                    device: settings.device.as_ptr(),
                    device_length: settings.device.len(),
                    channel: settings.channel.as_ptr(),
                    channel_length: settings.channel.len(),
                    output: u32::from(direction.output()),
                    startup_percent: direction.percent(f64::from(startup)),
                    safe_percent: direction.percent(f64::from(safe)),
                };
                status(unsafe { table.open.unwrap()(&endpoint, &mut context) })?;
                Device::Hwmon(context, table)
            }
            SiteBackend::Iio if !direction.output() => Device::Iio(
                IioReader::open(
                    Path::new("/sys/bus/iio/devices"),
                    &settings.device,
                    &settings.channel,
                )
                .map_err(|_| ReadingQuality::Unavailable)?,
            ),
            // CM119 and parallel access belongs to the existing radio owner.
            _ => return Err(ReadingQuality::Unavailable),
        };
        let endpoint = Self {
            device,
            direction,
            _backends: backends,
        };
        let mut capabilities = 0;
        let (context, query) = match &endpoint.device {
            Device::Gpiod(context, table) => (*context, table.capabilities.unwrap()),
            Device::Hwmon(context, table) => (*context, table.capabilities.unwrap()),
            Device::Iio(_) => return Ok(endpoint),
        };
        if context.is_null() {
            return Err(ReadingQuality::Fault);
        }
        status(unsafe { query(context, &mut capabilities) })?;
        let required = if direction.output() { 2 } else { 1 };
        if capabilities & required == 0 {
            return Err(ReadingQuality::Unavailable);
        }
        Ok(endpoint)
    }
    /// Read current data, never a zero substituted for a failed acquisition.
    pub fn read(&mut self) -> Result<Sample, ReadingQuality> {
        if self.direction.output() {
            return Err(ReadingQuality::Unavailable);
        }
        match &mut self.device {
            Device::Gpiod(context, table) => {
                let mut reading = abi::rptadv_gpiod_observation_v1 {
                    struct_size: size_of::<abi::rptadv_gpiod_observation_v1>() as u32,
                    abi_version: 1,
                    quality: 0,
                    active: 0,
                };
                status(unsafe { table.read.unwrap()(*context, &mut reading) })?;
                observation::<abi::rptadv_gpiod_observation_v1>(
                    reading.struct_size,
                    reading.abi_version,
                    reading.quality,
                )?;
                match reading.active {
                    0 | 1 => Ok(Sample::Digital(reading.active == 1)),
                    _ => Err(ReadingQuality::Fault),
                }
            }
            Device::Hwmon(context, table) => {
                let mut reading = abi::rptadv_hwmon_observation_v1 {
                    struct_size: size_of::<abi::rptadv_hwmon_observation_v1>() as u32,
                    abi_version: 1,
                    quality: 0,
                    value: 0.0,
                };
                status(unsafe { table.read.unwrap()(*context, &mut reading) })?;
                observation::<abi::rptadv_hwmon_observation_v1>(
                    reading.struct_size,
                    reading.abi_version,
                    reading.quality,
                )?;
                reading
                    .value
                    .is_finite()
                    .then_some(Sample::Number(reading.value))
                    .ok_or(ReadingQuality::Fault)
            }
            Device::Iio(reader) => reader
                .read()
                .map(Sample::Number)
                .map_err(|_| ReadingQuality::Fault),
        }
    }
    /// Submit normalized output demand. Fractional duty requires actual hardware PWM.
    pub fn write(&mut self, demand: f64) -> Result<(), ReadingQuality> {
        let code = match &mut self.device {
            Device::Iio(_) => return Err(ReadingQuality::Unavailable),
            _ if !self.direction.output() => return Err(ReadingQuality::Unavailable),
            _ if !demand.is_finite() || !(0.0..=1.0).contains(&demand) => {
                return Err(ReadingQuality::Fault);
            }
            Device::Gpiod(context, table) => {
                if demand != 0.0 && demand != 1.0 {
                    return Err(ReadingQuality::Unavailable);
                }
                let request = abi::rptadv_gpiod_output_v1 {
                    struct_size: size_of::<abi::rptadv_gpiod_output_v1>() as u32,
                    abi_version: 1,
                    active: u32::from(demand == 1.0),
                };
                unsafe { table.write.unwrap()(*context, &request) }
            }
            Device::Hwmon(context, table) => {
                let request = abi::rptadv_hwmon_output_v1 {
                    struct_size: size_of::<abi::rptadv_hwmon_output_v1>() as u32,
                    abi_version: 1,
                    percent: self.direction.percent(demand),
                };
                unsafe { table.write.unwrap()(*context, &request) }
            }
        };
        status(code)
    }
    /// Whether this acquired output provides hardware PWM.
    pub fn pwm(&self) -> bool {
        self.direction.output() && matches!(self.device, Device::Hwmon(..))
    }
    /// Commit compatible retirement metadata on the serial owner, without I/O.
    /// Startup is only used at first acquisition, never reapplied during reload.
    pub fn reconfigure(&mut self, direction: Direction) -> Result<(), ReadingQuality> {
        if !self.direction.same_io(direction) {
            return Err(ReadingQuality::Unavailable);
        }
        let safe = direction.levels().1;
        let changed = self.direction.levels().1 != safe;
        let code = match &self.device {
            Device::Gpiod(context, table) if changed => unsafe {
                table.set_safe.unwrap()(*context, u32::from(safe))
            },
            Device::Hwmon(context, table) if changed => unsafe {
                table.set_safe.unwrap()(*context, direction.percent(f64::from(safe)))
            },
            // Inputs and startup-only edits have no retirement metadata change.
            _ => 0,
        };
        status(code)?;
        self.direction = direction;
        Ok(())
    }
    /// Confirm safe output submission before allowing ownership to retire.
    pub fn safe(&mut self) -> Result<(), ReadingQuality> {
        if self.direction.output() {
            self.write(f64::from(self.direction.levels().1))
        } else {
            Ok(())
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // Explicit safe() must succeed before a replacement may acquire ownership.
        // Destruction alone is only a best-effort final shutdown attempt.
        unsafe {
            match &self.device {
                Device::Gpiod(context, table) => table.destroy.unwrap()(*context),
                Device::Hwmon(context, table) => table.destroy.unwrap()(*context),
                Device::Iio(_) => (),
            }
        }
    }
}

#[cfg(test)]
#[path = "endpoint_tests.rs"]
mod tests;
