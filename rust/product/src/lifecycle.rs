//! Serialized product host lifecycle. No callback unwinds across the public ABI.
use crate::{
    abi,
    control::{ControlClient, Descriptor as ControlDescriptor},
    host::{Host, PendingLinkDial, PhoneLinkReply},
    media::{FileDescriptor, NativeMediaPreparer, SpeechDescriptor},
    scheduler_health::SchedulerHealth,
    services::HostServices,
};
use rpt_advanced_core::{
    command::LinkAction,
    runtime::{
        dtmf::DigitOperation,
        links::{ConnectAttempt, LinkEffect, RetryWork},
    },
};
use rpt_advanced_core::{
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

#[cfg(target_os = "linux")]
#[path = "lifecycle_asl.rs"]
pub(crate) mod asl;

#[cfg(test)]
static FAIL_DIAL_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static SKIP_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static FAIL_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static PANIC_TICKER_THREAD: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static HOST_STOP_MODE: AtomicU64 = AtomicU64::new(0);

static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);

fn spawn_dial(operation: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    #[cfg(test)]
    if FAIL_DIAL_THREAD.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other("injected dial-thread failure"));
    }
    std::thread::Builder::new()
        .name("rpt-link-dial".into())
        .spawn(operation)
}

#[cfg(target_os = "linux")]
#[path = "lifecycle_bootstrap.rs"]
pub(crate) mod bootstrap;
#[cfg(target_os = "linux")]
#[path = "lifecycle_management.rs"]
mod management;
#[cfg(target_os = "linux")]
#[path = "lifecycle_native.rs"]
mod native;
#[path = "lifecycle_site.rs"]
mod site;
#[cfg(target_os = "linux")]
pub(crate) use native::PreparedSite;
#[cfg(test)]
pub(crate) use site::{
    install_delayed_site_bank, site_bank_delay, site_bank_electrical, site_bank_sample,
};

pub(crate) struct Engine {
    executor: ControlClient,
    host: Mutex<Option<Host>>,
    services: HostServices,
    origin: Instant,
    health: SchedulerHealth,
    accepting: Arc<AtomicBool>,
    reloading: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
    lost_digits: AtomicBool,
    calls: Mutex<usize>,
    drained: Condvar,
    ticker: Mutex<Option<JoinHandle<()>>>,
    jobs: Mutex<Vec<JoinHandle<()>>>,
    // The service and its management writer both retain this Engine. A failed
    // shutdown keeps the global owner; the last drained owner releases the lease.
    #[cfg(target_os = "linux")]
    _managed_store: Option<Arc<crate::management::ManagedStore>>,
    #[cfg(target_os = "linux")]
    management_state: management::ManagementState,
}
/// Read-only fence for asynchronous media and dial receipts. The atomics cannot keep the Engine alive.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(crate) struct MediaAdmission {
    accepting: Arc<AtomicBool>,
    reloading: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
    expected: u64,
}

#[cfg(target_os = "linux")]
impl MediaAdmission {
    pub(crate) fn current(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
            && !self.reloading.load(Ordering::Acquire)
            && self.revision.load(Ordering::Acquire) == self.expected
    }
}

