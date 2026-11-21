//! Retained configuration transaction advanced only by the control owner.

use super::*;

#[derive(Clone, Copy)]
enum DeviceProgress {
    Prepare,
    Staging(usize),
    Handoff(usize),
    Quiescing(usize),
    Open(Option<usize>),
    Waiting(Option<usize>),
    Ready,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RestoreProgress {
    Close,
    Open,
    Waiting,
    Failed,
    Done,
}

/// Own every prepared object until all leases are ready or rollback is safe.
pub(super) struct ReloadTransaction<A: Send, C: Send> {
    pub(super) source: ConfigurationSource,
    pub(super) warnings: Vec<ConfigWarning>,
    pub(super) candidates: Vec<Candidate<A, C>>,
    pub(super) events: Vec<Event>,
    pub(super) scheduler: EventScheduler,
    pub(super) revision: u64,
    pub(super) generation_id: u64,
    pub(super) changed: Vec<usize>,
    pub(super) updated: Vec<String>,
    pub(super) opened: Vec<usize>,
    states: Vec<DeviceProgress>,
    failure: Option<RuntimeError>,
    fresh_closed: usize,
    restoring: Vec<RestoreProgress>,
    updates_restored: usize,
    restored: bool,
    commits_started: bool,
    updates_committed: usize,
    uncertain: bool,
}

impl<A: Send, C: Send> Candidate<A, C> {
    fn keeps_station(&self, node: &RuntimeNode<A, C>) -> bool {
        node.bounds.device == self.bounds.device && !self.station_changed
    }
}

impl<A: Send, C: Send> RuntimeNode<A, C> {
    /// Stop physical callbacks and drain their generation guards before closing either lease.
    fn quiesce_handoff(&mut self, now_ms: u64) -> bool {
        if !self.device.quiesce() {
            return false;
        }
        self.host.control().gate_callbacks();
        self.status(now_ms).protected_owners == 0
    }
}

impl<A: Send, C: Send> ReloadTransaction<A, C> {
    pub(super) fn new(
        source: ConfigurationSource,
        warnings: Vec<ConfigWarning>,
        candidates: Vec<Candidate<A, C>>,
        events: Vec<Event>,
        scheduler: EventScheduler,
        revisions: (u64, u64),
    ) -> Self {
        let states = vec![DeviceProgress::Prepare; candidates.len()];
        Self {
            source,
            warnings,
            candidates,
            events,
            scheduler,
            revision: revisions.0,
            generation_id: revisions.1,
            changed: Vec::new(),
            updated: Vec::new(),
            opened: Vec::new(),
            states,
            failure: None,
            fresh_closed: 0,
            restoring: Vec::new(),
            updates_restored: 0,
            restored: true,
            commits_started: false,
            updates_committed: 0,
            uncertain: false,
        }
    }

    /// Poll all independent leases once; a slow rig never stalls another owner.
    pub(super) fn advance(
        &mut self,
        runtime: &mut Runtime<A, C>,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        if self.uncertain || runtime.stopping && self.commits_started {
            return self.drain_uncertain(runtime, clock.now_ms);
        }
        if runtime.stopping && self.failure.is_none() {
            self.fail(RuntimeError::Busy);
        }
        if self.failure.is_none() {
            for index in 0..self.candidates.len() {
                if let Err(error) = self.forward(runtime, index, clock.now_ms) {
                    if matches!(self.source, ConfigurationSource::Modular(..)) {
                        // A restored device resumes its old callbacks. Fence every incumbent
                        // whose physical plan differs from the accepted desired generation
                        // before any reverse open, including stations omitted by the candidate.
                        for node in &mut runtime.nodes {
                            if !self.candidates.iter().any(|candidate| {
                                candidate.name == node.name && candidate.keeps_station(node)
                            }) {
                                node.failed_station_replacement = true;
                                node.publish_rf_inhibit();
                            }
                        }
                    }
                    self.fail(error);
                    break;
                }
            }
        }
        if self.failure.is_some() {
            return self.rollback(runtime, clock.now_ms);
        }
        if !self
            .states
            .iter()
            .all(|state| matches!(state, DeviceProgress::Ready))
        {
            return Ok(false);
        }
        while let Some(name) = self.updated.get(self.updates_committed) {
            self.commits_started = true;
            let node = runtime
                .nodes
                .iter_mut()
                .find(|node| &node.name == name)
                .expect("staged device retained");
            match node.device.poll_finish_update(true) {
                Ok(true) => self.updates_committed += 1,
                Ok(false) => return Ok(false),
                Err(_) => return self.drain_uncertain(runtime, clock.now_ms),
            }
        }
        // Removing a physical owner is part of this commit, even when no
        // replacement requires its device. Keep the old revision until every
        // affected provider has confirmed quiescence; peer detachment can follow.
        let mut ready = true;
        for node in &mut runtime.nodes {
            if node.device_open
                && !self
                    .candidates
                    .iter()
                    .any(|candidate| candidate.name == node.name)
            {
                self.commits_started = true;
                stop_node(node, clock.now_ms, &mut runtime.effects);
                ready &= !node.device_open;
            }
        }
        Ok(ready)
    }

