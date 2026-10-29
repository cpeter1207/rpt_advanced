//! EchoLink telemetry eligibility beneath the controller's normal audience rules.
use super::PeerReply;
use crate::config::EchoLinkTelemetry;

/// ASL's LINK_HANG_TIME: demand is a command-response window, not RF activity.
const DEMAND_MS: u64 = 120_000;

/// Control-owned account policy. Connection windows live with exact reply tokens,
/// so reconnecting the same station cannot inherit a previous QSO's permission.
pub struct EchoTelemetryPolicy {
    mode: EchoLinkTelemetry,
    dynamic: bool,
    selected_until_ms: u64,
}
impl EchoTelemetryPolicy {
    /// Configuration reload establishes a fresh mode with demand initially closed.
    pub fn new(mode: EchoLinkTelemetry, dynamic: bool) -> Self {
        Self {
            mode,
            dynamic,
            selected_until_ms: 0,
        }
    }
    /// An already-authorized runtime selection; fixed account policies reject it.
    /// Selecting demand explicitly opens ASL's initial two-minute command window.
    pub fn select(&mut self, mode: EchoLinkTelemetry, now_ms: u64) -> bool {
        if !self.dynamic {
            return false;
        }
        self.mode = mode;
        self.selected_until_ms = if mode == EchoLinkTelemetry::Demand {
            now_ms.saturating_add(DEMAND_MS)
        } else {
            0
        };
        true
    }
    /// Refresh only at a telemetry-producing command/result boundary. No audio,
    /// directory, chat, heartbeat or arbitrary received digit calls this method.
    pub fn requested(&self, reply: &PeerReply, now_ms: u64) {
        if self.dynamic && self.mode == EchoLinkTelemetry::Demand && !reply.ended() {
            reply.set_telemetry_window(now_ms.saturating_add(DEMAND_MS));
        }
    }
    /// Additional eligibility only; this never broadens an existing audience.
    /// Follow uses local telemetry eligibility, never receiver/talker activity.
    pub fn allows(&self, reply: &PeerReply, now_ms: u64, local_enabled: bool) -> bool {
        !reply.ended()
            && match self.mode {
                EchoLinkTelemetry::Off => false,
                EchoLinkTelemetry::On => true,
                EchoLinkTelemetry::Follow => local_enabled,
                EchoLinkTelemetry::Demand => {
                    now_ms < self.selected_until_ms || now_ms < reply.telemetry_window()
                }
            }
    }
}
