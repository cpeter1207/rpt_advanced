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
