//! Bounded radio-to-control digit handoff and existing linking command policy.

use crate::{
    audio::{DtmfDetector, DtmfError},
    command::{Command, CommandCollector, DtmfCommandMap, LinkAction},
    dtmf_macro::{CompiledDtmfMacro, DtmfMacroLimits, MacroStep},
    remote_base_command::{RemoteCommand, RemoteDtmfAccess},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// One copied radio event; no borrowed callback memory crosses to control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigitEvent {
    /// Hardware-timestamped actual digit or NUL timeout tick.
    Digit {
        /// Decoded symbol or timeout marker.
        digit: char,
        /// Captured monotonic timestamp, never dispatcher execution time.
        now_ms: u64,
    },
    /// Actual receive falling edge, distinct from a literal hash digit.
    Unkey {
        /// Captured monotonic timestamp.
        now_ms: u64,
    },
    /// A queue/submission loss invalidates the partial command before further digits.
    Lost,
}

#[derive(Clone, Copy)]
struct StampedDigit {
    event: Option<DigitEvent>,
    cut: u64,
}

/// Copied RF input retains its exact detector/dispatcher pair and onset cut.
/// A graph-only adapter replacement transfers that pair without changing custody.
pub struct RadioDigit {
    stamped: StampedDigit,
    source: Arc<AtomicU64>,
}
impl RadioDigit {
    pub(super) fn event(&self) -> Option<DigitEvent> {
        self.stamped.event
    }
}

