//! Cooperative macro frames and action admission on the serialized runtime owner.
use super::*;
use crate::command_language::{
    Action, CommandSource, Invocation, InvocationContext, MacroBudget, MacroOutcome,
    ProtectedOperand,
};
use crate::runtime::{
    dtmf::{CommandProfile, MappedCollector},
    scheduler::{BeginDecision, EventTicket, MacroAdmission},
};
use std::{collections::VecDeque, sync::atomic::AtomicU8};

const QUEUE_CAPACITY: usize = 64;
struct Input {
    symbols: ProtectedOperand<String>,
    next: usize,
    collector: MappedCollector,
    context: InvocationContext,
}
enum Frame {
    Actions { key: String, next: usize },
    Single(Option<Invocation>),
    Digits(Input),
}
struct Program {
    terminal: CommandResponse,
    _collector: Option<Arc<()>>,
    budget: MacroBudget,
    frames: Vec<Frame>,
    due: u64,
    wait_context: Option<InvocationContext>,
    deadline: u64,
    receipt: Option<Arc<AtomicU8>>,
    ticket: Option<EventTicket>,
    begun: bool,
}
enum Progress {
    Wait,
    Effect(Invocation),
    Done(MacroOutcome),
}
impl Program {
    fn new(
        profile: &Arc<CommandProfile>,
        frame: Frame,
        context: InvocationContext,
        now: u64,
        ticket: Option<EventTicket>,
    ) -> Self {
        Self {
            _collector: None,
            terminal: CommandResponse(Arc::new(AtomicU8::new(0))),
            budget: MacroBudget::start(profile.language.clone(), context, now),
            frames: vec![frame],
            due: now,
            wait_context: None,
            deadline: now.saturating_add(profile.language.limits.max_duration_ms),
            receipt: None,
            begun: false,
            ticket,
        }
    }
    fn advance(
        &mut self,
        profile: &Arc<CommandProfile>,
        grant: bool,
        radio: bool,
        now: u64,
    ) -> Result<Progress, MacroOutcome> {
        self.budget.resume(grant, now)?;
        if let Some(context) = &self.wait_context {
            context
                .recheck(grant)
                .map_err(|_| MacroOutcome::Cancelled)?;
        }
        if let Some(receipt) = &self.receipt {
            match receipt.load(Ordering::Acquire) {
                0 => return Ok(Progress::Wait),
                1 => self.receipt = None,
                _ => return Err(MacroOutcome::Rejected),
            }
        }
        if now < self.due {
            return Ok(Progress::Wait);
        }
        self.wait_context = None;
        loop {
            let invocation = match self.frames.last_mut().ok_or(MacroOutcome::Rejected)? {
                Frame::Single(action) => match action.take() {
                    Some(invocation) => invocation,
                    None => return Ok(Progress::Done(MacroOutcome::Completed)),
                },
                Frame::Actions { key, next } => {
                    let actions = self
                        .budget
                        .language()
                        .macros
                        .get(key)
                        .ok_or(MacroOutcome::Rejected)?;
                    let Some(p) = actions.get(*next) else {
                        self.frames.pop();
                        if let Some(done) = self.budget.finish_frame() {
                            return Ok(Progress::Done(done));
                        }
                        continue;
                    };
                    *next += 1;
                    p.admit(self.budget.context(), grant)
                        .map_err(|_| MacroOutcome::Rejected)?
                }
                Frame::Digits(input) => {
                    input
                        .context
                        .recheck(grant)
                        .map_err(|_| MacroOutcome::Cancelled)?;
                    let event = match input.symbols.as_bytes().get(input.next) {
                        Some(&c) => {
                            input.next += 1;
                            DigitEvent::Digit {
                                digit: char::from(c),
                                now_ms: now,
                            }
                        }
                        None if input.collector.active() => DigitEvent::Digit {
                            digit: '\0',
                            now_ms: now,
                        },
                        None => {
                            self.frames.pop();
                            continue;
                        }
                    };
                    let tail = matches!(event, DigitEvent::Digit { digit: '\0', .. });
                    let Some(DigitOutcome::Mapped(input_result)) =
                        input.collector.feed(event, true)
                    else {
                        if tail && input.collector.active() {
                            return Ok(Progress::Wait);
                        }
                        continue;
                    };
                    profile
                        .language
                        .match_completed(&input_result.symbols)
                        .ok_or(MacroOutcome::Rejected)?
                        .admit(&input.context, grant, radio, true)
                        .map_err(|_| MacroOutcome::Rejected)?
                }
            };
            self.budget.charge(&invocation.action)?;
            match invocation.action {
                Action::Pause(duration) => {
                    self.wait_context = Some(invocation.context);
                    self.due =
                        now.saturating_add(duration.unwrap_or(profile.language.limits.pause_ms));
                    return Ok(Progress::Wait);
                }
                Action::CallMacro(key) => {
                    self.budget.enter_child(invocation.context.requires_grant)?;
                    self.frames.push(Frame::Actions { key, next: 0 });
                }
                Action::ExecuteDtmf(symbols) => {
                    self.frames.push(Frame::Digits(Input {
                        symbols,
                        next: 0,
                        collector: MappedCollector::new(
                            profile.clone(),
                            invocation.context.generation.id(),
                        ),
                        context: invocation.context,
                    }));
                }
                _ => {
                    self.wait_context = Some(invocation.context.clone());
                    return Ok(Progress::Effect(invocation));
                }
            }
        }
    }
}

