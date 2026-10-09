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
    /// Owning configured-group identity, absent for standalone routes.
    pub group_label: Option<String>,
    /// Operator-facing group name, absent when not configured.
    pub group_name: Option<String>,
    /// Zero-based order within the configured group, absent for standalone routes.
    pub group_priority: Option<usize>,
}
/// One same-node replacement window with existing post-window inactivity behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementSpec {
    /// Ordered replacement group route indices.
    pub routes: Vec<usize>,
    /// Permanent route indices suppressed while this window requests its group.
    pub replaced: Vec<usize>,
    /// Validated local date/time selection.
    pub window: ScheduledWindow,
    /// Required receive-idle time after the window; zero ends immediately.
    pub end_inactivity_ms: u64,
    /// Ordered warning leads before the next window start.
    pub warning_before_start_ms: Vec<u64>,
    /// Ordered warning leads before expected schedule disconnection.
    pub warning_before_end_ms: Vec<u64>,
    /// Stable catalog message selected for warnings.
    pub warning_message_id: Option<String>,
}

/// One due schedule warning, ready for control-plane localization and queuing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScheduleWarning {
    /// Stable catalog message ID.
    pub message_id: String,
    /// Expected time until the related boundary, rounded only during formatting.
    pub remaining_ms: u64,
    /// Deadline after which this warning must no longer be queued.
    pub deadline_ms: u64,
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
    last_activity: Option<u64>,
    initialized: bool,
    was_active: bool,
    waiting: bool,
    initial_deadline: Option<u64>,
    start_occurrence: Option<u64>,
    start_warned: Vec<u64>,
    end_deadline: Option<(u64, u64)>,
    end_warned: Vec<u64>,
}

pub(super) struct WarningGate<F> {
    pub(super) source_active: F,
    pub(super) capacity: bool,
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
    /// Configured group identity used by final topology admission.
    pub fn group_label(&self) -> Option<&str> {
        self.spec.group_label.as_deref()
    }
}

/// Serialized permanent-link intent; independent of dialing and taskprocessor mechanics.
pub struct LinkScheduler {
    generation: u64,
    nonce: u64,
    routes: Vec<Route>,
    windows: Vec<Window>,
}

fn same_route_identity(left: &RouteSpec, right: &RouteSpec) -> bool {
    left.local == right.local
        && left.remote == right.remote
        && left.permanent == right.permanent
        && left.group_label == right.group_label
}