struct DtmfPublisher {
    writer: Producer<StampedDigit>,
    hold: Arc<AtomicU64>,
    cut: u64,
    #[cfg(test)]
    release_on_publish: bool,
    drops: Arc<AtomicU64>,
    last_ms: u64,
    timeout: bool,
    receiving: bool,
}
impl DtmfPublisher {
    fn new(generation: u64) -> (Self, DtmfDispatcher) {
        let (writer, reader) = RingBuffer::new(256);
        let drops = Arc::new(AtomicU64::new(0));
        let hold = Arc::new(AtomicU64::new(0));
        (
            Self {
                writer,
                hold: Arc::clone(&hold),
                cut: 0,
                #[cfg(test)]
                release_on_publish: false,
                drops: drops.clone(),
                last_ms: 0,
                timeout: false,
                receiving: false,
            },
            DtmfDispatcher {
                reader,
                hold,
                drops,
                reported: 0,
                generation,
            },
        )
    }
    fn begin_tone(&mut self) {
        // Reserve the final value as permanent fail-closed exhaustion. It can
        // never alias an earlier releasable onset, even at the counter limit.
        self.cut = self
            .cut
            .checked_add(2)
            .filter(|cut| *cut < u64::MAX - 1)
            .unwrap_or(u64::MAX);
        self.hold.store(self.cut | 1, Ordering::Release);
    }
    fn queue(&mut self, digit: char, now_ms: u64) {
        self.publish(DigitEvent::Digit { digit, now_ms });
    }
    fn publish(&mut self, event: DigitEvent) {
        self.publish_stamped(Some(event));
    }
    fn publish_stamped(&mut self, event: Option<DigitEvent>) {
        if self
            .writer
            .push(StampedDigit {
                event,
                cut: self.cut,
            })
            .is_err()
        {
            self.drops.fetch_add(1, Ordering::Release);
        }
        #[cfg(test)]
        if self.release_on_publish {
            // Deterministic control release between event publication and the
            // producer's final PCM decision, using the same exact compare.
            let _ = self.hold.compare_exchange(
                self.cut | 1,
                self.cut,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
    fn decoded(&mut self, digit: char, now_ms: u64) {
        self.queue(digit, now_ms);
        self.last_ms = now_ms;
        self.timeout = true;
    }
    fn finish_frame(&mut self, receiving: bool, now_ms: u64, emitted: bool) {
        if !emitted && self.timeout && now_ms.saturating_sub(self.last_ms) >= 3000 {
            self.queue('\0', now_ms);
            self.timeout = false;
        }
        if self.receiving && !receiving {
            self.publish(DigitEvent::Unkey { now_ms });
            self.timeout = false;
        }
        self.receiving = receiving;
    }
}

/// Receive-private detector and bounded SPSC producer. Construct before audio starts.
pub struct DtmfWorker {
    detector: DtmfDetector,
    publisher: DtmfPublisher,
    qualified: bool,
    muting: bool,
}
impl DtmfWorker {
    /// Allocate the fixed 256-event queue and return its separate control consumer.
    pub fn new(generation: u64, muting: bool) -> Result<(Self, DtmfDispatcher), DtmfError> {
        let (publisher, dispatcher) = DtmfPublisher::new(generation);
        Ok((
            Self {
                detector: DtmfDetector::new(muting)?,
                publisher,
                qualified: false,
                muting,
            },
            dispatcher,
        ))
    }
    /// Decode every digit completion and publish without allocation, submission, or locks.
    pub fn process(&mut self, receiving: bool, audio: &mut [f32], now_ms: u64) {
        let Self {
            detector,
            publisher,
            qualified,
            muting,
        } = self;
        let mut mute_block = *muting && publisher.hold.load(Ordering::Acquire) & 1 != 0;
        let mut emitted = false;
        detector.process(receiving, audio, |digit| {
            // Completion owns a fresh decision cut: an earlier discard/release
            // cannot clear it, including a complete tone within one callback.
            if *muting {
                publisher.begin_tone();
                mute_block = true;
            }
            publisher.decoded(digit, now_ms);
            *qualified = false;
            emitted = true;
        });
        let active = detector.suppressing();
        if active && !*qualified {
            publisher.begin_tone();
            mute_block = true;
        } else if !active && *qualified {
            // A short qualified candidate may be rejected without a completed
            // digit. Settle its exact hold without inventing parser input.
            publisher.begin_tone();
            publisher.publish_stamped(None);
            mute_block = true;
        }
        *qualified = active;
        publisher.finish_frame(receiving, now_ms, emitted);
        if mute_block || (*muting && publisher.hold.load(Ordering::Acquire) & 1 != 0) {
            // Keep this local fact even if control already released the event.
            // No command-interval PCM may later emerge from the delayed ring.
            audio.fill(0.0);
        }
    }

    /// Current receive-side muting state for the delayed PCM consumer.
    #[must_use]
    pub fn suppressing(&self) -> bool {
        self.detector.suppressing()
            || (self.muting && self.publisher.hold.load(Ordering::Acquire) & 1 != 0)
    }
}

/// Non-audio consumer of one generation's fixed SPSC endpoints. It invalidates queued
/// prefixes after overflow; no other generation can publish into this queue.
pub struct DtmfDispatcher {
    reader: Consumer<StampedDigit>,
    hold: Arc<AtomicU64>,
    drops: Arc<AtomicU64>,
    reported: u64,
    generation: u64,
}
impl DtmfDispatcher {
    /// Discard the observed prefix after control admission was suppressed or lost.
    pub fn discard(&mut self) {
        let cut = self.discard_prefix();
        self.release_cut(cut);
        self.reported = self.drops.load(Ordering::Acquire);
    }
    fn discard_prefix(&mut self) -> u64 {
        let mut cut = self.hold.load(Ordering::Acquire) & !1;
        let queued = self.reader.slots();
        for _ in 0..queued {
            if let Ok(event) = self.reader.pop() {
                cut = cut.max(event.cut);
            }
        }
        cut
    }
    fn release_cut(&self, cut: u64) {
        if cut != 0 && cut < u64::MAX - 1 {
            let _ = self
                .hold
                .compare_exchange(cut | 1, cut, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    pub(super) fn matches(&self, event: &RadioDigit) -> bool {
        Arc::ptr_eq(&self.hold, &event.source)
    }
    pub(super) fn release(&self, event: &RadioDigit) {
        if self.matches(event) {
            self.release_cut(event.stamped.cut);
        }
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    fn next_stamped(&mut self, current_generation: u64) -> Option<StampedDigit> {
        let dropped = self.drops.load(Ordering::Acquire);
        if self.generation != current_generation {
            self.discard();
            return None;
        }
        if dropped != self.reported {
            let cut = self.discard_prefix();
            self.reported = dropped;
            return Some(StampedDigit {
                event: Some(DigitEvent::Lost),
                cut,
            });
        }
        self.reader.pop().ok()
    }
    /// Low-level tone-only consumption releases each popped exact cut. Product
    /// command processing uses receipt-aware delivery through Runtime instead.
    pub fn next(&mut self, current_generation: u64) -> Option<DigitEvent> {
        for _ in 0..self.reader.slots().saturating_add(1) {
            let stamped = self.next_stamped(current_generation)?;
            self.release_cut(stamped.cut);
            if let Some(event) = stamped.event {
                return Some(event);
            }
        }
        None
    }
    pub(super) fn next_radio(&mut self, current_generation: u64) -> Option<RadioDigit> {
        self.next_stamped(current_generation)
            .map(|stamped| RadioDigit {
                stamped,
                source: Arc::clone(&self.hold),
            })
    }
    /// Cumulative radio-event drops; read outside audio for diagnostics.
    pub fn dropped(&self) -> u64 {
        self.drops.load(Ordering::Acquire)
    }
    /// Current bounded queue occupancy.
    pub fn queued(&self) -> usize {
        self.reader.slots()
    }
}

/// Existing linking operation, optionally forwarding one digit to a selected direct peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigitOperation {
    /// Typed linking command and resolved destination (`0` means the last named peer).
    pub command: Command,
    /// Remote-command symbol, absent for ordinary link operations.
    pub digit: Option<char>,
    /// Typed rig payload; never stored in the link-destination field.
    pub remote: Option<RemoteCommand>,
    /// Exact originating peer; absent for the existing local/CLI path.
    pub reply: Option<crate::link::PeerReply>,
    /// Claimed source label for relayed commands; never an authorization identity.
    pub origin: Option<String>,
    pub(crate) admin_authorized: bool,
    pub(super) macro_permit: Option<MacroPermit>,
    // Custody only: keep the original peer collector discoverable while copied
    // remote work is owned. Dispatch still checks its exact source and live grant.
    source_custody: Option<Arc<()>>,
}

/// Shared cancellation follows copied operations into ordinary dispatch queues.
#[derive(Clone, Debug)]
pub(super) struct MacroPermit {
    active: Arc<AtomicBool>,
    protected: bool,
    deadline_ms: u64,
}
impl PartialEq for MacroPermit {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.active, &other.active)
    }
}
impl Eq for MacroPermit {}
impl MacroPermit {
    pub(super) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub(super) fn current_at(&self, now_ms: u64) -> bool {
        self.active() && now_ms <= self.deadline_ms
    }
}

struct ActiveDtmfMacro {
    definition: usize,
    next: usize,
    due_ms: u64,
}

/// Bounded local keypad capture; only actual receiver unkey completes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadTestResult {
    /// First 127 ASCII keypad symbols.
    pub digits: String,
    /// Additional symbols were discarded.
    pub overflowed: bool,
}

/// One RF telephone symbol bound to an opaque admitted call, never a node alias.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TelephoneDigit {
    call: u64,
    symbol: char,
}
impl TelephoneDigit {
    /// Exact call intent retained when this actual RF symbol was interpreted.
    pub fn call(&self) -> u64 {
        self.call
    }
    /// Completed telephone keypad symbol; never include it in diagnostics.
    pub fn symbol(&self) -> char {
        self.symbol
    }
}
impl std::fmt::Debug for TelephoneDigit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TelephoneDigit")
    }
}

