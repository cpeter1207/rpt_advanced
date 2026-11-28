//! Calendar reservations on the serialized EventScheduler owner.
use super::{EventScheduler, SchedulerError};
use crate::{
    command_language::MacroOutcome,
    config::resources::{
        candidate::{ResourceId, ResourceScope},
        events::{Boundary, Event, MacroRef, ScopedEvents, Warning},
    },
    runtime::RuntimeClock,
};
use serde::Serialize;
use std::sync::Arc;

/// Existing qualified local receive or peer input activity, including nonselected active peers.
/// Controller telemetry, transmit hang and mere connection presence do not count.
#[derive(Clone, Copy, Debug)]
pub struct ReceiveActivity {
    /// Qualifying input in the current callback.
    pub active: bool,
    /// Most recent qualifying sample in the captured monotonic epoch.
    pub last_active_ms: Option<u64>,
}

/// Boundary or independently occurring warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventWork {
    /// Calendar start, recovered start or safe activation.
    Start,
    /// Calendar end.
    End,
    /// Warning attached to the start group.
    StartWarning(usize),
    /// Warning attached to the end group.
    EndWarning(usize),
}
/// Latest accepted calendar sample and the latest gap spanning an unobserved minute.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CalendarObservation {
    current_wall_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_wall_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_abnormal_gap: Option<CalendarGap>,
}
#[derive(Clone, Copy, Debug, Serialize)]
struct CalendarGap {
    previous_wall_seconds: i64,
    current_wall_seconds: i64,
}

/// Terminal observations for the three kinds of work on one event.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct EventTerminals {
    #[serde(skip_serializing_if = "Option::is_none")]
    last_start: Option<BoundaryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_end: Option<BoundaryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_warning: Option<WarningResult>,
}
#[derive(Clone, Copy, Debug, Serialize)]
struct BoundaryResult {
    nominal_wall_seconds: i64,
    observed_wall_seconds: i64,
    outcome: &'static str,
}
#[derive(Clone, Copy, Debug, Serialize)]
struct WarningResult {
    boundary: &'static str,
    result: BoundaryResult,
}
impl EventTerminals {
    fn record(&mut self, ticket: &EventTicket, outcome: MacroOutcome) {
        let result = BoundaryResult {
            nominal_wall_seconds: ticket.nominal,
            observed_wall_seconds: ticket.observed.wall_seconds,
            outcome: match outcome {
                MacroOutcome::Completed => "completed",
                MacroOutcome::Rejected => "rejected",
                MacroOutcome::Cancelled => "cancelled",
                MacroOutcome::Limit => "limit",
            },
        };
        match ticket.kind {
            EventWork::Start => self.last_start = Some(result),
            EventWork::End => self.last_end = Some(result),
            EventWork::StartWarning(_) | EventWork::EndWarning(_) => {
                self.last_warning = Some(WarningResult {
                    boundary: if matches!(ticket.kind, EventWork::StartWarning(_)) {
                        "start"
                    } else {
                        "end"
                    },
                    result,
                });
            }
        }
    }
}
/// Result of offering one macro to its normal bounded executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroAdmission {
    /// Queue accepted the reservation exactly once.
    Queued,
    /// Queue temporarily full. Retry the same occurrence.
    Busy,
    /// Terminal preparation or authorization failure. Consume this work.
    Rejected,
}
/// First-execution decision after checking the supplied captured clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginDecision {
    /// Execute the first action now.
    Begin,
    /// Retain the queued continuation until a valid window observation.
    Wait,
    /// The generation or occurrence is stale, cancelled, or already started.
    Retired,
}
/// Controller-run and generation-fenced cooperative macro handoff.
#[derive(Clone, Debug)]
pub struct EventTicket {
    run: Arc<()>,
    reservation: Arc<()>,
    generation: u64,
    /// Stable invoking controller.
    pub owner: ResourceScope,
    /// Stable declaring event identity.
    pub event: ResourceId,
    /// The original real, civil and monotonic tick that observed this work.
    pub observed: RuntimeClock,
    /// Distinguishes lifecycle, recovery and ordinary calendar admission.
    pub origin: EventOrigin,
    /// Original UTC occurrence second.
    pub nominal: i64,
    /// Fraction of the captured startup second, zero for a provider occurrence.
    pub nominal_subsecond_ms: u16,
    /// Boundary or warning.
    pub kind: EventWork,
    /// Original warning boundary and the exact macro it would run.
    pub(crate) warning_boundary: Option<WarningBoundary>,
    /// Resolved macro in the exact invoking scope.
    pub command: MacroRef,
}
impl PartialEq for EventTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
            && Arc::ptr_eq(&self.reservation, &other.reservation)
            && self.generation == other.generation
            && self.owner == other.owner
            && self.event == other.event
            && self.observed == other.observed
            && self.origin == other.origin
            && self.nominal == other.nominal
            && self.nominal_subsecond_ms == other.nominal_subsecond_ms
            && self.kind == other.kind
            && self.command == other.command
    }
}
impl Eq for EventTicket {}

