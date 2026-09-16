//! Serialized product host lifecycle. No callback unwinds across the public ABI.
use crate::{
    abi,
    control::{ControlClient, Descriptor as ControlDescriptor},
    host::Host,
    media::{FileDescriptor, NativeMediaPreparer, SpeechDescriptor},
    services::HostServices,
};
use rpt_advanced_core::{
    command::LinkAction,
    runtime::{
        RuntimeDispatch,
        dtmf::DigitOperation,
        links::{ConnectAttempt, LinkEffect, RetryWork},
    },
};
use rpt_advanced_core::{
    config::{ConfigDocument, Schema},
    control::{ControlExecutor, ControlTask},
    runtime::{RuntimeClock, RuntimeError},
};
use std::{
    any::Any,
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

type ErasedResult = Box<dyn Any + Send>;
type HostOperation = Box<dyn FnOnce(&mut Host) -> Result<ErasedResult, RuntimeError> + Send>;

#[cfg(test)]
static FAIL_DIAL_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SKIP_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static FAIL_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static PANIC_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static HOST_STOP_MODE: AtomicU64 = AtomicU64::new(0);

static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);

pub(crate) struct Engine {
    executor: ControlClient,
    host: Mutex<Option<Host>>,
    services: HostServices,
    origin: Instant,
    accepting: AtomicBool,
    reloading: AtomicBool,
    revision: AtomicU64,
    lost_digits: AtomicBool,
    event_busy: AtomicU64,
    calls: Mutex<usize>,
    drained: Condvar,
    ticker: Mutex<Option<JoinHandle<()>>>,
    jobs: Mutex<Vec<JoinHandle<()>>>,
}
pub(crate) struct Call(Arc<Engine>);
enum Dial {
    Connect(ConnectAttempt),
    Retry(RetryWork),
}
struct DialJob {
    local: String,
    attempt: Dial,
    event: Option<RuntimeDispatch>,
    report: bool,
}
impl Dial {
    fn remote(&self) -> &str {
        match self {
            Self::Connect(work) => work.remote(),
            Self::Retry(work) => work.remote(),
        }
    }
    fn mode(&self) -> rpt_advanced_core::link::Mode {
        match self {
            Self::Connect(work) => work.mode(),
            Self::Retry(work) => work.mode(),
        }
    }
    fn current(&self) -> bool {
        match self {
            Self::Connect(work) => work.current(),
            Self::Retry(work) => work.current(),
        }
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        let mut count = self.0.calls.lock().unwrap_or_else(|e| e.into_inner());
        *count -= 1;
        self.0.drained.notify_all();
    }
}
impl Engine {
    pub(crate) fn execute(
        self: &Arc<Self>,
        local: String,
        operation: DigitOperation,
    ) -> Result<(), RuntimeError> {
        let revision = self.revision.load(Ordering::Acquire);
        let effect = self.operation(&local, operation)?;
        self.effect(local, effect, revision)
    }
    fn operation(
        self: &Arc<Self>,
        local: &str,
        operation: DigitOperation,
    ) -> Result<LinkEffect, RuntimeError> {
        let verify = operation.command.action == LinkAction::Command && operation.digit.is_none();
        let verified = if verify {
            let name = local.to_owned();
            let remote = operation.command.node.clone();
            self.run(move |host| host.lookup(&name, &remote, None))
                .is_ok()
        } else {
            false
        };
        let name = local.to_owned();
        let clock = self.clock();
        self.run(move |host| host.runtime.command(&name, operation, clock, verified))
    }
    fn effect(
        self: &Arc<Self>,
        local: String,
        effect: LinkEffect,
        revision: u64,
    ) -> Result<(), RuntimeError> {
        if let LinkEffect::Connect(attempt) = effect {
            self.dial(local, Dial::Connect(attempt), revision)
        } else {
            let clock = self.clock();
            let engine = Arc::clone(self);
            self.run(move |host| {
                if !engine.current(revision) {
                    return Err(RuntimeError::Rejected);
                }
                host.immediate(&local, effect, clock)
            })
        }
    }
    fn dial(
        self: &Arc<Self>,
        local: String,
        attempt: Dial,
        revision: u64,
    ) -> Result<(), RuntimeError> {
        let name = local.clone();
        let remote = attempt.remote().to_owned();
        let destination_remote = remote.clone();
        let destination = self.run(move |host| host.lookup(&name, &destination_remote, None));
        let io = destination.ok().and_then(|destination| {
            self.services
                .dial(&destination, &local, crate::host::MAXIMUM_FRAMES, || {
                    self.current(revision) && attempt.current()
                })
                .ok()
        });
        let engine = Arc::clone(self);
        self.run_inner(true, move |host| {
            let clock = engine.clock();
            let mode = attempt.mode();
            let current = engine.current(revision);
            let attached = match attempt {
                Dial::Connect(attempt) if !current => {
                    if let Some(node) = host.runtime.node(&local) {
                        node.links().cancel_connect(attempt);
                    }
                    return Err(RuntimeError::Rejected);
                }
                Dial::Connect(attempt) => {
                    host.runtime
                        .finish_connect(&local, attempt, io.is_some(), clock)?
                }
                Dial::Retry(attempt) => host
                    .runtime
                    .node(&local)
                    .ok_or(RuntimeError::MissingNode)?
                    .links()
                    .finish_retry(attempt, current && io.is_some(), clock.now_ms)
                    .map_err(RuntimeError::Link)?,
            };
            if attached {
                let result = host.attach_peer(
                    &local,
                    &remote,
                    mode,
                    io.ok_or(RuntimeError::Rejected)?,
                    clock,
                );
                if result.is_err() {
                    host.reject_peer(&local, &remote, clock.now_ms);
                }
                result
            } else {
                Err(RuntimeError::Rejected)
            }
        })
    }
    fn background(
        self: &Arc<Self>,
        local: String,
        effect: LinkEffect,
        revision: u64,
        report: bool,
    ) -> Result<(), RuntimeError> {
        if let LinkEffect::Connect(attempt) = effect {
            self.spawn_dial(local, Dial::Connect(attempt), revision, None, report)
        } else {
            let result = self.effect(local.clone(), effect, revision);
            if report {
                self.services.command_notice(&local, result.is_ok());
            }
            result
        }
    }
    fn spawn_dial(
        self: &Arc<Self>,
        local: String,
        attempt: Dial,
        revision: u64,
        event: Option<RuntimeDispatch>,
        report: bool,
    ) -> Result<(), RuntimeError> {
        let call = self.enter()?;
        let engine = Arc::clone(self);
        let job = Arc::new(Mutex::new(Some(DialJob {
            local,
            attempt,
            event,
            report,
        })));
        let worker_job = Arc::clone(&job);
        let operation = move || {
            let _call = call;
            let job = worker_job
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .expect("accepted dial owns its job");
            let _ = engine.complete_dial(job, revision);
        };
        #[cfg(test)]
        let thread = if FAIL_DIAL_THREAD.swap(false, Ordering::AcqRel) {
            Err(std::io::Error::other("injected dial-thread failure"))
        } else {
            std::thread::Builder::new()
                .name("rpt-link-dial".into())
                .spawn(operation)
        };
        #[cfg(not(test))]
        let thread = std::thread::Builder::new()
            .name("rpt-link-dial".into())
            .spawn(operation);
        let thread = match thread {
            Ok(thread) => thread,
            Err(_) => {
                let job = job
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .ok_or(RuntimeError::Preparation)?;
                return self.complete_dial(job, revision);
            }
        };
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(thread);
        Ok(())
    }
    fn complete_dial(self: &Arc<Self>, job: DialJob, revision: u64) -> Result<(), RuntimeError> {
        let result = self.dial(job.local.clone(), job.attempt, revision);
        if job.report {
            self.services.command_notice(&job.local, result.is_ok());
        }
        if let Some(event) = job.event {
            let completion = self.run(move |host| {
                host.runtime.complete_event(&event);
                Ok(())
            });
            let _ =
                self.event_busy
                    .compare_exchange(revision, 0, Ordering::AcqRel, Ordering::Acquire);
            if result.is_ok() {
                completion?;
            }
        }
        result
    }
    fn enter(self: &Arc<Self>) -> Result<Call, RuntimeError> {
        let mut count = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        if !self.accepting.load(Ordering::Acquire) || self.reloading.load(Ordering::Acquire) {
            return Err(RuntimeError::Rejected);
        }
        *count += 1;
        Ok(Call(Arc::clone(self)))
    }
    pub(crate) fn run<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Host) -> Result<T, RuntimeError> + Send + 'static,
    ) -> Result<T, RuntimeError> {
        self.run_inner(false, operation)
    }
    fn run_inner<T: Send + 'static>(
        self: &Arc<Self>,
        reload: bool,
        operation: impl FnOnce(&mut Host) -> Result<T, RuntimeError> + Send + 'static,
    ) -> Result<T, RuntimeError> {
        self.run_erased(
            reload,
            Box::new(move |host| operation(host).map(|result| Box::new(result) as ErasedResult)),
        )
        .map(|result| *result.downcast::<T>().expect("operation result type"))
    }
    fn run_erased(
        self: &Arc<Self>,
        reload: bool,
        operation: HostOperation,
    ) -> Result<ErasedResult, RuntimeError> {
        let engine = Arc::clone(self);
        let revision = self.revision.load(Ordering::Acquire);
        let (send, receive) = mpsc::sync_channel(1);
        self.executor
            .submit(ControlTask::lifecycle(move || {
                let result = if !engine.accepting.load(Ordering::Acquire)
                    || (!reload && engine.reloading.load(Ordering::Acquire))
                    || engine.revision.load(Ordering::Acquire) != revision
                {
                    Err(RuntimeError::Rejected)
                } else {
                    let mut host = engine.host.lock().unwrap_or_else(|e| e.into_inner());
                    host.as_mut()
                        .ok_or(RuntimeError::Rejected)
                        .and_then(operation)
                };
                let _ = send.send(result);
            }))
            .map_err(|_| {
                self.lost_digits.store(true, Ordering::Release);
                RuntimeError::Rejected
            })?;
        receive.recv().map_err(|_| RuntimeError::Rejected)?
    }
    pub(crate) fn clock(&self) -> RuntimeClock {
        capture(self.origin, self.services)
    }
    pub(crate) fn current(&self, revision: u64) -> bool {
        self.accepting.load(Ordering::Acquire)
            && !self.reloading.load(Ordering::Acquire)
            && self.revision.load(Ordering::Acquire) == revision
    }
    fn pump(self: &Arc<Self>, clock: RuntimeClock, schedule: bool) -> Result<(), RuntimeError> {
        let revision = self.revision.load(Ordering::Acquire);
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let mut index = 0;
        while index < jobs.len() {
            if jobs[index].is_finished() {
                let _ = jobs.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        drop(jobs);
        let lost = self.lost_digits.swap(false, Ordering::AcqRel);
        let operations = self.run(move |host| {
            if lost {
                host.lost_digits();
            }
            host.pump(clock)?;
            if schedule {
                host.runtime.tick_links(clock);
            }
            Ok(host.take_operations())
        })?;
        for (local, operation) in operations {
            if self
                .operation(&local, operation)
                .and_then(|effect| self.background(local, effect, revision, true))
                .is_err()
            {
                self.lost_digits.store(true, Ordering::Release);
            }
        }
        if schedule {
            loop {
                let next = self.run(|host| Ok(host.runtime.next_link()))?;
                let Some((local, effect)) = next else {
                    break;
                };
                self.background(local, effect, revision, false)?;
            }
            let retries = self.run(move |host| {
                let mut retries = Vec::new();
                for local in host
                    .runtime
                    .node_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
                {
                    let node = host.runtime.node(&local).ok_or(RuntimeError::MissingNode)?;
                    let work = node
                        .work()
                        .expect("configured node retains an active generation");
                    if let Some(retry) = node.links().take_retry(clock.now_ms, work) {
                        retries.push((local, retry));
                    }
                }
                Ok(retries)
            })?;
            for (local, retry) in retries {
                self.spawn_dial(local, Dial::Retry(retry), revision, None, false)?;
            }
            loop {
                let event = self.run(move |host| host.runtime.next_event(clock))?;
                let Some(event) = event else {
                    break;
                };
                if self.event_busy.load(Ordering::Acquire) == revision {
                    break;
                }
                let local = event.local().to_owned();
                let (effect, event) = self.run(move |host| {
                    host.runtime.queue_event(&event, &host.media)?;
                    let effect = host.runtime.event_command(&event, clock);
                    Ok((effect, event))
                })?;
                match effect {
                    Ok(LinkEffect::Connect(attempt)) => {
                        self.event_busy.store(revision, Ordering::Release);
                        if self
                            .spawn_dial(local, Dial::Connect(attempt), revision, Some(event), false)
                            .is_err()
                        {
                            self.event_busy.store(0, Ordering::Release);
                        }
                        break;
                    }
                    effect => {
                        if let Ok(effect) = effect {
                            let _ = self.effect(local, effect, revision);
                        }
                        self.run(move |host| {
                            host.runtime.complete_event(&event);
                            Ok(())
                        })?;
                    }
                }
            }
        }
        Ok(())
    }
    fn start_ticker(self: &Arc<Self>) -> Result<(), RuntimeError> {
        #[cfg(test)]
        if SKIP_TICKER_THREAD.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        #[cfg(test)]
        if FAIL_TICKER_THREAD.swap(false, Ordering::AcqRel) {
            return Err(RuntimeError::Preparation);
        }
        let engine = Arc::clone(self);
        let thread = std::thread::Builder::new()
            .name("rpt-control-tick".into())
            .spawn(move || {
                #[cfg(test)]
                if PANIC_TICKER_THREAD.swap(false, Ordering::AcqRel) {
                    panic!("injected ticker failure");
                }
                let mut last_second = None;
                let mut failed_wall = false;
                while engine.accepting.load(Ordering::Acquire) {
                    let clock = engine.clock();
                    let schedule = if clock.civil.is_none() {
                        let first = !failed_wall;
                        failed_wall = true;
                        first
                    } else {
                        failed_wall = false;
                        last_second != Some(clock.wall_seconds)
                    };
                    if schedule {
                        last_second = Some(clock.wall_seconds);
                    }
                    if !engine.reloading.load(Ordering::Acquire) {
                        let _ = engine.pump(clock, schedule);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
            .map_err(|_| RuntimeError::Preparation)?;
        *self.ticker.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
        Ok(())
    }
    fn stop(self: &Arc<Self>) -> bool {
        self.accepting.store(false, Ordering::Release);
        self.revision.fetch_add(1, Ordering::AcqRel);
        if let Some(ticker) = self.ticker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if ticker.join().is_err() {
                return false;
            }
        }
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        while *calls != 0 {
            calls = self.drained.wait(calls).unwrap_or_else(|e| e.into_inner());
        }
        drop(calls);
        for job in self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            if job.join().is_err() {
                return false;
            }
        }
        let engine = Arc::clone(self);
        let (send, receive) = mpsc::sync_channel(1);
        if self
            .executor
            .submit(ControlTask::lifecycle(move || {
                let mut host = engine.host.lock().unwrap_or_else(|e| e.into_inner());
                #[cfg(test)]
                let stop_mode = HOST_STOP_MODE.swap(0, Ordering::AcqRel);
                #[cfg(test)]
                if stop_mode == 2 {
                    panic!("injected host-stop failure");
                }
                let stopped = host
                    .as_mut()
                    .is_none_or(|host| host.stop(engine.clock().now_ms));
                #[cfg(test)]
                let stopped = match stop_mode {
                    1 => false,
                    _ => stopped,
                };
                if stopped {
                    *host = None;
                }
                let _ = send.send(stopped);
            }))
            .is_err()
        {
            return false;
        }
        if receive.recv() != Ok(true) || self.executor.stop_and_drain().is_err() {
            return false;
        }
        true
    }
}

pub(crate) fn selected() -> Result<(Arc<Engine>, Call), RuntimeError> {
    let engine = ENGINE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
        .ok_or(RuntimeError::Rejected)?;
    let call = engine.enter()?;
    Ok((engine, call))
}
fn capture(origin: Instant, services: HostServices) -> RuntimeClock {
    capture_at(origin, SystemTime::now(), services)
}
fn capture_at(origin: Instant, wall: SystemTime, services: HostServices) -> RuntimeClock {
    let wall_seconds = wall
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(-1);
    let civil = (wall_seconds >= 0)
        .then(|| services.local_time(wall_seconds))
        .flatten();
    RuntimeClock {
        now_ms: origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        wall_seconds,
        civil,
    }
}
fn document(text: &str) -> Result<ConfigDocument, RuntimeError> {
    let document = ConfigDocument::parse(text)?;
    Schema::validate(&document)?;
    Ok(document)
}

unsafe fn input<'a>(
    pointer: *const std::ffi::c_char,
    length: usize,
) -> Result<&'a str, RuntimeError> {
    if pointer.is_null() && length != 0 {
        return Err(RuntimeError::Preparation);
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer.cast(), length) }
    };
    std::str::from_utf8(bytes).map_err(|_| RuntimeError::Preparation)
}