pub(crate) struct Call(Arc<Engine>);
enum Dial {
    Connect(ConnectAttempt),
    Retry(RetryWork),
}
enum DialTarget {
    Host {
        destination: String,
        wire_local: String,
    },
    #[cfg(target_os = "linux")]
    Managed(Box<crate::host::asl::Account>, Option<u64>),
}
struct DialJob {
    local: String,
    attempt: Dial,
    report: bool,
}
impl Dial {
    fn phone_reply(&self) -> Option<&rpt_advanced_core::link::PeerReply> {
        match self {
            Self::Connect(work) => work.phone_reply(),
            Self::Retry(work) => work.phone_reply(),
        }
    }

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
    fn current(&self, now_ms: u64) -> bool {
        match self {
            Self::Connect(work) => work.current_at(now_ms),
            Self::Retry(work) => work.current_at(now_ms),
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
    fn new(services: HostServices, executor: ControlClient, origin: Instant) -> Self {
        Self {
            executor,
            host: Mutex::new(None),
            services,
            origin,
            health: SchedulerHealth::default(),
            accepting: Arc::new(AtomicBool::new(true)),
            reloading: Arc::new(AtomicBool::new(false)),
            revision: Arc::new(AtomicU64::new(1)),
            lost_digits: AtomicBool::new(false),
            calls: Mutex::new(0),
            drained: Condvar::new(),
            ticker: Mutex::new(None),
            jobs: Mutex::new(Vec::new()),
            #[cfg(target_os = "linux")]
            _managed_store: None,
            #[cfg(target_os = "linux")]
            management_state: Default::default(),
        }
    }

    /// Start the serialized control owner without selecting a radio configuration.
    /// The process owner retains the descriptors until successful stop and drain.
    #[cfg(target_os = "linux")]
    pub(crate) unsafe fn start_management(
        store: Arc<crate::management::ManagedStore>,
        host: *const abi::rptadv_host_services_v11,
        control: *const ControlDescriptor,
        file: *const FileDescriptor,
        speech: *const SpeechDescriptor,
    ) -> Result<Arc<Self>, RuntimeError> {
        // SAFETY: the caller supplies process-lifetime descriptor storage for all four owners.
        let (services, executor, media) = unsafe { host_components(host, control, file, speech) }?;
        let origin = Instant::now();
        let engine = Arc::new(Self {
            _managed_store: Some(store),
            ..Self::new(services, executor, origin)
        });
        let target = Arc::clone(&engine);
        let (send, receive) = mpsc::sync_channel(1);
        engine.enqueue(move || {
            let result = Host::management_only(media, services, origin).map(|host| {
                let gate = host.runtime.configuration_gate();
                gate.store(true, Ordering::Release);
                target.management_state.install_gate(gate);
                *target
                    .host
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(host);
            });
            let _ = send.send(result);
        })?;
        receive.recv().map_err(|_| RuntimeError::Rejected)??;
        Ok(engine)
    }

    pub(crate) fn execute(
        self: &Arc<Self>,
        local: String,
        operation: DigitOperation,
    ) -> Result<(), RuntimeError> {
        self.execute_with_source(local, operation, false)
    }
    fn execute_digit(
        self: &Arc<Self>,
        local: String,
        outcome: rpt_advanced_core::runtime::dtmf::DigitOutcome,
    ) -> Result<(), RuntimeError> {
        if let rpt_advanced_core::runtime::dtmf::DigitOutcome::Operation(operation) = outcome {
            return self.execute_with_source(local, operation, true);
        }
        let revision = self.revision.load(Ordering::Acquire);
        let effect = self.digit_effect(&local, outcome)?;
        self.effect(local, effect, revision)
    }
    fn digit_effect(
        self: &Arc<Self>,
        local: &str,
        outcome: rpt_advanced_core::runtime::dtmf::DigitOutcome,
    ) -> Result<LinkEffect, RuntimeError> {
        use rpt_advanced_core::runtime::dtmf::DigitOutcome;
        match outcome {
            DigitOutcome::Mapped(input) => {
                let name = local.to_owned();
                let clock = self.clock();
                self.run(move |host| host.runtime.dispatch_mapping(&name, input, clock))
            }
            DigitOutcome::MacroStopped { reply } => {
                let local = local.to_owned();
                self.run(move |host| host.runtime.queue_macro_stopped(&local, reply))?;
                Ok(LinkEffect::None)
            }
            DigitOutcome::Operation(operation) => self.operation(local, operation),
            DigitOutcome::RemoteCandidate(operation) => {
                self.operation(local, operation).or(Ok(LinkEffect::None))
            }
            DigitOutcome::PadTest(report) => {
                let local = local.to_owned();
                self.run(move |host| host.runtime.queue_pad_test(&local, &report))?;
                Ok(LinkEffect::None)
            }
            DigitOutcome::ReceivedDigit(symbol, origin) => {
                let local = local.to_owned();
                self.run(move |host| {
                    host.regenerate_digit(
                        &local,
                        crate::link::regeneration::RegenerationDigit {
                            symbol,
                            origin: Some(origin),
                        },
                    )
                })?;
                Ok(LinkEffect::None)
            }
            DigitOutcome::Regenerate(digit) => {
                let local = local.to_owned();
                self.run(move |host| host.regenerate(&local, digit))?;
                Ok(LinkEffect::None)
            }
        }
    }
    fn execute_with_source(
        self: &Arc<Self>,
        local: String,
        operation: DigitOperation,
        dtmf_source: bool,
    ) -> Result<(), RuntimeError> {
        let revision = self.revision.load(Ordering::Acquire);
        let peer = operation.command.node.clone();
        let reply = operation.reply.clone();
        match self.operation(&local, operation) {
            Ok(effect) => self.effect(local, effect, revision),
            Err(RuntimeError::Link(rpt_advanced_core::link::AdmissionError::Loop))
                if dtmf_source =>
            {
                let now_ms = self.clock().now_ms;
                self.run(move |host| {
                    if let Some(reply) = &reply {
                        host.runtime.echo_response_requested(&local, reply, now_ms);
                    }
                    host.runtime.queue_loop_rejected(&local, &peer, reply)
                })
            }
            Err(RuntimeError::Link(rpt_advanced_core::link::AdmissionError::UnsupportedRoute))
                if dtmf_source =>
            {
                let now_ms = self.clock().now_ms;
                self.run(move |host| {
                    if let Some(reply) = &reply {
                        host.runtime.echo_response_requested(&local, reply, now_ms);
                    }
                    host.runtime
                        .queue_remote_command_unsupported(&local, &peer, reply)
                })
            }
            Err(error) => Err(error),
        }
    }
    fn operation(
        self: &Arc<Self>,
        local: &str,
        operation: DigitOperation,
    ) -> Result<LinkEffect, RuntimeError> {
        let local = local.to_owned();
        let clock = self.clock();
        self.run(move |host| host.command(&local, operation, clock))
    }
    fn effect(
        self: &Arc<Self>,
        local: String,
        effect: LinkEffect,
        revision: u64,
    ) -> Result<(), RuntimeError> {
        match effect {
            LinkEffect::Connect(attempt) => self.dial(local, Dial::Connect(attempt), revision),
            #[cfg(target_os = "linux")]
            LinkEffect::PhoneDial(number, permit) => {
                self.begin_phone_dial(local, number, permit, revision, false)
            }
            #[cfg(target_os = "linux")]
            LinkEffect::PhoneHangup(permit) => {
                let engine = Arc::clone(self);
                self.run(move |host| {
                    if !engine.current(revision) || !permit.current(Some(engine.elapsed_ms())) {
                        return Err(RuntimeError::Rejected);
                    }
                    host.hangup_outgoing_phone(&local)
                })
            }
            effect => {
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
    }
    #[cfg(target_os = "linux")]
    fn begin_phone_dial(
        self: &Arc<Self>,
        local: String,
        number: rpt_advanced_core::command_language::ProtectedOperand<String>,
        permit: rpt_advanced_core::runtime::dtmf::ActionPermit,
        revision: u64,
        report: bool,
    ) -> Result<(), RuntimeError> {
        let engine = Arc::clone(self);
        self.run(move |host| {
            if !engine.current(revision) || !permit.current(Some(engine.elapsed_ms())) {
                return Err(RuntimeError::Rejected);
            }
            host.begin_phone_dial(
                local,
                &number,
                Arc::downgrade(&engine),
                permit,
                revision,
                report,
            )
        })
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
        let destination = if self.current(revision) && attempt.current(self.elapsed_ms()) {
            self.run(move |host| {
                #[cfg(target_os = "linux")]
                if host.runtime.modular_configuration().is_some()
                    && rpt_advanced_core::link::PeerIdentity::from_operator(&destination_remote)
                        .map_err(|_| RuntimeError::Rejected)?
                        .protocol()
                        == rpt_advanced_core::link::ProtocolId::AllStarLink
                {
                    let account =
                        host.asl_account(name.parse().map_err(|_| RuntimeError::Rejected)?)?;
                    if !account.permits(&destination_remote) {
                        return Err(RuntimeError::Rejected);
                    }
                    return Ok(DialTarget::Managed(
                        Box::new(account),
                        host.handoff_owner(&name),
                    ));
                }
                Ok(DialTarget::Host {
                    destination: host.lookup(&name, &destination_remote, None)?,
                    wire_local: host.wire_local(&name, &destination_remote)?,
                })
            })
        } else {
            Err(RuntimeError::Rejected)
        };
        #[cfg(target_os = "linux")]
        let mut managed = None;
        #[cfg(target_os = "linux")]
        let mut observed_asl = None;
        let current = || self.current(revision) && attempt.current(self.elapsed_ms());
        let io = destination
            .ok()
            .filter(|_| current())
            .and_then(|target| match target {
                DialTarget::Host {
                    destination,
                    wire_local,
                } => self
                    .services
                    .dial(
                        &destination,
                        &wire_local,
                        crate::host::MAXIMUM_FRAMES,
                        current,
                    )
                    .ok(),
                #[cfg(target_os = "linux")]
                DialTarget::Managed(account, generation) => {
                    observed_asl =
                        generation.map(|generation| crate::host::status::PendingAslDial {
                            station: account.station,
                            account: account.identity(),
                            generation,
                            admission: self.media_admission(revision),
                        });
                    let (peer, proof) =
                        asl::Outbound::originate(self, *account, &remote, current).ok()?;
                    managed = Some(proof);
                    Some(peer)
                }
            });
        let engine = Arc::clone(self);
        self.run_inner(true, move |host| {
            let clock = engine.clock();
            let mode = attempt.mode();
            let current = engine.current(revision);
            #[cfg(target_os = "linux")]
            let io = io.filter(|_| {
                managed.as_ref().is_none_or(|proof| {
                    current && attempt.current(clock.now_ms) && proof.current(host, &remote)
                })
            });
            #[cfg(target_os = "linux")]
            let observed_asl = observed_asl.filter(|_| current && attempt.current(clock.now_ms));
            let phone = if current {
                attempt.phone_reply().and_then(|reply| {
                    let generation = host.runtime.node(&local)?.command_work()?;
                    Some(PhoneLinkReply {
                        reply: reply.clone(),
                        generation,
                        report_failure: matches!(attempt, Dial::Connect(_)),
                    })
                })
            } else {
                None
            };
            let mut dial = PendingLinkDial {
                #[cfg(target_os = "linux")]
                asl: observed_asl,
                phone,
            };
            let attached = match attempt {
                Dial::Connect(attempt) if !current => {
                    if let Some(node) = host.runtime.node(&local) {
                        node.links().cancel_connect(attempt);
                    }
                    return Err(RuntimeError::Rejected);
                }
                Dial::Connect(attempt) => {
                    host.runtime
                        .finish_connect(&local, attempt, io.is_some(), clock)
                }
                Dial::Retry(attempt) => host
                    .runtime
                    .node(&local)
                    .ok_or(RuntimeError::MissingNode)?
                    .links()
                    .finish_retry(attempt, current && io.is_some(), clock.now_ms)
                    .map_err(RuntimeError::Link),
            };
            let result = attached.and_then(|attached| {
                if !attached {
                    return Err(RuntimeError::Rejected);
                }
                let result = host.attach_peer_observed(
                    &local,
                    &remote,
                    mode,
                    io.ok_or(RuntimeError::Rejected)?,
                    clock,
                    &mut dial,
                );
                if result.is_err() {
                    host.reject_peer(&local, &remote, clock.now_ms);
                }
                result
            });
            if result.is_err() {
                host.settle_link_dial(dial, &local, &remote, false);
            }
            result
        })
    }
    fn background(
        self: &Arc<Self>,
        local: String,
        effect: LinkEffect,
        revision: u64,
        report: bool,
    ) -> Result<(), RuntimeError> {
        match effect {
            LinkEffect::Connect(attempt) => {
                self.spawn_dial(local, Dial::Connect(attempt), revision, report)
            }
            #[cfg(target_os = "linux")]
            LinkEffect::PhoneDial(number, permit) => {
                self.begin_phone_dial(local, number, permit, revision, report)
            }
            effect => {
                let result = self.effect(local.clone(), effect, revision);
                if report {
                    self.services.command_notice(&local, result.is_ok());
                }
                result
            }
        }
    }
    fn spawn_dial(
        self: &Arc<Self>,
        local: String,
        attempt: Dial,
        revision: u64,
        report: bool,
    ) -> Result<(), RuntimeError> {
        let call = self.enter()?;
        let engine = Arc::clone(self);
        let job = Arc::new(Mutex::new(Some(DialJob {
            local,
            attempt,
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
        let thread = spawn_dial(operation);
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
        result
    }
    fn enter(self: &Arc<Self>) -> Result<Call, RuntimeError> {
        self.enter_reload(false)
    }
    fn enter_reload(self: &Arc<Self>, reload: bool) -> Result<Call, RuntimeError> {
        let mut count = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        if !self.accepting.load(Ordering::Acquire)
            || !reload && self.reloading.load(Ordering::Acquire)
        {
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
        self.enqueue(move || {
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
        })
        .inspect_err(|_| {
            self.lost_digits.store(true, Ordering::Release);
        })?;
        receive.recv().map_err(|_| RuntimeError::Rejected)?
    }
    /// Observe the shared executor without adding work or changing captured policy time.
    fn enqueue(
        self: &Arc<Self>,
        operation: impl FnOnce() + Send + 'static,
    ) -> Result<(), RuntimeError> {
        let engine = Arc::clone(self);
        let enqueued = Instant::now();
        self.executor
            .submit(ControlTask::lifecycle(move || {
                engine.queue_started(enqueued);
                operation();
            }))
            .map_err(|rejected| {
                if self.health.submission_failed(self.elapsed_ms()) {
                    self.services.scheduler_notice(&format!(
                        "scheduler control task submission failed: {:?}",
                        rejected.reason
                    ));
                }
                RuntimeError::Rejected
            })
    }
    /// Publish latency only when its independent monotonic warning budget allows it.
    fn queue_started(&self, enqueued: Instant) {
        let delay_us = enqueued.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        if self.health.started(delay_us, self.elapsed_ms()) {
            self.services.scheduler_notice(&format!(
                "scheduler control queue start delayed {delay_us} us (warning threshold 1000000 us)"
            ));
        }
    }
    /// Use monotonic process time for diagnostics, independent of civil-clock steps.
    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
    pub(crate) fn clock(&self) -> RuntimeClock {
        capture(self.origin, self.services)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn media_admission(&self, expected: u64) -> MediaAdmission {
        MediaAdmission {
            accepting: Arc::clone(&self.accepting),
            reloading: Arc::clone(&self.reloading),
            revision: Arc::clone(&self.revision),
            expected,
        }
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
        let (operations, site_links, lookups) = self.run(move |host| {
            if lost {
                host.lost_digits();
            }
            host.pump(clock)?;
            #[cfg(target_os = "linux")]
            host.poll_phone_dials(clock);
            if schedule {
                host.runtime.tick_links(clock);
            }
            #[cfg(target_os = "linux")]
            let lookups = host.take_target_lookups();
            #[cfg(not(target_os = "linux"))]
            let lookups = ();
            Ok((host.take_operations(), host.take_site_links(), lookups))
        })?;
        #[cfg(target_os = "linux")]
        for lookup in lookups {
            let services = self.services;
            // A spawn failure disconnects the one-shot response; never resolve on control.
            if let Ok(job) = spawn_dial(move || lookup.resolve(services)) {
                self.jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(job);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = lookups;
        for (local, effect) in site_links {
            self.background(local, effect, revision, false)?;
        }
        for (local, operation) in operations {
            let mapped = matches!(
                operation,
                rpt_advanced_core::runtime::dtmf::DigitOutcome::Mapped(_)
            );
            if self
                .digit_effect(&local, operation)
                .and_then(|effect| self.background(local, effect, revision, true))
                .is_err()
                && !mapped
            {
                self.lost_digits.store(true, Ordering::Release);
            }
        }
        let locals = self.run(|host| {
            Ok(host
                .runtime
                .node_names()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>())
        })?;
        for local in locals {
            let name = local.clone();
            let origin = self.origin;
            if let Some((effect, receipt)) = self.run(move |host| {
                // A management action may have arrived after the ticker sampled the wall clock.
                let now_ms = origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                let at_dispatch = RuntimeClock {
                    now_ms: clock.now_ms.max(now_ms),
                    ..clock
                };
                host.runtime.advance_commands(&name, at_dispatch)
            })? {
                if let LinkEffect::RemoteBase(operation) = effect {
                    // This provider returns a completion ticket, independently of queue admission.
                    // A rejected or abandoned submission drops the receipt and ends the macro.
                    let _ = self.run(move |host| {
                        host.submit_command_remote(&local, operation, clock, Some(receipt))
                    });
                } else if let LinkEffect::VerifyCommandTarget(operation) = effect {
                    let engine = Arc::clone(self);
                    let _ = self.run(move |host| {
                        if !engine.current(revision) {
                            return Err(RuntimeError::Rejected);
                        }
                        host.command_with_receipt(&local, operation, engine.clock(), Some(receipt))
                            .map(drop)
                    });
                } else {
                    receipt.settle(self.background(local, effect, revision, false).is_ok());
                }
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
                    // A failed station restore retains its configuration, but has no radio to retry.
                    if let Some(work) = node.work()
                        && let Some(retry) = node.links().take_retry(clock.now_ms, work)
                    {
                        retries.push((local, retry));
                    }
                }
                Ok(retries)
            })?;
            for (local, retry) in retries {
                self.spawn_dial(local, Dial::Retry(retry), revision, false)?;
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
                        engine.pump_observed(clock, schedule);
                    } else {
                        // Hardware readiness cannot suspend existing peers or other
                        // radios. Only configuration/scheduled-action publication waits.
                        let _ = engine.run_inner(true, move |host| host.pump(clock));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
            .map_err(|_| RuntimeError::Preparation)?;
        *self.ticker.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
        Ok(())
    }
    /// Report live-cycle failures; expected reload/unload retirement is not a fault.
    fn pump_observed(self: &Arc<Self>, clock: RuntimeClock, schedule: bool) {
        let revision = self.revision.load(Ordering::Acquire);
        if let Err(error) = self.pump(clock, schedule) {
            if self.current(revision) && self.health.cycle_failed(self.elapsed_ms()) {
                self.services
                    .scheduler_notice(&format!("scheduler control cycle failed: {error:?}"));
            }
        }
    }
    pub(crate) fn stop(self: &Arc<Self>) -> bool {
        self.accepting.store(false, Ordering::Release);
        // Callbacks outlive control admission while site safe-output and device
        // retirement are pending. Latch their independent TX gate before joining.
        #[cfg(target_os = "linux")]
        self.inhibit_modular();
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
        #[cfg(target_os = "linux")]
        {
            let pending = self
                .host
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(Host::has_target_selections);
            if pending && self.site_control(Host::cancel_target_selections).is_err() {
                return false;
            }
        }
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            // Directory callbacks have no cancellation ABI. Retain providers and retry
            // through the existing stop poll; joining here must never wait for a resolver.
            if jobs.iter().any(|job| !job.is_finished()) {
                return false;
            }
            for job in jobs.drain(..) {
                if job.join().is_err() {
                    return false;
                }
            }
        }
        if !self.stop_sites() {
            return false;
        }
        let engine = Arc::clone(self);
        let (send, receive) = mpsc::sync_channel(1);
        if self
            .enqueue(move || {
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
                let retired = if stopped {
                    host.as_mut()
                        .map(|host| (host.take_status_posts(), host.take_sites()))
                } else {
                    None
                };
                if stopped {
                    *host = None;
                }
                let _ = send.send((stopped, retired));
            })
            .is_err()
        {
            return false;
        }
        let Ok((true, retired)) = receive.recv() else {
            return false;
        };
        if self.executor.stop_and_drain().is_err() {
            return false;
        }
        // HTTP and physical-device owners may still be leaving their worker loops.
        // Join them outside the serialized lifecycle executor.
        drop(retired);
        true
    }
}

pub(crate) fn selected() -> Result<(Arc<Engine>, Call), RuntimeError> {
    selected_reload(false)
}
fn selected_reload(reload: bool) -> Result<(Arc<Engine>, Call), RuntimeError> {
    let engine = ENGINE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
        .ok_or(RuntimeError::Rejected)?;
    let call = engine.enter_reload(reload)?;
    Ok((engine, call))
}
fn capture(origin: Instant, services: HostServices) -> RuntimeClock {
    capture_at(origin, SystemTime::now(), services)
}
fn capture_at(origin: Instant, wall: SystemTime, services: HostServices) -> RuntimeClock {
    let now_ms = origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let elapsed = wall.duration_since(UNIX_EPOCH).ok();
    let wall_seconds = elapsed
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(-1);
    let civil = (wall_seconds >= 0)
        .then(|| services.local_time(wall_seconds))
        .flatten();
    RuntimeClock {
        now_ms,
        wall_seconds,
        wall_subsecond_ms: elapsed.map_or(0, |duration| duration.subsec_millis() as u16),
        civil,
    }
}
/// Borrow validated UTF-8 bytes for the duration of one product ABI call.
pub(crate) unsafe fn input<'a>(
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
    request: *const abi::rptadv_incoming_peer_v1,
) -> Result<(rpt_advanced_core::link::ProtocolId, String, String, String), RuntimeError> {
    if request.is_null()
        || unsafe { std::ptr::addr_of!((*request).struct_size).read() }
            < size_of::<abi::rptadv_incoming_peer_v1>() as u32
    {
        return Err(RuntimeError::Rejected);
    }
    let request = unsafe { &*request };
    let protocol = match request.protocol {
        1 => rpt_advanced_core::link::ProtocolId::AllStarLink,
        2 => rpt_advanced_core::link::ProtocolId::EchoLink,
        3 => rpt_advanced_core::link::ProtocolId::Phone,
        _ => return Err(RuntimeError::Rejected),
    };
    let phone = protocol == rpt_advanced_core::link::ProtocolId::Phone;
    if request.abi_version != 1
        || request.local.length
            > if phone {
                63
            } else if protocol == rpt_advanced_core::link::ProtocolId::EchoLink {
                36
            } else {
                32
            }
        || request.remote.length > 32
        || request.source.length > 64
    {
        return Err(RuntimeError::Rejected);
    }
    let local = unsafe { input(request.local.data.cast(), request.local.length) }?.to_owned();
    if phone && rpt_advanced_core::config::NodeId::new(local.as_str()).is_err() {
        return Err(RuntimeError::Rejected);
    }
    Ok((
        protocol,
        local,
        unsafe { input(request.remote.data.cast(), request.remote.length) }?.to_owned(),
        unsafe { input(request.source.data.cast(), request.source.length) }?.to_owned(),
    ))
}

/// Select the checked service, control and media owners for this product lifetime.
unsafe fn host_components(
    host: *const abi::rptadv_host_services_v11,
    control: *const ControlDescriptor,
    file: *const FileDescriptor,
    speech: *const SpeechDescriptor,
) -> Result<(HostServices, ControlClient, NativeMediaPreparer), RuntimeError> {
    let services = unsafe { HostServices::open(host) }.map_err(|_| RuntimeError::Preparation)?;
    let executor = unsafe { ControlClient::open(control, "rpt_advanced/links", 512) }
        .map_err(|_| RuntimeError::Preparation)?;
    let media = unsafe {
        NativeMediaPreparer::from_descriptors(
            file,
            speech,
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            30000,
            (services.reaper_acquire(), services.reaper_release()),
        )
    }
    .map_err(|_| RuntimeError::Preparation)?;
    Ok((services, executor, media))
}

/// Reload only the checked JSON working store owned by the managed service.
extern "C" fn rptadv_product_reload_working() -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<i32, RuntimeError> {
        let (engine, _call) = selected()?;
        #[cfg(target_os = "linux")]
        {
            let mut owner = engine
                .management_state
                .reload
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            owner
                .as_mut()
                .ok_or(RuntimeError::Rejected)?
                .begin()
                .map_err(|_| RuntimeError::Rejected)?;
            Ok(1)
        }
        #[cfg(not(target_os = "linux"))]
        Err(RuntimeError::Rejected)
    }))
    .ok()
    .and_then(Result::ok)
    .unwrap_or(-1)
}
/// Poll the existing control owner; a pending call retains both generations.
unsafe extern "C" fn rptadv_product_reload_poll() -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let (engine, _call) = selected_reload(true)?;
        #[cfg(target_os = "linux")]
        if let Some(reload) = engine
            .management_state
            .reload
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            return reload.poll().map_err(|_| RuntimeError::Rejected);
        }
        Err(RuntimeError::Rejected)
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |ready| i32::from(!ready))
}
/// Gate admission, stop/join owners, and drain accepted tasks before releasing registrations.
pub(crate) extern "C" fn rptadv_product_stop() -> i32 {
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
    request: *const abi::rptadv_incoming_peer_v1,
    peer: *mut c_void,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let (engine, _call) = selected()?;
        let (protocol, local, remote, source) = unsafe { incoming_identity(request) }?;
        if protocol == rpt_advanced_core::link::ProtocolId::Phone {
            let metadata = unsafe { engine.services.inspect_peer(peer) }
                .map_err(|_| RuntimeError::Rejected)?;
            if metadata.identity().protocol() != protocol
                || metadata.identity().wire_identity() != remote
                || metadata.local() != local
            {
                return Err(RuntimeError::Rejected);
            }
            return engine.run(move |host| {
                let (account, generation) = metadata.phone_owner().ok_or(RuntimeError::Rejected)?;
                host.authorize_phone_owner(&local, account, generation)
            });
        }
        // Echo admission must inspect the live provider QSO. A null/pre-answer
        // authorization path is available only for conventional IAX channels.
        if protocol != rpt_advanced_core::link::ProtocolId::AllStarLink || !peer.is_null() {
            return Err(RuntimeError::Rejected);
        }
        engine.run(move |host| {
            let owner = host
                .runtime
                .network_owner(&local)
                .ok_or(RuntimeError::MissingNode)?
                .to_owned();
            host.lookup(&owner, &remote, Some(&source))?;
            host.runtime.authorize_incoming(&owner, &remote, true)
        })
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

/// Admit one already-inspected and answered peer. Peer ownership always transfers.
unsafe extern "C" fn rptadv_product_incoming(
    request: *const abi::rptadv_incoming_peer_v1,
    peer: *mut c_void,
) -> i32 {
    let consumed = std::cell::Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let (engine, _call) = selected()?;
        let (protocol, local, remote, source) = unsafe { incoming_identity(request) }?;
        let peer = unsafe { engine.services.peer(peer) }.map_err(|_| RuntimeError::Rejected)?;
        consumed.set(true);
        if peer.identity().protocol() != protocol
            || peer.identity().wire_identity() != remote
            || peer.local() != local
        {
            return Err(RuntimeError::Rejected);
        }
        let clock = engine.clock();
        engine.run(move |host| host.incoming_peer(peer, &source, clock))
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
            DigitOperation::new(
                rpt_advanced_core::command::Command {
                    action,
                    node: unsafe { input(remote, remote_length) }?.to_owned(),
                },
                None,
            ),
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
        let mut text = engine.run(move |host| host.status_text(&local))?;
        engine.health.status(&mut text);
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
            engine.execute_digit(local, operation)?;
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
    abi_version: 8,
    capability: *b"rptadv.prod8\0\0\0\0",
    reload_working: Some(rptadv_product_reload_working),
    stop: Some(rptadv_product_stop),
    authorize_incoming: Some(rptadv_product_authorize_incoming),
    incoming: Some(rptadv_product_incoming),
    link_command: Some(rptadv_product_link_command),
    link_status: Some(rptadv_product_link_status),
    digit: Some(rptadv_product_digit),
    site_command: Some(crate::site::command::execute),
    reload_poll: Some(rptadv_product_reload_poll),
};

/// Return immutable process-lifetime product metadata.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_product_descriptor_v1() -> *const abi::rptadv_product_descriptor_v1 {
    &DESCRIPTOR
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
pub(crate) mod tests;