use crate::config::resources::events::Trigger;
use crate::schedule::recurrence::{END_SECOND, Error as CalendarError, MIN_SECOND, Recurrence};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Queued,
    Running,
}
/// Cause of the reserved event macro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventOrigin {
    /// Ordinary observed calendar boundary or warning.
    Calendar,
    /// Captured safe lifecycle activation.
    Startup,
    /// Start reassertion inside an existing window on controller restart.
    Recovery,
}

#[derive(Clone)]
struct Work {
    id: Arc<()>,
    observed: RuntimeClock,
    kind: EventWork,
    nominal: i64,
    subsecond: u16,
    origin: EventOrigin,
    command: MacroRef,
    warning_boundary: Option<WarningBoundary>,
    due_ms: i128,
    grace_ms: i128,
    last_opportunity: Option<i128>,
    eligible: bool,
    carries_group_hold: bool,
    phase: Phase,
}
impl Work {
    fn matches_ticket(&self, t: &EventTicket) -> bool {
        Arc::ptr_eq(&self.id, &t.reservation)
            && t.observed == self.observed
            && t.origin == self.origin
            && t.nominal == self.nominal
            && t.nominal_subsecond_ms == self.subsecond
            && t.command == self.command
    }
    fn check(&mut self, clock: RuntimeClock, rx: ReceiveActivity, idle_origin: u64) {
        if self.phase != Phase::Waiting {
            return;
        }
        let elapsed = i128::from(clock.now_ms) - self.due_ms;
        if elapsed < 0 {
            return;
        }
        if elapsed >= self.grace_ms {
            self.eligible = true;
            return;
        }
        let minute = elapsed / 60000;
        let idle_from = rx.last_active_ms.unwrap_or(idle_origin);
        let quiet =
            !rx.active && i128::from(clock.now_ms.saturating_sub(idle_from)) >= self.grace_ms;
        if !quiet {
            self.eligible = false;
        }
        if self.last_opportunity == Some(minute) {
            return;
        }
        self.last_opportunity = Some(minute);
        self.eligible = quiet;
    }
}
#[derive(Clone, Copy)]
struct Window {
    start: i64,
    end: i64,
    begun: bool,
}
pub(crate) type WarningRoute = (crate::command_language::ProtectedOperand<String>, bool);
#[derive(Clone, Debug)]
pub(crate) struct WarningBoundary {
    pub(crate) due: i64,
    pub(crate) route: Option<WarningRoute>,
}
struct CapturedWarning {
    kind: EventWork,
    nominal: i64,
    command: MacroRef,
    boundary: Option<WarningBoundary>,
}
#[derive(Clone)]
struct Entry {
    start_route: Option<WarningRoute>,
    end_route: Option<WarningRoute>,
    spec: Event,
    last_check: Option<i64>,
    warning_seen: BTreeMap<EventWork, i64>,
    poll: Option<CalendarError>,
    issue: Option<CalendarError>,
    terminals: EventTerminals,
    next_check: Option<i64>,
    window: Option<Window>,
    work: Vec<Work>,
    // An end may wait for its own grace after the calendar window has closed.
    held: BTreeSet<i64>,
}
#[derive(Default)]
struct Capture {
    poll: Option<CalendarError>,
    issue: Option<CalendarError>,
    opening: Option<(i64, Option<i64>, EventOrigin)>,
    next_check: Option<i64>,
    end_due: bool,
    warnings: Vec<CapturedWarning>,
}
impl Capture {
    // Required queries fail closed for this stream and leave a value-free diagnostic.
    fn result<T>(&mut self, query: Result<Option<T>, CalendarError>) -> Option<T> {
        match query {
            Ok(value) => value,
            Err(error) => {
                self.issue = Some(error);
                None
            }
        }
    }
    fn wake(&mut self, at: &Recurrence, now: i64) {
        if self.poll.is_some() {
            return;
        }
        match at.next_after(now) {
            Ok(Some(next)) => {
                self.next_check = Some(self.next_check.map_or(next, |bound| bound.min(next)))
            }
            Ok(None) => {}
            Err(error) => {
                self.issue = Some(error);
                // Cache failure is not exhaustion. Stop rerunning the bounded next search.
                self.poll = Some(error);
            }
        }
    }
}
fn warnings(spec: &Event) -> impl Iterator<Item = (EventWork, &Warning)> {
    spec.start
        .warnings
        .iter()
        .enumerate()
        .map(|(i, w)| (EventWork::StartWarning(i), w))
        .chain(spec.end.iter().flat_map(|b| {
            b.warnings
                .iter()
                .enumerate()
                .map(|(i, w)| (EventWork::EndWarning(i), w))
        }))
}
fn same_action(a: &Boundary, b: &Boundary) -> bool {
    a.at == b.at && a.command == b.command && a.politeness_minutes == b.politeness_minutes
}
fn schedule(trigger: &Trigger) -> Option<&Recurrence> {
    match trigger {
        Trigger::Calendar(c) => Some(c),
        Trigger::ControllerStart => None,
    }
}
fn due_ms(clock: RuntimeClock, second: i64) -> i128 {
    i128::from(clock.now_ms)
        - (i128::from(clock.wall_seconds) - i128::from(second)) * 1000
        - i128::from(clock.wall_subsecond_ms)
}
fn active_window(
    start: &Recurrence,
    end: &Recurrence,
    now: i64,
) -> Result<Option<(i64, i64)>, CalendarError> {
    let latest = match start.match_minute(now)? {
        Some(s) => Some(s),
        None => start.previous_before(now)?,
    };
    let Some(latest) = latest else {
        return Ok(None);
    };
    let Some(close) = end.next_after(latest)? else {
        return Ok(None);
    };
    if now >= close {
        return Ok(None);
    }
    // The occurrence began at the first start since the previous end; later overlapping
    // starts within this window do not change its identity.
    let lower = end.previous_before(latest)?.unwrap_or(MIN_SECOND);
    let first = if end.match_minute(latest)? == Some(latest) {
        Some(latest)
    } else if start.match_minute(lower)? == Some(lower) {
        Some(lower)
    } else {
        start.next_after(lower)?
    };
    Ok(first.map(|s| (s, close)))
}

