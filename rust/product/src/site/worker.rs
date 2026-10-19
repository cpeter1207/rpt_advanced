//! Bounded serial physical-device acquisition, never an audio or station-policy owner.
use super::backends::{
    SiteBackends,
    endpoint::{Direction, Endpoint, Sample},
};
use rpt_advanced_core::{config::SiteEndpoint, runtime::GenerationWork, site::ReadingQuality};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

const RUN: u64 = 0;
const PAUSE: u64 = 1;
const SAFE: u64 = 2;
const CLOSE: u64 = 3;
const PREPARE: u64 = 4;
const INSTALL: u64 = 5;
const KIND: u64 = 7;

struct Fence {
    request: AtomicU64,
    acknowledged: AtomicU64,
    quality: AtomicU8,
    // A committed queue exchange cannot be superseded by pause/close intent.
    install: AtomicBool,
}
struct Request {
    index: usize,
    demand: Option<f64>,
    work: GenerationWork,
}

/// Replacement queues cross the device boundary once, while ordinary work is fenced.
struct Handoff {
    settings: Vec<(SiteEndpoint, Direction)>,
    requests: Consumer<Request>,
    replies: Producer<Reply>,
}
struct Staged {
    requests: Producer<Request>,
    replies: Consumer<Reply>,
    pending: Vec<bool>,
}

/// An acquisition or acknowledged command; no output claims electrical readback.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Successfully acquired source-unit/logical value.
    Reading(Sample),
    /// Submitted demand with actual hardware PWM capability.
    Submitted {
        /// Whether fractional duty is supported.
        pwm: bool,
    },
}

/// Completed operation retaining its permit until station control validates it.
pub struct Reply {
    /// Index in the owner's currently committed endpoint list.
    pub index: usize,
    /// Generation against which the operation was admitted.
    pub work: GenerationWork,
    /// Acquisition/completion time on the host's monotonic clock.
    pub completed_at_ms: u64,
    /// Fresh observation or truthful error; never a fabricated measurement.
    pub outcome: Result<Outcome, ReadingQuality>,
}

