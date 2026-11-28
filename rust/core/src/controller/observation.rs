//! Bounded public telemetry lifecycle observations from the sole transmit owner.
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Report the first real PCM separately from queue admission and retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TelemetryPhase {
    /// An admitted occurrence is waiting for its render opportunity.
    Queued,
    /// The first nonempty PCM block was admitted to this audience.
    Started,
    /// The selected playback drained.
    Finished,
    /// A controller decision retired the playback.
    Cancelled,
    /// The playback source failed.
    Failed,
}

/// One public playback edge. Identifiers are opaque within the station generation.
#[derive(Clone, Copy, Debug)]
pub struct TelemetryObservation {
    /// Monotonic occurrence ID local to this station generation.
    pub id: u64,
    /// Actual transmission-owner sample position (48 kHz).
    pub sample_position: u64,
    /// Public playback class independent of message text.
    pub category: &'static str,
    /// Intended delivery audience, with no invocation details.
    pub audience: &'static str,
    /// Actual playback transition.
    pub phase: TelemetryPhase,
    /// Effective timing deadline in the same sample clock, when known.
    pub deadline_sample: Option<u64>,
    /// Bounded cause for exceptional retirement.
    pub reason: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub(super) struct PlaybackTicket {
    id: u64,
    category: &'static str,
    audience: &'static str,
    pub(super) deadline_sample: Option<u64>,
    started: bool,
}

pub(super) struct Trace {
    next: u64,
    events: Producer<TelemetryObservation>,
    lost: Arc<AtomicBool>,
}

pub(super) struct Observations {
    events: Consumer<TelemetryObservation>,
    lost: Arc<AtomicBool>,
}

pub(super) fn channel() -> (Trace, Observations) {
    let (producer, consumer) = RingBuffer::new(128);
    let lost = Arc::new(AtomicBool::new(false));
    (
        Trace {
            next: 0,
            events: producer,
            lost: lost.clone(),
        },
        Observations {
            events: consumer,
            lost,
        },
    )
}

impl Observations {
    pub(super) fn pop(&mut self) -> Option<TelemetryObservation> {
        self.events.pop().ok()
    }

    pub(super) fn lost(&self) -> bool {
        self.lost.swap(false, Ordering::AcqRel)
    }
}

impl Trace {
    pub(super) fn queue(
        &mut self,
        sample: u64,
        category: &'static str,
        audience: &'static str,
        deadline_sample: Option<u64>,
    ) -> PlaybackTicket {
        self.next = self.next.saturating_add(1);
        let ticket = PlaybackTicket {
            id: self.next,
            category,
            audience,
            deadline_sample,
            started: false,
        };
        self.record(ticket, sample, TelemetryPhase::Queued, None);
        ticket
    }

    pub(super) fn started(&mut self, ticket: &mut PlaybackTicket, sample: u64) {
        if !ticket.started {
            ticket.started = true;
            self.record(*ticket, sample, TelemetryPhase::Started, None);
        }
    }

    pub(super) fn record(
        &mut self,
        ticket: PlaybackTicket,
        sample: u64,
        phase: TelemetryPhase,
        reason: Option<&'static str>,
    ) {
        if self
            .events
            .push(TelemetryObservation {
                id: ticket.id,
                sample_position: sample,
                category: ticket.category,
                audience: ticket.audience,
                phase,
                deadline_sample: ticket.deadline_sample,
                reason,
            })
            .is_err()
        {
            self.lost.store(true, Ordering::Release);
        }
    }
}
