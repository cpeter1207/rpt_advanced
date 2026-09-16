//! Fixed radio callback ownership and control-only worker start/stop.
use crate::{Error, link::ring::InboundConsumer, services::Radio};
use rpt_advanced_core::{link::LinkAudio, runtime::RuntimeAudioOwners};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::Instant,
};

#[cfg(test)]
thread_local! {
    static NEXT_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One test-only radio-worker failure injection point shared by product lifecycle tests.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum TestFailure {
    /// Fail candidate worker thread creation without consuming the reserved radio.
    Prepare = 1,
    /// Reject one candidate audio-owner attachment before runtime publication.
    Attach = 2,
}

#[cfg(test)]
pub(crate) fn take_test_failure(failure: TestFailure) -> bool {
    NEXT_FAILURE.with(|next| {
        if next.get() == failure as u8 {
            next.set(0);
            true
        } else {
            false
        }
    })
}

/// Unique prepared link state consumed by the radio TX half.
pub type Audio = LinkAudio<InboundConsumer>;
/// Fixed RX/TX registrations moved once into a radio worker, never cloned per frame.
pub type AudioOwners = RuntimeAudioOwners<Audio>;

/// Stable control-readable local carrier edge; one packed atomic keeps state and time coherent.
#[derive(Clone, Default)]
pub struct RadioStatus(Arc<AtomicU64>);
impl RadioStatus {
    /// Current hardware receive state and its original monotonic transition time.
    pub fn snapshot(&self) -> (bool, u64) {
        let value = self.0.load(Ordering::Acquire);
        (value & 1 != 0, value >> 1)
    }
    fn update(&self, receiving: bool, now_ms: u64) {
        if self.0.load(Ordering::Relaxed) & 1 != u64::from(receiving) {
            self.0.store(
                (now_ms.min(u64::MAX >> 1) << 1) | u64::from(receiving),
                Ordering::Release,
            );
        }
    }
}

/// Process one shared-clock pair without allocation, locking, logging, or reference counting.
pub fn render(owners: &mut AudioOwners, receiving: bool, samples: &mut [f32], now_ms: u64) -> bool {
    let Some(mut generation) = owners.0.acquire_pair(&mut owners.1) else {
        samples.fill(0.0);
        return false;
    };
    generation.receive().process(receiving, samples, now_ms);
    let transmit = generation.transmit();
    match transmit
        .adapter
        .process(&mut transmit.controller, receiving, samples)
    {
        Ok(keyed) => keyed,
        Err(_) => {
            samples.fill(0.0);
            false
        }
    }
}

/// Prepared owner thread. It waits for fixed registrations before entering any audio callback.
/// The lifecycle caller must stop/join it before closing its channel or unloading callback code.
pub struct RadioWorker {
    start: Option<SyncSender<AudioOwners>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<(Radio, Option<AudioOwners>)>>,
}
impl RadioWorker {
    /// Allocate/start a parked thread during candidate preparation. Failure returns the exact
    /// reserved radio; no live callback registration has been consumed yet.
    pub fn prepare(
        radio: Radio,
        epoch: Instant,
        status: RadioStatus,
    ) -> Result<Self, (Error, Radio)> {
        let slot = Arc::new(Mutex::new(Some(radio)));
        let input = Arc::clone(&slot);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let (start, receive) = mpsc::sync_channel(1);
        let spawn = move || {
            std::thread::Builder::new()
                .name("rpt-radio".into())
                .spawn(move || {
                    // Setup only: the mutex/refcount operations precede the first callback.
                    let mut radio = input
                        .lock()
                        .expect("prepared radio slot is private and cannot be poisoned")
                        .take()
                        .expect("unique prepared radio");
                    drop(input);
                    let Ok(mut owners) = receive.recv() else {
                        return (radio, None);
                    };
                    while !stopped.load(Ordering::Acquire) {
                        match radio.ready() {
                            Ok(false) => continue,
                            Err(_) => break,
                            Ok(true) => {}
                        }
                        let now_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                        if radio
                            .exchange(now_ms, |receiving, samples| {
                                status.update(receiving, now_ms);
                                render(&mut owners, receiving, samples, now_ms)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    (radio, Some(owners))
                })
        };
        #[cfg(test)]
        let thread = if take_test_failure(TestFailure::Prepare) {
            Err(std::io::Error::other("injected worker preparation failure"))
        } else {
            spawn()
        };
        #[cfg(not(test))]
        let thread = spawn();
        match thread {
            Ok(thread) => Ok(Self {
                start: Some(start),
                stop,
                thread: Some(thread),
            }),
            Err(_) => Err((
                Error::Thread,
                slot.lock()
                    .expect("unstarted private radio slot cannot be poisoned")
                    .take()
                    .expect("unstarted radio"),
            )),
        }
    }
    /// Inject one shared worker failure at the selected product lifecycle boundary.
    #[cfg(test)]
    pub(crate) fn fail_next_for_test(failure: TestFailure) {
        NEXT_FAILURE.with(|next| next.set(failure as u8));
    }
    /// Return a terminated worker that has released its radio so stop/error handling is testable.
    #[cfg(test)]
    pub(crate) fn terminated_for_test(radio: Radio) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn(move || -> (Radio, Option<AudioOwners>) {
            drop(radio);
            panic!("injected terminated radio worker");
        });
        Self {
            start: None,
            stop,
            thread: Some(thread),
        }
    }
    /// Return a worker whose parked receiver has already disappeared for send-failure coverage.
    #[cfg(test)]
    pub(crate) fn disconnected_for_test(radio: Radio) -> Self {
        let (start, receive) = mpsc::sync_channel(1);
        drop(receive);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn(move || (radio, None));
        Self {
            start: Some(start),
            stop,
            thread: Some(thread),
        }
    }
    /// Activate the parked thread after generation publication, transferring fixed owners once.
    pub fn attach(&mut self, owners: AudioOwners) -> Result<(), AudioOwners> {
        let Some(start) = self.start.take() else {
            return Err(owners);
        };
        start.send(owners).map_err(|error| error.0)
    }
    /// Stop and return the exact device and fixed registrations for a controlled handoff.
    pub fn stop(mut self) -> Result<(Radio, Option<AudioOwners>), Error> {
        self.stop.store(true, Ordering::Release);
        self.start = None;
        self.thread
            .take()
            .expect("prepared worker has one join handle")
            .join()
            .map_err(|_| Error::Thread)
    }
}
impl Drop for RadioWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.start = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
