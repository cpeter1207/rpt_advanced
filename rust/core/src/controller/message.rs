//! Bounded preparation handoff for recurring composed telemetry.

use super::{ControllerError, PreparedMedia};
use crate::audio::{MorseSwitch, PcmRead, PcmStreamReader};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Identity retained through preparation, receive interruption, and retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageAttemptId {
    /// Controller/media generation, retained when only its device adapter changes.
    pub generation: u64,
    /// Monotonic sequence shared by this generation's configured recipes.
    pub sequence: u64,
}

/// Scalar request published by recurring playback; no strings cross from audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageRequest {
    /// Exact attempt to complete, including failed preparation.
    pub attempt: MessageAttemptId,
    /// Immutable control-owned recipe index.
    pub content_index: usize,
}

/// Rejected preparation returned intact for control-side retry or destruction.
pub struct MessageCompletion {
    /// Requested attempt.
    pub attempt: MessageAttemptId,
    /// Caller-owned prepared media or its failure.
    pub result: Result<PreparedMedia, ControllerError>,
}

type Stream = Box<dyn PcmStreamReader>;
struct Ready {
    attempt: MessageAttemptId,
    result: Result<Stream, ControllerError>,
}

struct ControlPort {
    alive: Arc<AtomicBool>,
    requests: Consumer<MessageRequest>,
    ready: Producer<Ready>,
    retired: Consumer<Stream>,
    waiting: Option<MessageAttemptId>,
}

/// Control-owned ports; each configured recipe has at most one outstanding stream.
pub(crate) struct MessagePreparation {
    generation: u64,
    sequence: Arc<AtomicU64>,
    ports: Vec<ControlPort>,
    next: usize,
}

impl MessagePreparation {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            sequence: Arc::new(AtomicU64::new(0)),
            ports: Vec::new(),
            next: 0,
        }
    }

    /// Allocate only at configuration admission, before the audio owner starts.
    pub fn register(&mut self, content_index: usize) -> Result<PreparedMedia, ControllerError> {
        let (requests, receiver) = RingBuffer::new(1);
        let (ready, consumer) = RingBuffer::new(1);
        let (retired, reclaimer) = RingBuffer::new(1);
        let alive = Arc::new(AtomicBool::new(true));
        self.ports.push(ControlPort {
            alive: alive.clone(),
            requests: receiver,
            ready,
            retired: reclaimer,
            waiting: None,
        });
        PreparedMedia::new_stream(Box::new(MessageReader {
            alive,
            generation: self.generation,
            sequence: self.sequence.clone(),
            content_index,
            desired: None,
            outstanding: false,
            failed: false,
            stream: None,
            requests,
            ready: consumer,
            retired,
        }))
    }

    pub fn take_request(&mut self) -> Option<MessageRequest> {
        // One-shot reply readers are returned through the existing status queue
        // before being dropped on control. Their bounded ports retire here too.
        self.ports.retain(|port| port.alive.load(Ordering::Acquire));
        if self.next >= self.ports.len() {
            self.next = 0;
        }
        for port in &mut self.ports {
            while let Ok(stream) = port.retired.pop() {
                drop(stream);
            }
        }
        for _ in 0..self.ports.len() {
            let index = self.next;
            self.next = (self.next + 1) % self.ports.len();
            let port = &mut self.ports[index];
            if let Ok(request) = port.requests.pop() {
                port.waiting = Some(request.attempt);
                return Some(request);
            }
        }
        None
    }

    pub fn complete(
        &mut self,
        attempt: MessageAttemptId,
        result: Result<PreparedMedia, ControllerError>,
    ) -> Result<(), MessageCompletion> {
        let Some(port) = self
            .ports
            .iter_mut()
            .find(|port| port.waiting == Some(attempt))
        else {
            return Err(MessageCompletion { attempt, result });
        };
        // One request credit covers pending, playing, and returned ownership.
        // Only control converts the envelope or drops any of its other fields.
        if port.ready.is_full() {
            return Err(MessageCompletion { attempt, result });
        }
        let result = result.and_then(|mut media| media.0.take_stream().ok_or(ControllerError));
        let queued = port.ready.push(Ready { attempt, result });
        assert!(
            queued.is_ok(),
            "single preparation producer owns reserved capacity"
        );
        port.waiting = None;
        Ok(())
    }
}

/// Callback-safe facade preserving the controller's existing single-playback scheduler.
struct MessageReader {
    alive: Arc<AtomicBool>,
    generation: u64,
    sequence: Arc<AtomicU64>,
    content_index: usize,
    desired: Option<MessageAttemptId>,
    outstanding: bool,
    failed: bool,
    stream: Option<Stream>,
    requests: Producer<MessageRequest>,
    ready: Consumer<Ready>,
    retired: Producer<Stream>,
}

impl MessageReader {
    fn retire(&mut self, mut stream: Stream) {
        stream.cancel();
        // A new request is taken only after control reclaims the previous stream.
        let returned = self.retired.push(stream);
        assert!(
            returned.is_ok(),
            "one stream credit reserves its return slot"
        );
    }

    fn poll(&mut self) {
        if let Ok(ready) = self.ready.pop() {
            self.outstanding = false;
            match ready.result {
                Ok(mut stream) if self.desired == Some(ready.attempt) => {
                    stream.start();
                    self.stream = Some(stream);
                }
                Ok(stream) => self.retire(stream),
                Err(_) if self.desired == Some(ready.attempt) => self.failed = true,
                Err(_) => {}
            }
        }
        if let Some(attempt) = self
            .desired
            .filter(|_| !self.failed && !self.outstanding && self.stream.is_none())
        {
            if self
                .requests
                .push(MessageRequest {
                    attempt,
                    content_index: self.content_index,
                })
                .is_ok()
            {
                self.outstanding = true;
            }
        }
    }
}

impl PcmStreamReader for MessageReader {
    fn rewind(&mut self) {
        if let Some(stream) = &mut self.stream {
            stream.rewind();
        }
    }

    fn defer_before_output(&mut self) -> bool {
        self.cancel();
        true
    }

    fn start(&mut self) {
        self.cancel();
        self.failed = false;
        self.desired = self
            .sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .ok()
            .map(|sequence| MessageAttemptId {
                generation: self.generation,
                sequence,
            });
        self.failed = self.desired.is_none();
        self.poll();
    }

    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        self.poll();
        if self.failed {
            PcmRead::Failed
        } else {
            self.stream
                .as_mut()
                .map_or(PcmRead::Pending, |stream| stream.render(output))
        }
    }

    fn select_morse_fallback(&mut self) -> MorseSwitch {
        self.poll();
        if self.failed {
            MorseSwitch::Unavailable
        } else {
            self.stream.as_mut().map_or(MorseSwitch::Pending, |stream| {
                stream.select_morse_fallback()
            })
        }
    }

    fn cancel(&mut self) {
        self.desired = None;
        if let Some(stream) = self.stream.take() {
            self.retire(stream);
        }
        self.poll();
    }
}

impl Drop for MessageReader {
    fn drop(&mut self) {
        self.cancel();
        self.alive.store(false, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod tests;