impl Entry {
    fn prepared(spec: Event, prior: Option<&Entry>) -> Self {
        let mut entry = Self {
            spec,
            start_route: None,
            end_route: None,
            last_check: None,
            warning_seen: BTreeMap::new(),
            poll: None,
            issue: None,
            terminals: EventTerminals::default(),
            next_check: Some(i64::MIN),
            window: None,
            work: Vec::new(),
            held: BTreeSet::new(),
        };
        let Some(old) = prior else { return entry };
        if old.spec == entry.spec {
            return Self {
                spec: entry.spec,
                terminals: EventTerminals::default(),
                ..old.clone()
            };
        }
        let same_start = old.spec.start.at == entry.spec.start.at;
        let same_pair = same_start
            && old.spec.end.as_ref().map(|b| &b.at) == entry.spec.end.as_ref().map(|b| &b.at);
        let same_group = old.spec.replace_permanent == entry.spec.replace_permanent;
        if same_start {
            entry.last_check = old.last_check;
        }
        if same_pair {
            entry.window = old.window;
            if same_group {
                entry.held = old.held.clone();
            }
        }
        // Each warning is an independent stream. Match occurrences one-to-one even when
        // inserting or reordering array members changes their public ordinal.
        let mut prior_warnings: Vec<_> = warnings(&old.spec).collect();
        let mut moved = BTreeMap::new();
        let mut replaced = Vec::new();
        for (kind, warning) in warnings(&entry.spec) {
            let same_side = |other: EventWork| {
                matches!(
                    (kind, other),
                    (EventWork::StartWarning(_), EventWork::StartWarning(_))
                        | (EventWork::EndWarning(_), EventWork::EndWarning(_))
                )
            };
            let index = prior_warnings
                .iter()
                .position(|(k, w)| *k == kind && *w == warning)
                .or_else(|| {
                    prior_warnings
                        .iter()
                        .position(|(k, w)| same_side(*k) && *w == warning)
                });
            if let Some(index) = index {
                let (old_kind, _) = prior_warnings.remove(index);
                moved.insert(old_kind, kind);
                if let Some(&seen) = old.warning_seen.get(&old_kind) {
                    entry.warning_seen.insert(kind, seen);
                }
            } else {
                replaced.push(kind);
            }
        }
        // A replacement at an occupied ordinal retires the old continuation. Preserve
        // elapsed observations without treating an independently added array item as consumed.
        for kind in replaced {
            if prior_warnings.iter().any(|(old_kind, _)| *old_kind == kind)
                && let Some(&seen) = old.warning_seen.get(&kind)
            {
                entry.warning_seen.insert(kind, seen);
            }
        }
        for work in &old.work {
            let retained = match work.kind {
                EventWork::Start => {
                    same_pair
                        && same_group
                        && same_action(&old.spec.start, &entry.spec.start)
                        && (work.origin != EventOrigin::Recovery || entry.spec.restart_recovery)
                }
                EventWork::End => {
                    same_pair
                        && same_group
                        && old
                            .spec
                            .end
                            .as_ref()
                            .zip(entry.spec.end.as_ref())
                            .is_some_and(|(a, b)| same_action(a, b))
                }
                _ => moved.contains_key(&work.kind),
            };
            if retained {
                let mut work = work.clone();
                if let Some(&kind) = moved.get(&work.kind) {
                    work.kind = kind;
                }
                entry.work.push(work);
            } else if work.kind == EventWork::End {
                // A retired pending end has no remaining continuation to release its hold.
                entry.held.remove(&work.nominal);
            }
        }
        entry
    }
    fn opening(
        &self,
        now: i64,
        startup: bool,
    ) -> Result<Option<(i64, Option<i64>, EventOrigin)>, CalendarError> {
        let end = self.spec.end.as_ref().and_then(|b| schedule(&b.at));
        match (schedule(&self.spec.start.at), end) {
            (None, _) => Ok(startup.then_some((now, None, EventOrigin::Startup))),
            (Some(start), Some(end)) if startup => {
                Ok(active_window(start, end, now)?
                    .map(|(s, e)| (s, Some(e), EventOrigin::Recovery)))
            }
            (Some(start), _) if self.window.is_none_or(|w| now >= w.end) => {
                let Some(s) = start
                    .match_minute(now)?
                    .filter(|due| self.last_check.is_none_or(|last| *due > last))
                else {
                    return Ok(None);
                };
                match end {
                    Some(end) => Ok(end
                        .next_after(s)?
                        .filter(|e| now < *e)
                        .map(|e| (s, Some(e), EventOrigin::Calendar))),
                    None => Ok(Some((s, None, EventOrigin::Calendar))),
                }
            }
            _ => Ok(None),
        }
    }
    fn capture(&self, now: i64, startup: bool) -> Capture {
        let mut capture = Capture {
            poll: self.poll,
            issue: self.poll,
            ..Capture::default()
        };
        capture.opening = capture.result(self.opening(now, startup));
        let active_end = capture
            .opening
            .and_then(|(_, end, _)| end)
            .or(self.window.filter(|w| now < w.end).map(|w| w.end));
        if let Some(end) = active_end {
            capture.next_check = Some(end);
        } else if let Some(start) = schedule(&self.spec.start.at) {
            capture.wake(start, now);
        }
        if let Some(w) = self.window.filter(|w| w.begun && now >= w.end)
            && let Some(end) = self.spec.end.as_ref().and_then(|b| schedule(&b.at))
        {
            capture.end_due = capture.result(end.match_minute(now)) == Some(w.end);
        }
        for (kind, w) in warnings(&self.spec) {
            capture.wake(&w.at, now);
            if let Some(second) = capture
                .result(w.at.match_minute(now))
                .filter(|due| self.warning_seen.get(&kind).is_none_or(|seen| due > seen))
            {
                let boundary = match kind {
                    EventWork::StartWarning(_) => Some((&self.spec.start, &self.start_route)),
                    EventWork::EndWarning(_) => {
                        self.spec.end.as_ref().map(|b| (b, &self.end_route))
                    }
                    _ => None,
                }
                .and_then(|(b, route)| {
                    let due =
                        capture.result(schedule(&b.at)?.next_after(second.saturating_sub(1)))?;
                    Some(WarningBoundary {
                        due,
                        route: route.clone(),
                    })
                });
                capture.warnings.push(CapturedWarning {
                    kind,
                    nominal: second,
                    command: w.command.clone(),
                    boundary,
                });
            }
        }
        if capture.issue.is_some() {
            // POSIX seconds, rather than a UTC civil-minute cache: historical zone
            // boundaries can begin between UTC minute edges. Never enumerate missed ticks.
            capture.next_check = Some(now + 1);
        }
        capture
    }
    fn add(
        &mut self,
        kind: EventWork,
        nominal: i64,
        origin: EventOrigin,
        command: MacroRef,
        clock: RuntimeClock,
        minutes: u64,
    ) -> Option<&mut Work> {
        // The existing command-only policy keeps the oldest occupied stream reservation.
        // Independent warnings and the opposite boundary have their own slots.
        if self.work.iter().any(|w| w.kind == kind) {
            return None;
        }
        self.work.push(Work {
            id: Arc::new(()),
            kind,
            nominal,
            origin,
            command,
            warning_boundary: None,
            due_ms: if origin == EventOrigin::Calendar {
                due_ms(clock, nominal)
            } else {
                i128::from(clock.now_ms)
            },
            subsecond: if origin == EventOrigin::Startup {
                clock.wall_subsecond_ms
            } else {
                0
            },
            observed: clock,
            grace_ms: i128::from(minutes) * 60000,
            last_opportunity: None,
            eligible: false,
            carries_group_hold: false,
            phase: Phase::Waiting,
        });
        self.work.last_mut()
    }
    fn boundary(
        &mut self,
        kind: EventWork,
        nominal: i64,
        origin: EventOrigin,
        clock: RuntimeClock,
    ) {
        let b = if kind == EventWork::Start {
            &self.spec.start
        } else {
            self.spec.end.as_ref().expect("paired window")
        };
        let _ = self.add(
            kind,
            nominal,
            origin,
            b.command.clone(),
            clock,
            if origin == EventOrigin::Recovery {
                0
            } else {
                b.politeness_minutes
            },
        );
    }
    fn record(&mut self, clock: RuntimeClock, c: Capture) {
        if let Some(w) = self.window.filter(|w| clock.wall_seconds >= w.end) {
            self.window = None;
            if !w.begun {
                self.work
                    .retain(|t| !(t.kind == EventWork::Start && t.nominal == w.start));
            } else if c.end_due {
                self.boundary(EventWork::End, w.end, EventOrigin::Calendar, clock);
            } else {
                self.held.remove(&w.end);
            }
        }
        if let Some((start, end, origin)) = c.opening {
            self.window = end.map(|end| Window {
                start,
                end,
                begun: false,
            });
            if origin != EventOrigin::Recovery || self.spec.restart_recovery {
                self.boundary(EventWork::Start, start, origin, clock)
            }
        }
        for (kind, _) in warnings(&self.spec) {
            let seen = self.warning_seen.entry(kind).or_insert(clock.wall_seconds);
            *seen = (*seen).max(clock.wall_seconds);
        }
        for CapturedWarning {
            kind,
            nominal,
            command,
            boundary,
        } in c.warnings
        {
            if let Some(work) = self.add(kind, nominal, EventOrigin::Calendar, command, clock, 0) {
                work.warning_boundary = boundary;
            }
        }
        self.poll = c.poll;
        self.issue = c.issue;
        self.last_check = Some(
            self.last_check
                .map_or(clock.wall_seconds, |last| last.max(clock.wall_seconds)),
        );
        self.next_check = c.next_check;
    }
}
pub(super) struct Calendar {
    port_key: Option<crate::config::resources::PortKey>,
    run: Arc<()>,
    owner: ResourceScope,
    started: Option<u64>,
    latest: Option<RuntimeClock>,
    observation: Option<CalendarObservation>,
    events: Vec<Entry>,
    valid_clock: bool,
}
impl Calendar {
    fn retain_progress(&mut self, previous: &Self) {
        self.run = Arc::clone(&previous.run);
        self.started = previous.started;
        self.latest = previous.latest;
        self.valid_clock = previous.valid_clock;
        for entry in &mut self.events {
            if let Some(prior) = previous
                .events
                .iter()
                .find(|e| e.spec.binding == entry.spec.binding)
            {
                let retained = Entry::prepared(entry.spec.clone(), Some(prior));
                *entry = Entry {
                    start_route: entry.start_route.take(),
                    end_route: entry.end_route.take(),
                    ..retained
                };
            }
        }
    }