/// One owner per physical device, with at most one outstanding operation per endpoint.
pub struct DeviceWorker {
    requests: Producer<Request>,
    replies: Consumer<Reply>,
    pending: Vec<bool>,
    identity: SiteEndpoint,
    handoffs: Producer<Handoff>,
    staged: Option<Staged>,
    fence: Arc<Fence>,
    thread: Option<JoinHandle<()>>,
}
impl DeviceWorker {
    /// Prepare changed bindings while paused, retaining unchanged physical contexts.
    pub fn stage(&mut self, settings: Vec<(SiteEndpoint, Direction)>) -> bool {
        if self.barrier().is_none()
            || self.thread.is_none()
            || self.pending.iter().any(|pending| *pending)
            || !same_device(&settings, &self.identity)
        {
            return false;
        }
        let (requests, input) = RingBuffer::new(settings.len());
        let (output, replies) = RingBuffer::new(settings.len());
        let pending = vec![false; settings.len()];
        assert!(
            self.handoffs
                .push(Handoff {
                    settings,
                    requests: input,
                    replies: output
                })
                .is_ok()
        );
        self.staged = Some(Staged {
            requests,
            replies,
            pending,
        });
        self.signal(PREPARE);
        true
    }
    /// Commit prepared bindings while still paused; resume after its acknowledgement.
    pub fn commit(&mut self) -> bool {
        if self.fence.request.load(Ordering::Acquire) & KIND != PREPARE
            || self.barrier() != Some(Ok(()))
        {
            return false;
        }
        let Some(staged) = self.staged.take() else {
            return false;
        };
        self.requests = staged.requests;
        self.replies = staged.replies;
        self.pending = staged.pending;
        self.fence.install.store(true, Ordering::Release);
        self.signal(INSTALL);
        true
    }
    /// Start a dedicated non-audio acquisition owner; endpoints open only there.
    pub fn start(
        backends: Arc<SiteBackends>,
        settings: Vec<(SiteEndpoint, Direction)>,
        epoch: Instant,
    ) -> Result<Self, ReadingQuality> {
        let Some((first, _)) = settings.first() else {
            return Err(ReadingQuality::Unavailable);
        };
        if !same_device(&settings, first) {
            return Err(ReadingQuality::Fault);
        }
        let identity = first.clone();
        let (requests, input) = RingBuffer::new(settings.len());
        let (output, replies) = RingBuffer::new(settings.len());
        let (handoffs, changes) = RingBuffer::new(1);
        let pending = vec![false; settings.len()];
        let fence = Arc::new(Fence {
            request: AtomicU64::new(RUN),
            acknowledged: AtomicU64::new(RUN),
            quality: AtomicU8::new(0),
            install: AtomicBool::new(false),
        });
        let shared = Arc::clone(&fence);
        let thread = thread::Builder::new()
            .name("rptadv-site-io".into())
            .spawn(move || acquire(backends, settings, epoch, input, output, changes, shared))
            .map_err(|_| ReadingQuality::Unavailable)?;
        Ok(Self {
            requests,
            replies,
            pending,
            identity,
            handoffs,
            staged: None,
            fence,
            thread: Some(thread),
        })
    }
    /// Queue one read (`None`) or normalized write without waiting for device I/O.
    pub fn submit(&mut self, index: usize, demand: Option<f64>, work: GenerationWork) -> bool {
        if self.thread.is_none()
            || self.fence.request.load(Ordering::Acquire) & KIND != RUN
            || self.pending.get(index) != Some(&false)
            || !work.is_current()
        {
            return false;
        }
        // The pending bit reserves one request and one response slot for this endpoint.
        assert!(
            self.requests
                .push(Request {
                    index,
                    demand,
                    work
                })
                .is_ok()
        );
        self.pending[index] = true;
        self.wake();
        true
    }
    /// Drain one completed operation. Its generation must still be current when applied.
    pub fn poll(&mut self) -> Option<Reply> {
        let reply = self.replies.pop().ok()?;
        self.pending[reply.index] = false;
        Some(reply)
    }
    /// Cancel any prepared binding change and fence ordinary work. Safe handoff also
    /// submits every output's safe state. Drain replies before staging a replacement.
    pub fn pause(&mut self, safe: bool) {
        self.staged = None;
        self.signal(if safe { SAFE } else { PAUSE });
    }
    /// Return an acknowledged quiescent barrier, including any safe-write failure.
    pub fn barrier(&self) -> Option<Result<(), ReadingQuality>> {
        let request = self.fence.request.load(Ordering::Acquire);
        if request & KIND == RUN || self.fence.acknowledged.load(Ordering::Acquire) != request {
            return None;
        }
        Some(match self.fence.quality.load(Ordering::Acquire) {
            0 => Ok(()),
            _ => Err(ReadingQuality::Fault),
        })
    }
    /// Resume an unchanged owner only after the barrier has acknowledged.
    pub fn resume(&mut self) -> bool {
        if self.barrier() != Some(Ok(())) || self.thread.is_none() || self.staged.is_some() {
            return false;
        }
        self.signal(RUN);
        true
    }
    /// Close after confirmed safe handoff. Join only outside station control.
    pub fn finish(&mut self) -> bool {
        if self.fence.request.load(Ordering::Acquire) & KIND != SAFE
            || self.barrier() != Some(Ok(()))
        {
            return false;
        }
        self.signal(CLOSE);
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }

