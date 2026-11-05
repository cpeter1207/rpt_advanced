//! Retained configuration transaction advanced only by the control owner.

use super::*;

#[derive(Clone, Copy)]
enum DeviceProgress {
    Prepare,
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
    pub(super) document: ConfigDocument,
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
}

impl<A: Send, C: Send> ReloadTransaction<A, C> {
    pub(super) fn new(
        document: ConfigDocument,
        warnings: Vec<ConfigWarning>,
        candidates: Vec<Candidate<A, C>>,
        events: Vec<Event>,
        scheduler: EventScheduler,
        revisions: (u64, u64),
    ) -> Self {
        let states = vec![DeviceProgress::Prepare; candidates.len()];
        Self {
            document,
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
        }
    }

    /// Poll all independent leases once; a slow rig never stalls another owner.
    pub(super) fn advance(
        &mut self,
        runtime: &mut Runtime<A, C>,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        if runtime.stopping && self.failure.is_none() {
            self.fail(RuntimeError::Busy);
        }
        if self.failure.is_none() {
            for index in 0..self.candidates.len() {
                if let Err(error) = self.forward(runtime, index, clock.now_ms) {
                    self.fail(error);
                    break;
                }
            }
        }
        if self.failure.is_some() {
            self.rollback(runtime, clock.now_ms)
        } else {
            Ok(self
                .states
                .iter()
                .all(|state| matches!(state, DeviceProgress::Ready)))
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
                        if node.bounds.device == candidate.bounds.device
                            && node.settings.radio == candidate.settings.radio
                        {
                            self.states[index] = DeviceProgress::Ready;
                            return Ok(());
                        }
                        if node.bounds.device == candidate.bounds.device
                            && node.bounds.receive_maximum == candidate.bounds.receive_maximum
                            && node.bounds.transmit_maximum == candidate.bounds.transmit_maximum
                        {
                            match node.device.update(&candidate.bounds) {
                                Ok(true) => {
                                    self.updated.push(node.name.clone());
                                    self.states[index] = DeviceProgress::Ready;
                                    return Ok(());
                                }
                                Ok(false) => (),
                                Err(error) => {
                                    self.updated.push(node.name.clone());
                                    return Err(error);
                                }
                            }
                        }
                        // Reject before touching a device with externally held work.
                        let status = node.status(now_ms);
                        if status.outstanding_work != 0 || status.protected_owners != 0 {
                            return Err(RuntimeError::Busy);
                        }
                        // Once retirement starts, another node's failure must also
                        // restore this owner, even if its stop is still pending.
                        self.changed.push(old_index);
                        self.states[index] = DeviceProgress::Quiescing(old_index);
                    } else {
                        self.states[index] = DeviceProgress::Open(None);
                    }
                }
                DeviceProgress::Quiescing(old_index) => {
                    let node = &mut runtime.nodes[old_index];
                    if !node.device.quiesce() {
                        return Ok(());
                    }
                    node.host.control().gate_callbacks();
                    if node.status(now_ms).protected_owners != 0 {
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

    fn rollback(&mut self, runtime: &mut Runtime<A, C>, now_ms: u64) -> Result<bool, RuntimeError> {
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
        for position in (0..self.changed.len()).rev() {
            let node = &mut runtime.nodes[self.changed[position]];
            loop {
                match self.restoring[position] {
                    RestoreProgress::Close => {
                        if !node.device.quiesce() {
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
                        node.device_open = node.device.open(&node.bounds);
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
                        node.host.control().stop(now_ms).ok();
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
            if node.device.finish_update(false).is_err() {
                stop_node(node, now_ms, &mut runtime.effects);
                self.restored = false;
            }
            self.updates_restored += 1;
        }
        if self.fresh_closed != self.opened.len()
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
