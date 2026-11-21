//! Longest completed binding and handler-specific conversion, without collection or effects.
use super::{
    Action, ActionError, CommandSource, InvocationContext, MuteTarget, RadioAction, action,
    fields::{Fields, Result},
    prepare_runtime_action, radio,
};
use crate::{command::LinkAction, remote_base_command::parse_remote_command};
use serde_json::Value;

/// A validated complete command binding. The key remains independent of this spelling.
#[derive(Clone)]
pub struct Mapping {
    /// Empty disables this binding even when the parent had a code.
    pub command: String,
    /// This gate augments the domain's existing permissions.
    pub protected: bool,
    /// The built-in phone mute binding belongs to the actual phone source.
    pub phone_only: bool,
    pub(super) decoder: Decoder,
}
#[derive(Clone)]
pub(super) enum Decoder {
    Fixed(Action),
    Link(LinkAction),
    Radio(Option<radio::RadioDecoder>),
    Administration(bool),
    MutePeer,
    MutePhone,
    SendDigit(char),
}
impl Mapping {
    /// Only radio-family commands consume real peer unkey.
    pub(crate) fn radio(&self) -> bool {
        matches!(
            self.decoder,
            Decoder::Radio(_) | Decoder::Fixed(Action::Radio(_))
        )
    }

