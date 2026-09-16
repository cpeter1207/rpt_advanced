//! Provider-local results; no controller types or implementation enter this DSO.

/// Preparation/descriptor failure mapped to the documented integer C ABI status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaError {
    /// Invalid settings or borrowed arguments.
    InvalidRequest,
    /// Local resource is unavailable.
    Unavailable,
    /// I/O or internal failure.
    Io,
    /// Child exited unsuccessfully.
    ProcessFailed,
    /// Child deadline expired.
    TimedOut,
    /// Request cancellation was observed.
    Cancelled,
    /// Decoder returned empty, malformed or non-finite samples.
    InvalidOutput,
    /// Descriptor does not provide the requested ABI.
    IncompatibleAdapter,
}

#[derive(Debug, PartialEq)]
pub(crate) struct PreparedAudio {
    rate: u32,
    samples: Vec<f32>,
}
impl PreparedAudio {
    pub(crate) fn new(rate: u32, samples: Vec<f32>) -> Result<Self, MediaError> {
        if rate == 0 || samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(MediaError::InvalidOutput);
        }
        Ok(Self { rate, samples })
    }
    pub(crate) fn samples(&self) -> &[f32] {
        &self.samples
    }
    pub(crate) fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
}

#[cfg(test)]
#[path = "result_tests.rs"]
mod tests;