/// Control-side interpretation; media and directory work stay outside collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DigitOutcome {
    /// Actual local RF input for one still-current outgoing telephone call.
    Telephone(TelephoneDigit),
    /// Source-only failure notice; expansion and credential digits are never included.
    MacroStopped {
        /// Exact originating connection, absent only for the local RF source.
        reply: Option<crate::link::PeerReply>,
    },
    /// Complete operation or one digit for a selected remote.
    Operation(DigitOperation),
    /// Incomplete destination to select only if the directory recognizes it exactly.
    RemoteCandidate(DigitOperation),
    /// Local keypad report after actual unkey.
    PadTest(PadTestResult),
    /// Idle addressed peer digit eligible for configured local-TX regeneration.
    Regenerate(char),
}

impl DigitOperation {
    /// Construct a non-administrative typed operation for trusted control interfaces.
    pub fn new(command: Command, digit: Option<char>) -> Self {
        Self {
            command,
            digit,
            remote: None,
            reply: None,
            origin: None,
            admin_authorized: false,
            macro_permit: None,
            source_custody: None,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum TelephoneInput {
    #[default]
    Idle,
    Star,
    Quoted,
    CommandTerminator,
}

/// Serialized command collection; authorization/peer availability stays with link control.
pub struct DtmfCommands {
    map: DtmfCommandMap,
    collector: CommandCollector,
    last_node: String,
    remote: String,
    peer_source: bool,
    pad: Option<PadTestResult>,
    digit_deadline: Option<u64>,
    inter_digit_timeout_ms: u64,
    admin_unlock_hash: String,
    admin_lock_hash: String,
    admin_timeout_ms: u64,
    admin_until_ms: u64,
    remote_access: RemoteDtmfAccess,
    macros: Arc<Vec<CompiledDtmfMacro>>,
    macro_limits: DtmfMacroLimits,
    active_macro: Option<ActiveDtmfMacro>,
    macro_permit: Option<MacroPermit>,
    macro_stopped: bool,
    pending_remote: Weak<()>,
    telephone_call: Option<u64>,
    telephone_input: TelephoneInput,
}
impl DtmfCommands {
    /// Create independent collection and authorization state with the same resolved policy.
    pub(super) fn for_source(&self) -> Self {
        let mut commands = Self::new(self.map.clone());
        commands.peer_source = true;
        commands.remote_access = self.remote_access;
        commands.configure_admin(
            &self.admin_unlock_hash,
            &self.admin_lock_hash,
            self.admin_timeout_ms,
        );
        commands.macros = self.macros.clone();
        commands.macro_limits = self.macro_limits;
        commands
    }
    /// Start with the resolved current command map.
    pub fn new(map: DtmfCommandMap) -> Self {
        let collector = map.collector();
        Self {
            map,
            collector,
            last_node: String::new(),
            remote: String::new(),
            peer_source: false,
            pad: None,
            digit_deadline: None,
            inter_digit_timeout_ms: 3000,
            admin_unlock_hash: String::new(),
            admin_lock_hash: String::new(),
            admin_timeout_ms: 300_000,
            admin_until_ms: 0,
            remote_access: RemoteDtmfAccess::Disabled,
            macros: Arc::new(Vec::new()),
            macro_limits: DtmfMacroLimits::default(),
            active_macro: None,
            macro_permit: None,
            macro_stopped: false,
            pending_remote: Weak::new(),
            telephone_call: None,
            telephone_input: TelephoneInput::Idle,
        }
    }
    /// Apply only to the independently owned source collector, never the radio publisher.
    pub(super) fn set_inter_digit_timeout_ms(&mut self, timeout_ms: u64) {
        self.inter_digit_timeout_ms = timeout_ms;
    }

    /// Borrow the final remapped command namespace during candidate preparation.
    pub(crate) fn command_map(&self) -> &DtmfCommandMap {
        &self.map
    }

    /// Publish precompiled direct triggers on control; old queued invocations are revoked.
    pub fn configure_macros(
        &mut self,
        macros: Vec<CompiledDtmfMacro>,
        limits: DtmfMacroLimits,
    ) -> Result<(), crate::command::CommandMapError> {
        let triggers = macros
            .iter()
            .map(|definition| {
                definition
                    .trigger
                    .strip_prefix('*')
                    .map(str::to_owned)
                    .ok_or(crate::command::CommandMapError::InvalidPrefix)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let map = self.map.with_macro_triggers(triggers)?;
        self.cancel_macro();
        self.map = map;
        self.collector = self.map.collector();
        self.telephone_input = TelephoneInput::Idle;
        self.macros = Arc::new(macros);
        self.macro_limits = limits;
        Ok(())
    }

    /// Explicitly cancel this source's expansion and every queued copy of its work.
    pub fn cancel_macro(&mut self) {
        self.active_macro = None;
        if let Some(permit) = self.macro_permit.take() {
            permit.active.store(false, Ordering::Release);
        }
    }
    pub(super) fn take_macro_stopped(&mut self) -> Option<DigitOutcome> {
        std::mem::take(&mut self.macro_stopped)
            .then_some(DigitOutcome::MacroStopped { reply: None })
    }

    /// Recheck cancellation, elapsed bounds and the exact live source grant at dispatch.
    pub(crate) fn macro_operation_current(
        &mut self,
        operation: &DigitOperation,
        now_ms: u64,
    ) -> bool {
        operation.macro_permit.as_ref().is_none_or(|permit| {
            self.macro_permit
                .as_ref()
                .is_some_and(|current| current == permit)
                && permit.current_at(now_ms)
                && (!permit.protected || self.admin_unlocked(now_ms))
        })
    }

    fn admin_unlocked(&mut self, now_ms: u64) -> bool {
        if now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        self.admin_until_ms != 0
    }

    /// Advance at most one complete command without sleeping the serialized control owner.
    pub fn advance_macro(&mut self, now_ms: u64) -> Option<DigitOperation> {
        let permit = self.macro_permit.clone()?;
        if !permit.current_at(now_ms) {
            self.macro_stopped = self.active_macro.is_some();
            self.cancel_macro();
            return None;
        }
        loop {
            let active = self.active_macro.as_mut()?;
            if now_ms < active.due_ms {
                return None;
            }
            let definition = &self.macros[active.definition];
            let protected = definition.protected;
            let Some(step) = definition.steps.get(active.next).cloned() else {
                self.active_macro = None;
                return None;
            };
            active.next += 1;
            match step {
                MacroStep::SelectTarget(_) => continue, // Compiler permits only this node.
                MacroStep::Pause(duration) => {
                    active.due_ms = active.due_ms.saturating_add(duration);
                    continue;
                }
                MacroStep::Command(text) => {
                    active.due_ms = now_ms;
                    if protected && !self.admin_unlocked(now_ms) {
                        self.macro_stopped = true;
                        self.cancel_macro();
                        return None;
                    }
                    if self.map.mappings().iter().any(|mapping| {
                        mapping.action == LinkAction::AdminLock && mapping.digits == text
                    }) {
                        self.admin_until_ms = 0;
                        return None;
                    }
                    let Some(mut operation) = self.complete_command(&text, now_ms, false) else {
                        self.macro_stopped = true;
                        self.cancel_macro();
                        return None;
                    };
                    operation.macro_permit = Some(permit);
                    return Some(operation);
                }
            }
        }
    }
    /// Atomically install the enabled rig's family and mutation policy. An empty
    /// prefix removes the family; invalid mappings leave the current collector intact.
    pub fn configure_remote(
        &mut self,
        prefix: &str,
        access: RemoteDtmfAccess,
    ) -> Result<(), crate::command::CommandMapError> {
        let mut mappings = self.map.mappings().to_vec();
        if let Some(mapping) = mappings
            .iter_mut()
            .find(|m| m.action == LinkAction::RemoteBase)
        {
            mapping.digits = prefix.into();
        } else {
            mappings.push(crate::command::CommandMapping::new(
                prefix,
                LinkAction::RemoteBase,
            ));
        }
        let map = DtmfCommandMap::new(mappings)?.with_macro_triggers(
            self.macros
                .iter()
                .map(|definition| definition.trigger[1..].into())
                .collect(),
        )?;
        self.cancel_macro();
        self.collector = map.collector();
        self.telephone_input = TelephoneInput::Idle;
        self.map = map;
        self.remote_access = access;
        Ok(())
    }
    /// Install hashed DTMF credentials on control; administration stays unavailable unless both
    /// credential digests are valid Argon2id PHC strings.
    pub fn configure_admin(&mut self, unlock_hash: &str, lock_hash: &str, timeout_ms: u64) {
        self.cancel_macro();
        self.admin_unlock_hash.clear();
        self.admin_lock_hash.clear();
        if valid_hash(unlock_hash) && valid_hash(lock_hash) && timeout_ms > 0 {
            self.admin_unlock_hash.push_str(unlock_hash);
            self.admin_lock_hash.push_str(lock_hash);
            self.admin_timeout_ms = timeout_ms;
        }
        self.admin_until_ms = 0;
    }
    /// Select an independently authorized direct peer after the link owner validates it.
    pub fn select_remote(&mut self, remote: &str) {
        self.remote = remote.into();
        self.collector = self.map.collector();
        self.telephone_input = TelephoneInput::Idle;
    }
    /// Clear selection when its peer disconnects or a scheduled route is withdrawn.
    pub fn disconnect(&mut self, remote: &str) {
        if self.remote == remote {
            self.remote.clear();
        }
    }
    /// Interpret actual RF input while retaining the existing command collector.
    pub fn feed_phone(&mut self, event: DigitEvent, call: Option<u64>) -> Option<DigitOutcome> {
        let call = call.filter(|id| *id != 0 && !self.peer_source);
        if self.telephone_call != call {
            if matches!(
                self.telephone_input,
                TelephoneInput::Star | TelephoneInput::Quoted
            ) {
                self.feed(DigitEvent::Lost);
            }
            self.telephone_call = call;
            self.telephone_input = TelephoneInput::Idle;
        }
        let Some(call) = call else {
            return self.feed(event);
        };
        let DigitEvent::Digit { digit, now_ms } = event else {
            return self.feed(event);
        };
        if !"0123456789ABCD*#".contains(digit) {
            self.telephone_input = TelephoneInput::Idle;
            return self.feed(event);
        }
        if self.telephone_input != TelephoneInput::Idle
            && self
                .digit_deadline
                .is_some_and(|deadline| now_ms >= deadline)
        {
            self.feed(DigitEvent::Digit {
                digit: '\0',
                now_ms,
            });
        }
        match self.telephone_input {
            TelephoneInput::Quoted => {
                self.telephone_input = TelephoneInput::Idle;
                self.observe_input(event);
                return Some(DigitOutcome::Telephone(TelephoneDigit {
                    call,
                    symbol: digit,
                }));
            }
            TelephoneInput::Star => {
                self.telephone_input = TelephoneInput::Idle;
                if digit == '*' {
                    // Only the pending one-star prefix is replaced. Existing
                    // selected-remote and keypad modes never enter this state.
                    self.feed(DigitEvent::Lost);
                    self.observe_input(event);
                    self.telephone_input = TelephoneInput::Quoted;
                    return None;
                }
                return self.feed_phone_command(event);
            }
            TelephoneInput::CommandTerminator => {
                self.telephone_input = TelephoneInput::Idle;
                if digit == '#' {
                    return self.feed(event);
                }
            }
            TelephoneInput::Idle => {}
        }
        if !self.collector.active() && self.remote.is_empty() && self.pad.is_none() {
            if digit == '*' {
                self.telephone_input = TelephoneInput::Star;
            } else {
                self.observe_input(event);
                return Some(DigitOutcome::Telephone(TelephoneDigit {
                    call,
                    symbol: digit,
                }));
            }
        }
        self.feed_phone_command(event)
    }
    fn feed_phone_command(&mut self, event: DigitEvent) -> Option<DigitOutcome> {
        let collecting = self.collector.active();
        let outcome = self.feed(event);
        if collecting
            && !self.collector.active()
            && matches!(event, DigitEvent::Digit { digit, .. } if digit != '#' && digit != '\0')
        {
            // Some mapped commands complete before their optional hash. That
            // terminator belongs to the command, not to the telephone IVR.
            self.telephone_input = TelephoneInput::CommandTerminator;
        }
        outcome
    }
    /// Interpret copied events on control. Call `Lost` also after control submission rejection.
    fn observe_input(&mut self, event: DigitEvent) {
        if matches!(event, DigitEvent::Lost | DigitEvent::Digit { digit: '0'..='9' | 'A'..='D' | '*' | '#', .. })
        {
            self.cancel_macro();
            self.macro_stopped = false;
        }
        self.digit_deadline = match event {
            DigitEvent::Digit { digit, now_ms } if "0123456789ABCD*".contains(digit) => {
                Some(now_ms.saturating_add(self.inter_digit_timeout_ms))
            }
            _ => None,
        };
    }
    /// Interpret copied events with the unchanged ordinary RF/peer grammar.
    pub fn feed(&mut self, event: DigitEvent) -> Option<DigitOutcome> {
        self.observe_input(event);
        if matches!(
            event,
            DigitEvent::Lost | DigitEvent::Unkey { .. } | DigitEvent::Digit { digit: '\0', .. }
        ) {
            self.telephone_input = TelephoneInput::Idle;
        }
        if event == DigitEvent::Lost {
            self.collector = self.map.collector();
            self.telephone_input = TelephoneInput::Idle;
            self.pad = None;
            return None;
        }
        if let Some(pad) = self.pad.as_mut() {
            match event {
                DigitEvent::Unkey { .. } => return self.pad.take().map(DigitOutcome::PadTest),
                DigitEvent::Digit { digit, .. } if "0123456789ABCD*#".contains(digit) => {
                    if pad.digits.len() < 127 {
                        pad.digits.push(digit);
                    } else {
                        pad.overflowed = true;
                    }
                }
                _ => {}
            }
            return None;
        }
        // Ordinary commands still terminate on unkey; selected remote mode does not.
        let (digit, now_ms) = match event {
            DigitEvent::Digit { digit, now_ms } => (digit, now_ms),
            DigitEvent::Unkey { now_ms } if self.remote.is_empty() => ('#', now_ms),
            _ => return None,
        };
        if !self.remote.is_empty() {
            if digit == '#' {
                self.remote.clear();
                return None;
            }
            return "0123456789ABCD*".contains(digit).then(|| {
                DigitOutcome::Operation(DigitOperation {
                    command: Command {
                        action: LinkAction::Command,
                        node: self.remote.clone(),
                    },
                    digit: Some(digit),
                    remote: None,
                    reply: None,
                    origin: None,
                    admin_authorized: false,
                    macro_permit: None,
                    source_custody: None,
                })
            });
        }
        if now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        if self.peer_source
            && !self.collector.active()
            && digit != '*'
            && "0123456789ABCD#".contains(digit)
            && matches!(event, DigitEvent::Digit { .. })
        {
            return Some(DigitOutcome::Regenerate(digit));
        }
        let Some(text) =
            self.collector
                .feed_with_timeout(digit, now_ms, self.inter_digit_timeout_ms)
        else {
            return self
                .collector
                .remote_candidate()
                .filter(|node| node.len() < 64)
                .map(|node| {
                    DigitOutcome::RemoteCandidate(DigitOperation::new(
                        Command {
                            action: LinkAction::Command,
                            node,
                        },
                        None,
                    ))
                });
        };
        if let Some(index) = self
            .macros
            .iter()
            .position(|definition| definition.trigger[1..] == text)
        {
            if !self.macros[index].protected || self.admin_unlocked(now_ms) {
                self.active_macro = Some(ActiveDtmfMacro {
                    definition: index,
                    next: 0,
                    due_ms: now_ms,
                });
                self.macro_permit = Some(MacroPermit {
                    active: Arc::new(AtomicBool::new(true)),
                    protected: self.macros[index].protected,
                    deadline_ms: now_ms.saturating_add(self.macro_limits.max_duration_ms),
                });
            } else {
                return Some(DigitOutcome::MacroStopped { reply: None });
            }
            return None;
        }
        self.complete_command(&text, now_ms, true)
            .map(DigitOutcome::Operation)
    }

    /// Shared typed preparation; expansions cannot enter credential or interactive branches.
    fn complete_command(&mut self, text: &str, now_ms: u64, human: bool) -> Option<DigitOperation> {
        self.admin_unlocked(now_ms);
        if let Some(remote) = self.map.parse_remote(text) {
            let authorized = self.admin_until_ms != 0;
            if !self.remote_authorized(remote.administrative(), authorized, now_ms) {
                return None;
            }
            let mut operation = DigitOperation::new(
                Command {
                    action: LinkAction::RemoteBase,
                    node: String::new(),
                },
                None,
            );
            operation.remote = Some(remote);
            operation.admin_authorized = authorized;
            if self.peer_source {
                operation.source_custody =
                    Some(self.pending_remote.upgrade().unwrap_or_else(|| {
                        let custody = Arc::new(());
                        self.pending_remote = Arc::downgrade(&custody);
                        custody
                    }));
            }
            return Some(operation);
        }
        let mut command = self.map.parse(text)?;
        if matches!(command.action, LinkAction::Message | LinkAction::SiteStatus) {
            // The command map already requires a nonempty argument for these actions.
            return crate::config::message_selector_valid(&command.node)
                .then(|| DigitOperation::new(command, None));
        }
        let mut admin_authorized = false;
        if command.node.len() >= 64 {
            return None;
        }
        match command.action {
            LinkAction::PadTest => {
                if human && !self.peer_source {
                    self.pad = Some(PadTestResult {
                        digits: String::with_capacity(127),
                        overflowed: false,
                    });
                }
                return None;
            }
            LinkAction::AdminUnlock => {
                if !human {
                    return None;
                }
                self.admin_until_ms = if self.credentials_configured()
                    && verify_hash(&self.admin_unlock_hash, &command.node)
                {
                    now_ms.saturating_add(self.admin_timeout_ms)
                } else {
                    0
                };
                return None;
            }
            LinkAction::AdminLock => {
                if !human {
                    return None;
                }
                if self.credentials_configured()
                    && verify_hash(&self.admin_lock_hash, &command.node)
                {
                    self.admin_until_ms = 0;
                }
                return None;
            }
            LinkAction::ParrotEnable
            | LinkAction::ParrotDisable
            | LinkAction::EchoTelemetry
            | LinkAction::PhoneBypass => {
                if self.admin_until_ms == 0 {
                    return None;
                }
                admin_authorized = true;
            }
            _ => {}
        }
        if matches!(
            command.action,
            LinkAction::PhoneDial | LinkAction::PhoneHangup
        ) {
            // Telephone digits are literal, never an ordinary last-node alias.
            return Some(DigitOperation::new(command, None));
        }
        if command.node == "0" {
            if self.last_node.is_empty() {
                return None;
            }
            command.node = self.last_node.clone();
        }
        if human && !command.node.is_empty() {
            self.last_node.clone_from(&command.node);
        }
        Some(DigitOperation {
            command,
            digit: None,
            remote: None,
            reply: None,
            origin: None,
            admin_authorized,
            macro_permit: None,
            source_custody: None,
        })
    }

    pub(super) fn holds_radio_audio(&self) -> bool {
        self.collector.active()
            || (!self.remote.is_empty() && self.digit_deadline.is_some())
            || self.pad.is_some()
            || self.telephone_input == TelephoneInput::Quoted
    }

    /// Inspect collection without clearing selection, macros or source grants.
    /// Peer collectors cannot enter the local-RF-only keypad test.
    pub(super) fn collecting(&self) -> bool {
        self.collector.active()
    }

    /// Preserve owned remote work, commands, selected/remembered destinations and live grants.
    pub(super) fn retains_state(&self, now_ms: u64) -> bool {
        self.collector.active()
            || self.pending_remote.strong_count() != 0
            || self.active_macro.is_some()
            || self
                .macro_permit
                .as_ref()
                .is_some_and(|permit| Arc::strong_count(&permit.active) > 1)
            || !self.remote.is_empty()
            || !self.last_node.is_empty()
            || self.pad.is_some()
            || now_ms < self.admin_until_ms
    }
    pub(super) fn remote_collecting(&self) -> bool {
        self.collector.active() && self.collector.remote_family()
    }
    /// Outer phone quote/prefix keys count as activity without entering node syntax.
    /// Do not create collection or revive input already due for the normal sweep.
    pub(super) fn phone_input_activity(&mut self, now_ms: u64) {
        if !self.collecting() {
            return;
        }
        if let Some(deadline) = self
            .digit_deadline
            .as_mut()
            .filter(|deadline| now_ms < **deadline)
        {
            *deadline = (*deadline).max(now_ms.saturating_add(self.inter_digit_timeout_ms));
        }
    }

    /// Expire only this origin's pending command, without ending remote selection.
    pub(super) fn expire(&mut self, now_ms: u64) -> Option<DigitOutcome> {
        if self
            .digit_deadline
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.feed(DigitEvent::Digit {
                digit: '\0',
                now_ms,
            })
        } else {
            None
        }
    }

    /// Check the one DTMF grant before an administrative action is applied.
    pub(crate) fn consume_administrative_authorization(
        &mut self,
        authorized: bool,
        now_ms: u64,
    ) -> bool {
        let authorized = authorized && self.admin_until_ms != 0 && now_ms < self.admin_until_ms;
        if !authorized && now_ms >= self.admin_until_ms {
            self.admin_until_ms = 0;
        }
        authorized
    }

    /// Recheck a queued rig request against its exact source's current grant.
    pub(crate) fn remote_authorized(
        &mut self,
        administrative: bool,
        authorized: bool,
        now_ms: u64,
    ) -> bool {
        !administrative
            || match self.remote_access {
                RemoteDtmfAccess::Open => true,
                RemoteDtmfAccess::Locked => {
                    self.consume_administrative_authorization(authorized, now_ms)
                }
                RemoteDtmfAccess::Disabled => false,
            }
    }

    /// Renew the source grant only after a successful administrative completion.
    pub(crate) fn confirm_administrative_action(&mut self, now_ms: u64) {
        if self.admin_until_ms != 0 && now_ms < self.admin_until_ms {
            self.admin_until_ms = now_ms.saturating_add(self.admin_timeout_ms);
        }
    }

    fn credentials_configured(&self) -> bool {
        !self.admin_unlock_hash.is_empty() && !self.admin_lock_hash.is_empty()
    }
}

impl Drop for DtmfCommands {
    fn drop(&mut self) {
        self.cancel_macro();
    }
}

fn valid_hash(value: &str) -> bool {
    use argon2::password_hash::PasswordHash;
    PasswordHash::new(value).is_ok_and(|hash| hash.algorithm.as_str() == "argon2id")
}

fn verify_hash(hash: &str, code: &str) -> bool {
    use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHash};
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(code.as_bytes(), &parsed)
            .is_ok()
    })
}

#[cfg(test)]
#[path = "dtmf_tests.rs"]
mod tests;