    pub(super) fn prepare(key: &str, body: &Value) -> Result<Self> {
        let mut obj = body.as_object().ok_or(ActionError::Invalid)?.clone();
        let command = obj
            .remove("command")
            .and_then(|v| {
                v.as_str()
                    .filter(|s| s.len() <= 63 && s.chars().all(crate::link::valid_digit))
                    .map(String::from)
            })
            .ok_or(ActionError::Field("command"))?;
        let protected = obj
            .remove("protected")
            .and_then(|v| v.as_bool())
            .ok_or(ActionError::Field("protected"))?;
        if obj.contains_key("parameter")
            || obj.contains_key("code")
            || obj.get("pin").is_some_and(|p| !p.is_object())
        {
            return Err(ActionError::SourceOnly);
        }
        let action = Value::Object(obj);
        let mut f = Fields::new(&action)?;
        let a = f.text("action")?;
        let decoder = match a {
            "unlock_administration" | "lock_administration" => {
                Decoder::Administration(a == "unlock_administration")
            }
            "site_status" if action.get("selector").is_none() => {
                Decoder::Link(LinkAction::SiteStatus)
            }
            "message" if action.get("message").is_none() && action.get("destination").is_none() => {
                Decoder::Link(LinkAction::Message)
            }
            "set_echo_telemetry" if action.get("mode").is_none() => {
                Decoder::Link(LinkAction::EchoTelemetry)
            }
            "radio" if action.get("operation").is_none() => Decoder::Radio(None),
            "rx_mute_toggle"
                if matches!(action["target"].as_str(), Some("phone_patch" | "peer")) =>
            {
                if f.text("target")? == "peer" {
                    Decoder::MutePeer
                } else {
                    Decoder::MutePhone
                }
            }
            "send_command_digit" => Decoder::SendDigit(f.digit("digit")?),
            _ => {
                if let Some(a) = action::link_action(a, &mut f)?
                    && action::node_action(a)
                {
                    Decoder::Link(a)
                } else {
                    let fixed = prepare_runtime_action(&action);
                    let decoder = match fixed {
                        Ok(p) if matches!(&p.action,Action::Host(h) if h.source_only()) => {
                            return Err(ActionError::SourceOnly);
                        }
                        Ok(p) => Decoder::Fixed(p.action),
                        Err(original) if a == "radio" => {
                            let d = radio::decoder(&mut f).map_err(|e| {
                                if e == ActionError::Field("operation") {
                                    original
                                } else {
                                    e
                                }
                            })?;
                            f.finish().map_err(|_| original)?;
                            Decoder::Radio(Some(d))
                        }
                        Err(e) => return Err(e),
                    };
                    return Ok(Self {
                        command,
                        protected,
                        phone_only: false,
                        decoder,
                    });
                }
            }
        };
        f.finish()?;
        let phone_only = key == "rx_mute_toggle" && matches!(decoder, Decoder::MutePhone);
        Ok(Self {
            command,
            protected,
            phone_only,
            decoder,
        })
    }
}
/// The complete candidate and exact unmatched tail, before grant checks and argument decoding.
pub struct Completed<'a> {
    /// Winning binding, even if authorization or its decoder subsequently rejects it.
    pub mapping: &'a Mapping,
    /// Unchanged raw tail; an empty tail remains a legitimate decoder input.
    pub parameter: &'a str,
}
impl Completed<'_> {
    /// Decode the selected transport input. This performs no admission or effects.
    pub fn decode(&self) -> Result<Action> {
        use Decoder as D;
        let p = self.parameter;
        Ok(match &self.mapping.decoder {
            D::Fixed(a) if p.is_empty() => a.clone(),
            D::Fixed(_) => return Err(ActionError::Parameter),
            D::Link(LinkAction::Message | LinkAction::SiteStatus)
                if p.is_empty() || !crate::config::message_selector_valid(p) =>
            {
                return Err(ActionError::Parameter);
            }
            D::Link(LinkAction::EchoTelemetry) if !matches!(p, "0" | "1" | "2" | "3") => {
                return Err(ActionError::Parameter);
            }
            D::Link(a) => action::link(*a, Some(p))?,
            D::Radio(d) => Action::Radio(
                match d {
                    Some(d) => d.decode(p),
                    None => parse_remote_command(p).map(RadioAction::Command),
                }
                .ok_or(ActionError::Parameter)?,
            ),
            D::Administration(unlock) => {
                if p.len() > 63 || !p.bytes().all(|b| b.is_ascii_digit()) || *unlock && p.is_empty()
                {
                    return Err(ActionError::Parameter);
                }
                Action::Administration(*unlock, (!p.is_empty()).then(|| p.into()))
            }
            D::MutePeer => Action::Mute(MuteTarget::Peer(action::node(Some(p))?)),
            D::MutePhone => Action::Mute(MuteTarget::PhonePatch((!p.is_empty()).then(|| p.into()))),
            D::SendDigit(d) => Action::SendCommandDigit(action::node(Some(p))?, (*d).into()),
        })
    }
    /// Recheck exact-source eligibility and entry protection before invoking the domain owner.
    /// The caller derives grant and radio-owner presence from its current control state.
    pub fn admit(
        &self,
        context: &InvocationContext,
        live_grant: bool,
        radio_enabled: bool,
        injected: bool,
    ) -> Result<super::Invocation> {
        let mut context = context.clone();
        context.requires_grant |= self.mapping.protected;
        context.recheck(live_grant)?;
        let source = &context.source;
        let phone = matches!(source,CommandSource::Connection{reply:r,..} if r.phone_source().is_some() && r.phone_authenticated());
        let actual = matches!(
            source,
            CommandSource::Receiver | CommandSource::Connection { .. }
        );
        if matches!(source,CommandSource::Connection{reply:r,..} if r.ended() || r.phone_source().is_some() && !r.phone_authenticated())
            || self.mapping.phone_only && !phone
            || !radio_enabled
                && matches!(
                    self.mapping.decoder,
                    Decoder::Radio(_) | Decoder::Fixed(Action::Radio(_))
                )
            || matches!(self.mapping.decoder, Decoder::Fixed(Action::PadTest))
                && (injected || !matches!(source, CommandSource::Receiver))
            || matches!(self.mapping.decoder, Decoder::Administration(_))
                && (!actual
                    || injected
                        && (!self.parameter.is_empty()
                            || matches!(self.mapping.decoder, Decoder::Administration(true))))
        {
            return Err(ActionError::SourceOnly);
        }
        let action = self.decode()?;
        action.check_scope(&context)?;
        if matches!(action, Action::Maintenance(_))
            && !matches!(source, CommandSource::Management(_) | CommandSource::Host)
        {
            return Err(ActionError::SourceOnly);
        }
        Ok(super::Invocation {
            context,
            action,
            parameter: Some(self.parameter.into()),
            origin: super::ParameterOrigin::Dtmf,
        })
    }
}
