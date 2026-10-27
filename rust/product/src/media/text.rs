//! Bounded, origin-ordered local speech production, independent of link forwarding.

use super::{
    NativeMediaPreparer,
    finite::{SegmentConsumer, SegmentProgress},
};
use crate::link::ring::{InboundObserver, InboundPolicy, InboundRing};
use crossbeam_queue::ArrayQueue;
use rpt_advanced_core::{
    link::HumanMessage,
    link::TextOrigin,
    media::{Cancellation, MediaError, SpeechRequest, SpeechSource},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Local rendition admission; forwarding has its own independent admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Reserved one bounded local job.
    Accepted,
    /// More than 4096 UTF-8 bytes; do not truncate.
    TooLarge,
    /// Four jobs are already admitted from this origin.
    OriginFull,
    /// Sixty-four jobs are already admitted at this station.
    NodeFull,
    /// Origin/generation is retired or the executor has stopped.
    Stopping,
}

struct Origin {
    statistics: Arc<Statistics>,
    identity: Arc<TextOrigin>,
    node_pending: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    sequence: AtomicU64,
    next: AtomicU64,
    running: AtomicBool,
    cancelled: AtomicBool,
    // Worker endpoints migrate between the two producers, never concurrently.
    // Neither mutex is held across synthesis, ring waits or audio callbacks.
    ready: Mutex<Producer<ReadyText>>,
    retired: Mutex<Consumer<ReadyText>>,
    observer: Mutex<Option<InboundObserver>>,
}
impl Origin {
    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.statistics
                .cancelled_origins
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}
struct Ticket {
    origin_pending: Arc<AtomicUsize>,
    node_pending: Arc<AtomicUsize>,
    failed: AtomicBool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.origin_pending.fetch_sub(1, Ordering::AcqRel);
        self.node_pending.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Job {
    origin: Arc<Origin>,
    ticket: Arc<Ticket>,
    sequence: u64,
    message: HumanMessage,
    speech: SpeechSource,
}
/// Exact-generation endpoint, moved back to a worker rather than freed on audio.
struct ReadyText {
    origin: Arc<TextOrigin>,
    ticket: Arc<Ticket>,
    sequence: u64,
    delay: u64,
    reader: SegmentConsumer,
}
struct State {
    statistics: Arc<Statistics>,
    failures: ArrayQueue<TextFailure>,
    jobs: ArrayQueue<Job>,
    origins: Mutex<BTreeMap<TextOrigin, Arc<Origin>>>,
    stopping: AtomicBool,
    preparer: NativeMediaPreparer,
}

#[derive(Default)]
struct Statistics {
    admission: [AtomicU64; 5],
    failures: AtomicU64,
    cancelled_origins: AtomicU64,
    forwarded: AtomicU64,
    forwarding_rejected: AtomicU64,
}
/// Independently current, product-wide metadata. Contains no human body or secrets.
pub struct TextObservation {
    /// Jobs accepted since this executor generation started.
    pub admitted: u64,
    /// TooLarge, OriginFull, NodeFull and Stopping counts, in that order.
    pub rejected: [u64; 4],
    /// Failed synthesis jobs, independent of canceled origins.
    pub failures: u64,
    /// Distinct canceled origin generations.
    pub cancelled_origins: u64,
    /// Jobs awaiting one of the two ordinary workers.
    pub queued: usize,
    /// Currently synthesizing jobs, at most two.
    pub active: usize,
    /// All admitted jobs, including ready/playing PCM until retirement.
    pub pending: usize,
    /// Original text accepted by peer egress queues.
    pub forwarded: u64,
    /// Original text rejected by individual peer egress queues.
    pub forwarding_rejected: u64,
}
/// Metadata-only first failure of one synthesis attempt, consumed off audio.
pub struct TextFailure {
    /// Authenticated source generation, not claimed message attribution.
    pub origin: Arc<TextOrigin>,
    /// Per-origin sequence.
    pub job: u64,
    /// Value-free failure classification.
    pub error: MediaError,
}
/// Current source ring metadata. Occupancy uses the ring's source sample clock.
pub struct TextOriginObservation {
    /// Exact retained origin generation.
    pub origin: TextOrigin,
    /// Admitted jobs including playing PCM.
    pub pending: usize,
    /// Source-rate samples currently buffered.
    pub occupancy_samples: u64,
    /// Source PCM sample rate, or zero before its first stream.
    pub source_rate_hz: u32,
}

/// Two ordinary-priority workers. Creation, admission and shutdown are off audio.
pub struct TextExecutor {
    state: Arc<State>,
    workers: Vec<JoinHandle<()>>,
    nodes: BTreeMap<String, Arc<AtomicUsize>>,
    node_count: usize,
}
impl TextExecutor {
    /// Allocate checked admission storage before publishing a station generation.
    pub fn new(node_count: usize, preparer: NativeMediaPreparer) -> Result<Self, MediaError> {
        let mut executor = Self::prepare(node_count, preparer)?;
        executor.start()?;
        Ok(executor)
    }
    /// Allocate a replacement without starting workers beside the current pair.
    pub fn prepare(node_count: usize, preparer: NativeMediaPreparer) -> Result<Self, MediaError> {
        let capacity = node_count
            .checked_mul(64)
            .filter(|v| *v != 0)
            .ok_or(MediaError::InvalidRequest)?;
        let state = Arc::new(State {
            statistics: Arc::default(),
            failures: ArrayQueue::new(capacity),
            jobs: ArrayQueue::new(capacity),
            origins: Mutex::new(BTreeMap::new()),
            stopping: AtomicBool::new(false),
            preparer,
        });
        Ok(Self {
            state,
            workers: Vec::new(),
            nodes: BTreeMap::new(),
            node_count,
        })
    }
    /// Start after the previous executor pair has joined on serialized control.
    pub fn start(&mut self) -> Result<(), MediaError> {
        if self.state.stopping.load(Ordering::Acquire) {
            return Err(MediaError::Cancelled);
        }
        if !self.workers.is_empty() {
            return Ok(());
        }
        for index in 0..2 {
            let state = Arc::clone(&self.state);
            self.workers.push(
                thread::Builder::new()
                    .name(format!("rpt-text-{index}"))
                    .spawn(move || worker(&state))
                    .map_err(|_| MediaError::Io)?,
            );
        }
        Ok(())
    }
    /// Prepare one origin's fixed audio handoff off callback. Origins are never reused.
    pub fn register(
        &mut self,
        local: &str,
        identity: TextOrigin,
    ) -> Result<TextReader, MediaError> {
        if self.state.stopping.load(Ordering::Acquire) {
            return Err(MediaError::Cancelled);
        }
        if !self.nodes.contains_key(local) && self.nodes.len() == self.node_count {
            return Err(MediaError::InvalidRequest);
        }
        let mut origins = self.state.origins.lock().unwrap();
        if origins.contains_key(&identity) {
            return Err(MediaError::InvalidRequest);
        }
        let node_pending = self.nodes.entry(local.to_owned()).or_default().clone();
        let (ready_tx, ready) = RingBuffer::new(4);
        let (retired, retired_rx) = RingBuffer::new(4);
        let origin = Arc::new(Origin {
            statistics: Arc::clone(&self.state.statistics),
            identity: Arc::new(identity.clone()),
            node_pending,
            pending: Arc::default(),
            sequence: AtomicU64::new(1),
            next: AtomicU64::new(1),
            running: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            ready: Mutex::new(ready_tx),
            retired: Mutex::new(retired_rx),
            observer: Mutex::new(None),
        });
        origins.insert(identity, Arc::clone(&origin));
        Ok(TextReader {
            origin,
            ready,
            retired,
            current: None,
            completed_sequence: 0,
        })
    }
    /// Admit literal text without waiting for Piper or affecting protocol forwarding.
    pub fn admit(&self, message: HumanMessage, speech: SpeechSource) -> Admission {
        if self.state.stopping.load(Ordering::Acquire) {
            return self.record(Admission::Stopping);
        }
        if message.body.len() > 4096 {
            return self.record(Admission::TooLarge);
        }
        let origin = self
            .state
            .origins
            .lock()
            .unwrap()
            .get(&message.origin)
            .cloned();
        let Some(origin) = origin.filter(|o| !o.cancelled.load(Ordering::Acquire)) else {
            return self.record(Admission::Stopping);
        };
        if !reserve(&origin.pending, 4) {
            return self.record(Admission::OriginFull);
        }
        if !reserve(&origin.node_pending, 64) {
            origin.pending.fetch_sub(1, Ordering::AcqRel);
            return self.record(Admission::NodeFull);
        }
        let sequence = origin.sequence.fetch_add(1, Ordering::Relaxed);
        let ticket = Arc::new(Ticket {
            origin_pending: Arc::clone(&origin.pending),
            node_pending: Arc::clone(&origin.node_pending),
            failed: AtomicBool::new(false),
        });
        // Total admitted jobs includes running and ready PCM, so the global
        // queue always has space for every reserved job, even during requeue.
        if let Err(job) = self.state.jobs.push(Job {
            origin,
            ticket,
            sequence,
            message,
            speech,
        }) {
            job.origin.cancel();
            return self.record(Admission::Stopping);
        }
        for worker in &self.workers {
            worker.thread().unpark();
        }
        self.record(Admission::Accepted)
    }
    fn record(&self, admission: Admission) -> Admission {
        self.state.statistics.admission[admission as usize].fetch_add(1, Ordering::Relaxed);
        admission
    }
    /// Count text sends independently of local synthesis admission or failure.
    pub fn record_forwarding(&self, accepted: usize, rejected: usize) {
        self.state
            .statistics
            .forwarded
            .fetch_add(accepted as u64, Ordering::Relaxed);
        self.state
            .statistics
            .forwarding_rejected
            .fetch_add(rejected as u64, Ordering::Relaxed);
    }
    /// Copy bounded queue/worker counts; never call this from an audio callback.
    pub fn observation(&self) -> TextObservation {
        let counts = &self.state.statistics;
        let origins = self.state.origins.lock().unwrap();
        TextObservation {
            admitted: counts.admission[0].load(Ordering::Relaxed),
            rejected: std::array::from_fn(|index| {
                counts.admission[index + 1].load(Ordering::Relaxed)
            }),
            failures: counts.failures.load(Ordering::Relaxed),
            cancelled_origins: counts.cancelled_origins.load(Ordering::Relaxed),
            queued: self.state.jobs.len(),
            active: origins
                .values()
                .filter(|origin| origin.running.load(Ordering::Acquire))
                .count(),
            pending: origins
                .values()
                .map(|origin| origin.pending.load(Ordering::Acquire))
                .sum(),
            forwarded: counts.forwarded.load(Ordering::Relaxed),
            forwarding_rejected: counts.forwarding_rejected.load(Ordering::Relaxed),
        }
    }
    /// Drain first-cause metadata on control; errors never include message content.
    pub fn take_failure(&self) -> Option<TextFailure> {
        self.state.failures.pop()
    }
    /// Observe each retained origin without borrowing its unique audio consumer.
    pub fn origins(&self) -> Vec<TextOriginObservation> {
        self.state
            .origins
            .lock()
            .unwrap()
            .values()
            .map(|origin| {
                let observer = origin.observer.lock().unwrap();
                TextOriginObservation {
                    origin: (*origin.identity).clone(),
                    pending: origin.pending.load(Ordering::Acquire),
                    occupancy_samples: observer
                        .as_ref()
                        .and_then(|observer| observer.observe().ok())
                        .map_or(0, |o| o.available_samples),
                    source_rate_hz: observer.as_ref().map_or(0, InboundObserver::rate),
                }
            })
            .collect()
    }
    /// Cancel only this authenticated station/connection generation.
    pub fn cancel(&self, origin: &TextOrigin) {
        if let Some(origin) = self.state.origins.lock().unwrap().get(origin) {
            origin.cancel();
        }
        for worker in &self.workers {
            worker.thread().unpark();
        }
    }
    /// Join bounded work before speech contexts can unload; does not touch hardware.
    pub fn stop(&mut self) {
        self.state.stopping.store(true, Ordering::Release);
        for origin in self.state.origins.lock().unwrap().values() {
            origin.cancel();
        }
        for worker in &self.workers {
            worker.thread().unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        while self.state.jobs.pop().is_some() {}
        reclaim(&self.state);
    }
}
impl Drop for TextExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Callback-side SPSC consumer. Never synthesizes, allocates, locks or frees endpoints.
pub struct TextReader {
    origin: Arc<Origin>,
    ready: Consumer<ReadyText>,
    retired: Producer<ReadyText>,
    current: Option<ReadyText>,
    completed_sequence: u64,
}
impl TextReader {
    /// Exact authenticated generation retained independently of any reused peer name.
    pub fn origin(&self) -> &TextOrigin {
        &self.origin.identity
    }
    fn retire(&mut self) -> bool {
        let Some(item) = self.current.take() else {
            return true;
        };
        match self.retired.push(item) {
            Ok(()) => true,
            Err(rtrb::PushError::Full(item)) => {
                self.current = Some(item);
                false
            }
        }
    }
    /// Add available speech to this local-only bus; pending synthesis is not activity.
    pub fn render(&mut self, output: &mut [f32]) -> bool {
        let mut active = false;
        for sample in output {
            loop {
                if self.current.is_none() {
                    self.current = self.ready.pop().ok();
                }
                let Some(item) = &mut self.current else {
                    return active;
                };
                if !Arc::ptr_eq(&item.origin, &self.origin.identity)
                    || item.sequence <= self.completed_sequence
                    || self.origin.cancelled.load(Ordering::Acquire)
                    || item.ticket.failed.load(Ordering::Acquire)
                    || item.reader.finished(false)
                {
                    if Arc::ptr_eq(&item.origin, &self.origin.identity) {
                        self.completed_sequence = self.completed_sequence.max(item.sequence);
                    }
                    if !self.retire() {
                        return active;
                    }
                    continue;
                }
                match item.reader.render_sample() {
                    Ok(Some(value)) if item.delay == 0 => {
                        *sample += value;
                        active = true;
                        break;
                    }
                    Ok(Some(_)) => item.delay -= 1,
                    Ok(None) => break,
                    Err(_) => {
                        item.ticket.failed.store(true, Ordering::Release);
                    }
                }
            }
        }
        active
    }
}
impl Drop for TextReader {
    fn drop(&mut self) {
        self.origin.cancel();
    }
}

fn reserve(value: &AtomicUsize, maximum: usize) -> bool {
    value
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < maximum).then_some(n + 1)
        })
        .is_ok()
}
fn reclaim(state: &State) {
    // Only short registry/endpoint bookkeeping is serialized. No external work.
    let mut origins = state.origins.lock().unwrap();
    for origin in origins.values() {
        let mut retired = origin.retired.lock().unwrap();
        while retired.pop().is_ok() {}
    }
    origins.retain(|_, origin| {
        !origin.cancelled.load(Ordering::Acquire) || Arc::strong_count(origin) != 1
    });
}
fn worker(state: &State) {
    loop {
        reclaim(state);
        if state.stopping.load(Ordering::Acquire) {
            return;
        }
        let Some(job) = state.jobs.pop() else {
            thread::park_timeout(Duration::from_millis(10));
            continue;
        };
        let origin = Arc::clone(&job.origin);
        if origin.cancelled.load(Ordering::Acquire) {
            continue;
        }
        if job.sequence != origin.next.load(Ordering::Acquire)
            || origin
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            if let Err(job) = state.jobs.push(job) {
                job.origin.cancel();
            }
            thread::park_timeout(Duration::from_millis(1));
            continue;
        }
        if let Err(error) = produce(state, &job) {
            job.ticket.failed.store(true, Ordering::Release);
            if !state.stopping.load(Ordering::Acquire) && !origin.cancelled.load(Ordering::Acquire)
            {
                state.statistics.failures.fetch_add(1, Ordering::Relaxed);
                let _ = state.failures.push(TextFailure {
                    origin: Arc::clone(&origin.identity),
                    job: job.sequence,
                    error,
                });
            }
        }
        origin.next.fetch_add(1, Ordering::Release);
        origin.running.store(false, Ordering::Release);
    }
}
fn produce(state: &State, job: &Job) -> Result<(), MediaError> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(state.preparer.timeout_ms));
    let cancelled = || {
        state.stopping.load(Ordering::Acquire)
            || job.origin.cancelled.load(Ordering::Acquire)
            || job.ticket.failed.load(Ordering::Acquire)
            || Instant::now() >= deadline
    };
    let token = Cancellation::default();
    let mut stream = state.preparer.open_speech_stream(&SpeechRequest {
        text: &job.message.body,
        model: &job.speech.model,
        speed_percent: job.speech.speed_percent,
        level_db: job.speech.level_db,
        cancellation: &token,
    })?;
    let mut chunk = [0.0; 4096];
    let first = stream.read(&mut chunk, &cancelled)?;
    if first == 0 {
        return Err(MediaError::InvalidOutput);
    }
    let rate = stream.sample_rate_hz();
    let (producer, ring) =
        InboundRing::open(rate, InboundPolicy::Media).map_err(|_| MediaError::Io)?;
    let progress = Arc::new(SegmentProgress::default());
    *job.origin.observer.lock().unwrap() = Some(producer.observer());
    let mut writer = super::finite::FiniteWriter::new(producer, Arc::clone(&progress), rate)?;
    let delay = writer.delay();
    writer.write(&chunk[..first], &cancelled)?;
    let reader = SegmentConsumer {
        ring,
        progress,
        consumed: 0,
    };
    let ready = ReadyText {
        origin: Arc::clone(&job.origin.identity),
        ticket: Arc::clone(&job.ticket),
        sequence: job.sequence,
        delay,
        reader,
    };
    job.origin
        .ready
        .lock()
        .unwrap()
        .push(ready)
        .map_err(|_| MediaError::Io)?;
    loop {
        let count = stream.read(&mut chunk, &cancelled)?;
        if count == 0 {
            break;
        }
        writer.write(&chunk[..count], &cancelled)?;
    }
    writer.finish(&cancelled)
}

#[cfg(test)]
#[path = "text_tests.rs"]
pub(crate) mod tests;