/// Completion of an external action's normal admission. Dropped responses reject the waiting sequence.
pub struct CommandReceipt(Arc<AtomicU8>);
impl CommandReceipt {
    /// A normal asynchronous connect is admitted when its bounded dial work is accepted.
    pub fn settle(self, accepted: bool) {
        self.0
            .store(if accepted { 1 } else { 2 }, Ordering::Release);
    }
}
impl Drop for CommandReceipt {
    fn drop(&mut self) {
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// A pollable terminal result retained by the owner of one admitted invocation.
#[derive(Clone)]
pub struct CommandResponse(Arc<AtomicU8>);
impl CommandResponse {
    /// None while pending; a terminal result remains readable after subsequent invocations.
    pub fn outcome(&self) -> Option<MacroOutcome> {
        match self.0.load(Ordering::Acquire) {
            1 => Some(MacroOutcome::Completed),
            2 => Some(MacroOutcome::Rejected),
            3 => Some(MacroOutcome::Cancelled),
            4 => Some(MacroOutcome::Limit),
            _ => None,
        }
    }
    fn finish(&self, outcome: MacroOutcome) {
        let value = match outcome {
            MacroOutcome::Completed => 1,
            MacroOutcome::Rejected => 2,
            MacroOutcome::Cancelled => 3,
            MacroOutcome::Limit => 4,
        };
        let _ = self
            .0
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire);
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        self.terminal.finish(MacroOutcome::Cancelled);
    }
}
/// Result observed on the running port after complete execution, including children and waits.
pub struct CommandCompletion {
    /// Original source for private command feedback.
    pub source: CommandSource,
    /// Terminal status of the root invocation.
    pub outcome: MacroOutcome,
}
#[derive(Default)]
pub(super) struct CommandPrograms {
    pub(super) generation: Option<super::super::generation::ScopedGeneration>,
    queue: VecDeque<Program>,
    pub(super) scheduler: Option<EventScheduler>,
    pub(super) owner: Option<crate::config::resources::candidate::ResourceScope>,
    activation_pending: bool,
    completed: Option<CommandCompletion>,
}
impl CommandPrograms {
    fn push(
        &mut self,
        profile: &Arc<CommandProfile>,
        frame: Frame,
        ctx: InvocationContext,
        now: u64,
        ticket: Option<EventTicket>,
        collector: Option<Arc<()>>,
    ) -> Result<CommandResponse, RuntimeError> {
        if self.queue.len() >= QUEUE_CAPACITY {
            return Err(RuntimeError::Busy);
        }
        if matches!(&frame,Frame::Actions{key,..} if !profile.language.macros.contains_key(key)) {
            return Err(RuntimeError::Rejected);
        }
        let mut p = Program::new(profile, frame, ctx, now, ticket);
        p._collector = collector;
        let response = p.terminal.clone();
        self.queue.push_back(p);
        Ok(response)
    }
    pub(super) fn activate_scheduler(&mut self, mut next: EventScheduler, pending: bool) {
        // A replaced generation cannot execute any old continuation. Release retained
        // reservations using the scheduler's matching token before installing it.
        while let Some(program) = self.queue.pop_front() {
            if let Some(ticket) = program
                .ticket
                .as_ref()
                .and_then(|ticket| next.rebind(ticket))
            {
                next.terminate(&ticket, MacroOutcome::Cancelled);
            }
            self.finish(program, MacroOutcome::Cancelled);
        }
        self.scheduler = Some(next);
        self.activation_pending = pending;
    }
    fn finish(&mut self, p: Program, outcome: MacroOutcome) {
        p.terminal.finish(outcome);
        if let Some(ticket) = &p.ticket
            && let Some(s) = &mut self.scheduler
        {
            s.terminate(ticket, outcome);
        }
        self.completed = Some(CommandCompletion {
            source: p.budget.context().source.clone(),
            outcome,
        });
    }
}
impl<A: Send, C: Send> RuntimeNode<A, C> {
    pub(super) fn enqueue_program(
        &mut self,
        key: String,
        ctx: InvocationContext,
        now: u64,
        deadline: Option<u64>,
    ) -> Result<CommandResponse, RuntimeError> {
        let pin = self
            .links
            .command_source(&ctx.source)
            .and_then(|c| c.collector_pin());
        let p = self.links.command_profile().ok_or(RuntimeError::Rejected)?;
        let receipt =
            self.programs
                .push(p, Frame::Actions { key, next: 0 }, ctx, now, None, pin)?;
        if let Some(deadline) = deadline {
            let child = self.programs.queue.back_mut().expect("just enqueued");
            child.deadline = child.deadline.min(deadline);
        }
        Ok(receipt)
    }
    pub(super) fn enqueue_single(
        &mut self,
        action: Invocation,
        now: u64,
    ) -> Result<CommandResponse, RuntimeError> {
        let context = action.context.clone();
        let pin = self
            .links
            .command_source(&context.source)
            .and_then(|c| c.collector_pin());
        let profile = self.links.command_profile().ok_or(RuntimeError::Rejected)?;
        self.programs.push(
            profile,
            Frame::Single(Some(action)),
            context,
            now,
            None,
            pin,
        )
    }
    fn offer_program(&mut self, clock: RuntimeClock) -> Result<(), RuntimeError> {
        let activity = self.event_activity();
        let Some(profile) = self.links.command_profile().cloned() else {
            return Ok(());
        };
        let Some(s) = self.programs.scheduler.as_mut() else {
            return Ok(());
        };
        let observation = if self.programs.activation_pending {
            s.activate(clock, activity)
        } else {
            s.observe(clock, activity)
        };
        if observation.is_err() {
            return Ok(());
        }
        self.programs.activation_pending = false;
        self.links.apply_event_groups(&self.name, s);
        let Some(ticket) = s.offer() else {
            return Ok(());
        };
        let ctx = InvocationContext {
            owner: profile.owner,
            source: CommandSource::Scheduler,
            generation: self.command_work().ok_or(RuntimeError::Rejected)?,
            requires_grant: false,
        };
        let result = self.programs.push(
            &profile,
            Frame::Actions {
                key: ticket.command.key.clone(),
                next: 0,
            },
            ctx,
            clock.now_ms,
            Some(ticket.clone()),
            None,
        );
        self.programs
            .scheduler
            .as_mut()
            .expect("offering scheduler")
            .admission(
                &ticket,
                match result {
                    Ok(_) => MacroAdmission::Queued,
                    Err(RuntimeError::Busy) => MacroAdmission::Busy,
                    _ => MacroAdmission::Rejected,
                },
            );
        Ok(())
    }
    /// Take the terminal result from the serialized command pump.
    pub fn take_command_completion(&mut self) -> Option<CommandCompletion> {
        self.programs.completed.take()
    }
}
impl<A: Send, C: Send> Runtime<A, C> {
    /// Advance the current invocation and offer its next domain effect for actual host admission.
    /// The next step awaits the returned receipt, preserving ordering across pauses and backpressure.
    pub fn advance_commands(
        &mut self,
        local: &str,
        clock: RuntimeClock,
    ) -> Result<Option<(LinkEffect, CommandReceipt)>, RuntimeError> {
        let node = self.node(local).ok_or(RuntimeError::MissingNode)?;
        node.offer_program(clock)?;
        let Some(mut p) = node.programs.queue.pop_front() else {
            return Ok(None);
        };
        let profile = node.links.command_profile().cloned();
        let radio = node.links.radio_commands_enabled();
        let grant = node
            .links
            .command_source(&p.budget.context().source)
            .is_some_and(|c| c.admin_unlocked(clock.now_ms));
        if profile.is_none() {
            node.programs.finish(p, MacroOutcome::Cancelled);
            return Ok(None);
        }
        if clock.now_ms > p.deadline {
            node.programs.finish(p, MacroOutcome::Limit);
            return Ok(None);
        }
        if let Err(outcome) = p.budget.resume(grant, clock.now_ms) {
            node.programs.finish(p, outcome);
            return Ok(None);
        }
        if let Some(ticket) = &p.ticket
            && !p.begun
        {
            let activity = node.event_activity();
            match node.programs.scheduler.as_mut().and_then(|s| {
                let decision = s.begin_at(ticket, clock, activity).ok()?;
                node.links.apply_event_groups(&node.name, s);
                Some(decision)
            }) {
                Some(BeginDecision::Begin) => p.begun = true,
                Some(BeginDecision::Retired) => {
                    node.programs.finish(p, MacroOutcome::Cancelled);
                    return Ok(None);
                }
                _ => {
                    node.programs.queue.push_front(p);
                    return Ok(None);
                }
            }
        }
        let progress = profile
            .as_ref()
            .ok_or(MacroOutcome::Cancelled)
            .and_then(|profile| p.advance(profile, grant, radio, clock.now_ms));
        match progress {
            Ok(Progress::Effect(invocation)) => {
                match self.dispatch_action(
                    local,
                    invocation,
                    clock,
                    Some((p.deadline, p.ticket.as_ref())),
                ) {
                    Ok(effect) => {
                        let receipt = Arc::new(AtomicU8::new(0));
                        p.receipt = Some(receipt.clone());
                        self.node(local)
                            .expect("same serialized port")
                            .programs
                            .queue
                            .push_front(p);
                        Ok(Some((effect, CommandReceipt(receipt))))
                    }
                    Err(_) => {
                        self.node(local)
                            .expect("same serialized port")
                            .programs
                            .finish(p, MacroOutcome::Rejected);
                        Ok(None)
                    }
                }
            }
            Ok(Progress::Wait) => {
                node.programs.queue.push_front(p);
                Ok(None)
            }
            Ok(Progress::Done(outcome)) | Err(outcome) => {
                node.programs.finish(p, outcome);
                Ok(None)
            }
        }
    }
}
