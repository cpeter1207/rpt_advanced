//! Generation/nonce-checked configuration-owned permanent and replacement links.

use super::scheduler::SchedulerError;
use crate::schedule::{CivilTime, ScheduledWindow};

/// One exact directed route owned by configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteSpec {
    /// Owning local node.
    pub local: String,
    /// Exact remote node, never a transitive topology match.
    pub remote: String,
    /// Desired outside a replacement window.
    pub permanent: bool,
}
/// One same-node replacement window with existing post-window inactivity behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementSpec {
    /// Replacement route index in this schedule.
    pub route: usize,
    /// Permanent route index suppressed while this window requests its route.
    pub replaced: usize,
    /// Validated local date/time selection.
    pub window: ScheduledWindow,
    /// Required receive-idle time after the window; zero ends immediately.
    pub end_inactivity_ms: u64,
}
/// Physical configured-route transition, performed outside the scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTransition {
    /// Attach or retain permanent recovery intent.
    Attach,
    /// Remove an exact configured permanent route and retry intent.
    Detach,
}

struct Route {
    spec: RouteSpec,
    issued: bool,
    desired: bool,
    paused: bool,
    pending: Option<u64>,
}
struct Window {
    spec: ReplacementSpec,
    last_activity: u64,
    initialized: bool,
    was_active: bool,
    waiting: bool,
    initial_deadline: Option<u64>,
}

/// Copied admission token. Its private nonce prevents stale dialing from claiming a new request.
#[derive(Clone, Debug)]
pub struct LinkReservation {
    generation: u64,
    index: usize,
    nonce: u64,
    spec: RouteSpec,
    action: LinkTransition,
}
impl LinkReservation {
    /// Owning local node.
    pub fn local(&self) -> &str {
        &self.spec.local
    }
    /// Exact selected peer.
    pub fn remote(&self) -> &str {
        &self.spec.remote
    }
    /// Requested physical transition.
    pub fn action(&self) -> LinkTransition {
        self.action
    }
}

/// Serialized permanent-link intent; independent of dialing and taskprocessor mechanics.
pub struct LinkScheduler {
    generation: u64,
    nonce: u64,
    routes: Vec<Route>,
    windows: Vec<Window>,
}
impl LinkScheduler {
    /// Whether final link publication must refresh civil-time window policy.
    pub fn requires_civil_time(&self) -> bool {
        !self.windows.is_empty()
    }
    /// Validate all endpoint/window references before constructing candidate state.
    pub fn new(
        generation: u64,
        routes: Vec<RouteSpec>,
        windows: Vec<ReplacementSpec>,
        previous: Option<&Self>,
    ) -> Result<Self, SchedulerError> {
        if generation == 0
            || previous.is_some_and(|old| generation <= old.generation)
            || routes.iter().enumerate().any(|(index, route)| {
                route.local.is_empty()
                    || route.remote.is_empty()
                    || route.local == route.remote
                    || routes[..index]
                        .iter()
                        .any(|other| other.local == route.local && other.remote == route.remote)
            })
            || windows.iter().any(|window| {
                window.route == window.replaced
                    || !routes.get(window.replaced).is_some_and(|replaced| {
                        replaced.permanent
                            && routes
                                .get(window.route)
                                .is_some_and(|route| route.local == replaced.local)
                    })
            })
        {
            return Err(SchedulerError::Invalid);
        }
        let window_states = windows
            .into_iter()
            .map(|spec| {
                let old = previous.and_then(|old| {
                    old.windows.iter().find(|window| {
                        window.spec.window == spec.window
                            && window.spec.end_inactivity_ms == spec.end_inactivity_ms
                            && old.routes[window.spec.route].spec == routes[spec.route]
                            && old.routes[window.spec.replaced].spec == routes[spec.replaced]
                    })
                });
                // Changed windows still inherit node receive activity, never silently resetting idle.
                let activity = previous.map_or(0, |old| {
                    old.windows
                        .iter()
                        .filter(|window| {
                            old.routes[window.spec.route].spec.local == routes[spec.route].local
                        })
                        .map(|window| window.last_activity)
                        .max()
                        .unwrap_or(0)
                });
                Window {
                    spec,
                    last_activity: activity,
                    initialized: old.is_some_and(|old| old.initialized),
                    was_active: old.is_some_and(|old| old.was_active),
                    waiting: old.is_some_and(|old| old.waiting),
                    initial_deadline: old.and_then(|old| old.initial_deadline),
                }
            })
            .collect();
        let routes = routes
            .into_iter()
            .map(|spec| {
                let old = previous.and_then(|old| {
                    old.routes.iter().find(|route| {
                        route.spec.local == spec.local && route.spec.remote == spec.remote
                    })
                });
                Route {
                    desired: spec.permanent,
                    issued: old.is_some_and(|old| old.issued),
                    paused: old.is_some_and(|old| old.paused),
                    spec,
                    pending: None,
                }
            })
            .collect();
        Ok(Self {
            generation,
            nonce: 0,
            routes,
            windows: window_states,
        })
    }

    /// Current immutable endpoint declarations, useful for building a reload candidate.
    pub fn route_specs(&self) -> Vec<RouteSpec> {
        self.routes.iter().map(|route| route.spec.clone()).collect()
    }
    /// Current immutable window declarations.
    pub fn window_specs(&self) -> Vec<ReplacementSpec> {
        self.windows
            .iter()
            .map(|window| window.spec.clone())
            .collect()
    }
    /// Issued old endpoints absent in a candidate; withdraw these before publishing it.
    pub fn removed_routes(&self, candidate: &Self) -> Vec<RouteSpec> {
        self.routes
            .iter()
            .filter(|old| {
                old.issued
                    && !candidate.routes.iter().any(|new| {
                        new.spec.local == old.spec.local && new.spec.remote == old.spec.remote
                    })
            })
            .map(|old| old.spec.clone())
            .collect()
    }

