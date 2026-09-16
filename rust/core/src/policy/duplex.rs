//! Half- and full-duplex transmitter keying policy.

/// Stateful transmitter hang-time policy.
#[derive(Default)]
pub struct DuplexPolicy {
    keyed: bool,
    last_audio_ms: u64,
    release_hang_ms: u64,
}

impl DuplexPolicy {
    /// Apply one receiver/program-audio state change and return whether PTT remains asserted.
    pub fn update(
        &mut self,
        full_duplex: bool,
        receiver_active: bool,
        transmit_active: bool,
        now_ms: u64,
        hang_ms: u64,
    ) -> bool {
        if !full_duplex && receiver_active {
            self.keyed = false;
        } else if transmit_active || (full_duplex && receiver_active) {
            self.keyed = true;
            self.last_audio_ms = now_ms;
            self.release_hang_ms = hang_ms;
        } else if self.keyed && now_ms - self.last_audio_ms >= self.release_hang_ms {
            self.keyed = false;
        }
        self.keyed
    }
}

#[cfg(test)]
#[path = "duplex_tests.rs"]
mod tests;
