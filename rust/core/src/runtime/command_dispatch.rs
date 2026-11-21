//! Execute prepared actions on the same serialized owners used by the product host.
use super::*;
use crate::{
    command_language::{
        Action, CommandSource, Invocation, InvocationContext, ParameterOrigin, RadioAction,
    },
    config::resources::candidate::{ModularCandidate, ResourceId, ResourceScope},
    runtime::dtmf::{CommandProfile, MappedInput},
};

/// Renderer inputs already resolved by the owning generation.
pub struct CommandRenderers<'a> {
    /// Installed locale catalog used by the active station.
    pub catalog: MessageCatalog,
    /// Effective language of this port.
    pub language: &'a unic_langid::LanguageIdentifier,
    /// Effective public speech renderer defaults.
    pub speech: &'a crate::config::ResolvedSpeechSettings,
    /// Effective public Morse renderer defaults.
    pub morse: &'a crate::config::ResolvedMorseSettings,
}
pub(super) struct CommandMessages {
    messages: crate::config::resources::profiles::public_messages::ScopedMessages,
    fixed: std::collections::BTreeMap<String, crate::composition::Composition>,
    port: super::super::MessagePortIdentity,
    selectors: std::collections::BTreeMap<String, String>,
}
impl<A: Send, C: Send> RuntimeNode<A, C> {
    /// Obtain the current port command generation independently of its audio routing.
    pub fn command_work(&self) -> Option<Arc<GenerationWork>> {
        self.programs.generation.as_ref().map(|g| g.0.clone())
    }
    /// The authenticated ASL account used on the wire. A registered port may lack an account.
    pub fn network_account(&self) -> Option<&str> {
        match self.links.command_profile() {
            Some(p) => p.network_node.as_deref(),
            None => self
                .name
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then_some(&self.name),
        }
    }
    /// Bind the registry's command language to an already-running port. Legacy network names
    /// identify a running slot only when its explicit AllStarLink account actually matches.
    pub fn install_command_profile(
        &mut self,
        candidate: &ModularCandidate<'_>,
        port: ResourceId,
        zone: Option<&str>,
        clock: RuntimeClock,
        renderers: CommandRenderers<'_>,
    ) -> Result<(), RuntimeError> {
        let profiles = candidate
            .resolve_protocol_profiles()
            .map_err(|_| RuntimeError::Preparation)?;
        let registered = profiles.port(port).ok_or(RuntimeError::MissingNode)?;
        let account = registered
            .allstarlink
            .as_ref()
            .and_then(|p| p.settings.node_number);
        let network_node = account.map(|n| n.to_string());
        // Numeric and UUID runtime slots have exact registry owners. Public labels are
        // only eligible when they do not name a different account or port.
        let numeric_slot = self
            .name
            .parse::<std::num::NonZeroU64>()
            .ok()
            .filter(|_| self.name.bytes().all(|b| b.is_ascii_digit()));
        let other_owner = profiles.ports().any(|(id, p)| {
            id != port
                && (self.name == id.to_string()
                    || p.allstarlink
                        .as_ref()
                        .and_then(|a| a.settings.node_number)
                        .is_some_and(|n| Some(n) == numeric_slot))
        });
        if !registered.metadata.enabled
            || other_owner
            || !(self.name == port.to_string()
                || account.is_some() && numeric_slot == account
                || self.name == registered.key.as_str())
        {
            return Err(RuntimeError::MissingNode);
        }
        let scope = ResourceScope::Port(port);
        if self.programs.owner.is_some_and(|owner| owner != scope) {
            return Err(RuntimeError::Rejected);
        }
        let settings = candidate
            .command_language(scope)
            .map_err(|_| RuntimeError::Preparation)?;
        let messages = candidate
            .message_profiles(
                scope,
                renderers.catalog,
                renderers.language,
                renderers.speech,
                renderers.morse,
            )
            .map_err(|_| RuntimeError::Preparation)?;
        let fixed = messages
            .command_messages(&settings)
            .map_err(|_| RuntimeError::Preparation)?;
        let selectors = messages
            .effective
            .iter()
            .filter_map(|(name, (_, body))| {
                body.get("dtmf_id")?
                    .as_str()
                    .filter(|selector| !selector.is_empty())
                    .map(|selector| (selector.to_owned(), name.clone()))
            })
            .collect();
        let projections = CommandMessages {
            messages,
            fixed,
            port: registered.into(),
            selectors,
        };
        let events = candidate
            .event_profiles(&settings, zone)
            .map_err(|_| RuntimeError::Preparation)?;
        let generation = match self.programs.scheduler.as_ref() {
            Some(previous) => previous
                .generation()
                .checked_add(1)
                .ok_or(RuntimeError::Preparation)?
                .max(self.control.generation),
            None => self.control.generation,
        };
        let mut scheduler = EventScheduler::from_profiles(
            generation,
            &events,
            self.programs
                .scheduler
                .as_ref()
                .filter(|_| self.programs.owner == Some(scope)),
        )
        .map_err(|_| RuntimeError::Preparation)?;
        scheduler.bind_warning_routes(|command| warning_route(&settings.language, &command.key));
        let pending = match scheduler.activate(clock, self.event_activity()) {
            Ok(()) => false,
            Err(
                super::super::scheduler::SchedulerError::Clock
                | super::super::scheduler::SchedulerError::Calendar(_),
            ) => true,
            Err(_) => return Err(RuntimeError::Clock),
        };
        self.programs.activate_scheduler(scheduler, pending);
        self.programs.generation =
            Some(super::super::generation::CommandGeneration::new(generation));
        self.programs.owner = Some(scope);
        self.command_messages = Some(projections);
        self.links.install_profile(
            Arc::new(CommandProfile {
                owner: scope,
                public_key: registered.key.clone(),
                network_node,
                route_generation: self.links.schedule().map(LinkScheduler::generation),
                language: Arc::new(settings.language),
            }),
            generation,
        );
        Ok(())
    }
}

