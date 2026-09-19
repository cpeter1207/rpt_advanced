//! Continuous-source transmit watchdog and unkey-gated recovery.

#[derive(Default)]
pub(super) struct TimeoutPolicy {
    key: u64,
    until: Option<u64>,
}
impl TimeoutPolicy {
    pub fn unkey(&mut self, now: u64) {
        self.key = now;
    }
    pub fn apply(
        &mut self,
        requested: bool,
        was_keyed: bool,
        receive: bool,
        now: u64,
        timeout: u64,
        lockout: u64,
    ) -> bool {
        if self.until.is_some_and(|until| !receive && now >= until) {
            self.until = None;
        }
        if self.until.is_some() {
            return false;
        }
        if requested && !was_keyed {
            self.key = now;
        } else if requested && timeout != 0 && now.saturating_sub(self.key) >= timeout {
            self.until = Some(now.saturating_add(lockout));
            return false;
        }
        requested
    }
}