    fn observe_wall(&mut self, wall_seconds: i64) {
        if self
            .observation
            .is_some_and(|last| last.current_wall_seconds == wall_seconds)
        {
            return;
        }
        let previous = self.observation.map(|last| last.current_wall_seconds);
        let last_abnormal_gap = previous
            .filter(|last| wall_seconds.div_euclid(60) - last.div_euclid(60) >= 2)
            .map(|previous_wall_seconds| CalendarGap {
                previous_wall_seconds,
                current_wall_seconds: wall_seconds,
            })
            .or_else(|| self.observation.and_then(|last| last.last_abnormal_gap));
        self.observation = Some(CalendarObservation {
            current_wall_seconds: wall_seconds,
            previous_wall_seconds: previous,
            last_abnormal_gap,
        });
    }

    fn current_window(&self, e: &Entry, w: &Work) -> bool {
        w.origin != EventOrigin::Recovery
            || self.latest.is_some_and(|t| {
                e.window
                    .is_some_and(|open| open.start <= t.wall_seconds && t.wall_seconds < open.end)
            })
    }
    fn selected(&self) -> Option<(usize, usize)> {
        if !self.valid_clock {
            return None;
        }
        let startup = self
            .events
            .iter()
            .any(|e| e.work.iter().any(|t| t.origin == EventOrigin::Startup));
        // One head per original instant. Earlier pending work keeps its place in that batch;
        // a different instant's warning never inherits a boundary's politeness gate.
        let mut batches = BTreeMap::new();
        for (ei, e) in self.events.iter().enumerate() {
            for (wi, w) in e.work.iter().enumerate() {
                if startup && w.origin == EventOrigin::Recovery {
                    continue;
                }
                let kind = match w.kind {
                    EventWork::End => 0,
                    EventWork::Start => 2,
                    _ => 1,
                };
                let chain = match (e.spec.sequence_number, kind) {
                    (None, 0) => 0,
                    (Some(_), _) => 1,
                    _ => 2,
                };
                let rank = (
                    w.phase == Phase::Waiting,
                    chain,
                    e.spec.sequence_number.unwrap_or(0),
                    kind,
                );
                let head = batches
                    .entry((w.nominal, w.subsecond))
                    .or_insert((rank, ei, wi));
                if rank < head.0 {
                    *head = (rank, ei, wi);
                }
            }
        }
        batches.into_values().find_map(|(_, ei, wi)| {
            let entry = &self.events[ei];
            let work = &entry.work[wi];
            // A later opening may wait for a preceding end to finish. Its own nominal
            // deadline still applies and its paired end can cancel it before execution.
            let previous_close = work.kind == EventWork::Start
                && entry.spec.end.is_some()
                && entry
                    .work
                    .iter()
                    .any(|w| w.kind == EventWork::End && w.nominal <= work.nominal);
            (work.phase == Phase::Waiting
                && work.eligible
                && !previous_close
                && self.current_window(entry, work))
            .then_some((ei, wi))
        })
    }
    fn ticket(&self, generation: u64, e: &Entry, w: &Work) -> EventTicket {
        EventTicket {
            run: self.run.clone(),
            reservation: w.id.clone(),
            generation,
            owner: self.owner,
            event: e.spec.binding.id,
            warning_boundary: w.warning_boundary.clone(),
            origin: w.origin,
            observed: w.observed,
            nominal: w.nominal,
            nominal_subsecond_ms: w.subsecond,
            kind: w.kind,
            command: w.command.clone(),
        }
    }
    fn remove_work(&mut self, ei: usize, wi: usize) {
        let closing = &mut self.events[ei];
        let work = closing.work.remove(wi);
        if work.kind != EventWork::End || !closing.held.remove(&work.nominal) {
            return;
        }
        let group = closing.spec.replace_permanent.clone();
        // Bridge adjacent paired windows only while their exact start is eligible.
        // A warning may run between them. Removing a rejected start drops its bridge.
        for entry in &mut self.events {
            if entry.spec.replace_permanent == group
                && entry
                    .window
                    .is_some_and(|w| !w.begun && w.start == work.nominal)
                && let Some(start) = entry
                    .work
                    .iter_mut()
                    .find(|w| w.kind == EventWork::Start && w.nominal == work.nominal && w.eligible)
            {
                start.carries_group_hold = true;
            }
        }
    }

