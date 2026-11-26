type Station = [u8; 16];
type Rows = BTreeMap<Station, Row>;
type Registrar = Arc<Mutex<ManagedRegistrar>>;

#[derive(Clone, PartialEq, Eq)]
struct Registration {
    version: [u8; 16],
    url: String,
    seconds: u16,
}

#[derive(Clone)]
struct Row {
    authority: Authority,
    delays: Delays,
    registration: Option<Registration>,
    worker: Option<Registrar>,
}

struct Selection {
    attempt: NonZeroU64,
    basis: Option<NonZeroU64>,
    next: Rows,
    previous: Rows,
    affected: BTreeSet<Station>,
    callbacks: abi::rptadv_asl_admission_v1,
}

struct Pending {
    selection: Arc<Selection>,
    queued: Option<u32>,
    running: bool,
    reply: Option<(u32, i32)>,
}

#[derive(Default)]
struct State {
    committed: Option<NonZeroU64>,
    last: u64,
    registrar_serial: u64,
    published: Rows,
    callbacks: Option<abi::rptadv_asl_admission_v1>,
    pending: Option<Pending>,
    peers: PeerRegistry,
    ingress: bool,
    polling: bool,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    network: NetworkAccess,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

pub(super) struct Frontend {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Frontend {
    pub(super) fn start(network: NetworkAccess) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Default::default(),
            wake: Condvar::new(),
            network,
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("rpt-managed-iax".into())
            .spawn(move || run(worker))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(super) fn open_ingress(&self) {
        self.shared.lock().ingress = true;
    }

    /// The next stop attempt may enter product only after the last individual poll returned.
    pub(super) fn close_ingress(&self) -> bool {
        let mut state = self.shared.lock();
        state.ingress = false;
        !state.polling && idle_admissions(&state.peers)
    }