impl<A: Send, C: Send> RuntimeNode<A, C> {
    fn queue_command_message(
        &mut self,
        text: &str,
        audience: Option<crate::controller::PlaybackDestination>,
        configured_audience: bool,
        reply: Option<crate::link::PeerReply>,
        clock: RuntimeClock,
        event: Option<&super::super::scheduler::EventTicket>,
    ) -> Result<LinkEffect, RuntimeError> {
        use crate::{composition::MorseRequirement, controller::PreparedMedia};
        let source = self
            .command_messages
            .as_ref()
            .ok_or(RuntimeError::Unavailable)?;
        let compiled;
        let recipe = if let Some(fixed) = source.fixed.get(text) {
            fixed
        } else if let Some(named) = source.messages.recipes.get(text) {
            named
        } else {
            compiled = source
                .messages
                .compile_inline(text, MorseRequirement::Optional)
                .map_err(|_| RuntimeError::Rejected)?;
            &compiled
        };
        if reply.as_ref().is_some_and(crate::link::PeerReply::ended) {
            return Err(RuntimeError::Rejected);
        }
        let settings = recipe.settings_for(None);
        let audience = audience.unwrap_or(settings.playback.destination);
        let phone = reply.as_ref().is_some_and(|r| r.phone_source().is_some());
        if (reply.is_none() || configured_audience && !phone)
            && audience == crate::controller::PlaybackDestination::Peers
            && !self
                .links
                .manager()
                .snapshot()
                .iter()
                .any(|p| !p.ended && !p.retrying && p.mode.transmits())
        {
            return Ok(LinkEffect::None);
        }
        self.control.telemetry.reclaim().for_each(drop);
        if !phone && !self.control.telemetry.can_queue_status() {
            return Err(RuntimeError::Busy);
        }
        let profile = self.links.command_profile().ok_or(RuntimeError::Rejected)?;
        let mut values = self.message_values(
            clock,
            &source.messages.catalog,
            profile.public_key.as_str(),
            source.port.clone(),
        )?;
        if let Some(ticket) = event {
            capture_event_message(ticket, &source.messages.catalog, &mut values)?;
        }
        let program = recipe
            .prepare(&source.messages.catalog, &values)
            .map_err(|_| RuntimeError::Preparation)?;
        let stream = self
            .control
            .media_session
            .as_mut()
            .ok_or(RuntimeError::Unavailable)?
            .register_program(program, true)
            .map_err(|_| RuntimeError::Preparation)?;
        if phone {
            return Ok(LinkEffect::PrivatePhoneMedia(
                reply.expect("private caller"),
                stream,
            ));
        }
        let mut policy = settings.playback;
        policy.destination = audience;
        let deadline = self
            .control
            .activity
            .current_sample()
            .saturating_add(settings.delivery.maximum_wait_ms.saturating_mul(48));
        let prepared = PreparedMedia::new_stream(stream)
            .map_err(|_| RuntimeError::Preparation)?
            .with_playback_policy(policy)
            .with_delivery(deadline, settings.delivery);
        let prepared = if configured_audience {
            prepared.wait_for_unkey(reply)
        } else {
            prepared.reply_to(reply)
        };
        self.control
            .telemetry
            .queue_prepared_status(prepared)
            .then_some(LinkEffect::None)
            .ok_or(RuntimeError::Busy)
    }
}

