//! Opt-in full-path loopback experiment, never a timing-threshold CI test.
use super::*;
use rpt_advanced_standalone::{
    host_services::HostServicesOwner, providers::ProviderSet, secrets::SecretsFile,
};
use std::{
    fs,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};
#[path = "egress_simulator_tests.rs"]
mod simulator;

struct CallbackCounters {
    calls: AtomicU64,
    maximum_ns: AtomicU64,
    late: AtomicU64,
    maximum_late_ns: AtomicU64,
    durations: [AtomicU64; 1001],
}
impl Default for CallbackCounters {
    fn default() -> Self {
        Self {
            calls: 0.into(),
            maximum_ns: 0.into(),
            late: 0.into(),
            maximum_late_ns: 0.into(),
            durations: std::array::from_fn(|_| 0.into()),
        }
    }
}
static CALLBACKS: OnceLock<Arc<CallbackCounters>> = OnceLock::new();
type SendPrepared = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, usize, u32) -> i32;
static SEND: OnceLock<SendPrepared> = OnceLock::new();
static BLOCK: AtomicBool = AtomicBool::new(false);
static BLOCKED_PEER: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn send_prepared(
    context: *mut c_void,
    peer: *mut c_void,
    bytes: *const u8,
    count: usize,
    timestamp: u32,
) -> i32 {
    let _ = BLOCKED_PEER.compare_exchange(0, peer as usize, Ordering::Relaxed, Ordering::Relaxed);
    if BLOCK.load(Ordering::Relaxed) && BLOCKED_PEER.load(Ordering::Relaxed) == peer as usize {
        return 1;
    }
    unsafe { SEND.get().unwrap()(context, peer, bytes, count, timestamp) }
}
struct Radio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Radio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
unsafe extern "C" fn radio_open(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: usize,
    _: usize,
    output: *mut *mut c_void,
) -> i32 {
    unsafe {
        *output = Box::into_raw(Box::new(Radio {
            stop: Arc::default(),
            thread: None,
        }))
        .cast();
    }
    0
}
unsafe extern "C" fn radio_destroy(_: *mut c_void, radio: *mut c_void) {
    drop(unsafe { Box::from_raw(radio.cast::<Radio>()) });
}
unsafe extern "C" fn bind_radio(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn radio_activate(
    _: *mut c_void,
    radio: *mut c_void,
    receive: abi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    let radio = unsafe { &mut *radio.cast::<Radio>() };
    let stop = radio.stop.clone();
    let (receive, transmit) = (receive.unwrap(), transmit.unwrap());
    let contexts = (receive_context as usize, transmit_context as usize);
    let counters = CALLBACKS.get().unwrap().clone();
    radio.thread = Some(
        thread::Builder::new()
            .name("load-radio".into())
            .spawn(move || {
                let waveform: [f32; 48] =
                    std::array::from_fn(|i| (i as f32 * std::f32::consts::TAU / 48.0).sin() * 0.1);
                let mut input = [0.0; 4096];
                let mut output = [0.0; 4096];
                let mut samples = 0_u64;
                let start = Instant::now();
                let mut block = 0;
                while !stop.load(Ordering::Acquire) {
                    let count = [240, 480, 960, 1440][block % 4];
                    let due = start + Duration::from_nanos(samples * 1_000_000_000 / 48_000);
                    thread::sleep(due.saturating_duration_since(Instant::now()));
                    let now = Instant::now();
                    let late = now.saturating_duration_since(due).as_nanos() as u64;
                    let budget = count as u64 * 1_000_000_000 / 48_000;
                    counters.maximum_late_ns.fetch_max(late, Ordering::Relaxed);
                    if late > budget {
                        counters.late.fetch_add(1, Ordering::Relaxed);
                    }
                    for (i, sample) in input[..count].iter_mut().enumerate() {
                        *sample = waveform[(samples as usize + i) % 48];
                    }
                    let started = Instant::now();
                    let (mut ptt, mut ctcss) = (0, 0);
                    unsafe {
                        receive(
                            contexts.0 as *mut c_void,
                            u32::from(samples / 48_000 % 7 != 6),
                            input.as_mut_ptr(),
                            count as u32,
                        );
                        transmit(
                            contexts.1 as *mut c_void,
                            output.as_mut_ptr(),
                            count as u32,
                            &mut ptt,
                            &mut ctcss,
                        );
                    }
                    let duration = started.elapsed().as_nanos() as u64;
                    counters.durations[(duration / 100_000).min(1000) as usize]
                        .fetch_add(1, Ordering::Relaxed);
                    counters.calls.fetch_add(1, Ordering::Relaxed);
                    counters.maximum_ns.fetch_max(duration, Ordering::Relaxed);
                    samples += count as u64;
                    block += 1;
                }
            })
            .unwrap(),
    );
    0
}
fn load_clock(epoch: Instant) -> RuntimeClock {
    RuntimeClock {
        now_ms: epoch.elapsed().as_millis() as u64,
        wall_seconds: 0,
        wall_subsecond_ms: 0,
        civil: None,
    }
}
fn settled(host: &mut Host, epoch: Instant) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        host.pump(load_clock(epoch)).unwrap();
        if host.runtime.status(0)[0].1.retiring.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "generation failed to retire");
        thread::sleep(Duration::from_millis(1));
    }
}
fn proc_value(name: &str) -> u64 {
    fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}
