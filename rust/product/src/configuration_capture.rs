//! Read-only capture of working files selected by the controller's resource registry.

use rpt_advanced_core::config::resources::candidate::{
    CandidateError, CapturedResource, ModularCandidate, ResourceBinding, ResourceKind,
    ResourceRegistry,
};
use rptadv_peer_support::working_files::{
    CaptureError, CaptureLimits, CapturePath, EditGuard, FileRequirement, Fingerprint,
    WorkingSnapshot,
};
use std::{fmt, path::PathBuf};

/// A filesystem or structural error assembling registered working files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisteredCaptureError {
    /// The protected working files could not be captured.
    Capture(CaptureError),
    /// The captured documents could not be assembled.
    Candidate(CandidateError),
}

impl fmt::Display for RegisteredCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capture(error) => error.fmt(formatter),
            Self::Candidate(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RegisteredCaptureError {}

/// Staged resource data and the working snapshot from which it was assembled.
pub struct RegisteredCapture<'registry> {
    candidate: ModularCandidate<'registry>,
    snapshot: WorkingSnapshot,
}

impl<'registry> RegisteredCapture<'registry> {
    /// Borrow the assembled candidate for later domain validation.
    pub fn candidate(&self) -> &ModularCandidate<'registry> {
        &self.candidate
    }

    /// Return the fingerprint of the selected paths and exact bytes.
    pub fn fingerprint(&self) -> Fingerprint {
        self.snapshot.fingerprint()
    }

    /// Check the captured selection again under the edit lock.
    pub fn verify_current(&self, guard: &EditGuard<'_>) -> Result<(), CaptureError> {
        guard.verify_current(&self.snapshot)
    }
}

impl fmt::Debug for RegisteredCapture<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredCapture")
            .finish_non_exhaustive()
    }
}

/// Capture registered working paths and assemble their resource documents.
///
/// Site and port metadata are required. Other bound sections are optional.
/// The snapshot covers only the paths selected by this registry.
pub fn capture_registered<'registry>(
    guard: &EditGuard<'_>,
    registry: &'registry ResourceRegistry,
    limits: CaptureLimits,
) -> Result<RegisteredCapture<'registry>, RegisteredCaptureError> {
    let bindings = registry.bindings();
    if bindings.len() > limits.max_paths {
        return Err(RegisteredCaptureError::Capture(CaptureError::Limits));
    }
    let paths = bindings
        .map(|binding| {
            let relative = relative_path(binding);
            if relative.as_os_str().len() > limits.max_path_bytes {
                return Err(CaptureError::Limits);
            }
            let requirement = if binding.kind == ResourceKind::Metadata {
                FileRequirement::Required
            } else {
                FileRequirement::Optional
            };
            CapturePath::new(relative, requirement)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(RegisteredCaptureError::Capture)?;
    let snapshot = guard
        .capture(&paths, limits)
        .map_err(RegisteredCaptureError::Capture)?;
    let captured: Vec<_> = registry
        .bindings()
        .zip(&paths)
        .filter_map(|(binding, path)| {
            let json = snapshot.file(path.relative())?;
            Some(CapturedResource { binding, json })
        })
        .collect();
    let candidate = ModularCandidate::assemble(registry, &captured)
        .map_err(RegisteredCaptureError::Candidate)?;
    Ok(RegisteredCapture {
        candidate,
        snapshot,
    })
}

/// Derive the working filename from the scope UUID and fixed kind layout.
fn relative_path(binding: ResourceBinding) -> PathBuf {
    rpt_advanced_core::config::resources::catalog::relative_path(binding).into()
}

#[cfg(test)]
mod tests;