    pub(super) fn join(&mut self) -> bool {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_one();
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

impl Drop for Frontend {
    fn drop(&mut self) {
        self.join();
    }
}

fn serial(next: &mut u64) -> Option<NonZeroU64> {
    *next = next.checked_add(1)?;
    NonZeroU64::new(*next)
}

fn authority(id: &abi::rptadv_asl_identity_v1) -> Option<Authority> {
    if id.reserved != 0 || id.station_uuid == [0; 16] || id.account_uuid == [0; 16] {
        return None;
    }
    Some(Authority {
        station: id.station_uuid,
        account: id.account_uuid,
        generation: NonZeroU64::new(id.generation)?,
        wire: NonZeroU64::new(id.wire)?,
        port: NonZeroU16::new(u16::try_from(id.udp_port).ok()?)?,
    })
}

fn identity(a: Authority) -> abi::rptadv_asl_identity_v1 {
    abi::rptadv_asl_identity_v1 {
        station_uuid: a.station,
        account_uuid: a.account,
        generation: a.generation.get(),
        wire: a.wire.get(),
        udp_port: a.port.get().into(),
        reserved: 0,
    }
}

unsafe fn borrowed<'a>(span: abi::rptadv_peer_bytes_v1, limit: usize) -> Option<&'a str> {
    if span.length > limit || (span.length != 0 && span.data.is_null()) {
        return None;
    }
    if span.length == 0 {
        return Some("");
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(span.data, span.length) }).ok()
}
unsafe fn callbacks(
    p: *const abi::rptadv_asl_admission_v1,
) -> Option<abi::rptadv_asl_admission_v1> {
    if p.is_null() || !p.is_aligned() {
        return None;
    }
    let [extent, version] = unsafe { p.cast::<[u32; 2]>().read() };
    if version != 1 || (extent as usize) < size_of::<abi::rptadv_asl_admission_v1>() {
        return None;
    }
    let table = unsafe { p.read() };
    (table.authorize.is_some() && table.complete.is_some()).then_some(table)
}

impl State {
    unsafe fn selection(
        &mut self,
        basis: u64,
        attempt: u64,
        rows: &[abi::rptadv_asl_account_v2],
        callbacks: abi::rptadv_asl_admission_v1,
    ) -> Option<Selection> {
        let attempt = NonZeroU64::new(attempt)?;
        let basis = NonZeroU64::new(basis);
        if self.shutdown
            || self.committed != basis
            || attempt.get() <= self.last
            || self.pending.is_some()
        {
            return None;
        }
        let mut next = Rows::new();
        let mut affected = BTreeSet::new();
        let mut serial_number = self.registrar_serial;
        for input in rows {
            if input.struct_size as usize != size_of::<abi::rptadv_asl_account_v2>()
                || input.abi_version != 2
                || input.jitter_authored > 1
                || (MEDIA_KIND == 2 && input.jitter_authored != 0)
                || input.admission_changed > 1
            {
                return None;
            }
            let a = authority(&input.id)?;
            let delays = Delays::new(
                input.jitter_min_ms,
                input.jitter_initial_ms,
                input.jitter_max_ms,
            )?;
            let url = unsafe { borrowed(input.registration_url, 4 * 1024 * 1024) }?;
            let credential = unsafe { borrowed(input.registration_credential, 4 * 1024 * 1024) }?;
            let seconds = u16::try_from(input.registration_interval_s).ok()?;
            let registration = if credential.is_empty() {
                if input.record_version != [0; 16] {
                    return None;
                }
                None
            } else {
                if !url.starts_with("https://") || input.record_version == [0; 16] {
                    return None;
                }
                Some(Registration {
                    version: input.record_version,
                    url: url.to_owned(),
                    seconds,
                })
            };
            let old = self
                .published
                .get(&a.station)
                .filter(|old| old.authority == a);
            let worker = if let Some(key) = &registration {
                if let Some(old) = old.filter(|old| old.registration.as_ref() == Some(key)) {
                    old.worker.clone()
                } else {
                    let identity = RegistrarIdentity {
                        station: a.station,
                        account: a.account,
                        record_version: key.version,
                        attempt: serial(&mut serial_number)?,
                    };
                    Some(Arc::new(Mutex::new(ManagedRegistrar::prepare(
                        identity,
                        a.wire,
                        Some(url),
                        a.port,
                        seconds,
                        Some(credential),
                    )?)))
                }
            } else {
                None
            };
            if input.admission_changed != 0 || old.is_none_or(|old| old.delays != delays) {
                affected.insert(a.station);
            }
            if next
                .insert(
                    a.station,
                    Row {
                        authority: a,
                        delays,
                        registration,
                        worker,
                    },
                )
                .is_some()
            {
                return None;
            }
        }
        validate_rows(&next)?;
        affected.extend(
            self.published
                .keys()
                .filter(|id| !next.contains_key(*id))
                .copied(),
        );
        self.last = attempt.get();
        self.registrar_serial = serial_number;
        Some(Selection {
            attempt,
            basis,
            next,
            previous: self.published.clone(),
            affected,
            callbacks,
        })
    }
}

unsafe extern "C" fn prepare(
    host: *mut c_void,
    basis: u64,
    attempt: u64,
    rows: *const abi::rptadv_asl_account_v2,
    count: usize,
    product: *const abi::rptadv_asl_admission_v1,
    out: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        if out.is_null() || !out.is_aligned() {
            return -1;
        }
        unsafe { out.write(ptr::null_mut()) };
        let (Some(host), Some(callbacks)) = (frontend(host), unsafe { callbacks(product) }) else {
            return -1;
        };
        // The controller file inventory bounds the number and aggregate extent of rows.
        if count > 32_768
            || count
                .checked_mul(size_of::<abi::rptadv_asl_account_v2>())
                .is_none_or(|bytes| bytes > isize::MAX as usize)
        {
            return -1;
        }
        let rows = if count == 0 {
            &[]
        } else {
            if rows.is_null() || !rows.is_aligned() {
                return -1;
            }
            unsafe { std::slice::from_raw_parts(rows, count) }
        };
        let mut state = host.shared.lock();
        let Some(selection) = (unsafe { state.selection(basis, attempt, rows, callbacks) }) else {
            return -1;
        };
        let selection = Arc::new(selection);
        unsafe { out.write(Arc::as_ptr(&selection).cast_mut().cast()) };
        state.pending = Some(Pending {
            selection,
            queued: None,
            running: false,
            reply: None,
        });
        0
    })
}

unsafe extern "C" fn step(host: *mut c_void, token: *mut c_void, phase: u32) -> i32 {
    boundary(-1, || {
        let Some(host) = frontend(host) else {
            return -1;
        };
        let mut state = host.shared.lock();
        let Some(pending) = state
            .pending
            .as_mut()
            .filter(|pending| ptr::eq(Arc::as_ptr(&pending.selection).cast::<c_void>(), token))
        else {
            return -1;
        };
        if !(1..=4).contains(&phase) {
            return -1;
        }
        if pending.running || pending.queued.is_some() {
            return 1;
        }
        if let Some((completed, result)) = pending.reply.take() {
            if completed == phase && result != 1 {
                if phase == 4 && result == 0 {
                    state.pending = None;
                }
                return result;
            }
        }
        pending.queued = Some(phase);
        host.shared.wake.notify_one();
        1
    })
}

unsafe extern "C" fn observe(
    host: *mut c_void,
    id: *const abi::rptadv_asl_identity_v1,
    out: *mut u32,
) -> i32 {
    boundary(-1, || {
        if out.is_null() || !out.is_aligned() {
            return -1;
        }
        unsafe { out.write(0) };
        let (Some(host), Some(id)) = (frontend(host), unsafe { id.as_ref() }.and_then(authority))
        else {
            return -1;
        };
        let worker = {
            let state = host.shared.lock();
            let Some(row) = state
                .published
                .get(&id.station)
                .filter(|r| r.authority == id)
            else {
                return -1;
            };
            let Some(worker) = &row.worker else { return 0 };
            Arc::clone(worker)
        };
        #[cfg(test)]
        before_observing_worker();
        let observed = worker.lock().unwrap_or_else(|e| e.into_inner()).state();
        let state = host.shared.lock();
        if !state
            .published
            .get(&id.station)
            .filter(|row| row.authority == id)
            .and_then(|row| row.worker.as_ref())
            .is_some_and(|live| Arc::ptr_eq(live, &worker))
        {
            return -1;
        }
        let value = match observed {
            RegistrarState::Dormant => 1,
            RegistrarState::Pending => 2,
            RegistrarState::Registered => 3,
            RegistrarState::Retry => 4,
            RegistrarState::Withdrawn => 5,
        };
        unsafe { out.write(value) };
        0
    })
}

#[derive(Default)]
struct Owner {
    listeners: Listeners,
    materialized: bool,
    applied: bool,
    published: bool,
    restored: bool,
    previous: Option<Rows>,
}

fn changed_workers<'a>(
    from: &'a Rows,
    to: &'a Rows,
) -> impl Iterator<Item = (Station, &'a Registrar)> {
    from.iter().filter_map(move |(id, row)| {
        let worker = row.worker.as_ref()?;
        (!to.get(id)
            .and_then(|old| old.worker.as_ref())
            .is_some_and(|old| Arc::ptr_eq(old, worker)))
        .then_some((*id, worker))
    })
}