fn cpu_ticks() -> u64 {
    let stat = fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}
fn udp_errors() -> [u64; 3] {
    let text = fs::read_to_string("/proc/net/snmp").unwrap();
    let mut udp = text.lines().filter(|line| line.starts_with("Udp:"));
    let names: Vec<_> = udp.next().unwrap().split_whitespace().collect();
    let values: Vec<_> = udp.next().unwrap().split_whitespace().collect();
    ["InErrors", "RcvbufErrors", "SndbufErrors"].map(|name| {
        values[names.iter().position(|field| *field == name).unwrap()]
            .parse()
            .unwrap()
    })
}
fn percentile(histogram: &[u64], percent: u64) -> usize {
    let rank = histogram
        .iter()
        .sum::<u64>()
        .saturating_mul(percent)
        .div_ceil(100)
        .max(1);
    let mut count = 0;
    histogram
        .iter()
        .position(|value| {
            count += value;
            count >= rank
        })
        .unwrap_or(0)
}

#[test]
fn known_age_histograms_have_exact_percentiles() {
    assert_eq!(percentile(&[], 99), 0);
    assert_eq!(percentile(&[0, 1], 50), 1);
    let histogram = [50, 0, 45, 4, 1];
    assert_eq!(percentile(&histogram, 50), 0);
    assert_eq!(percentile(&histogram, 95), 2);
    assert_eq!(percentile(&histogram, 99), 3);
    assert_eq!(percentile(&histogram, 100), 4);
}