    /// Evaluate one captured civil instant and monotonic receive-activity snapshot on control.
    /// `owns` must test exact permanent ownership/retry intent, never transitive reachability.
    pub fn tick(
        &mut self,
        local: CivilTime,
        second: u8,
        now_ms: u64,
        mut activity: impl FnMut(&str) -> u64,
        mut owns: impl FnMut(&RouteSpec) -> bool,
    ) {
        for route in &mut self.routes {
            route.desired = route.spec.permanent;
            if route.issued && !owns(&route.spec) {
                route.issued = false;
            }
        }
        for window in &mut self.windows {
            let observed = activity(&self.routes[window.spec.route].spec.local);
            if observed > window.last_activity {
                window.last_activity = observed;
                window.initial_deadline = None;
            }
            let active = window.spec.window.matches(&local);
            if !window.initialized {
                window.initialized = true;
                if !active && window.spec.end_inactivity_ms != 0 {
                    if let Some(elapsed) = window.spec.window.elapsed_after_end(&local, second) {
                        if window.last_activity != 0 {
                            window.waiting = true;
                        } else if elapsed < window.spec.end_inactivity_ms {
                            window.waiting = true;
                            window.initial_deadline = Some(
                                now_ms.saturating_add(window.spec.end_inactivity_ms - elapsed),
                            );
                        }
                    }
                }
            }
            if active {
                window.was_active = true;
                window.waiting = false;
                window.initial_deadline = None;
            } else if window.was_active {
                window.was_active = false;
                window.waiting = true;
            }
            if window.waiting {
                let quiet = window.initial_deadline.map_or_else(
                    || {
                        window.spec.end_inactivity_ms == 0
                            || window.last_activity == 0
                            || (now_ms >= window.last_activity
                                && now_ms - window.last_activity >= window.spec.end_inactivity_ms)
                    },
                    |deadline| now_ms >= deadline,
                );
                if quiet {
                    window.waiting = false;
                    window.initial_deadline = None;
                }
            }
            if active || window.waiting {
                self.routes[window.spec.route].desired = true;
                self.routes[window.spec.replaced].desired = false;
            }
        }
    }

    /// Reserve withdrawals first. Pending withdrawal must settle before any new attachment.
    pub fn next_operation(&mut self) -> Option<LinkReservation> {
        let withdrawal_pending = self
            .routes
            .iter()
            .any(|route| !route.paused && route.issued && !route.desired);
        let selected = self.routes.iter().enumerate().find(|(_, route)| {
            !route.paused
                && route.pending.is_none()
                && if withdrawal_pending {
                    route.issued && !route.desired
                } else {
                    !route.issued && route.desired
                }
        });
        let (index, _) = selected?;
        self.nonce = self.nonce.checked_add(1)?;
        let route = &mut self.routes[index];
        route.pending = Some(self.nonce);
        Some(LinkReservation {
            generation: self.generation,
            index,
            nonce: self.nonce,
            spec: route.spec.clone(),
            action: if withdrawal_pending {
                LinkTransition::Detach
            } else {
                LinkTransition::Attach
            },
        })
    }
    fn reserved(&self, reservation: &LinkReservation) -> bool {
        reservation.generation == self.generation
            && self.routes.get(reservation.index).is_some_and(|route| {
                route.pending == Some(reservation.nonce) && route.spec == reservation.spec
            })
    }
    /// Recheck after each slow lookup/dial and immediately before publishing attachment.
    pub fn current(&self, reservation: &LinkReservation) -> bool {
        self.reserved(reservation)
            && self.routes.get(reservation.index).is_some_and(|route| {
                // Pausing clears the nonce, so `reserved` already excludes paused work.
                route.desired == (reservation.action == LinkTransition::Attach)
            })
    }
    /// Settle exactly one reservation; stale completion cannot consume a replacement token.
    pub fn complete(&mut self, reservation: &LinkReservation, accepted: bool) -> bool {
        if !self.reserved(reservation) {
            return false;
        }
        let current = self.current(reservation);
        let route = &mut self.routes[reservation.index];
        route.pending = None;
        if current && accepted {
            route.issued = reservation.action == LinkTransition::Attach;
        }
        current
    }
    /// Operator disconnect-all pauses configured work and invalidates in-flight reservations.
    pub fn pause(&mut self, local: &str) {
        for route in self
            .routes
            .iter_mut()
            .filter(|route| route.spec.local == local)
        {
            route.paused = true;
            route.pending = None;
        }
    }
    /// Operator reconnect resumes intent; the next tick revalidates windows before dialing.
    pub fn resume(&mut self, local: &str) {
        for route in self
            .routes
            .iter_mut()
            .filter(|route| route.spec.local == local)
        {
            route.paused = false;
        }
    }
    /// Withdraw obsolete retained intent before an operator resumes automatic recovery.
    pub(super) fn withdraw_undesired(&mut self) -> Vec<String> {
        self.routes
            .iter_mut()
            .filter(|route| !route.desired && route.issued)
            .map(|route| {
                route.issued = false;
                route.pending = None;
                route.spec.remote.clone()
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "link_schedule_tests.rs"]
mod tests;