fn capture_event_message(
    ticket: &super::super::scheduler::EventTicket,
    catalog: &MessageCatalog,
    values: &mut super::super::MessageValueSnapshot,
) -> Result<(), RuntimeError> {
    use super::super::scheduler::EventWork;
    let kind = match ticket.kind {
        EventWork::Start => "start",
        EventWork::End => "end",
        EventWork::StartWarning(_) => "start_warning",
        EventWork::EndWarning(_) => "end_warning",
    };
    values.event = Some(super::super::MessageEventIdentity {
        kind: kind.into(),
        peer_protocol: None,
        peer_number: None,
        peer_callsign: None,
    });
    let Some(boundary) = &ticket.warning_boundary else {
        return Ok(());
    };
    let seconds = boundary
        .due
        .saturating_sub(ticket.observed.wall_seconds)
        .max(0) as u64;
    let Some((civil, _)) = ticket.observed.civil else {
        return Ok(());
    };
    let duration = catalog
        .format_duration(seconds, civil)
        .map_err(|_| RuntimeError::Preparation)?;
    values.time_remaining = Some(MessageArgument::Forms(duration.clone()));
    let Some((target, connecting)) = &boundary.route else {
        return Ok(());
    };
    let target = target.as_str();
    values.capture_builtin(&Message::ScheduledLinkChange {
        target,
        connecting: *connecting,
        time_remaining: &duration.text,
    });
    let args = values
        .builtin
        .get_mut("scheduled-link-change")
        .expect("captured request");
    args.insert("time_remaining".into(), MessageArgument::Forms(duration));
    args.insert(
        "target".into(),
        MessageArgument::Forms(MessageForms {
            text: target.to_owned(),
            tts: render::telemetry_speech(target, &[target]),
            morse: target.to_owned(),
        }),
    );
    Ok(())
}