unsafe fn incoming_identity(
    local: *const std::ffi::c_char,
    local_length: usize,
    remote: *const std::ffi::c_char,
    remote_length: usize,
    source: *const std::ffi::c_char,
    source_length: usize,
) -> Result<(String, String, String), RuntimeError> {
    Ok((
        unsafe { input(local, local_length) }?.to_owned(),
        unsafe { input(remote, remote_length) }?.to_owned(),
        unsafe { input(source, source_length) }?.to_owned(),
    ))
}

/// Prepare and register the product using the loader's real module and mandatory capabilities.
///
/// # Safety
/// The complete descriptor/code allocations must remain live through successful stop.
unsafe extern "C" fn rptadv_product_start(
    host: *const abi::rptadv_host_services_v1,
    control: *const ControlDescriptor,
    file: *const FileDescriptor,
    speech: *const SpeechDescriptor,
    configuration: *const std::ffi::c_char,
    configuration_length: usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let mut selected = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if selected.is_some() {
            return Err(RuntimeError::Busy);
        }
        let services =
            unsafe { HostServices::open(host) }.map_err(|_| RuntimeError::Preparation)?;
        let executor = unsafe { ControlClient::open(control, "rpt_advanced/links", 512) }
            .map_err(|_| RuntimeError::Preparation)?;
        let media = unsafe {
            NativeMediaPreparer::from_descriptors(
                file,
                speech,
                Path::new("/usr/bin/ffmpeg"),
                Path::new("/usr/bin/piper"),
                Path::new("/tmp"),
                30000,
                (services.reaper_acquire(), services.reaper_release()),
            )
        }
        .map_err(|_| RuntimeError::Preparation)?;
        // Mandatory released composition is validated even for a disabled configuration.
        drop(crate::link::ring::InboundRing::open(48000).map_err(|_| RuntimeError::Preparation)?);
        drop(crate::link::egress::Egress::new(48000, 960).map_err(|_| RuntimeError::Preparation)?);
        let document = document(unsafe { input(configuration, configuration_length) }?)?;
        let origin = Instant::now();
        let clock = capture(origin, services);
        let engine = Arc::new(Engine {
            executor,
            host: Mutex::new(None),
            services,
            origin,
            accepting: AtomicBool::new(true),
            reloading: AtomicBool::new(false),
            revision: AtomicU64::new(1),
            lost_digits: AtomicBool::new(false),
            event_busy: AtomicU64::new(0),
            calls: Mutex::new(0),
            drained: Condvar::new(),
            ticker: Mutex::new(None),
            jobs: Mutex::new(Vec::new()),
        });
        let target = Arc::clone(&engine);
        let (send, receive) = mpsc::sync_channel(1);
        engine
            .executor
            .submit(ControlTask::lifecycle(move || {
                let result = Host::start(document, media, services, origin, clock);
                let result = result.map(|host| {
                    *target.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(host)
                });
                let _ = send.send(result);
            }))
            .map_err(|_| RuntimeError::Rejected)?;
        if let Err(error) = receive.recv().unwrap_or(Err(RuntimeError::Rejected)) {
            if !engine.stop() {
                *selected = Some(engine);
                return Ok(());
            }
            return Err(error);
        }
        if let Err(error) = engine.start_ticker() {
            if !engine.stop() {
                *selected = Some(engine);
                return Ok(());
            }
            return Err(error);
        }
        *selected = Some(engine);
        Ok(())
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(1, |()| 0)
}
/// Prepare a replacement without discarding the live configuration on failure.
unsafe extern "C" fn rptadv_product_reload(
    configuration: *const std::ffi::c_char,
    configuration_length: usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let document = document(unsafe { input(configuration, configuration_length) }?)?;
        let (engine, _call) = selected()?;
        engine.reloading.store(true, Ordering::Release);
        engine.revision.fetch_add(1, Ordering::AcqRel);
        engine.lost_digits.store(true, Ordering::Release);
        let clock = engine.clock();
        let result = catch_unwind(AssertUnwindSafe(|| {
            engine.run_inner(true, move |host| host.reload(document, clock))
        }))
        .unwrap_or(Err(RuntimeError::Preparation));
        engine.reloading.store(false, Ordering::Release);
        if result.is_ok() {
            let _ = engine.pump(engine.clock(), true);
        }
        result
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}
/// Gate admission, stop/join owners, and drain accepted tasks before releasing registrations.
extern "C" fn rptadv_product_stop() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let mut selected = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(engine) = selected.as_ref() else {
            return 0;
        };
        if !engine.stop() {
            return -1;
        }
        selected.take();
        0
    }))
    .unwrap_or(-1)
}