    fn signal(&self, kind: u64) {
        // Only station control writes requests. Each request has a distinct acknowledgement.
        let previous = self.fence.request.load(Ordering::Relaxed);
        self.fence.request.store(
            (previous.wrapping_add(KIND + 1) & !KIND) | kind,
            Ordering::Release,
        );
        self.wake();
    }
    fn wake(&self) {
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}

/// Every binding belongs to this serial physical-device owner; empty means removal.
fn same_device(settings: &[(SiteEndpoint, Direction)], identity: &SiteEndpoint) -> bool {
    settings.iter().all(|(endpoint, _)| {
        endpoint.backend == identity.backend && endpoint.device == identity.device
    })
}

impl Drop for DeviceWorker {
    fn drop(&mut self) {
        // Last-resort process teardown, off control/audio. Normal retirement calls
        // pause(true)/barrier/finish and retains self on safe-state failure.
        self.signal(CLOSE);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn acquire(
    backends: Arc<SiteBackends>,
    mut settings: Vec<(SiteEndpoint, Direction)>,
    epoch: Instant,
    mut requests: Consumer<Request>,
    mut replies: Producer<Reply>,
    mut changes: Consumer<Handoff>,
    fence: Arc<Fence>,
) {
    // Contexts are constructed, used, and destroyed only on this device owner.
    let mut endpoints: Vec<Option<Endpoint>> = (0..settings.len()).map(|_| None).collect();
    let mut seen = RUN;
    let mut prepared: Option<Handoff> = None;
    loop {
        // Acquire intent first: a post-commit pause must observe the committed
        // queue exchange before it can discard an uncommitted preparation.
        let request = fence.request.load(Ordering::Acquire);
        if fence.install.swap(false, Ordering::AcqRel) {
            let next = prepared.take().expect("commit requires a prepared barrier");
            let retained = next
                .settings
                .iter()
                .map(|binding| {
                    settings
                        .iter()
                        .position(|old| old == binding)
                        .and_then(|index| endpoints[index].take())
                })
                .collect();
            endpoints = retained; // Only safely retired old contexts are dropped.
            settings = next.settings;
            requests = next.requests;
            replies = next.replies;
        }
        if request != seen {
            if request & KIND != RUN {
                while let Ok(cancelled) = requests.pop() {
                    respond(
                        &mut replies,
                        cancelled,
                        Err(ReadingQuality::Unavailable),
                        epoch,
                    );
                }
                let mut failed = false;
                let already_safe =
                    seen & KIND == SAFE && fence.quality.load(Ordering::Relaxed) == 0;
                if request & KIND == SAFE || (request & KIND == CLOSE && !already_safe) {
                    for endpoint in endpoints.iter_mut().flatten() {
                        failed |= endpoint.safe().is_err();
                    }
                }
                match request & KIND {
                    PREPARE => {
                        let next = changes.pop().expect("stage publishes before its fence");
                        for (binding, endpoint) in settings.iter().zip(&mut endpoints) {
                            if !next.settings.contains(binding) {
                                if let Some(endpoint) = endpoint {
                                    failed |= endpoint.safe().is_err();
                                }
                            }
                        }
                        prepared = Some(next);
                    }
                    _ => {
                        prepared = None;
                        while changes.pop().is_ok() {}
                    }
                }
                fence.quality.store(u8::from(failed), Ordering::Relaxed);
                fence.acknowledged.store(request, Ordering::Release);
            }
            seen = request;
        }
        if request & KIND == CLOSE {
            return;
        }
        if request & KIND == RUN {
            if let Ok(operation) = requests.pop() {
                let result = perform(&backends, &settings, &mut endpoints, &operation);
                respond(&mut replies, operation, result, epoch);
                continue;
            }
        }
        thread::park();
    }
}

fn perform(
    backends: &Arc<SiteBackends>,
    settings: &[(SiteEndpoint, Direction)],
    endpoints: &mut [Option<Endpoint>],
    request: &Request,
) -> Result<Outcome, ReadingQuality> {
    if !request.work.is_current() {
        return Err(ReadingQuality::Unavailable);
    }
    let endpoint = &mut endpoints[request.index];
    if endpoint.is_none() {
        let (settings, direction) = &settings[request.index];
        *endpoint = Some(Endpoint::open(Arc::clone(backends), settings, *direction)?);
    }
    // Opening can block. Revalidate before a separate physical read/write.
    if !request.work.is_current() {
        return Err(ReadingQuality::Unavailable);
    }
    let endpoint = endpoint
        .as_mut()
        .expect("successful acquisition retains its context");
    match request.demand {
        None => endpoint.read().map(Outcome::Reading),
        Some(demand) => endpoint.write(demand).map(|()| Outcome::Submitted {
            pwm: endpoint.pwm(),
        }),
    }
}

fn respond(
    replies: &mut Producer<Reply>,
    request: Request,
    outcome: Result<Outcome, ReadingQuality>,
    epoch: Instant,
) {
    let outcome = if request.work.is_current() {
        outcome
    } else {
        Err(ReadingQuality::Unavailable)
    };
    assert!(
        replies
            .push(Reply {
                index: request.index,
                work: request.work,
                completed_at_ms: u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX),
                outcome,
            })
            .is_ok(),
        "each in-flight endpoint reserves a response slot"
    );
}