fn duplicate_routes_overlap(routes: &[RouteSpec], windows: &[ReplacementSpec]) -> bool {
    for (index, route) in routes.iter().enumerate() {
        for (other_index, other) in routes[..index].iter().enumerate() {
            if route.local != other.local || route.remote != other.remote {
                continue;
            }
            if route.permanent || other.permanent || route.group_label == other.group_label {
                return true;
            }
            let route_windows = windows
                .iter()
                .filter(|window| window.routes.contains(&index));
            let other_windows = windows
                .iter()
                .filter(|window| window.routes.contains(&other_index));
            let route_windows: Vec<_> = route_windows.map(|window| &window.window).collect();
            let other_windows: Vec<_> = other_windows.map(|window| &window.window).collect();
            if route_windows.is_empty()
                || other_windows.is_empty()
                || route_windows
                    .iter()
                    .any(|window| other_windows.iter().any(|other| window.overlaps(other)))
            {
                return true;
            }
        }
    }
    false
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
            || duplicate_routes_overlap(&routes, &windows)
            || routes.iter().any(|route| {
                route.local.is_empty() || route.remote.is_empty() || route.local == route.remote
            })
            || windows.iter().any(|window| {
                let invalid = window.routes.is_empty()
                    || window.replaced.is_empty()
                    || window
                        .routes
                        .iter()
                        .any(|route| window.replaced.contains(route))
                    || window
                        .routes
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != window.routes.len()
                    || window
                        .replaced
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != window.replaced.len()
                    || window
                        .routes
                        .first()
                        .and_then(|index| routes.get(*index))
                        .is_none_or(|first| {
                            first.permanent
                                || window.replaced.iter().any(|index| {
                                    !routes.get(*index).is_some_and(|replaced| {
                                        replaced.permanent && first.local == replaced.local
                                    })
                                })
                                || (window.routes.len() > 1
                                    && (first.group_label.is_none()
                                        || window.routes.iter().enumerate().any(
                                            |(priority, index)| {
                                                !routes.get(*index).is_some_and(|route| {
                                                    !route.permanent
                                                        && route.local == first.local
                                                        && route.group_label == first.group_label
                                                        && route.group_name == first.group_name
                                                        && route.group_priority == Some(priority)
                                                })
                                            },
                                        )))
                        })
                    || window
                        .replaced
                        .first()
                        .and_then(|index| routes.get(*index))
                        .is_none_or(|first| {
                            window
                                .replaced
                                .iter()
                                .any(|index| routes[*index].group_label != first.group_label)
                        });
                invalid
            })
        {
            return Err(SchedulerError::Invalid);
        }
        let window_states = windows
            .into_iter()
            .map(|spec| {
                let old = previous.and_then(|previous| {
                    previous.windows.iter().find(|window| {
                        window.spec.window == spec.window
                            && window.spec.end_inactivity_ms == spec.end_inactivity_ms
                            && window.spec.warning_before_start_ms == spec.warning_before_start_ms
                            && window.spec.warning_before_end_ms == spec.warning_before_end_ms
                            && window.spec.warning_message_id == spec.warning_message_id
                            && window.spec.routes.len() == spec.routes.len()
                            && window
                                .spec
                                .routes
                                .iter()
                                .zip(&spec.routes)
                                .all(|(old, new)| previous.routes[*old].spec == routes[*new])
                            && window.spec.replaced.len() == spec.replaced.len()
                            && window.spec.replaced.iter().zip(&spec.replaced).all(
                                |(old_index, new_index)| {
                                    previous.routes[*old_index].spec == routes[*new_index]
                                },
                            )
                    })
                });
                // Changed windows still inherit node receive activity, never silently resetting idle.
                let activity = previous.and_then(|old| {
                    old.windows
                        .iter()
                        .filter(|window| {
                            old.routes[window.spec.routes[0]].spec.local
                                == routes[spec.routes[0]].local
                        })
                        .filter_map(|window| window.last_activity)
                        .max()
                });
                Window {
                    spec,
                    last_activity: activity,
                    initialized: old.is_some_and(|old| old.initialized),
                    was_active: old.is_some_and(|old| old.was_active),
                    waiting: old.is_some_and(|old| old.waiting),
                    initial_deadline: old.and_then(|old| old.initial_deadline),
                    start_occurrence: old.and_then(|old| old.start_occurrence),
                    start_warned: old.map_or_else(Vec::new, |old| old.start_warned.clone()),
                    end_deadline: old.and_then(|old| old.end_deadline),
                    end_warned: old.map_or_else(Vec::new, |old| old.end_warned.clone()),
                }
            })
            .collect();
        let routes = routes
            .into_iter()
            .map(|spec| {
                let old = previous.and_then(|old| {
                    old.routes
                        .iter()
                        .find(|route| same_route_identity(&route.spec, &spec))
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
    /// Whether a direct peer is managed as a member of any permanent group.
    pub fn is_group_member(&self, remote: &str) -> bool {
        self.routes
            .iter()
            .any(|route| route.spec.remote == remote && route.spec.group_label.is_some())
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
                    && !candidate
                        .routes
                        .iter()
                        .any(|new| same_route_identity(&new.spec, &old.spec))
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
        activity: impl FnMut(&str) -> Option<u64>,
        owns: impl FnMut(&RouteSpec) -> bool,
    ) {
        let _ = self.tick_with_warnings(
            local,
            second,
            now_ms,
            activity,
            owns,
            WarningGate {
                source_active: |_: &str| false,
                capacity: false,
            },
        );
    }

    /// Evaluate policy and return at most one idle-only warning per control tick.
    pub(super) fn tick_with_warnings(
        &mut self,
        local: CivilTime,
        second: u8,
        now_ms: u64,
        mut activity: impl FnMut(&str) -> Option<u64>,
        mut owns: impl FnMut(&RouteSpec) -> bool,
        mut warning_gate: WarningGate<impl FnMut(&str) -> bool>,
    ) -> Option<ScheduleWarning> {
        let mut due_warning = None;
        for route in &mut self.routes {
            route.desired = route.spec.permanent;
            if route.issued && !owns(&route.spec) {
                route.issued = false;
            }
        }
        for window in &mut self.windows {
            let observed = activity(&self.routes[window.spec.routes[0]].spec.local);
            if observed
                .is_some_and(|observed| window.last_activity.is_none_or(|last| observed > last))
            {
                window.last_activity = observed;
                window.initial_deadline = None;
                if window.waiting {
                    window.end_deadline = None;
                    window.end_warned.clear();
                }
            }
            let window_active = window.spec.window.matches(&local);
            if !window.initialized {
                window.initialized = true;
                if !window_active && window.spec.end_inactivity_ms != 0 {
                    if let Some(elapsed) = window.spec.window.elapsed_after_end(&local, second) {
                        if window.last_activity.is_some() {
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
            if window_active {
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
                            || window.last_activity.is_none()
                            || window.last_activity.is_some_and(|last| {
                                now_ms >= last && now_ms - last >= window.spec.end_inactivity_ms
                            })
                    },
                    |deadline| now_ms >= deadline,
                );
                if quiet {
                    window.waiting = false;
                    window.initial_deadline = None;
                }
            }
            if window_active || window.waiting {
                for index in &window.spec.routes {
                    self.routes[*index].desired = true;
                }
                for index in &window.spec.replaced {
                    self.routes[*index].desired = false;
                }
            }
            if due_warning.is_none() {
                due_warning = next_warning(
                    window,
                    local,
                    second,
                    now_ms,
                    (warning_gate.source_active)(&self.routes[window.spec.routes[0]].spec.local),
                    warning_gate.capacity,
                );
            }
        }
        due_warning
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

fn next_warning(
    window: &mut Window,
    local: CivilTime,
    second: u8,
    now_ms: u64,
    active: bool,
    capacity: bool,
) -> Option<ScheduleWarning> {
    let message_id = window.spec.warning_message_id.as_deref()?;
    if !capacity {
        return None;
    }
    if !window.spec.warning_before_start_ms.is_empty() {
        let (occurrence, remaining_ms) = window.spec.window.next_start(&local, second)?;
        if window.start_occurrence != Some(occurrence) {
            window.start_occurrence = Some(occurrence);
            window.start_warned.clear();
        }
        if let Some(lead) = window
            .spec
            .warning_before_start_ms
            .iter()
            .find(|lead| **lead >= remaining_ms && !window.start_warned.contains(lead))
            .copied()
        {
            if active {
                window.start_warned.push(lead);
                return None;
            }
            window.start_warned.push(lead);
            return Some(ScheduleWarning {
                message_id: message_id.to_owned(),
                remaining_ms,
                deadline_ms: now_ms.saturating_add(remaining_ms),
            });
        }
    }
    if window.spec.warning_before_end_ms.is_empty() {
        return None;
    }
    let (end_identity, end_ms) = if window.spec.window.matches(&local) {
        let (identity, remaining) = window.spec.window.next_end(&local, second)?;
        (identity, now_ms.saturating_add(remaining))
    } else if let Some(elapsed) = window.spec.window.elapsed_after_end(&local, second) {
        (
            now_ms.saturating_sub(elapsed),
            now_ms.saturating_sub(elapsed),
        )
    } else {
        let (identity, remaining) = window.spec.window.next_end(&local, second)?;
        (identity, now_ms.saturating_add(remaining))
    };
    let deadline = if window.spec.end_inactivity_ms == 0 {
        end_ms
    } else if let Some(last) = window.last_activity {
        end_ms.max(last.saturating_add(window.spec.end_inactivity_ms))
    } else {
        end_ms.saturating_add(window.spec.end_inactivity_ms)
    };
    let identity = (end_identity, deadline);
    if window.end_deadline != Some(identity) {
        window.end_deadline = Some(identity);
        window.end_warned.clear();
    }
    let remaining_ms = deadline.saturating_sub(now_ms);
    if remaining_ms == 0 {
        return None;
    }
    let lead = window
        .spec
        .warning_before_end_ms
        .iter()
        .find(|lead| **lead >= remaining_ms && !window.end_warned.contains(lead))
        .copied()?;
    if active {
        window.end_warned.push(lead);
        return None;
    }
    window.end_warned.push(lead);
    Some(ScheduleWarning {
        message_id: message_id.to_owned(),
        remaining_ms,
        deadline_ms: deadline,
    })
}

#[cfg(test)]
#[path = "link_schedule_tests.rs"]
mod tests;