    // A failed or interrupted commit may have consumed a reverse plan. Inhibit every touched
    // station and retain the candidate and device contexts until their owners have stopped.
    fn drain_uncertain(
        &mut self,
        runtime: &mut Runtime<A, C>,
        now_ms: u64,
    ) -> Result<bool, RuntimeError> {
        self.uncertain = true;
        let mut drained = true;
        for (index, node) in runtime.nodes.iter_mut().enumerate() {
            if self.changed.contains(&index) || self.updated.contains(&node.name) {
                stop_node(node, now_ms, &mut runtime.effects);
                drained &= !node.device_open;
            }
        }
        drained &= self.close_fresh();
        if drained {
            Err(RuntimeError::Restore)
        } else {
            Ok(false)
        }
    }

    fn fail(&mut self, error: RuntimeError) {
        self.failure = Some(error);
        self.restoring = vec![RestoreProgress::Close; self.changed.len()];
    }

    fn forward(
        &mut self,
        runtime: &mut Runtime<A, C>,
        index: usize,
        now_ms: u64,
    ) -> Result<(), RuntimeError> {
        loop {
            match self.states[index] {
                DeviceProgress::Prepare => {
                    let candidate = &self.candidates[index];
                    let old = runtime
                        .nodes
                        .iter()
                        .position(|node| node.name == candidate.name);
                    if let Some(old_index) = old {
                        let node = &mut runtime.nodes[old_index];
                        if candidate.keeps_station(node) {
                            self.states[index] = DeviceProgress::Ready;
                            return Ok(());
                        }
                        if node.bounds.device == candidate.bounds.device
                            && node.bounds.receive_maximum == candidate.bounds.receive_maximum
                            && node.bounds.transmit_maximum == candidate.bounds.transmit_maximum
                        {
                            self.updated.push(node.name.clone());
                            self.states[index] = DeviceProgress::Staging(old_index);
                        } else {
                            self.states[index] = DeviceProgress::Handoff(old_index);
                        }
                    } else {
                        self.states[index] = DeviceProgress::Open(None);
                    }
                }
                DeviceProgress::Staging(old_index) => {
                    let node = &mut runtime.nodes[old_index];
                    match node.device.poll_update(&self.candidates[index].bounds)? {
                        super::super::DeviceUpdate::Staged => {
                            self.states[index] = DeviceProgress::Ready
                        }
                        super::super::DeviceUpdate::Pending => return Ok(()),
                        super::super::DeviceUpdate::Handoff => {
                            self.updated.retain(|name| name != &node.name);
                            self.states[index] = DeviceProgress::Handoff(old_index);
                        }
                    }
                }
                DeviceProgress::Handoff(old_index) => {
                    let node = &mut runtime.nodes[old_index];
                    // Reject before touching a device with externally held work.
                    if node.status(now_ms).outstanding_work != 0 {
                        return Err(RuntimeError::Busy);
                    }
                    // Keep new work out between polls, including while the device is restoring.
                    node.handoff_pending = true;
                    // Once retirement starts, another node's failure must also
                    // restore this owner, even if its stop is still pending.
                    self.changed.push(old_index);
                    self.states[index] = DeviceProgress::Quiescing(old_index);
                }
                DeviceProgress::Quiescing(old_index) => {
                    let node = &mut runtime.nodes[old_index];
                    if !node.quiesce_handoff(now_ms) {
                        return Ok(());
                    }
                    node.device.close();
                    node.device_open = false;
                    self.states[index] = DeviceProgress::Open(Some(old_index));
                }
                DeviceProgress::Open(old) => {
                    let candidate = &mut self.candidates[index];
                    let accepted = if let Some(old) = old {
                        let node = &mut runtime.nodes[old];
                        node.device_open = node.device.open(&candidate.bounds);
                        node.device_open
                    } else {
                        self.opened.push(index);
                        candidate
                            .device
                            .as_mut()
                            .ok_or(RuntimeError::Preparation)?
                            .open(&candidate.bounds)
                    };
                    if !accepted {
                        return Err(RuntimeError::Device);
                    }
                    self.states[index] = DeviceProgress::Waiting(old);
                }
                DeviceProgress::Waiting(old) => {
                    let ready = if let Some(old) = old {
                        runtime.nodes[old].device.activation_ready()?
                    } else {
                        self.candidates[index]
                            .device
                            .as_mut()
                            .ok_or(RuntimeError::Preparation)?
                            .activation_ready()?
                    };
                    if !ready {
                        return Ok(());
                    }
                    self.states[index] = DeviceProgress::Ready;
                }
                DeviceProgress::Ready => return Ok(()),
            }
        }
    }