    fn find(&self, t: &EventTicket) -> Option<(usize, usize)> {
        if !Arc::ptr_eq(&self.run, &t.run) || self.owner != t.owner {
            return None;
        }
        self.events.iter().enumerate().find_map(|(ei, e)| {
            (e.spec.binding.id == t.event)
                .then(|| {
                    e.work
                        .iter()
                        .position(|w| w.kind == t.kind && w.matches_ticket(t))
                })
                .flatten()
                .map(|wi| (ei, wi))
        })
    }
}
impl EventScheduler {
    pub(crate) fn bind_warning_routes(
        &mut self,
        mut route: impl FnMut(&MacroRef) -> Option<WarningRoute>,
    ) {
        if let Some(calendar) = &mut self.calendar {
            for entry in &mut calendar.events {
                entry.start_route = route(&entry.spec.start.command);
                entry.end_route = entry.spec.end.as_ref().and_then(|b| route(&b.command));
            }
        }
    }
    /// Bind the prepared profiles and route key of one exact invoking controller.
    pub fn from_profiles(
        generation: u64,
        profiles: &ScopedEvents,
        previous: Option<&Self>,
    ) -> Result<Self, SchedulerError> {
        let mut scheduler = Self::from_events(
            generation,
            profiles.owner,
            profiles.events.clone(),
            previous,
        )?;
        scheduler
            .calendar
            .as_mut()
            .expect("modular scheduler")
            .port_key = profiles.port_key.clone();
        Ok(scheduler)
    }
    /// Prepare the existing serialized owner for flat events. Prior state must belong to this controller run.
    fn from_events(
        generation: u64,
        owner: ResourceScope,
        events: Vec<Event>,
        previous: Option<&Self>,
    ) -> Result<Self, SchedulerError> {
        let old = previous
            .map(|s| {
                s.calendar
                    .as_ref()
                    .filter(|c| c.owner == owner)
                    .ok_or(SchedulerError::Invalid)
            })
            .transpose()?;
        let mut ids = BTreeSet::new();
        if events
            .iter()
            .any(|e| e.owner != owner || !ids.insert(e.binding.id))
        {
            return Err(SchedulerError::Invalid);
        }
        let mut scheduler = Self::new(generation, Vec::new(), previous)?;
        let mut calendar = Calendar {
            port_key: None,
            run: Arc::new(()),
            owner,
            started: None,
            latest: None,
            observation: None,
            valid_clock: false,
            events: events
                .into_iter()
                .map(|spec| Entry::prepared(spec, None))
                .collect(),
        };
        if let Some(previous) = old {
            calendar.retain_progress(previous);
        }
        scheduler.calendar = Some(calendar);
        Ok(scheduler)
    }

