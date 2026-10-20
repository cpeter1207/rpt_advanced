//! Nonblocking site intent sent only through an existing radio owner.
use super::*;
use crate::site::acquisition::OutputStatus;
use rpt_advanced_core::{
    config::{ResolvedSiteSettings, SiteEndpoint},
    site::{FanOutput, SiteObservations},
};
use std::collections::{BTreeMap, BTreeSet};

/// One cached/control operation on the selected physical bank, never a hardware open.
pub enum Operation {
    /// Read the owner's published state without physical I/O.
    Snapshot,
    /// Queue a bank ownership change.
    Bind(abi::UrpAstSiteBinding),
    /// Queue steady output levels.
    Publish(abi::UrpAstSiteRequest),
}
/// Coalesced site bindings grouped by the node that already owns each radio.
#[derive(Default)]
pub struct RadioSite {
    banks: BTreeMap<(String, u32), Bank>,
    replacement: Option<Replacement>,
}
struct Replacement {
    banks: BTreeMap<(String, u32), Bank>,
    replaced: BTreeSet<String>,
    released: bool,
}
#[derive(Default)]
struct Bank {
    inputs: Vec<Input>,
    outputs: Vec<Output>,
    generation: u64,
    pending: Option<Pending>,
    fenced: u32,
    releasing: bool,
    settled: bool,
    touched: bool,
    error: Option<ReadingQuality>,
}
struct Input {
    node: String,
    name: String,
    mask: u32,
    active_high: bool,
    reading: Observation<bool>,
}
struct Output {
    node: String,
    name: String,
    mask: u32,
    active_high: bool,
    safe_on: bool,
    status: OutputStatus,
    observed: bool,
}
/// One accepted command; generation is an opaque hardware-owner token.
struct Pending {
    binding: bool,
    generation: u64,
    rejected: u64,
    mask: u32,
    high: u32,
    since_ms: u64,
}
impl Bank {
    fn target_mask(&self) -> u32 {
        self.mask()
            & if self.releasing {
                !self.fenced
            } else {
                u32::MAX
            }
    }
    fn mask(&self) -> u32 {
        self.outputs
            .iter()
            .fold(0, |mask, output| mask | output.mask)
    }
    fn high(&self, safe: bool) -> u32 {
        self.outputs.iter().fold(0, |mask, output| {
            let on = if safe || self.fenced & output.mask != 0 {
                output.safe_on
            } else {
                output.status.requested == 1.0
            };
            mask | if on == output.active_high {
                output.mask
            } else {
                0
            }
        })
    }
    fn fault(&mut self, quality: ReadingQuality) {
        self.error = Some(quality);
        self.settled = false;
        for output in &mut self.outputs {
            output.status.quality = quality;
            output.observed = true;
        }
    }
    fn observe(&mut self, snapshot: &abi::UrpAstSiteSnapshot, now_ms: u64) {
        self.settled = false;
        for value in &mut self.inputs {
            value.reading = input(snapshot, value.mask, value.active_high, now_ms);
        }
        if snapshot.generation == 0 {
            self.pending = None;
            self.generation = 0;
            self.fault(ReadingQuality::Unavailable);
            return;
        }
        if self.touched {
            if let Err(quality) = service_quality(snapshot) {
                self.fault(quality);
                return;
            }
        }
        self.error = None;
        if let Some(pending) = &self.pending {
            if snapshot.rejected_commands != pending.rejected
                || (!pending.binding && snapshot.generation != pending.generation)
            {
                self.pending = None;
                self.generation = 0;
                self.fault(ReadingQuality::Fault);
                return;
            }
            let generation_ready = (snapshot.generation != pending.generation) == pending.binding;
            if generation_ready && serviced(snapshot, pending.mask, pending.high) {
                self.generation = snapshot.generation;
                for output in self
                    .outputs
                    .iter_mut()
                    .filter(|output| output.mask & pending.mask != 0)
                {
                    output.status.submitted = Some((
                        f64::from((pending.high & output.mask != 0) == output.active_high),
                        now_ms,
                    ));
                    output.status.quality = ReadingQuality::Valid;
                    output.observed = true;
                }
                self.pending = None;
            } else if now_ms.saturating_sub(pending.since_ms) >= 5000 {
                // Do not enqueue another write behind an accepted but stalled request.
                self.fault(ReadingQuality::Stale);
            }
        }
        let mask = self.target_mask();
        // An unclaimed empty bank has no physical write to await.
        self.settled = self.pending.is_none()
            && ((mask == 0 && snapshot.claimed_mask == 0)
                || (self.generation == snapshot.generation
                    && serviced(snapshot, mask, self.high(false) & mask)));
        if self.settled {
            self.error = None;
            for output in self
                .outputs
                .iter_mut()
                .filter(|output| output.mask & mask != 0)
            {
                output.status.quality = ReadingQuality::Valid;
            }
        }
    }
    fn operation(&self, snapshot: &abi::UrpAstSiteSnapshot) -> Option<Operation> {
        if self.outputs.is_empty()
            || self.pending.is_some()
            || self.settled
            || matches!(
                self.error,
                Some(ReadingQuality::Stale | ReadingQuality::Unavailable)
            )
            || (self.fenced != 0 && !self.touched)
        {
            return None;
        }
        let mask = self.target_mask();
        let high = self.high(false) & mask;
        if self.generation != snapshot.generation || snapshot.claimed_mask != mask {
            Some(Operation::Bind(abi::UrpAstSiteBinding {
                struct_size: size_of::<abi::UrpAstSiteBinding>() as u32,
                abi_version: 1,
                generation: snapshot.generation,
                output_mask: mask,
                initial_high_mask: high,
                safe_high_mask: self.high(true) & mask,
            }))
        } else {
            // observe() already marks a fully serviced bank settled.
            Some(Operation::Publish(abi::UrpAstSiteRequest {
                struct_size: size_of::<abi::UrpAstSiteRequest>() as u32,
                abi_version: 1,
                generation: snapshot.generation,
                output_mask: mask,
                high_mask: high,
            }))
        }
    }
}
/// The radio owner timestamps its successful input/output service cycle, while
/// snapshot queries advance only snapshot_at_ms. Cached masks alone prove no
/// current liveness. Keep old submissions as history when that heartbeat stops.
fn service_quality(snapshot: &abi::UrpAstSiteSnapshot) -> Result<(), ReadingQuality> {
    if snapshot.observed_at_valid != 1 {
        return Err(ReadingQuality::Unavailable);
    }
    match snapshot.snapshot_at_ms.checked_sub(snapshot.observed_at_ms) {
        Some(age) if age < 5000 => Ok(()),
        Some(_) => Err(ReadingQuality::Stale),
        None => Err(ReadingQuality::Unavailable),
    }
}
fn serviced(snapshot: &abi::UrpAstSiteSnapshot, mask: u32, high: u32) -> bool {
    snapshot.claimed_mask == mask
        && snapshot.submitted_generation == snapshot.generation
        && snapshot.serviced_generation == snapshot.generation
        && snapshot.requested_high_mask & mask == high
        && snapshot.submitted_high_mask & mask == high
        && snapshot.serviced_high_mask & mask == high
}
fn same_output(first: &Output, second: &Output) -> bool {
    first.node == second.node
        && first.name == second.name
        && first.mask == second.mask
        && first.active_high == second.active_high
}
impl RadioSite {
    /// Begin safe handoff only for removed bindings or replaced radio owners.
    pub fn begin(&mut self, next: Self, replaced: &BTreeSet<String>) -> bool {
        if self.replacement.is_some() {
            return false;
        }
        for (key, bank) in &mut self.banks {
            bank.fenced = bank
                .outputs
                .iter()
                .filter(|output| {
                    replaced.contains(&key.0)
                        || next.banks.get(key).is_none_or(|next| {
                            !next
                                .outputs
                                .iter()
                                .any(|candidate| same_output(output, candidate))
                        })
                })
                .fold(0, |mask, output| mask | output.mask);
            if bank.fenced != 0 {
                bank.settled = false;
            }
        }
        self.replacement = Some(Replacement {
            banks: next.banks,
            replaced: replaced.clone(),
            released: false,
        });
        true
    }
    /// Inspect acknowledgments without waiting on the hardware owner.
    pub fn barrier(&self) -> Option<Result<(), ReadingQuality>> {
        let mut ready = true;
        for bank in self
            .banks
            .values()
            .filter(|bank| bank.fenced != 0 && bank.touched)
        {
            if let Some(error) = bank.error {
                return Some(Err(error));
            }
            ready &= bank.settled;
        }
        ready.then_some(Ok(()))
    }
    /// Release only pins already acknowledged at their safe level.
    pub fn stage(&mut self) -> bool {
        if self.barrier() != Some(Ok(())) {
            return false;
        }
        let Some(replacement) = &mut self.replacement else {
            return false;
        };
        if replacement.released {
            return false;
        }
        replacement.released = true;
        for bank in self.banks.values_mut().filter(|bank| bank.fenced != 0) {
            bank.releasing = true;
            bank.settled = false;
        }
        true
    }
    /// Adopt prepared configuration after the controller publishes it.
    pub fn commit(&mut self) -> bool {
        if self.barrier() != Some(Ok(()))
            || !self.replacement.as_ref().is_some_and(|next| next.released)
        {
            return false;
        }
        let mut next = self.replacement.take().expect("validated radio handoff");
        for (key, bank) in &mut next.banks {
            if let Some(old) = self.banks.remove(key) {
                for output in &mut bank.outputs {
                    if let Some(previous) = old
                        .outputs
                        .iter()
                        .find(|previous| same_output(output, previous))
                    {
                        output.status = previous.status.clone();
                        output.observed = previous.observed;
                    }
                }
                if !next.replaced.contains(&key.0) {
                    bank.pending = old.pending;
                    bank.touched = old.touched;
                }
                // Refresh binding metadata, including safe levels, without changing retained pins.
                bank.generation = 0;
            }
        }
        self.banks = next.banks;
        true
    }
    /// Retain prior bindings and restore live demand after a failed reload.
    pub fn abort(&mut self) {
        self.replacement = None;
        for bank in self.banks.values_mut() {
            bank.fenced = 0;
            bank.releasing = false;
            bank.settled = false;
        }
    }
    /// Resolve configured physical pins and exclude duplicate output aliases.
    pub fn prepare(
        settings: &BTreeMap<String, ResolvedSiteSettings>,
    ) -> Result<Self, ReadingQuality> {
        let mut result = Self::default();
        for (node, settings) in settings {
            for input in settings.inputs.iter().filter(|input| input.enabled) {
                if let Some(endpoint) = input.endpoint.as_ref().filter(|endpoint| radio(endpoint)) {
                    let (bank, mask) = pin(endpoint.backend, &endpoint.channel, false)?;
                    result
                        .banks
                        .entry((endpoint.device.clone(), bank))
                        .or_default()
                        .inputs
                        .push(Input {
                            node: node.clone(),
                            name: input.name.clone(),
                            mask,
                            active_high: input.active_high,
                            reading: Observation {
                                value: None,
                                observed_at_ms: None,
                                quality: ReadingQuality::Unavailable,
                            },
                        });
                }
            }
            for output in settings.outputs.iter().filter(|output| output.enabled) {
                if let Some(endpoint) = output.endpoint.as_ref().filter(|endpoint| radio(endpoint))
                {
                    let (bank, mask) = pin(endpoint.backend, &endpoint.channel, true)?;
                    let bank = result
                        .banks
                        .entry((endpoint.device.clone(), bank))
                        .or_default();
                    if bank.mask() & mask != 0 {
                        return Err(ReadingQuality::Fault);
                    }
                    bank.outputs.push(Output {
                        node: node.clone(),
                        name: output.name.clone(),
                        mask,
                        active_high: output.active_high,
                        safe_on: output.safe_on,
                        status: OutputStatus {
                            requested: f64::from(output.startup_on),
                            submitted: None,
                            quality: ReadingQuality::Unavailable,
                        },
                        observed: false,
                    });
                }
            }
        }
        Ok(result)
    }
    /// Observe and submit through the existing owner; acceptance is not service/readback.
    pub fn poll(
        &mut self,
        now_ms: u64,
        mut io: impl FnMut(&str, u32, Operation) -> Result<Option<abi::UrpAstSiteSnapshot>, i32>,
    ) -> BTreeMap<String, SiteObservations> {
        let mut observations: BTreeMap<String, SiteObservations> = BTreeMap::new();
        for ((owner, bank_id), bank) in &mut self.banks {
            if let Ok(Some(snapshot)) = io(owner, *bank_id, Operation::Snapshot) {
                bank.observe(&snapshot, now_ms);
                if snapshot.generation == 0
                    || (!bank.outputs.is_empty() && validate(&snapshot, bank.mask(), true).is_err())
                {
                    bank.fault(ReadingQuality::Unavailable);
                } else if let Some(operation) = bank.operation(&snapshot) {
                    let pending = Pending {
                        binding: matches!(operation, Operation::Bind(_)),
                        generation: snapshot.generation,
                        rejected: snapshot.rejected_commands,
                        mask: bank.target_mask(),
                        high: bank.high(false) & bank.target_mask(),
                        since_ms: now_ms,
                    };
                    if io(owner, *bank_id, operation).is_ok() {
                        bank.touched = true;
                        bank.pending = Some(pending);
                        bank.error = None;
                    } else {
                        bank.fault(ReadingQuality::Fault);
                    }
                }
            } else {
                bank.fault(ReadingQuality::Unavailable);
                for input in &mut bank.inputs {
                    input.reading.quality = ReadingQuality::Unavailable;
                }
            }
            for input in &bank.inputs {
                observations
                    .entry(input.node.clone())
                    .or_default()
                    .inputs
                    .insert(input.name.clone(), input.reading.clone());
            }
            for output in bank.outputs.iter().filter(|output| output.observed) {
                observations
                    .entry(output.node.clone())
                    .or_default()
                    .outputs
                    .insert(
                        output.name.clone(),
                        FanOutput {
                            available: output.status.quality == ReadingQuality::Valid,
                            pwm: false,
                        },
                    );
            }
        }
        observations
    }
    /// Replace latest logical demand; digital banks cannot provide fractional PWM.
    pub fn output(&mut self, node: &str, name: &str, demand: f64) -> Result<(), ReadingQuality> {
        if demand != 0.0 && demand != 1.0 {
            return Err(ReadingQuality::Fault);
        }
        let output = self
            .banks
            .values_mut()
            .flat_map(|bank| &mut bank.outputs)
            .find(|output| output.node == node && output.name == name)
            .ok_or(ReadingQuality::Unavailable)?;
        output.status.requested = demand;
        Ok(())
    }
    /// Requested and acknowledged submitted values; never independent electrical readback.
    pub fn output_status(&self, node: &str, name: &str) -> Option<OutputStatus> {
        self.banks
            .values()
            .flat_map(|bank| &bank.outputs)
            .find(|output| output.node == node && output.name == name)
            .map(|output| output.status.clone())
    }
}
fn radio(endpoint: &SiteEndpoint) -> bool {
    matches!(endpoint.backend, SiteBackend::Cm119 | SiteBackend::Parallel)
}