    fn close_fresh(&mut self) -> bool {
        while self.fresh_closed < self.opened.len() {
            let index = self.opened[self.opened.len() - self.fresh_closed - 1];
            let device = self.candidates[index]
                .device
                .as_mut()
                .expect("opened candidate lease");
            if !device.quiesce() {
                break;
            }
            device.close();
            self.fresh_closed += 1;
        }
        self.fresh_closed == self.opened.len()
    }

    fn rollback(&mut self, runtime: &mut Runtime<A, C>, now_ms: u64) -> Result<bool, RuntimeError> {
        let fresh_drained = self.close_fresh();
        for position in (0..self.changed.len()).rev() {
            let node = &mut runtime.nodes[self.changed[position]];
            loop {
                match self.restoring[position] {
                    RestoreProgress::Close => {
                        if !node.quiesce_handoff(now_ms) {
                            break;
                        }
                        node.device.close();
                        node.device_open = false;
                        self.restoring[position] = if runtime.stopping {
                            RestoreProgress::Done
                        } else {
                            RestoreProgress::Open
                        };
                    }
                    RestoreProgress::Open => {
                        node.device_open = node.device.restore(&node.bounds);
                        self.restoring[position] = if node.device_open {
                            RestoreProgress::Waiting
                        } else {
                            RestoreProgress::Failed
                        };
                    }
                    RestoreProgress::Waiting => match node.device.activation_ready() {
                        Ok(false) => break,
                        Ok(true) => {
                            node.host.control().restore_callbacks();
                            node.handoff_pending = false;
                            self.restoring[position] = RestoreProgress::Done;
                        }
                        Err(_) => self.restoring[position] = RestoreProgress::Failed,
                    },
                    RestoreProgress::Failed => {
                        if !node.device.quiesce() {
                            break;
                        }
                        node.device.close();
                        node.device_open = false;
                        stop_node(node, now_ms, &mut runtime.effects);
                        self.restored = false;
                        self.restoring[position] = RestoreProgress::Done;
                    }
                    RestoreProgress::Done => break,
                }
            }
        }
        while self.updates_restored < self.updated.len() {
            let name = &self.updated[self.updated.len() - self.updates_restored - 1];
            let node = runtime
                .nodes
                .iter_mut()
                .find(|node| &node.name == name)
                .expect("updated lease");
            match node.device.poll_finish_update(false) {
                Ok(false) => return Ok(false),
                Err(_) => return self.drain_uncertain(runtime, now_ms),
                Ok(true) => (),
            }
            self.updates_restored += 1;
        }
        if !fresh_drained
            || self
                .restoring
                .iter()
                .any(|state| *state != RestoreProgress::Done)
        {
            return Ok(false);
        }
        Err(if self.restored {
            self.failure.take().expect("rollback cause")
        } else {
            RuntimeError::Restore
        })
    }
}
