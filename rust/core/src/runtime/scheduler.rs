//! Serialized civil-event reservations and configuration-owned link intent.

use crate::{
    command::Command,
    schedule::{CivilTime, ScheduledEvent},
};

/// A resolved event identity. Message/macro preparation remains on its control owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSpec {
    /// Stable local node identity.
    pub local: String,
    /// Configured event label.
    pub name: String,
    /// Validated local-calendar trigger.
    pub trigger: ScheduledEvent,
}

/// Scheduler preparation or bounded rendering failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    /// Invalid identity, duplicated event, or generation regression.
    Invalid,
    /// The current occurrence could not render within its prepared bounds.
    Render,
}

/// Fully copied message/operation payload prepared outside all audio owners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchContent {
    /// Optional rendered status message; must be accepted before the macro completes.
    pub message: Option<String>,
    /// Optional existing typed linking operation.
    pub operation: Option<Command>,
}

#[derive(Clone, Copy)]
struct Occurrence {
    key: u64,
    civil: CivilTime,
}
struct EventState {
    spec: EventSpec,
    completed: Option<u64>,
    ready: Option<Occurrence>,
    pending: Option<Occurrence>,
    message_queued: bool,
    message_required: bool,
}

/// Copied reservation with private generation/occurrence identity for result validation.
pub struct ScheduledDispatch {
    generation: u64,
    index: usize,
    occurrence: Occurrence,
    spec: EventSpec,
    /// Independently owned message and operation, safe to carry through a slow dial.
    pub content: DispatchContent,
}
impl ScheduledDispatch {
    /// Configured event name.
    pub fn name(&self) -> &str {
        &self.spec.name
    }
    /// Selected local node.
    pub fn local(&self) -> &str {
        &self.spec.local
    }
    /// Trigger's captured civil minute, unaffected by a later dispatcher clock.
    pub fn civil_time(&self) -> CivilTime {
        self.occurrence.civil
    }
}

/// Control-owned zero-time event scheduler preserving configuration order and reload state.
pub struct EventScheduler {
    generation: u64,
    events: Vec<EventState>,
    last_tick: Option<i64>,
}
impl EventScheduler {
    /// Construct all event state before publication; unchanged names/triggers retain progress.
    pub fn new(
        generation: u64,
        specs: Vec<EventSpec>,
        previous: Option<&Self>,
    ) -> Result<Self, SchedulerError> {
        if generation == 0
            || previous.is_some_and(|old| generation <= old.generation)
            || specs.iter().enumerate().any(|(index, spec)| {
                spec.local.is_empty()
                    || spec.name.is_empty()
                    || specs[..index]
                        .iter()
                        .any(|other| other.local == spec.local && other.name == spec.name)
            })
        {
            return Err(SchedulerError::Invalid);
        }
        let events = specs
            .into_iter()
            .map(|spec| {
                let prior =
                    previous.and_then(|old| old.events.iter().find(|event| event.spec == spec));
                EventState {
                    completed: prior.and_then(|old| {
                        if old.message_queued {
                            old.pending.map(|p| p.key).or(old.completed)
                        } else {
                            old.completed
                        }
                    }),
                    ready: prior.and_then(|old| {
                        old.ready.or(if old.message_queued {
                            None
                        } else {
                            old.pending
                        })
                    }),
                    pending: None,
                    message_queued: false,
                    message_required: false,
                    spec,
                }
            })
            .collect();
        Ok(Self {
            generation,
            events,
            last_tick: previous.and_then(|old| old.last_tick),
        })
    }

    /// Capture due events before selecting the oldest/configuration-first reservation.
    /// `captured_time` is the trigger's wall-clock timestamp, captured before submission.
    pub fn next(
        &mut self,
        captured_time: i64,
        local: CivilTime,
        mut render: impl FnMut(&EventSpec, &CivilTime) -> Result<DispatchContent, SchedulerError>,
    ) -> Result<Option<ScheduledDispatch>, SchedulerError> {
        if self.last_tick.is_none_or(|last| captured_time >= last) {
            self.last_tick = Some(captured_time);
            for event in &mut self.events {
                if event.pending.is_some() || event.ready.is_some() {
                    continue;
                }
                if let Some(key) = event.spec.trigger.occurrence(&local) {
                    if event.completed != Some(key) {
                        event.ready = Some(Occurrence { key, civil: local });
                    }
                }
            }
        }
        let selected = self
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                event
                    .pending
                    .or(event.ready)
                    .map(|occurrence| (index, occurrence))
            })
            .min_by_key(|(_, occurrence)| occurrence.key);
        let Some((index, occurrence)) = selected else {
            return Ok(None);
        };
        let event = &mut self.events[index];
        let content = match render(&event.spec, &occurrence.civil) {
            Ok(content) => content,
            Err(error) => {
                event.pending = None;
                event.ready = None;
                event.message_queued = false;
                event.completed = Some(occurrence.key);
                return Err(error);
            }
        };
        if event.pending.is_none() {
            event.pending = Some(occurrence);
            event.ready = None;
            event.message_queued = false;
        }
        event.message_required = content.message.is_some();
        Ok(Some(ScheduledDispatch {
            generation: self.generation,
            index,
            occurrence,
            spec: event.spec.clone(),
            content,
        }))
    }

    fn pending(&mut self, dispatch: &ScheduledDispatch) -> Option<&mut EventState> {
        if dispatch.generation != self.generation {
            return None;
        }
        self.events.get_mut(dispatch.index).filter(|event| {
            event.spec == dispatch.spec
                && event
                    .pending
                    .is_some_and(|pending| pending.key == dispatch.occurrence.key)
        })
    }
    /// Validate a reservation before any expensive preparation or external effect.
    pub fn current(&mut self, dispatch: &ScheduledDispatch) -> bool {
        self.pending(dispatch).is_some()
    }
    /// Whether the reserved message was already admitted, preventing duplicate retry enqueue.
    pub fn is_message_queued(&mut self, dispatch: &ScheduledDispatch) -> bool {
        self.pending(dispatch)
            .is_some_and(|event| event.message_queued)
    }
    /// Record successful telemetry admission once; a rejected message leaves the event pending.
    pub fn message_queued(&mut self, dispatch: &ScheduledDispatch) -> bool {
        let Some(event) = self.pending(dispatch) else {
            return false;
        };
        event.message_queued = true;
        true
    }
    /// Confirm the message/operation completed; stale or unqueued-message results are ignored.
    pub fn complete(&mut self, dispatch: &ScheduledDispatch) -> bool {
        let Some(event) = self.pending(dispatch) else {
            return false;
        };
        if event.message_required && !event.message_queued {
            return false;
        }
        event.completed = Some(dispatch.occurrence.key);
        event.pending = None;
        event.message_queued = false;
        true
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