fn drain(from: &Rows, to: &Rows) -> bool {
    let mut ready = true;
    for (_, worker) in changed_workers(from, to) {
        ready &= worker.lock().unwrap_or_else(|e| e.into_inner()).withdraw();
    }
    ready
}

fn start(from: &Rows, to: &Rows) -> Result<(), ()> {
    for (_, worker) in changed_workers(from, to) {
        let mut worker = worker.lock().unwrap_or_else(|e| e.into_inner());
        if worker.state() == RegistrarState::Dormant {
            worker.start().map_err(|_| ())?;
        }
    }
    Ok(())
}

impl Owner {
    fn phase(&mut self, shared: &Shared, job: &Selection, phase: u32) -> Result<bool, ()> {
        match phase {
            1 => {
                if self.restored {
                    return Err(());
                }
                if !self.materialized {
                    let authorities = job.next.values().map(|r| r.authority).collect::<Vec<_>>();
                    self.listeners
                        .prepare(job.basis, job.attempt, &authorities)
                        .map_err(|_| ())?;
                    self.materialized = true;
                }
                shared.lock().peers.fence(&job.affected);
                if !self.applied {
                    apply_transport(&mut self.listeners, shared, job)?;
                    self.applied = true;
                }
                if !shared.lock().peers.adjust(&job.next, false)? {
                    return Ok(false);
                }
                if !drain(&job.previous, &job.next) {
                    return Ok(false);
                }
                start(&job.next, &job.previous)?;
                Ok(true)
            }
            2 if self.applied && !self.restored => {
                self.listeners.publish(job.attempt).map_err(|_| ())?;
                let mut state = shared.lock();
                state.published = job.next.clone();
                state.callbacks = Some(job.callbacks);
                self.published = true;
                Ok(true)
            }
            3 => {
                if self.restored {
                    return Ok(true);
                }
                if !drain(&job.next, &job.previous) {
                    return Ok(false);
                }
                if !shared.lock().peers.adjust(&job.previous, true)? {
                    return Ok(false);
                }
                if self.materialized {
                    self.listeners.reverse(job.attempt).map_err(|_| ())?;
                }
                if self.previous.is_none() {
                    let mut restored = job.previous.clone();
                    for (id, worker) in changed_workers(&job.previous, &job.next) {
                        let mut old = worker.lock().unwrap_or_else(|e| e.into_inner());
                        if old.state() == RegistrarState::Withdrawn {
                            if !old.withdraw() {
                                return Ok(false);
                            }
                            let next = serial(&mut shared.lock().registrar_serial).ok_or(())?;
                            restored.get_mut(&id).ok_or(())?.worker =
                                Some(Arc::new(Mutex::new(old.replacement(next))));
                        }
                    }
                    self.previous = Some(restored);
                }
                let restored = self.previous.as_ref().ok_or(())?;
                start(restored, &job.previous)?;
                shared.lock().published = restored.clone();
                self.restored = true;
                self.published = false;
                Ok(true)
            }
            4 => {
                let mut state = shared.lock();
                if self.published {
                    if !state.peers.drained(&job.next) {
                        return Ok(false);
                    }
                    self.listeners.finish(job.attempt).map_err(|_| ())?;
                    state.committed = Some(job.attempt);
                } else if self.materialized && !self.restored {
                    return Err(());
                }
                state.peers.released();
                self.materialized = false;
                self.applied = false;
                self.published = false;
                self.restored = false;
                self.previous = None;
                Ok(true)
            }
            _ => Err(()),
        }
    }
}

