//! Strict action objects shared by runtime requests, named macros and DTMF bindings.
use super::ProtectedOperand;
use super::{
    ActionError,
    fields::{Fields, Result},
    host::{self, HostAction, InterfaceAction, MaintenanceAction},
    radio::{self, RadioAction},
};
use crate::{
    command::{Command, LinkAction},
    link::{PeerIdentity, ProtocolId},
    phone_dtmf::PhoneDtmfAction,
    site::{OutputRequest, SiteCommand},
};
use serde_json::Value;
use std::sync::Arc;

/// Explicit playback routing override for one admitted message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Local output only.
    Local,
    /// Currently eligible peers only.
    Peers,
    /// Local output and currently eligible peers.
    LocalAndLinks,
}
/// The owning port is supplied by the dispatcher, independently of the target.
#[derive(Clone, PartialEq, Eq)]
pub enum MuteTarget {
    /// Local qualified receiver.
    Port,
    /// Exact current direct peer selection, never dialed.
    Peer(ProtectedOperand<String>),
    /// Current network peers, optionally restricted by transport protocol.
    Peers(Option<ProtocolId>),
    /// Calling phone or explicit call identifier, otherwise unique attached caller.
    PhonePatch(Option<ProtectedOperand<String>>),
}
/// Prepared intent at the owners' control boundary. No serialization or unrestricted Debug.
#[derive(Clone)]
pub enum Action {
    /// Existing link handler; its source/grant/loop/peer checks still apply.
    Link(Command<ProtectedOperand<String>>),
    /// Existing radio operating owner.
    Radio(RadioAction),
    /// Dial the complete parameter on the registered account of this station.
    PhoneDial(ProtectedOperand<String>),
    /// End the invoking station's sole pending or established outgoing call.
    PhoneHangup,
    /// Existing site controller.
    Site(SiteCommand),
    /// Existing physical maintenance owner and bounded lease.
    Maintenance(MaintenanceAction),
    /// Compatibility interface owner.
    Interface(InterfaceAction),
    /// Controller lifecycle, management, query or actual source-bound owner.
    Host(HostAction),
    /// Await this named macro in the invoking scope.
    CallMacro(String),
    /// Feed a fresh local macro collector, retaining its own inactivity completion.
    ExecuteDtmf(ProtectedOperand<String>),
    /// Queue a composition with the explicit output selection.
    Message(String, Destination),
    /// Cooperative wait; None uses the prepared per-invocation pause setting.
    Pause(Option<u64>),
    /// Toggle the selected current ingress group.
    Mute(MuteTarget),
    /// Clear the actual source's selected remote target.
    ExitCommandMode,
    /// Deliver one digit to an addressed target, subject to existing policy.
    SendCommandDigit(ProtectedOperand<String>, ProtectedOperand<char>),
    /// Source-owned grant action. True unlocks; credentials never enter configuration or logs.
    Administration(bool, Option<ProtectedOperand<String>>),
    /// Actual local receiver keypad test.
    PadTest,
}
/// A fully typed invocation retaining generic input under its original typed provenance.
pub struct PreparedAction {
    /// Domain-specific operation.
    pub action: Action,
    /// Generic caller text, independent of any decoded node/selector. May be sensitive.
    pub parameter: Option<ProtectedOperand<String>>,
}
fn digits(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}
pub(super) fn node(s: Option<&str>) -> Result<ProtectedOperand<String>> {
    let s = s.ok_or(ActionError::Parameter)?;
    PeerIdentity::operator_target(s).map_err(|_| ActionError::Parameter)?;
    Ok(s.into())
}
fn name(f: &mut Fields<'_>) -> Result<String> {
    let s = f.text("name")?;
    if s.is_empty()
        || s.len() > 63
        || s.contains(['[', ']', ':'])
        || s.chars().any(char::is_whitespace)
    {
        return Err(ActionError::Field("name"));
    }
    Ok(s.into())
}
fn symbols(f: &mut Fields<'_>) -> Result<ProtectedOperand<String>> {
    let s = f.text("digits")?;
    if s.is_empty() || s.len() > 4096 || !s.chars().all(crate::link::valid_digit) {
        return Err(ActionError::Field("digits"));
    }
    Ok(s.into())
}
pub(super) fn phone_dial(parameter: Option<&str>) -> Result<Action> {
    let number = parameter.filter(|number| {
        (1..=64).contains(&number.len())
            && number.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'*' | b'#' | b'+'))
    }).ok_or(ActionError::Parameter)?;
    Ok(Action::PhoneDial(number.into()))
}
pub(super) fn link_action(action: &str, f: &mut Fields<'_>) -> Result<Option<LinkAction>> {
    use LinkAction as L;
    Ok(Some(match action {
        "connect" | "connect_permanent" => {
            let mode = if f.take("mode").is_some() {
                f.choice("mode", &["transceive", "monitor", "local-monitor"])?
            } else {
                0
            };
            if action == "connect" {
                [L::Transceive, L::Monitor, L::LocalMonitor][mode]
            } else {
                [
                    L::PermanentTransceive,
                    L::PermanentMonitor,
                    L::PermanentLocalMonitor,
                ][mode]
            }
        }
        "disconnect" => L::Disconnect,
        "select_command_target" => L::Command,
        "disconnect_permanent" => L::DisconnectPermanent,
        "link_status" => L::Status,
        "disconnect_all" => L::DisconnectAll,
        "pause_permanent_links" => L::DisconnectPermanentAll,
        "last_keyed" => L::LastKeyed,
        "network_status" => L::FullStatus,
        "reconnect_all" => L::ReconnectAll,
        "resume_permanent_links" => L::ReconnectPermanentAll,
        "disconnect_nonpermanent_all" => L::DisconnectNonPermanentAll,
        "announce_time" => L::Time,
        "cancel_message" => L::CancelMessage,
        "set_parrot" => {
            if f.flag("enabled")? {
                L::ParrotEnable
            } else {
                L::ParrotDisable
            }
        }
        _ => return Ok(None),
    }))
}
pub(super) fn node_action(a: LinkAction) -> bool {
    matches!(
        a,
        LinkAction::Transceive
            | LinkAction::Monitor
            | LinkAction::LocalMonitor
            | LinkAction::PermanentTransceive
            | LinkAction::PermanentMonitor
            | LinkAction::PermanentLocalMonitor
            | LinkAction::Disconnect
            | LinkAction::Command
            | LinkAction::DisconnectPermanent
    )
}
pub(super) fn link(a: LinkAction, p: Option<&str>) -> Result<Action> {
    let arg = if node_action(a) {
        node(p)?
    } else if matches!(
        a,
        LinkAction::SiteStatus | LinkAction::Message | LinkAction::EchoTelemetry
    ) {
        p.unwrap_or_default().into()
    } else {
        "".into()
    };
    Ok(Action::Link(Command {
        action: a,
        node: arg,
    }))
}
fn host_action(action: &str, f: &mut Fields<'_>, p: Option<&str>) -> Result<HostAction> {
    use HostAction as H;
    Ok(match action {
        "controller_query" => match f.text("operation")? {
            "nodes" => H::Nodes,
            "configuration_path" => H::ConfigurationPath,
            "status" => H::Status,
            "help" => H::Help,
            "link_status" => H::LinkStatus,
            _ => return Err(ActionError::Field("operation")),
        },
        "controller_management" => match f.text("operation")? {
            "reload" => H::Reload,
            "validate_configuration" => H::ValidateConfiguration,
            "validate_secrets" => H::ValidateSecrets,
            "validate_providers" => H::ValidateProviders,
            _ => return Err(ActionError::Field("operation")),
        },
        "controller_lifecycle" => match f.text("operation")? {
            "start" => H::Start,
            "stop" => H::Stop,
            "reload_poll" => H::ReloadPoll,
            _ => return Err(ActionError::Field("operation")),
        },
        "radio_host_query" if f.text("operation")? == "receive_policy" => H::ReceivePolicy,
        "controller_admission" => match f.text("operation")? {
            "authorize_incoming" => H::AuthorizeIncoming,
            "admit_incoming" => H::AdmitIncoming,
            _ => return Err(ActionError::Field("operation")),
        },
        "feed_dtmf_symbol" => H::Symbol(f.digit("digit")?.into()),
        "actual_receiver_unkey" => H::ReceiverUnkey,
        "controller_input" if f.text("operation")? == "dtmf" => H::Input(symbols(f)?),
        "phone_session" => match f.text("operation")? {
            "authenticate" => {
                let mailbox = f.text("mailbox")?;
                let pin = f.text("pin")?;
                if !digits(mailbox, 1, 32) || !digits(pin, 4, 32) {
                    return Err(ActionError::Invalid);
                }
                H::PhoneAuthenticate(mailbox.into(), pin.into())
            }
            "retire" => H::PhoneRetire,
            _ => return Err(ActionError::Field("operation")),
        },
        action => {
            use PhoneDtmfAction as P;
            H::PhoneGrammar(Arc::new(match action {
                "phone_attach" => P::Attach(p.filter(|text|!text.is_empty()).map(|text|node(Some(text))).transpose()?),
                "phone_select_command_target" => P::SelectTarget(node(p)?),
                "phone_send_node_digit" => P::NodeDigit {
                    target: node(p)?,
                    digit: f.digit("digit")?.into(),
                },
                "phone_send_telephone_digit" => P::TelephoneDigit(f.digit("digit")?.into()),
                "phone_exit_command_mode" => P::ExitCommandMode,
                "phone_manual_key" => P::ManualKey,
                "phone_manual_unkey" => P::ManualUnkey,
                _ => return Err(ActionError::Invalid),
            }))
        }
    })
}
/// Parse one complete runtime action. No host authority is conferred by parsing.
pub fn prepare_runtime_action(v: &Value) -> Result<PreparedAction> {
    prepare(v, false)
}
/// Parse a complete persisted action with the narrower source/credential policy.
pub fn prepare_macro_action(v: &Value) -> Result<PreparedAction> {
    prepare(v, true)
}
fn prepare(v: &Value, persisted: bool) -> Result<PreparedAction> {
    let mut f = Fields::new(v)?;
    let a = f.text("action")?;
    let p = f
        .take("parameter")
        .map(|v| {
            v.as_str()
                .filter(|s| s.chars().count() <= 4096)
                .ok_or(ActionError::Parameter)
        })
        .transpose()?;
    if persisted
        && (a == "unlock_administration"
            || a == "lock_administration" && (p.is_some() || v.get("code").is_some()))
    {
        return Err(ActionError::SourceOnly);
    }
    let action = if let Some(a) = link_action(a, &mut f)? {
        link(a, p)?
    } else {
        match a {
            "site_status" => {
                let s = f.text("selector")?;
                if s.is_empty() || !crate::config::message_selector_valid(s) {
                    return Err(ActionError::Field("selector"));
                }
                link(LinkAction::SiteStatus, Some(s))?
            }
            "set_echo_telemetry" => link(
                LinkAction::EchoTelemetry,
                Some(["0", "1", "2", "3"][f.choice("mode", &["off", "on", "follow", "demand"])?]),
            )?,
            "exit_command_mode" => Action::ExitCommandMode,
            "send_command_digit" => Action::SendCommandDigit(node(p)?, f.digit("digit")?.into()),
            "radio" => Action::Radio(radio::typed(&mut f)?),
            "set_output" => Action::Site(SiteCommand::Output {
                name: name(&mut f)?,
                request: [OutputRequest::On, OutputRequest::Off, OutputRequest::Pulse]
                    [f.choice("state", &["on", "off", "pulse"])?],
            }),
            "acknowledge_alarm" => Action::Site(SiteCommand::AlarmAcknowledge(name(&mut f)?)),
            "reset_alarm" => Action::Site(SiteCommand::AlarmReset(name(&mut f)?)),
            "reset_extrema" => Action::Site(SiteCommand::ResetExtrema(name(&mut f)?)),
            "set_transmit_inhibit" => Action::Site(SiteCommand::Inhibit(f.flag("enabled")?)),
            "set_fan" => {
                let name = name(&mut f)?;
                let v = f.get("duty")?;
                let duty = if v.as_str() == Some("auto") {
                    None
                } else {
                    Some(
                        v.as_f64()
                            .filter(|n| (0.0..=1.0).contains(n))
                            .ok_or(ActionError::Field("duty"))?,
                    )
                };
                Action::Site(SiteCommand::Fan { name, duty })
            }
            "radio_maintenance" => Action::Maintenance(host::maintenance(&mut f)?),
            "interface_control" => Action::Interface(host::interface(&mut f)?),
            "call_macro" => {
                let key = f.text("macro")?;
                if key.is_empty() || key.chars().count() > 63 {
                    return Err(ActionError::Field("macro"));
                }
                if p.is_some_and(|s| !s.is_empty()) {
                    return Err(ActionError::Parameter);
                }
                Action::CallMacro(key.into())
            }
            "execute_dtmf" => Action::ExecuteDtmf(symbols(&mut f)?),
            "phone_dial" => phone_dial(p)?,
            "phone_hangup" => {
                if p.is_some() {
                    return Err(ActionError::Parameter);
                }
                Action::PhoneHangup
            }
            "message" => {
                let m = f.text("message")?;
                if m.chars().count() > 65536 {
                    return Err(ActionError::Field("message"));
                }
                Action::Message(
                    m.into(),
                    [
                        Destination::Local,
                        Destination::Peers,
                        Destination::LocalAndLinks,
                    ][f.choice("destination", &["local", "peers", "local_and_links"])?],
                )
            }
            "pause" => {
                let duration = if let Some(t) = f.take("timing") {
                    let mut t = Fields::new(t)?;
                    let d = t
                        .take("duration_ms")
                        .map(|_| t.uint("duration_ms", 1, 120000))
                        .transpose()?;
                    t.finish()?;
                    d
                } else {
                    None
                };
                Action::Pause(duration)
            }
            "rx_mute_toggle" => Action::Mute(match f.text("target")? {
                "port" => MuteTarget::Port,
                "peer" => MuteTarget::Peer(node(p)?),
                "peers" => MuteTarget::Peers(
                    f.take("link_type")
                        .map(|_| {
                            f.choice("link_type", &["allstarlink", "echolink"])
                                .map(|i| [ProtocolId::AllStarLink, ProtocolId::EchoLink][i])
                        })
                        .transpose()?,
                ),
                "phone_patch" => {
                    MuteTarget::PhonePatch(p.filter(|s| !s.is_empty()).map(Into::into))
                }
                _ => return Err(ActionError::Field("target")),
            }),
            "start_pad_test" => Action::PadTest,
            "unlock_administration" | "lock_administration" => {
                let code = f
                    .take("code")
                    .map(|v| v.as_str().ok_or(ActionError::Field("code")))
                    .transpose()?;
                if code.is_some() && p.is_some()
                    || a == "unlock_administration" && code.or(p).is_none()
                    || code.or(p).is_some_and(|s| !digits(s, 1, 63))
                {
                    return Err(ActionError::Parameter);
                }
                Action::Administration(a == "unlock_administration", code.or(p).map(Into::into))
            }
            _ => Action::Host(host_action(a, &mut f, p)?),
        }
    };
    if persisted
        && (matches!(&action,Action::Host(h) if h.source_only())
            || matches!(action, Action::PadTest))
    {
        return Err(ActionError::SourceOnly);
    }
    f.finish()?;
    Ok(PreparedAction {
        action,
        parameter: p.map(Into::into),
    })
}