    /// Carry the matching calendar forward from the incumbent at publication.
    pub(in crate::runtime) fn retain_calendar_progress(&mut self, previous: &Self) {
        if let (Some(next), Some(previous)) = (&mut self.calendar, &previous.calendar)
            && next.owner == previous.owner
        {
            next.retain_progress(previous);
        }
    }
    /// Start this run only after the host has safely activated the generation.
    pub fn activate(
        &mut self,
        clock: RuntimeClock,
        rx: ReceiveActivity,
    ) -> Result<(), SchedulerError> {
        self.observe_inner(clock, rx, true)
    }
    /// Observe one captured tick. Until safe activation this produces no work.
    pub fn observe(
        &mut self,
        clock: RuntimeClock,
        rx: ReceiveActivity,
    ) -> Result<(), SchedulerError> {
        self.observe_inner(clock, rx, false)
    }
    fn observe_inner(
        &mut self,
        clock: RuntimeClock,
        rx: ReceiveActivity,
        activation: bool,
    ) -> Result<(), SchedulerError> {
        let c = self.calendar.as_mut().ok_or(SchedulerError::Invalid)?;
        if !activation && c.started.is_none() {
            return Ok(());
        }
        if c.latest.is_some_and(|t| t.now_ms > clock.now_ms) {
            return Err(SchedulerError::Invalid);
        }
        c.valid_clock = false;
        if clock.wall_subsecond_ms >= 1000 || clock.wall_seconds == -1 && clock.civil.is_none() {
            return Err(SchedulerError::Clock);
        }
        if !(MIN_SECOND..END_SECOND).contains(&clock.wall_seconds) {
            return Err(SchedulerError::Calendar(CalendarError::Range));
        }
        let startup = c.started.is_none();

        let idle_origin = *c.started.get_or_insert(clock.now_ms);
        for e in &mut c.events {
            if e.next_check.is_some_and(|next| clock.wall_seconds >= next) {
                let capture = e.capture(clock.wall_seconds, startup);
                e.record(clock, capture);
            }
            for w in &mut e.work {
                w.check(clock, rx, idle_origin);
                w.carries_group_hold &= w.eligible;
            }
        }
        c.observe_wall(clock.wall_seconds);
        c.latest = Some(clock);
        c.valid_clock = true;
        Ok(())
    }
    /// Latest accepted wall samples and retained forward gap for this owner.
    pub fn calendar_observation(&self) -> Option<CalendarObservation> {
        self.calendar.as_ref()?.observation
    }

