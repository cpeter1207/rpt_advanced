//! Periodic and first-key identifier selection policy.

#[derive(Clone, Copy, Default)]
struct IdentifierState {
    satisfied_ms: u64,
    retry_after_ms: u64,
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
    next_first_key: usize,
}

impl IdentifierPolicy {
    /// Allocate state for an immutable ordered rule list outside the real-time path.
    #[must_use]
    pub fn new(rules: &[IdentifierRule]) -> Self {
        Self {
            states: vec![IdentifierState::default(); rules.len()],
            next_first_key: 0,
        }
    }

    /// Record conversation activity for every rule.
    pub fn activity(&mut self) {
        for state in &mut self.states {
            state.activity = true;
        }
    }

    /// Arm one idle-eligible initial ID in round-robin order and return its index.
    /// An already pending initial keeps its deadline and retry until completion.
    pub fn first_key(&mut self, rules: &[IdentifierRule], idle_ms: u64) -> Option<usize> {
        if self.states.iter().any(|state| state.first_key_pending) {
            return None;
        }
        let selected = (self.next_first_key..rules.len())
            .chain(0..self.next_first_key)
            .find(|&index| rules[index].first_key_only && idle_ms >= rules[index].interval_ms)?;
        self.states[selected].first_key_pending = true;
        self.next_first_key = (selected + 1) % rules.len();
        Some(selected)
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
        self.select_where(rules, now_ms, receiver_active, full_duplex, |_| true)
    }

    /// Select the highest-priority due rule allowed by current hang/deadline arbitration.
    pub(crate) fn select_where(
        &self,
        rules: &[IdentifierRule],
        now_ms: u64,
        receiver_active: bool,
        full_duplex: bool,
        eligible: impl Fn(usize) -> bool,
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
            if due
                && eligible(index)
                && now_ms >= state.retry_after_ms
                && selected.is_none_or(|current| rule.priority > rules[current].priority)
            {
                selected = Some(index);
            }
        }
        selected
    }

    /// Defer an unsuccessful attempt by its configured interval without satisfying any ID.
    pub fn defer(&mut self, rules: &[IdentifierRule], selected: usize, now_ms: u64) {
        if let Some((rule, state)) = rules.get(selected).zip(self.states.get_mut(selected)) {
            state.retry_after_ms = now_ms.saturating_add(rule.interval_ms);
        }
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