/// Check current policy and topology before the adapter answers an incoming channel.
unsafe extern "C" fn rptadv_product_authorize_incoming(
    local: *const std::ffi::c_char,
    local_length: usize,
    remote: *const std::ffi::c_char,
    remote_length: usize,
    source: *const std::ffi::c_char,
    source_length: usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let (engine, _call) = selected()?;
        let (local, remote, source) = unsafe {
            incoming_identity(
                local,
                local_length,
                remote,
                remote_length,
                source,
                source_length,
            )
        }?;
        engine.run(move |host| {
            host.lookup(&local, &remote, Some(&source))?;
            host.runtime.authorize_incoming(&local, &remote, true)
        })
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

/// Admit one already-inspected and answered peer. Peer ownership always transfers.
unsafe extern "C" fn rptadv_product_incoming(
    local: *const std::ffi::c_char,
    local_length: usize,
    remote: *const std::ffi::c_char,
    remote_length: usize,
    source: *const std::ffi::c_char,
    source_length: usize,
    peer: *mut c_void,
) -> i32 {
    let consumed = std::cell::Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let (engine, _call) = selected()?;
        let (local, remote, source) = unsafe {
            incoming_identity(
                local,
                local_length,
                remote,
                remote_length,
                source,
                source_length,
            )
        }?;
        let peer = unsafe { engine.services.peer(peer) }.map_err(|_| RuntimeError::Rejected)?;
        consumed.set(true);
        let clock = engine.clock();
        engine.run(move |host| {
            host.lookup(&local, &remote, Some(&source))?;
            host.runtime.incoming(&local, &remote, true)?;
            let result = host.attach_peer(
                &local,
                &remote,
                rpt_advanced_core::link::Mode::TRANSCEIVE,
                peer,
                clock,
            );
            if result.is_err() {
                host.reject_peer(&local, &remote, clock.now_ms);
            }
            result
        })
    }));
    match result {
        Ok(Ok(())) => 0,
        _ if consumed.get() => -1,
        _ => 1,
    }
}

