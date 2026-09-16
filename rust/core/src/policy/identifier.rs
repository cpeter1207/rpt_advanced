//! Periodic and first-key identifier selection policy.

#[derive(Clone, Copy, Default)]
struct IdentifierState {
    satisfied_ms: u64,
    activity: bool,
    first_key_pending: bool,
}

/// One resolved identifier rule.
#[derive(Clone, Copy)]
pub struct IdentifierRule {
    interval_ms: u64,
    priority: i32,
    first_key_only: bool,
    regardless_of_activity: bool,
}

impl IdentifierRule {
    /// Construct an identifier rule from its resolved interval and precedence settings.
    #[must_use]
    pub const fn new(
        interval_ms: u64,
        priority: i32,
        first_key_only: bool,
        regardless_of_activity: bool,
    ) -> Self {
        Self {
            interval_ms,
            priority,
            first_key_only,
            regardless_of_activity,
        }
    }
}

/// Per-node mutable identifier state, owned by its control path.
pub struct IdentifierPolicy {
    states: Vec<IdentifierState>,
}

impl IdentifierPolicy {
    /// Allocate state for an immutable ordered rule list outside the real-time path.
    #[must_use]
    pub fn new(rules: &[IdentifierRule]) -> Self {
        Self {
            states: vec![IdentifierState::default(); rules.len()],
        }
    }

    /// Record conversation activity for every rule.
    pub fn activity(&mut self) {
        for state in &mut self.states {
            state.activity = true;
        }
    }

    /// Arm eligible first-key rules after a qualifying idle interval.
    pub fn first_key(&mut self, rules: &[IdentifierRule], idle_ms: u64) {
        for (rule, state) in rules.iter().zip(&mut self.states) {
            if rule.first_key_only && idle_ms >= rule.interval_ms {
                state.first_key_pending = true;
            }
        }
    }

    /// Return the highest-priority due rule without consuming it.
    #[must_use]
    pub fn select(
        &self,
        rules: &[IdentifierRule],
        now_ms: u64,
        receiver_active: bool,
        full_duplex: bool,
    ) -> Option<usize> {
        if receiver_active && !full_duplex {
            return None;
        }
        let mut selected: Option<usize> = None;
        for (index, (rule, state)) in rules.iter().zip(&self.states).enumerate() {
            let due = if rule.first_key_only {
                state.first_key_pending
            } else {
                now_ms - state.satisfied_ms >= rule.interval_ms
                    && (rule.regardless_of_activity || state.activity)
            };
            if due && selected.is_none_or(|current| rule.priority > rules[current].priority) {
                selected = Some(index);
            }
        }
        selected
    }

    /// Mark a successfully completed rule and every strictly lower-priority rule satisfied.
    pub fn complete(&mut self, rules: &[IdentifierRule], selected: usize, now_ms: u64) {
        let Some(selected_rule) = rules.get(selected) else {
            return;
        };
        for (index, (rule, state)) in rules.iter().zip(&mut self.states).enumerate() {
            if index == selected || rule.priority < selected_rule.priority {
                *state = IdentifierState {
                    satisfied_ms: now_ms,
                    ..IdentifierState::default()
                };
            }
        }
    }
}

#[cfg(test)]
#[path = "identifier_tests.rs"]
mod tests;
