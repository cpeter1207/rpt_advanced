//! Bounded native-rate sums; representation clipping belongs to the adapter.
use super::{AdmissionError, Mode};

/// One already-rendered peer input at the fixed native rate.
pub struct MixSource<'a> {
    /// Generation-owned slot identity used for mix-minus exclusion.
    pub identity: usize,
    /// Source forwarding policy.
    pub mode: Mode,
    /// Whether current ring output represents active reception.
    pub active: bool,
    /// Native F32 output from this peer's sole inbound ring.
    pub audio: &'a [f32],
}

/// Stateless bounded mixing over prepared native-rate blocks, without I/O or locks.
pub struct NativeMixer {
    maximum: usize,
}
impl NativeMixer {
    /// Establish the maximum supplied callback length outside real-time work.
    pub fn new(maximum: usize) -> Result<Self, AdmissionError> {
        if maximum == 0 {
            return Err(AdmissionError::Invalid);
        }
        Ok(Self { maximum })
    }
    fn check(&self, sources: &[MixSource<'_>], count: usize) -> Result<(), AdmissionError> {
        if count > self.maximum || sources.iter().any(|source| source.audio.len() != count) {
            return Err(AdmissionError::Invalid);
        }
        Ok(())
    }
    /// Sum every active peer for local transmit, including local-monitor peers.
    pub fn local(
        &self,
        sources: &[MixSource<'_>],
        output: &mut [f32],
    ) -> Result<bool, AdmissionError> {
        self.check(sources, output.len())?;
        output.fill(0.0);
        let mut active = false;
        for source in sources.iter().filter(|source| source.active) {
            active = true;
            for (output, input) in output.iter_mut().zip(source.audio) {
                *output += input;
            }
        }
        Ok(active)
    }
    /// Build one peer's mix-minus block before outbound queuing and encoding.
    pub fn destination(
        &self,
        destination: usize,
        mode: Mode,
        local: &[f32],
        receiving: bool,
        sources: &[MixSource<'_>],
        output: &mut [f32],
    ) -> Result<bool, AdmissionError> {
        self.check(sources, output.len())?;
        if local.len() != output.len() {
            return Err(AdmissionError::Invalid);
        }
        output.fill(0.0);
        if !mode.transmits() {
            return Ok(false);
        }
        if receiving {
            output.copy_from_slice(local);
        }
        let mut active = receiving;
        for source in sources.iter().filter(|source| {
            source.active && source.identity != destination && source.mode.forwards()
        }) {
            active = true;
            for (output, input) in output.iter_mut().zip(source.audio) {
                *output += input;
            }
        }
        Ok(active)
    }
}
