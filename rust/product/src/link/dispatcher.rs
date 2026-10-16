//! Per-node bounded media distribution, independent of station control.

use super::{regeneration::RegenerationProducer, ring::InboundProducer};
use crate::Error;
use rpt_advanced_core::config::ResolvedDtmfRegenerationSettings;
use rpt_advanced_core::link::LinkDispatcher;
use rtrb::{Producer, RingBuffer};
use std::{
    ffi::c_void,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CLOSED: usize = 1 << (usize::BITS - 1);
type WakeFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
struct WakeTarget {
    call: WakeFn,
    context: *mut c_void,
    registration: *mut c_void,
}
// SAFETY: immutable signal-only target; close/count quiescence precedes provider detach.
unsafe impl Send for WakeTarget {}
// SAFETY: the provider's wake is thread-safe and the gate retains its token during calls.
unsafe impl Sync for WakeTarget {}
#[derive(Default)]
struct WakeState {
    gate: AtomicUsize,
    target: OnceLock<WakeTarget>,
}

/// Cloneable notification capability, never a clone of the peer's mutable state.
#[derive(Clone, Default)]
pub(crate) struct PeerWake(Arc<WakeState>);
impl PeerWake {
    /// Publish once after successful registration and before any destination publication.
    ///
    /// # Safety
    /// The signal-only callback and borrowed pointers remain live until close and quiescence.
    pub(crate) unsafe fn attach(
        &self,
        call: WakeFn,
        context: *mut c_void,
        registration: *mut c_void,
    ) {
        assert!(
            self.0
                .target
                .set(WakeTarget {
                    call,
                    context,
                    registration
                })
                .is_ok()
        );
    }
    /// Notify off audio. SeqCst orders every count admission against close: callers
    /// admitted before close retain the token; later callers never dereference it.
    pub(crate) fn notify(&self) {
        let before = self.0.gate.fetch_add(1, Ordering::SeqCst);
        if before & CLOSED == 0 {
            if let Some(target) = self.0.target.get() {
                unsafe { (target.call)(target.context, target.registration) };
            }
        }
        self.0.gate.fetch_sub(1, Ordering::SeqCst);
    }
    /// Prevent new token access; existing notifiers complete independently.
    pub(crate) fn close(&self) {
        self.0.gate.fetch_or(CLOSED, Ordering::SeqCst);
    }
    /// True only after close and the last admitted provider wake has returned.
    pub(crate) fn is_quiescent(&self) -> bool {
        self.0.gate.load(Ordering::SeqCst) == CLOSED
    }
}

const PREPARED: u8 = 0;
const ACTIVE: u8 = 1;
const STOPPING: u8 = 2;
const QUIET: u8 = 3;
struct DispatchState {
    phase: AtomicU8,
    clock: OnceLock<Arc<super::source_clock::SourceClock>>,
    batches: AtomicU64,
    maximum_ns: AtomicU64,
    regeneration_dropped: AtomicU64,
}
struct Dispatch {
    state: Arc<DispatchState>,
    dispatcher: LinkDispatcher,
    wakes: Vec<PeerWake>,
    regeneration: Option<RegenerationProducer>,
}
impl Drop for Dispatch {
    fn drop(&mut self) {
        // No more queue writes or wake calls follow this release publication.
        self.state.phase.store(QUIET, Ordering::Release);
    }
}

/// Control-owned lifetime handle; the media thread alone owns queue endpoints.
pub(crate) struct PreparedDispatch {
    state: Arc<DispatchState>,
    digits: Option<Producer<char>>,
}
impl PreparedDispatch {
    /// Drop new excess digits; never block the generation's control owner.
    pub(crate) fn queue_digit(&mut self, digit: char) -> bool {
        if self.state.phase.load(Ordering::Acquire) != ACTIVE || !"0123456789ABCD*#".contains(digit)
        {
            return false;
        }
        let Some(digits) = self.digits.as_mut() else {
            return false;
        };
        if digits.push(digit).is_ok() {
            return true;
        }
        self.state
            .regeneration_dropped
            .fetch_add(1, Ordering::Relaxed);
        false
    }
    /// Drop-new queue overflow or producer failures in this generation.
    pub(crate) fn regeneration_dropped(&self) -> u64 {
        self.state.regeneration_dropped.load(Ordering::Relaxed)
    }
    /// Completed nonempty batches and their maximum elapsed duration, off audio.
    pub(crate) fn timing(&self) -> (u64, u64) {
        (
            self.state.batches.load(Ordering::Relaxed),
            self.state.maximum_ns.load(Ordering::Relaxed),
        )
    }
    pub(crate) fn bind_clock(&self, clock: Arc<super::source_clock::SourceClock>) {
        let _ = self.state.clock.set(clock);
    }
    /// Infallible publication after the core has accepted the prepared generation.
    pub(crate) fn activate(&self) {
        let _ = self.state.phase.compare_exchange(
            PREPARED,
            ACTIVE,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
    /// Abandon remaining old media after the current bounded batch finishes.
    pub(crate) fn request_stop(&self) {
        let _ = self
            .state
            .phase
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |phase| {
                (phase < STOPPING).then_some(STOPPING)
            });
    }
    /// External reclamation still also requires callback and redirect quiescence.
    pub(crate) fn is_quiescent(&self) -> bool {
        self.state.phase.load(Ordering::Acquire) == QUIET
    }
}
impl Drop for PreparedDispatch {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// One node-lifetime OS thread; never performs codec, protocol, or network work.
pub(crate) struct MediaWorker {
    pending: Producer<Dispatch>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl MediaWorker {
    /// Reserve the bounded publication queue and start an idle media owner.
    pub(crate) fn start() -> std::io::Result<Self> {
        let (pending, mut incoming) = RingBuffer::<Dispatch>::new(2);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::Builder::new()
            .name("rpt-media".into())
            .spawn(move || {
                let mut contexts: [Option<Dispatch>; 2] = [None, None];
                while !stopping.load(Ordering::Acquire) {
                    let mut busy = false;
                    for slot in &mut contexts {
                        if slot.is_none() {
                            *slot = incoming.pop().ok();
                        }
                        let Some(context) = slot else { continue };
                        match context.state.phase.load(Ordering::Acquire) {
                            ACTIVE => {
                                let started = Instant::now();
                                if let Some(clock) = context.state.clock.get() {
                                    clock.observe();
                                }
                                let progress = context
                                    .dispatcher
                                    .dispatch_batch(64, |index| context.wakes[index].notify());
                                busy |= progress.pending;
                                if context
                                    .regeneration
                                    .as_mut()
                                    .is_some_and(|producer| producer.pump(started).is_err())
                                {
                                    context
                                        .state
                                        .regeneration_dropped
                                        .fetch_add(1, Ordering::Relaxed);
                                    context.regeneration = None;
                                }
                                if progress.destinations != 0 {
                                    context.state.batches.fetch_add(1, Ordering::Relaxed);
                                    context.state.maximum_ns.fetch_max(
                                        started.elapsed().as_nanos() as u64,
                                        Ordering::Relaxed,
                                    );
                                }
                            }
                            STOPPING => {
                                *slot = None;
                                busy = true;
                            }
                            _ => {}
                        }
                    }
                    if !busy {
                        thread::park_timeout(Duration::from_millis(1));
                    }
                }
                drop(contexts);
                while incoming.pop().is_ok() {}
            })?;
        Ok(Self {
            pending,
            stop,
            thread: Some(thread),
        })
    }
    /// Transfer prepared endpoints before generation publication; failure owns no registration.
    pub(crate) fn prepare(
        &mut self,
        dispatcher: LinkDispatcher,
        wakes: Vec<PeerWake>,
        regeneration: Option<(ResolvedDtmfRegenerationSettings, InboundProducer, usize)>,
    ) -> Result<PreparedDispatch, Error> {
        if self.stop.load(Ordering::Acquire) {
            return Err(Error::Reservation);
        }
        let state = Arc::new(DispatchState {
            phase: AtomicU8::new(PREPARED),
            clock: OnceLock::new(),
            batches: AtomicU64::new(0),
            maximum_ns: AtomicU64::new(0),
            regeneration_dropped: AtomicU64::new(0),
        });
        let (digits, regeneration) = match regeneration {
            Some((settings, writer, maximum)) => {
                let (digits, incoming) = RingBuffer::new(127);
                (
                    Some(digits),
                    Some(RegenerationProducer::new(
                        settings, incoming, writer, maximum,
                    )?),
                )
            }
            None => (None, None),
        };
        self.pending
            .push(Dispatch {
                state: state.clone(),
                dispatcher,
                wakes,
                regeneration,
            })
            .map_err(|_| Error::Reservation)?;
        self.thread
            .as_ref()
            .expect("running media owner")
            .thread()
            .unpark();
        Ok(PreparedDispatch { state, digits })
    }
    /// Join only off audio and peer callbacks; all retained dispatchers become quiescent.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for MediaWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "dispatcher_tests.rs"]
mod tests;