    /// Copied read-only health and terminal work for every event on this owner.
    pub fn observed_events(
        &self,
    ) -> impl Iterator<Item = (ResourceId, Option<CalendarError>, EventTerminals)> + '_ {
        self.calendar.iter().flat_map(|calendar| {
            calendar
                .events
                .iter()
                .map(|entry| (entry.spec.binding.id, entry.issue, entry.terminals))
        })
    }

    /// Current value-free provider issues by actual event member. An optional cache search
    /// limit retains current-time polling; a failed required query cannot authorize its work.
    pub fn calendar_issues(&self) -> impl Iterator<Item = (ResourceId, CalendarError)> + '_ {
        self.calendar.iter().flat_map(|c| {
            c.events
                .iter()
                .filter_map(|e| e.issue.map(|error| (e.spec.binding.id, error)))
        })
    }
    /// Reconcile current group holds with the existing route reservation owner.
    pub(in crate::runtime) fn apply_group_holds(
        &self,
        local: &str,
        links: &mut super::super::link_schedule::LinkScheduler,
    ) -> bool {
        self.apply_owned_group_holds(local, local, None, self.generation, links)
    }
    pub(in crate::runtime) fn apply_owned_group_holds(
        &self,
        local: &str,
        route_owner: &str,
        owner: Option<ResourceScope>,
        route_generation: u64,
        links: &mut super::super::link_schedule::LinkScheduler,
    ) -> bool {
        let Some(c) = self.calendar.as_ref() else {
            return false;
        };
        if owner.is_some_and(|owner| owner != c.owner)
            || c.port_key.as_ref().is_none_or(|key| key.as_str() != local)
        {
            return false;
        }
        let groups = c
            .events
            .iter()
            .filter(|e| !e.held.is_empty() || e.work.iter().any(|w| w.carries_group_hold))
            .filter_map(|e| {
                e.spec
                    .replace_permanent
                    .as_ref()
                    .map(|(_, key)| key.as_str())
            })
            .collect();
        links.event_groups(route_generation, route_owner, &groups)
    }
    /// Offer the oldest eligible reservation. A full macro queue leaves the same ticket pending.
    pub fn offer(&self) -> Option<EventTicket> {
        let c = self.calendar.as_ref()?;
        let (e, w) = c.selected()?;
        Some(c.ticket(self.generation, &c.events[e], &c.events[e].work[w]))
    }
    /// Settle one actual queue offer on the same serialized pass. A terminal rejection
    /// releases its batch position and never arms a paired end.
    pub fn admission(&mut self, t: &EventTicket, result: MacroAdmission) -> bool {
        if Some(t) != self.offer().as_ref() {
            return false;
        }
        let c = self.calendar.as_mut().expect("selected calendar");
        let (ei, wi) = c.find(t).expect("current reservation");
        match result {
            MacroAdmission::Busy => {}
            MacroAdmission::Queued => c.events[ei].work[wi].phase = Phase::Queued,
            MacroAdmission::Rejected => {
                c.events[ei].terminals.record(t, MacroOutcome::Rejected);
                c.remove_work(ei, wi);
            }
        }
        true
    }
    /// Recheck the executor's supplied captured pass immediately before its first action.
    /// Errors leave the continuation waiting for another usable clock. No clock is read here.
    pub fn begin_at(
        &mut self,
        t: &EventTicket,
        clock: RuntimeClock,
        rx: ReceiveActivity,
    ) -> Result<BeginDecision, SchedulerError> {
        if t.generation != self.generation {
            return Ok(BeginDecision::Retired);
        }
        self.observe(clock, rx)?;
        if self.begin(t) {
            return Ok(BeginDecision::Begin);
        }
        let queued = self.calendar.as_ref().is_some_and(|c| {
            c.find(t)
                .is_some_and(|(ei, wi)| c.events[ei].work[wi].phase == Phase::Queued)
        });
        Ok(if queued {
            BeginDecision::Wait
        } else {
            BeginDecision::Retired
        })
    }
    /// Start the queued macro in the current observed pass. False retires a cancelled or stale
    /// ticket without any action execution. The executor must check immediately before its first action.
    fn begin(&mut self, t: &EventTicket) -> bool {
        let Some(c) = self.calendar.as_mut() else {
            return false;
        };
        if !c.valid_clock || t.generation != self.generation {
            return false;
        }
        let Some((ei, wi)) = c.find(t) else {
            return false;
        };
        if !c.current_window(&c.events[ei], &c.events[ei].work[wi]) {
            return false;
        }
        let e = &mut c.events[ei];
        let w = &mut e.work[wi];
        if w.phase != Phase::Queued {
            return false;
        }
        w.phase = Phase::Running;
        if w.kind == EventWork::Start
            && let Some(open) = e.window.as_mut().filter(|open| open.start == w.nominal)
        {
            open.begun = true;
            w.carries_group_hold = false;
            if e.spec.replace_permanent.is_some() {
                e.held.insert(open.end);
            }
        }
        true
    }
    /// Release the order barrier after the outer macro terminates, including nested calls.
    /// A terminal cancellation before execution never arms a paired end.
    pub fn terminate(&mut self, t: &EventTicket, outcome: MacroOutcome) -> bool {
        let Some(c) = self.calendar.as_mut() else {
            return false;
        };
        if t.generation != self.generation {
            return false;
        }
        let Some((ei, wi)) = c.find(t) else {
            return false;
        };
        if c.events[ei].work[wi].phase == Phase::Waiting {
            return false;
        }
        c.events[ei].terminals.record(t, outcome);
        c.remove_work(ei, wi);
        true
    }
    /// Replace a stale generation token only for the very same retained queued/running continuation.
    pub fn rebind(&self, t: &EventTicket) -> Option<EventTicket> {
        let c = self.calendar.as_ref()?;
        if !Arc::ptr_eq(&c.run, &t.run) || c.owner != t.owner {
            return None;
        }
        let e = c.events.iter().find(|e| e.spec.binding.id == t.event)?;
        // The private, unforgeable reservation survives only a proved stream match.
        // The warning's ordinal can move while its occurrence and macro remain identical.
        let w = e.work.iter().find(|w| {
            w.matches_ticket(t)
                && w.phase != Phase::Waiting
                && std::mem::discriminant(&w.kind) == std::mem::discriminant(&t.kind)
        })?;
        Some(c.ticket(self.generation, e, w))
    }
}
#[cfg(test)]
#[path = "calendar_tests.rs"]
mod tests;