#[test]
#[ignore = "loopback load experiment: run tests/peer_egress_load.py"]
fn egress_full_path_load() {
    let peers: usize = std::env::var("RPTADV_LOAD_PEERS")
        .unwrap_or("1000".into())
        .parse()
        .unwrap();
    let seconds: u64 = std::env::var("RPTADV_LOAD_SECONDS")
        .unwrap_or("120".into())
        .parse()
        .unwrap();
    assert!(peers > 0 && seconds > 0);
    let output = std::env::var("RPTADV_LOAD_OUTPUT").unwrap();
    let callbacks = CALLBACKS
        .get_or_init(|| Arc::new(CallbackCounters::default()))
        .clone();
    let providers = Box::new(ProviderSet::load().unwrap());
    providers.validate_runtime().unwrap();
    // Host and executor are dropped before this stable provider box, including on unwind.
    let retained = unsafe { &*(providers.as_ref() as *const ProviderSet) };
    let owner =
        HostServicesOwner::new(vec![], Some(retained), SecretsFile::parse("").unwrap()).unwrap();
    let mut table = Box::new(unsafe {
        ptr::from_ref(owner.descriptor())
            .cast::<abi::rptadv_host_services_v7>()
            .read()
    });
    table.radio_open = Some(radio_open);
    table.radio_activate = Some(radio_activate);
    table.radio_destroy = Some(radio_destroy);
    table.peer_bind_radio = Some(bind_radio);
    SEND.set(table.peer_send_prepared.unwrap()).unwrap();
    table.peer_send_prepared = Some(send_prepared);
    let services = unsafe { HostServices::open(&*table) }.unwrap();
    let simulator = simulator::Simulator::start(peers);
    let epoch = Instant::now();
    let mut host = Host::start(
        document("load-radio", ""),
        media(),
        services,
        epoch,
        load_clock(epoch),
    )
    .unwrap();
    host.peers.try_reserve(peers).unwrap();
    let source_clock = host.leases[0].1.lock().unwrap().status.source_clock.clone();
    for index in 0..peers {
        let remote = (2000 + index).to_string();
        let LinkEffect::Connect(attempt) = host
            .runtime
            .command(
                "1000",
                operation(LinkAction::Transceive, &remote),
                load_clock(epoch),
                false,
            )
            .unwrap()
        else {
            panic!("peer admission failed at {index}");
        };
        let io = services.dial(
            &format!("radio@{}/{remote}", simulator.address),
            "1000",
            MAXIMUM_FRAMES,
            || true,
        );
        assert!(
            host.runtime
                .finish_connect("1000", attempt, io.is_ok(), load_clock(epoch))
                .unwrap()
        );
        // Prepare real sessions, then publish one membership generation. Normal
        // per-connection publication/reload has separate deterministic coverage;
        // repeatedly rebuilding N rings would time setup, not sustained egress.
        let (_, outbound) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
        let (mut session, control) =
            PeerSession::prepare(io.unwrap(), outbound, "1000", &remote).unwrap();
        session.bind_clock(source_clock.clone());
        assert!(host.peer_io.attach(session).is_ok());
        host.peers.push(PeerOwner {
            local: "1000".into(),
            remote,
            mode: Mode::TRANSCEIVE,
            group: None,
            announced: false,
            control,
        });
        if (index + 1) % 100 == 0 {
            eprintln!(
                "admitted {} peers ({} alive) in {:.1}s",
                index + 1,
                host.peers.len(),
                epoch.elapsed().as_secs_f64()
            );
        }
    }
    host.refresh("1000", load_clock(epoch)).unwrap();
    settled(&mut host, epoch);
    // The initial unconnected session rings have no audio consumer. Begin the
    // ingress workload only after all real generation endpoints are installed.
    simulator.ingress_enabled.store(true, Ordering::Relaxed);
    // Warm every converter and thread before the measured interval.
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_secs(3) {
        host.pump(load_clock(epoch)).unwrap();
        thread::sleep(Duration::from_millis(2));
    }
    let baseline = host
        .peers
        .iter()
        .map(|peer| (peer.remote.clone(), peer.control.snapshot().unwrap()))
        .collect::<std::collections::HashMap<_, _>>();
    for duration in &callbacks.durations {
        duration.store(0, Ordering::Relaxed);
    }
    callbacks.maximum_ns.store(0, Ordering::Relaxed);
    callbacks.maximum_late_ns.store(0, Ordering::Relaxed);
    let initial_calls = callbacks.calls.load(Ordering::Relaxed);
    let initial_late = callbacks.late.load(Ordering::Relaxed);
    let initial_cpu = cpu_ticks();
    let initial_udp = udp_errors();
    let initial_received = simulator.counts.media.load(Ordering::Relaxed);
    let initial_samples = simulator.counts.samples.load(Ordering::Relaxed);
    let initial_reordered = simulator.counts.out_of_order.load(Ordering::Relaxed);
    let initial_retries = simulator.counts.retransmissions.load(Ordering::Relaxed);
    BLOCKED_PEER.store(0, Ordering::Relaxed);
    let start = Instant::now();
    let mut paused_media_start = None;
    let mut paused_media = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        let elapsed = start.elapsed().as_millis();
        BLOCK.store(elapsed >= 5000, Ordering::Relaxed);
        // Media must progress while station control deliberately does no work.
        if !(10_000..11_000).contains(&elapsed) {
            if let Some(before) = paused_media_start.take() {
                paused_media = simulator.counts.samples.load(Ordering::Relaxed) - before;
            }
            host.pump(load_clock(epoch)).unwrap();
        } else {
            paused_media_start
                .get_or_insert_with(|| simulator.counts.samples.load(Ordering::Relaxed));
        }
        thread::sleep(Duration::from_millis(2));
    }
    BLOCK.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    let mut histogram = [0_u64; 201];
    let (
        mut accepted,
        mut blocked,
        mut expired,
        mut rejected,
        mut age,
        mut datagrams,
        mut socket_blocked,
        mut retries,
        mut backlog,
    ) = (0, 0, 0, 0, 0, 0, 0, 0, 0);
    let mut per_peer = Vec::new();
    for peer in &host.peers {
        let initial = &baseline[&peer.remote];
        let snapshot = peer.control.snapshot().unwrap();
        let metrics = snapshot.egress;
        accepted += metrics.accepted.saturating_sub(initial.egress.accepted);
        blocked += metrics.blocked.saturating_sub(initial.egress.blocked);
        expired += metrics
            .expired_native_samples
            .saturating_sub(initial.egress.expired_native_samples);
        rejected += metrics
            .rejected_native_samples
            .saturating_sub(initial.egress.rejected_native_samples);
        age = age.max(metrics.maximum_queue_age_ns);
        let peer_histogram = std::array::from_fn::<_, 201, _>(|index| {
            metrics.send_age_ms[index].saturating_sub(initial.egress.send_age_ms[index])
        });
        per_peer.push(serde_json::json!({
            "node": peer.remote,
            "accepted": metrics.accepted.saturating_sub(initial.egress.accepted),
            "blocked": metrics.blocked.saturating_sub(initial.egress.blocked),
            "expired_native_samples": metrics.expired_native_samples.saturating_sub(initial.egress.expired_native_samples),
            "rejected_native_samples": metrics.rejected_native_samples.saturating_sub(initial.egress.rejected_native_samples),
            "age_ms_p50": percentile(&peer_histogram, 50),
            "age_ms_p95": percentile(&peer_histogram, 95),
            "age_ms_p99": percentile(&peer_histogram, 99),
            "age_ms_max": peer_histogram.iter().rposition(|value| *value != 0).unwrap_or(0)
        }));
        for (index, value) in histogram.iter_mut().enumerate() {
            *value += metrics.send_age_ms[index].saturating_sub(initial.egress.send_age_ms[index]);
        }
        if let (Some(after), Some(before)) = (snapshot.peer, initial.peer) {
            datagrams += after
                .egress_datagrams
                .saturating_sub(before.egress_datagrams);
            socket_blocked += after
                .egress_would_block
                .saturating_sub(before.egress_would_block);
            retries += after
                .egress_reliable_retries
                .saturating_sub(before.egress_reliable_retries);
            backlog += u64::from(after.egress_reliable_pending + after.egress_reply_pending);
        }
    }
    let generation = host.runtime.status(0)[0].1.active.unwrap();
    let control = host
        .runtime
        .adapter_control_mut("1000", generation)
        .unwrap();
    let (batches, dispatch_ns) = control.dispatcher.timing();
    let dropped = control.status.dropped_blocks();
    let callback_histogram = callbacks
        .durations
        .each_ref()
        .map(|value| value.load(Ordering::Relaxed));
    let udp = udp_errors();
    let result = serde_json::json!({
        "peers_requested": peers, "peers_alive": host.peers.len(), "peer_owner_tasks": host.peers.len(),
        "peers_at_measurement_start": baseline.len(),
        "seconds": elapsed, "accepted": accepted, "explicit_blocked": blocked,
        "expired_native_samples": expired, "rejected_native_samples": rejected,
        "maximum_queue_age_ns": age, "send_age_ms": {
            "p50": percentile(&histogram,50), "p95": percentile(&histogram,95),
            "p99": percentile(&histogram,99), "max": histogram.iter().rposition(|value| *value != 0).unwrap_or(0)},
        "datagrams": datagrams, "socket_blocked": socket_blocked, "retries": retries,
        "reliable_backlog": backlog, "dispatcher_batches": batches, "dispatcher_maximum_ns": dispatch_ns,
        "program_blocks_dropped": dropped, "ulaw_samples_received_during_control_pause": paused_media,
        "callback_count": callbacks.calls.load(Ordering::Relaxed)-initial_calls,
        "callback_maximum_ns": callbacks.maximum_ns.load(Ordering::Relaxed),
        "callback_p99_us": percentile(&callback_histogram,99)*100,
        "callback_late_count": callbacks.late.load(Ordering::Relaxed)-initial_late,
        "callback_maximum_late_ns": callbacks.maximum_late_ns.load(Ordering::Relaxed),
        "cpu_ticks": cpu_ticks()-initial_cpu, "rss_kb": proc_value("VmRSS:"), "threads": proc_value("Threads:"),
        "receiver_media_datagrams_including_empty": simulator.counts.media.load(Ordering::Relaxed)-initial_received,
        "receiver_ulaw_samples": simulator.counts.samples.load(Ordering::Relaxed)-initial_samples,
        "receiver_send_errors": simulator.counts.send_errors.load(Ordering::Relaxed),
        "receiver_out_of_order": simulator.counts.out_of_order.load(Ordering::Relaxed)-initial_reordered,
        "receiver_retries": simulator.counts.retransmissions.load(Ordering::Relaxed)-initial_retries,
        "receiver_buffer_bytes": simulator.receive_buffer_bytes,
        "kernel_udp_errors": {"in_errors": udp[0]-initial_udp[0], "receive_buffer_errors": udp[1]-initial_udp[1], "send_buffer_errors": udp[2]-initial_udp[2]},
        "per_peer": per_peer,
        "measurement_notes": {
            "send_age_ms": "Provider acceptance age, not remote arrival or reliable retry age; 1ms bins",
            "callback_p99_us": "100us bins; final bucket includes durations at or above 100ms",
            "lifetime_values": "Maximum queue age, dispatcher timing, program drops and receiver send errors include warm-up",
            "resources": "CPU and RSS include the test UDP simulator and callback instrumentation; peer_owner_tasks counts retained media owners, not all Tokio tasks"
        }
    });
    fs::write(output, serde_json::to_string_pretty(&result).unwrap()).unwrap();
    eprintln!(
        "{peers} requested, {} alive, {accepted} accepted, {expired} expired native samples",
        host.peers.len()
    );
    assert_eq!(host.peers.len(), peers, "load disconnected a peer");
    assert!(accepted > 0, "no media reached prepared IAX send");
    if seconds > 11 {
        assert!(paused_media > 0, "control pause stopped real audio");
    }
    assert!(host.stop(load_clock(epoch).now_ms));
}
