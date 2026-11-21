//! Controller-owned admission and activity policy accompanying a prepared physical station.
use rpt_advanced_core::{
    config::resources::profiles::radio::RadioPortProfile, controller::ControllerSettings,
};

/// Settings consumed by controller admission and its existing transmit-policy machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerRadioPolicy {
    /// Allows the local owner to accept received programme.
    pub receive_enabled: bool,
    /// Allows the local owner to submit transmit programme and PTT demand.
    pub transmit_enabled: bool,
    parrot: bool,
    full_duplex: bool,
    hang: u32,
    timeout: u32,
    ctcss_on_input: bool,
    ctcss_hang: u32,
    kerchunk: u32,
}
impl ControllerRadioPolicy {
    pub(super) fn from_profile(p: &RadioPortProfile) -> Self {
        Self {
            receive_enabled: p.rx_audio.enabled,
            transmit_enabled: p.tx_audio.enabled,
            parrot: p.general.parrot_enabled,
            full_duplex: p.general.full_duplex(),
            hang: p.signals.tx.hang_ms,
            timeout: p.signals.tx.timeout_ms,
            ctcss_on_input: p.signals.tx.ctcss.on_input,
            ctcss_hang: p.signals.tx.ctcss.hang_ms,
            kerchunk: p.signals.rx.kerchunk_max_ms,
        }
    }
    /// Populate the existing controller's radio settings before preparing its graph.
    pub fn apply(&self, settings: &mut ControllerSettings) {
        settings.parrot_enabled = self.parrot;
        settings.full_duplex = self.full_duplex;
        settings.hang_ms = u64::from(self.hang);
        settings.ctcss_encode_on_input = self.ctcss_on_input;
        settings.ctcss_hang_ms = u64::from(self.ctcss_hang);
        settings.transmit_timeout_ms = u64::from(self.timeout);
        settings.kerchunk_max_ms = u64::from(self.kerchunk);
    }
}
