//! Per-node bounded media distribution, independent of station control.

use crate::Error;
use rpt_advanced_core::link::LinkDispatcher;
use rtrb::{Producer, RingBuffer};
use std::{
    ffi::c_void,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
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
    generation: AtomicU64,
}
struct Dispatch {
    state: Arc<DispatchState>,
    dispatcher: LinkDispatcher,
    wakes: Vec<PeerWake>,
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
}
impl PreparedDispatch {
    /// Infallible publication after the core has accepted the prepared generation.
    pub(crate) fn activate(&self, generation: u64) {
        self.state.generation.store(generation, Ordering::Relaxed);
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
                                let progress = context
                                    .dispatcher
                                    .dispatch_batch(64, |index| context.wakes[index].notify());
                                busy |= progress.pending;
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
    ) -> Result<PreparedDispatch, Error> {
        if self.stop.load(Ordering::Acquire) {
            return Err(Error::Reservation);
        }
        let state = Arc::new(DispatchState {
            phase: AtomicU8::new(PREPARED),
            generation: AtomicU64::new(0),
        });
        self.pending
            .push(Dispatch {
                state: state.clone(),
                dispatcher,
                wakes,
            })
            .map_err(|_| Error::Reservation)?;
        self.thread
            .as_ref()
            .expect("running media owner")
            .thread()
            .unpark();
        Ok(PreparedDispatch { state })
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