/// Execute one existing link command selected by the public adapter spelling.
unsafe extern "C" fn rptadv_product_link_command(
    local: *const std::ffi::c_char,
    local_length: usize,
    remote: *const std::ffi::c_char,
    remote_length: usize,
    action: u32,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let action = match action {
            1 => LinkAction::Transceive,
            2 => LinkAction::Monitor,
            3 => LinkAction::LocalMonitor,
            4 => LinkAction::Disconnect,
            _ => return Err(RuntimeError::Rejected),
        };
        let (engine, _call) = selected()?;
        engine.execute(
            unsafe { input(local, local_length) }?.to_owned(),
            DigitOperation {
                command: rpt_advanced_core::command::Command {
                    action,
                    node: unsafe { input(remote, remote_length) }?.to_owned(),
                },
                digit: None,
            },
        )
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

/// Emit one complete copied link-status snapshot.
unsafe extern "C" fn rptadv_product_link_status(
    local: *const std::ffi::c_char,
    local_length: usize,
    sink: abi::rptadv_text_sink_v1,
    sink_context: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let sink = sink.ok_or(RuntimeError::Rejected)?;
        let (engine, _call) = selected()?;
        let local = unsafe { input(local, local_length) }?.to_owned();
        let text = engine.run(move |host| host.status_text(&local))?;
        unsafe { sink(sink_context, text.as_ptr().cast(), text.len()) };
        Ok(())
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

/// Feed one validated DTMF digit and report whether a complete command executed.
unsafe extern "C" fn rptadv_product_digit(
    local: *const std::ffi::c_char,
    local_length: usize,
    digit: u8,
    completed: *mut u32,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        if completed.is_null() || !b"0123456789ABCD*#".contains(&digit) {
            return Err(RuntimeError::Rejected);
        }
        let (engine, _call) = selected()?;
        let local = unsafe { input(local, local_length) }?.to_owned();
        let name = local.clone();
        let operation = engine.run(move |host| {
            Ok(host.runtime.digit(
                &name,
                rpt_advanced_core::runtime::dtmf::DigitEvent::Digit {
                    digit: digit as char,
                    now_ms: 0,
                },
            ))
        })?;
        let value = if let Some(operation) = operation {
            engine.execute(local, operation)?;
            1
        } else {
            0
        };
        unsafe { completed.write(value) };
        Ok(())
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

static DESCRIPTOR: abi::rptadv_product_descriptor_v1 = abi::rptadv_product_descriptor_v1 {
    struct_size: size_of::<abi::rptadv_product_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.product\0\0",
    start: Some(rptadv_product_start),
    reload: Some(rptadv_product_reload),
    stop: Some(rptadv_product_stop),
    authorize_incoming: Some(rptadv_product_authorize_incoming),
    incoming: Some(rptadv_product_incoming),
    link_command: Some(rptadv_product_link_command),
    link_status: Some(rptadv_product_link_status),
    digit: Some(rptadv_product_digit),
};

/// Return immutable process-lifetime product metadata.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_product_descriptor_v1() -> *const abi::rptadv_product_descriptor_v1 {
    &DESCRIPTOR
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
