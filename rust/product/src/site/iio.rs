//! Read configured kernel IIO measurements without changing device settings.
//!
//! The Linux IIO ABI supplies voltage/current/power/temperature in milli-units.
//! Convert to V/A/W/degrees C here; engineering calibration remains in the core.
//! See <https://docs.kernel.org/driver-api/iio/core.html> and the kernel's
//! `Documentation/ABI/testing/sysfs-bus-iio` for attribute scaling rules.

use std::{
    fs::File,
    io::{self, Read, Seek},
    path::Path,
};

/// One configured measurement retained by its device acquisition owner.
///
/// Attribute handles stay bound to the selected device until retirement. A lost
/// device is an I/O failure, never permission to select another enumerated ADC.
pub struct IioReader {
    sample: File,
    scale: Option<File>,
    offset: Option<File>,
}

impl IioReader {
    /// Open one explicit device path (relative to `root`, or absolute) and an
    /// `in_voltage`, `in_current`, `in_power` or `in_temp` channel stem.
    ///
    /// Prefer an already processed `_input`. Otherwise retain `_raw` and its
    /// channel-specific or shared scale/offset. Unscaled current alone has the
    /// documented processed-milliamp meaning; other raw values require scale.
    /// Use a stable device path or administrator-maintained symlink, not discovery
    /// order. This blocks on device setup and must never run on an audio callback.
    pub fn open(root: &Path, device: &str, channel: &str) -> io::Result<Self> {
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        if device.is_empty()
            || !channel
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(invalid());
        }
        let family = ["in_voltage", "in_current", "in_power", "in_temp"]
            .into_iter()
            .find(|family| {
                channel.strip_prefix(family).is_some_and(|suffix| {
                    suffix.is_empty()
                        || suffix.starts_with(|ch: char| ch.is_ascii_digit() || ch == '_')
                })
            })
            .ok_or_else(invalid)?;
        let directory = root.join(device).canonicalize()?;
        if let Some(sample) = optional(&directory.join(format!("{channel}_input")))? {
            return Ok(Self {
                sample,
                scale: None,
                offset: None,
            });
        }
        let sample = File::open(directory.join(format!("{channel}_raw")))?;
        let shared = if family == "in_voltage" && channel.contains("-voltage") {
            "in_voltage-voltage"
        } else {
            family
        };
        let attribute = |suffix: &str| -> io::Result<Option<File>> {
            for prefix in [channel, shared, family] {
                if let Some(file) = optional(&directory.join(format!("{prefix}_{suffix}")))? {
                    return Ok(Some(file));
                }
            }
            Ok(None)
        };
        let scale = attribute("scale")?;
        let offset = attribute("offset")?;
        if scale.is_none() && !(family == "in_current" && offset.is_none()) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(Self {
            sample,
            scale,
            offset,
        })
    }

    /// Acquire one finite normalized value, applying offset before scale once.
    ///
    /// Failed reads return an error, not zero or the previous sample. The owner
    /// timestamps successful acquisition and controls polling. This performs
    /// physical I/O; station control and audio must only consume its observation.
    pub fn read(&mut self) -> io::Result<f64> {
        let sample = number(&mut self.sample)?;
        let offset = self.offset.as_mut().map(number).transpose()?.unwrap_or(0.0);
        let scale = self.scale.as_mut().map(number).transpose()?.unwrap_or(1.0);
        finite((sample + offset) * scale / 1000.0)
    }
}

fn optional(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn number(file: &mut File) -> io::Result<f64> {
    file.rewind()?;
    let mut text = String::new();
    file.take(129).read_to_string(&mut text)?;
    if text.len() > 128 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    finite(
        text.trim()
            .parse()
            .map_err(|_| io::ErrorKind::InvalidData)?,
    )
}

fn finite(value: f64) -> io::Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(io::ErrorKind::InvalidData.into())
    }
}