fn run(shared: Arc<Shared>) {
    let mut owner = Owner::default();
    loop {
        let (work, callbacks) = {
            let mut state = shared.lock();
            if state.shutdown {
                return;
            }
            let work = state.pending.as_mut().and_then(|p| {
                let phase = p.queued.take()?;
                p.running = true;
                Some((p.selection.clone(), phase))
            });
            (work, state.callbacks.filter(|_| state.ingress))
        };
        if let Some((job, phase)) = work {
            let result = match owner.phase(&shared, &job, phase) {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(()) => -1,
            };
            let mut state = shared.lock();
            let pending = state.pending.as_mut().expect("unacknowledged native phase");
            pending.running = false;
            pending.reply = Some((phase, result));
        }
        if let Some(callbacks) = callbacks {
            poll_transport(&mut owner.listeners, &shared, callbacks);
        }
        let state = shared.lock();
        let _idle = shared.wake.wait_timeout_while(state, POLL_DELAY, |s| {
            !s.shutdown && s.pending.as_ref().is_none_or(|p| p.queued.is_none())
        });
    }
}

fn validate_rows(rows: &Rows) -> Option<()> {
    let mut wires = BTreeSet::new();
    let mut accounts = BTreeSet::new();
    rows.values()
        .all(|row| wires.insert(row.authority.wire) && accounts.insert(row.authority.account))
        .then_some(())
}
