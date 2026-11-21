//! Exact control-plane payloads for the existing host, maintenance and interface owners.
use super::ProtectedOperand;
use super::{
    ActionError,
    fields::{Fields, Result},
};
use crate::phone_dtmf::PhoneDtmfAction;

/// Physical mixer channel.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum MixerTarget {
    /// Receive input.
    Receive,
    /// First transmitter output.
    TransmitA,
    /// Second transmitter output.
    TransmitB,
}
/// Maintenance intent. Submission retains the existing physical lease/capability checks.
#[derive(Clone)]
pub enum MaintenanceAction {
    /// Physical status.
    Status,
    /// Current processing status.
    ProcessingStatus,
    /// Public active configuration.
    ActiveConfiguration,
    /// Query one mixer.
    GetMixer(MixerTarget),
    /// Set one mixer to a validated level.
    SetMixer(MixerTarget, u16),
    /// Set test tone under the caller's lease.
    TestTone(bool),
    /// Read the tuning EEPROM.
    ReadEeprom,
    /// Write the tuning region; validity belongs to the provider.
    WriteEeprom(Box<[u16; 64]>),
    /// Set CTCSS inhibit.
    CtcssInhibit(bool),
    /// Set the subaudible override.
    SubaudibleOverride(bool),
    /// Save tuning to EEPROM.
    SaveTuning,
    /// Key the leased transmitter with an optional tone in tenths of Hz.
    Key(Option<u16>),
    /// Release manually keyed transmit.
    Unkey,
    /// Release the actual owner's manual overrides.
    ClearManual,
}
/// Hardware identifier within its actual pin namespace.
#[derive(Clone, Copy)]
pub enum Pin {
    /// Sound-card GPIO.
    SoundGpio(u8),
    /// Parallel port.
    Parallel(u8),
}
/// Interface intent, subject to the existing adapter's availability and pin ownership.
#[derive(Clone)]
pub enum InterfaceAction {
    /// Select a provider RF channel.
    Channel(u8),
    /// Control RX CTCSS.
    ReceiveCtcss(bool),
    /// Control TX CTCSS.
    TransmitCtcss(bool),
    /// Write a logical pin.
    Write(Pin, bool),
    /// Pulse a pin for a validated count of 100-millisecond ticks.
    Pulse(Pin, u32),
    /// Exact RX/TX hertz, optional RX/TX tone in tenths of Hz and high-power flag.
    Frequencies(u32, u32, Option<u16>, Option<u16>, bool),
    /// Query Asterisk radio-channel echo.
    GetEcho,
    /// Set Asterisk radio-channel echo.
    SetEcho(bool),
}
/// Host-owned command, keeping source-only and unavailable grammar intentions explicit.
#[derive(Clone)]
pub enum HostAction {
    /// Enumerate nodes.
    Nodes,
    /// Report the selected configuration path.
    ConfigurationPath,
    /// Report controller status.
    Status,
    /// Report supported commands.
    Help,
    /// Query link status through the existing result owner.
    LinkStatus,
    /// Request coordinated reload.
    Reload,
    /// Validate the staged configuration.
    ValidateConfiguration,
    /// Validate the protected configuration.
    ValidateSecrets,
    /// Validate provider availability.
    ValidateProviders,
    /// Start via the lifecycle owner.
    Start,
    /// Stop via the lifecycle owner.
    Stop,
    /// Poll the actual lifecycle owner.
    ReloadPoll,
    /// Host-only receive policy.
    ReceivePolicy,
    /// Real ingress symbol.
    Symbol(ProtectedOperand<char>),
    /// Real receiver falling edge.
    ReceiverUnkey,
    /// Real ingress symbols; never a macro collector.
    Input(ProtectedOperand<String>),
    /// Authenticated call grammar, currently inert.
    PhoneGrammar(std::sync::Arc<PhoneDtmfAction<ProtectedOperand<String>, ProtectedOperand<char>>>),
    /// Authenticate the actual call with ephemeral mailbox digits and PIN.
    PhoneAuthenticate(ProtectedOperand<String>, ProtectedOperand<String>),
    /// Retire the exact live call.
    PhoneRetire,
    /// Provider-owned incoming authorization.
    AuthorizeIncoming,
    /// Provider-owned incoming admission.
    AdmitIncoming,
}
impl HostAction {
    pub(super) fn source_only(&self) -> bool {
        !matches!(
            self,
            Self::Nodes
                | Self::ConfigurationPath
                | Self::Status
                | Self::Help
                | Self::LinkStatus
                | Self::Reload
                | Self::ValidateConfiguration
                | Self::ValidateSecrets
                | Self::ValidateProviders
        )
    }
}
fn mixer(f: &mut Fields<'_>) -> Result<MixerTarget> {
    Ok([
        MixerTarget::Receive,
        MixerTarget::TransmitA,
        MixerTarget::TransmitB,
    ][f.choice("target", &["receive", "transmit_a", "transmit_b"])?])
}
pub(super) fn maintenance(f: &mut Fields<'_>) -> Result<MaintenanceAction> {
    use MaintenanceAction as M;
    Ok(match f.text("operation")? {
        "status" => M::Status,
        "processing_status" => M::ProcessingStatus,
        "active_configuration" => M::ActiveConfiguration,
        "get_mixer" => M::GetMixer(mixer(f)?),
        "set_mixer" => M::SetMixer(mixer(f)?, f.uint("level", 0, 999)? as u16),
        "set_test_tone" => M::TestTone(f.flag("enabled")?),
        "read_eeprom" => M::ReadEeprom,
        "write_eeprom" => {
            let words = f
                .get("words")?
                .as_array()
                .filter(|v| v.len() == 64)
                .ok_or(ActionError::Field("words"))?;
            let mut region = Box::new([0; 64]);
            for (out, word) in region.iter_mut().zip(words) {
                *out = word
                    .as_u64()
                    .and_then(|n| n.try_into().ok())
                    .ok_or(ActionError::Field("words"))?;
            }
            M::WriteEeprom(region)
        }
        "set_ctcss_inhibit" => M::CtcssInhibit(f.flag("enabled")?),
        "set_subaudible_override" => M::SubaudibleOverride(f.flag("enabled")?),
        "save_tuning_eeprom" => M::SaveTuning,
        "clear_manual" => M::ClearManual,
        "transmit" if f.flag("keyed")? => M::Key(optional_tone(f, "ctcss_frequency_hz")?),
        "transmit" => M::Unkey,
        _ => return Err(ActionError::Field("operation")),
    })
}
fn optional_tone(f: &mut Fields<'_>, key: &'static str) -> Result<Option<u16>> {
    f.take(key)
        .map(|_| f.scaled(key, 1, 500, 3000).map(|n| n as u16))
        .transpose()
}
pub(super) fn interface(f: &mut Fields<'_>) -> Result<InterfaceAction> {
    use InterfaceAction as I;
    Ok(match f.text("operation")? {
        "set_channel" => I::Channel(f.uint("channel", 0, 255)? as u8),
        "set_receive_ctcss" => I::ReceiveCtcss(f.flag("enabled")?),
        "set_transmit_ctcss" => I::TransmitCtcss(f.flag("enabled")?),
        "get_echo" => I::GetEcho,
        "set_echo" => I::SetEcho(f.flag("enabled")?),
        "set_frequencies" => I::Frequencies(
            f.scaled("rx_frequency_mhz", 6, 1, u32::MAX.into())? as u32,
            f.scaled("tx_frequency_mhz", 6, 1, u32::MAX.into())? as u32,
            optional_tone(f, "rx_ctcss_hz")?,
            optional_tone(f, "tx_ctcss_hz")?,
            f.choice("power", &["low", "high"])? == 1,
        ),
        "write_pin" => {
            let mut p = Fields::new(f.get("pin")?)?;
            let pin = match p.text("domain")? {
                "sound_gpio" => Pin::SoundGpio(p.uint("pin", 1, 8)? as u8),
                "parallel" => Pin::Parallel(p.uint("pin", 2, 9)? as u8),
                _ => return Err(ActionError::Field("pin")),
            };
            p.finish()?;
            match f.text("state")? {
                "on" => I::Write(pin, true),
                "off" => I::Write(pin, false),
                "pulse" => {
                    let ms = f.duration(100, 429496729400)?;
                    if ms % 100 != 0 {
                        return Err(ActionError::Field("timing"));
                    }
                    I::Pulse(pin, (ms / 100) as u32)
                }
                _ => return Err(ActionError::Field("state")),
            }
        }
        _ => return Err(ActionError::Field("operation")),
    })
}