// A generic warning can name one unambiguous authored route. Dynamic expansion
// and multiple routes retain only their real event and deadline arguments.
fn warning_route(
    language: &crate::command_language::CommandLanguage,
    key: &str,
) -> Option<(crate::command_language::ProtectedOperand<String>, bool)> {
    let mut stack = vec![language.macros.get(key)?.iter()];
    let mut remaining = language.limits.max_actions;
    let mut route = None;
    while let Some(actions) = stack.last_mut() {
        let Some(action) = actions.next() else {
            stack.pop();
            continue;
        };
        remaining = remaining.checked_sub(1)?;
        match &action.action {
            Action::CallMacro(child) => stack.push(language.macros.get(child)?.iter()),
            Action::ExecuteDtmf(_) => return None,
            Action::Link(command) => {
                use LinkAction::*;
                let connecting = match command.action {
                    Monitor
                    | Transceive
                    | LocalMonitor
                    | PermanentMonitor
                    | PermanentTransceive
                    | PermanentLocalMonitor => true,
                    Disconnect | DisconnectPermanent => false,
                    DisconnectAll
                    | DisconnectPermanentAll
                    | DisconnectNonPermanentAll
                    | ReconnectAll
                    | ReconnectPermanentAll => return None,
                    _ => continue,
                };
                let next = (&command.node, connecting);
                if route.is_some_and(|old| old != next) {
                    return None;
                }
                route = Some(next);
            }
            _ => {}
        }
    }
    route.map(|(target, connecting)| (target.clone(), connecting))
}
impl<A: Send, C: Send> Runtime<A, C> {
    /// Submit an authenticated management action to the exact registry-bound running port.
    /// Ingress identity and grant state come only from their own live owners.
    pub fn management_action(
        &mut self,
        port: ResourceId,
        principal: &str,
        action: &crate::command_language::PreparedAction,
        clock: RuntimeClock,
    ) -> Result<(String, LinkEffect), RuntimeError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|n| {
                n.links
                    .command_profile()
                    .is_some_and(|p| p.owner == ResourceScope::Port(port))
            })
            .ok_or(RuntimeError::MissingNode)?;
        let name = node.name.clone();
        let context = InvocationContext {
            owner: ResourceScope::Port(port),
            source: CommandSource::Management(principal.into()),
            generation: node.command_work().ok_or(RuntimeError::Rejected)?,
            requires_grant: false,
        };
        let invocation = action
            .admit(&context, false)
            .map_err(|_| RuntimeError::Rejected)?;
        let effect = self.dispatch_action(&name, invocation, clock, None)?;
        Ok((name, effect))
    }

    /// Execute the invocation admitted by the durable management coordinator on
    /// its captured registry owner and generation. Replay remains owned by that coordinator.
    pub fn management_invocation(
        &mut self,
        invocation: Invocation,
        clock: RuntimeClock,
    ) -> Result<(String, LinkEffect), RuntimeError> {
        if !matches!(&invocation.context.source,CommandSource::Management(principal) if !principal.is_empty())
        {
            return Err(RuntimeError::Rejected);
        }
        let local = self
            .nodes
            .iter()
            .find(|node| {
                node.links
                    .command_profile()
                    .is_some_and(|p| p.owner == invocation.context.owner)
            })
            .ok_or(RuntimeError::MissingNode)?
            .name
            .clone();
        let effect = self.dispatch_action(&local, invocation, clock, None)?;
        Ok((local, effect))
    }
    /// Admit a consumed DTMF buffer against its exact current generation, scope, source and grant.
    pub fn dispatch_mapping(
        &mut self,
        local: &str,
        input: MappedInput,
        clock: RuntimeClock,
    ) -> Result<LinkEffect, RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        if !input.claim()
            || node
                .command_work()
                .is_none_or(|g| g.id() != input.generation)
            || !node
                .links
                .command_profile()
                .is_some_and(|p| Arc::ptr_eq(p, &input.profile))
        {
            return Err(RuntimeError::Rejected);
        }
        let source =
            input
                .connection
                .as_ref()
                .map_or(CommandSource::Receiver, |(origin, reply)| {
                    CommandSource::Connection {
                        origin: origin.clone(),
                        reply: reply.clone(),
                    }
                });
        let context = InvocationContext {
            owner: input.profile.owner,
            generation: node.command_work().ok_or(RuntimeError::Rejected)?,
            source,
            requires_grant: false,
        };
        let commands = node
            .links
            .command_source(&context.source)
            .ok_or(RuntimeError::Rejected)?;
        let invocation = input
            .profile
            .language
            .match_completed(&input.symbols)
            .ok_or(RuntimeError::Rejected)?
            .admit(
                &context,
                commands.admin_unlocked(clock.now_ms),
                commands.radio_enabled(),
                false,
            )
            .map_err(|_| RuntimeError::Rejected)?;
        self.dispatch_action(local, invocation, clock, None)
    }
    pub(super) fn dispatch_action(
        &mut self,
        local: &str,
        invocation: Invocation,
        clock: RuntimeClock,
        sequence: Option<(u64, Option<&super::super::scheduler::EventTicket>)>,
    ) -> Result<LinkEffect, RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        if node
            .links
            .command_profile()
            .is_none_or(|p| p.owner != invocation.context.owner)
            || node
                .command_work()
                .is_none_or(|work| !work.same_owner(&invocation.context.generation))
            || !invocation.context.generation.is_current()
        {
            return Err(RuntimeError::Rejected);
        }
        let source = &invocation.context.source;
        let mut commands = node.links.command_source(source);
        let grant = commands
            .as_deref_mut()
            .is_some_and(|c| c.admin_unlocked(clock.now_ms));
        invocation
            .context
            .recheck(grant)
            .map_err(|_| RuntimeError::Rejected)?;
        let mut permit = super::super::dtmf::ActionPermit {
            receiver: matches!(source, CommandSource::Receiver),
            reply: match source {
                CommandSource::Connection { reply, .. } => Some(reply.clone()),
                _ => None,
            },
            generation: invocation.context.generation.clone(),
            grant: if invocation.context.requires_grant {
                Some(
                    commands
                        .as_deref()
                        .ok_or(RuntimeError::Rejected)?
                        .grant_deadline(),
                )
            } else {
                None
            },
            deadline: sequence.map(|(deadline, _)| deadline),
        };
        let dtmf = invocation.origin == ParameterOrigin::Dtmf;
        let mut op = match invocation.action {
            Action::Link(c) => {
                let target = if matches!(
                    c.action,
                    LinkAction::Monitor
                        | LinkAction::Transceive
                        | LinkAction::LocalMonitor
                        | LinkAction::PermanentMonitor
                        | LinkAction::PermanentTransceive
                        | LinkAction::PermanentLocalMonitor
                        | LinkAction::Disconnect
                        | LinkAction::DisconnectPermanent
                        | LinkAction::Command
                ) {
                    if let Some(commands) = commands {
                        commands
                            .node_operand(&c.node, dtmf)
                            .ok_or(RuntimeError::Rejected)?
                    } else {
                        c.node.to_string()
                    }
                } else {
                    c.node.to_string()
                };
                DigitOperation::new(
                    Command {
                        action: c.action,
                        node: target,
                    },
                    None,
                )
            }
            Action::Site(command) => {
                if !matches!(
                    source,
                    CommandSource::Scheduler | CommandSource::Management(_) | CommandSource::Host
                ) && !grant
                {
                    return Err(RuntimeError::Rejected);
                }
                if matches!(
                    source,
                    CommandSource::Receiver | CommandSource::Connection { .. }
                ) {
                    permit.grant = Some(
                        commands
                            .as_deref()
                            .ok_or(RuntimeError::Rejected)?
                            .grant_deadline(),
                    );
                }
                return Ok(LinkEffect::SiteCommand(command, permit));
            }
            Action::Host(crate::command_language::HostAction::LinkStatus) => {
                return Ok(LinkEffect::Telemetry(
                    LinkAction::Status,
                    match source {
                        CommandSource::Connection { reply, .. } => Some(reply.clone()),
                        CommandSource::Receiver => None,
                        _ => return Err(RuntimeError::Unavailable),
                    },
                ));
            }
            Action::Mute(crate::command_language::MuteTarget::Port) => {
                node.control
                    .telemetry
                    .rx_mute()
                    .fetch_add(1, Ordering::AcqRel);
                return Ok(LinkEffect::None);
            }
            Action::Mute(target) => {
                node.links
                    .toggle_ingress(&target, source)
                    .map_err(RuntimeError::Link)?;
                return Ok(LinkEffect::None);
            }
            Action::Pause(_) | Action::ExecuteDtmf(_) => {
                return node
                    .enqueue_single(invocation, clock.now_ms)
                    .map(LinkEffect::CommandQueued);
            }
            Action::CallMacro(key) => {
                return node
                    .enqueue_program(key, invocation.context, clock.now_ms)
                    .map(LinkEffect::CommandQueued);
            }
            Action::Message(name, destination) => {
                use crate::command_language::Destination as D;
                use crate::controller::PlaybackDestination as P;
                let audience = match destination {
                    D::Local => P::Local,
                    D::Peers => P::Peers,
                    D::LocalAndLinks => P::LocalAndLinks,
                };
                return node.queue_command_message(
                    &name,
                    Some(audience),
                    false,
                    None,
                    clock,
                    sequence.and_then(|(_, t)| t),
                );
            }
            Action::Radio(RadioAction::Command(remote)) => {
                let mut operation = DigitOperation::new(
                    Command {
                        action: LinkAction::RemoteBase,
                        node: String::new(),
                    },
                    None,
                );
                operation.remote = Some(remote);
                operation
            }
            Action::Administration(unlock, code) => {
                return commands
                    .filter(|c| c.profile().is_some())
                    .ok_or(RuntimeError::Rejected)?
                    .apply_admin(unlock, code.as_ref().map(|s| s.as_str()), clock.now_ms)
                    .then_some(LinkEffect::None)
                    .ok_or(RuntimeError::Rejected);
            }
            Action::PadTest => {
                commands.ok_or(RuntimeError::Rejected)?.start_pad();
                return Ok(LinkEffect::None);
            }
            Action::ExitCommandMode => {
                commands.ok_or(RuntimeError::Rejected)?.select_remote("");
                return Ok(LinkEffect::None);
            }
            Action::SendCommandDigit(target, digit) => DigitOperation::new(
                Command {
                    action: LinkAction::Command,
                    node: target.to_string(),
                },
                Some(*digit),
            ),
            _ => return Err(RuntimeError::Unavailable),
        };
        if let CommandSource::Connection { origin, reply } = source {
            op.reply = Some(reply.clone());
            op.origin = Some(origin.clone());
        }
        op.admin_authorized = grant;
        op.action_permit = Some(permit);
        if op.command.action == LinkAction::Command && op.digit.is_none() {
            if !matches!(
                source,
                CommandSource::Receiver | CommandSource::Connection { .. }
            ) {
                return Err(RuntimeError::Unavailable);
            }
            return Ok(LinkEffect::VerifyCommandTarget(op));
        }
        let result = self.command(local, op, clock, false)?;
        if let LinkEffect::Message { selector, reply } = result {
            let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
            let name = node
                .command_messages
                .as_ref()
                .and_then(|source| source.selectors.get(&selector))
                .cloned()
                .ok_or(RuntimeError::Rejected)?;
            node.queue_command_message(
                &name,
                None,
                true,
                reply,
                clock,
                sequence.and_then(|(_, t)| t),
            )
        } else {
            Ok(result)
        }
    }
}
