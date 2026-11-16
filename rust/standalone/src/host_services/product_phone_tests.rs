//! Genuine product/private-IAX login over loopback. Only physical radio I/O is simulated.
use super::*;
use crate::{
    configuration::{resolve_phone_accounts, resolve_radio_nodes},
    iax::{
        IaxServer,
        accounts::{DialRequest, ProviderAccount, ProviderCall},
        tests::selected_client,
    },
    phone_accounts::{AccountPreparer, PreparedAccount, PreparedAccounts},
    providers::ProviderSet,
    runtime::ProductRuntime,
};
use std::{
    fs,
    net::UdpSocket,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct RadioPump {
    stop: Arc<AtomicBool>,
    worker: thread::JoinHandle<()>,
}
static RADIO: Mutex<Option<RadioPump>> = Mutex::new(None);
static RADIO_METADATA: Mutex<abi::rptadv_radio_metadata_v1> = Mutex::new(None);
static LINKED_OUTPUT_TURNS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn capture_radio_metadata(
    _: *mut c_void,
    _: *mut c_void,
    callback: abi::rptadv_radio_metadata_v1,
) -> i32 {
    *RADIO_METADATA.lock().unwrap() = callback;
    0
}
static PUBLIC_OUTPUT: AtomicUsize = AtomicUsize::new(0);
static PUBLIC_PCM_OUTPUT: AtomicUsize = AtomicUsize::new(0);
static LAST_PTT: AtomicBool = AtomicBool::new(false);
// Test-only 20-ms RF observations; a complete P occupies under 32 slots.
static RF_MORSE: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
// Test-only local RF input; the existing callback scratch remains the sole PCM block.
static LOCAL_INPUT: AtomicBool = AtomicBool::new(false);
// ASCII DTMF or a keyed silent gap, rendered in the same existing callback scratch.
static LOCAL_DTMF: AtomicUsize = AtomicUsize::new(0);
// Count actual simulated receiver callbacks, not intended test commands.
static LOCAL_RX_KEYED_TURNS: AtomicUsize = AtomicUsize::new(0);
static RADIO_TURNS: AtomicUsize = AtomicUsize::new(0);
static RADIO_JOIN_FAILURES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn activate(
    context: *mut c_void,
    radio: *mut c_void,
    receive: abi::rptadv_radio_receive_v3,
    receive_context: *mut c_void,
    transmit: abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    let (Some(receive), Some(transmit)) = (receive, transmit) else {
        return -1;
    };
    // Query the same resolved policy used by product reservation. The physical
    // fixture supplies qualified metadata, not a second classifier or delay line.
    let mut policy: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    policy.struct_size = size_of::<abi::rptadv_radio_control_v1>() as u32;
    policy.abi_version = 1;
    policy.operation = 7;
    policy.command.struct_size = size_of_val(&policy.command) as u32;
    policy.command.abi_version = 1;
    policy.command.command = abi::URP_AST_COMMAND_GET_RECEIVE_POLICY;
    assert_eq!(
        unsafe {
            crate::radio_host::radio_control(context, radio, &mut policy, None, ptr::null_mut())
        },
        0
    );
    let vad_mode = u32::try_from(policy.command.value).unwrap();
    let vad_preroll_frames = policy.command.flags;
    let contexts = (receive_context as usize, transmit_context as usize);
    let metadata_callback = *RADIO_METADATA.lock().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        let mut first = 0;
        let started = Instant::now();
        let mut next = started;
        let mut skipped = 0u64;
        while !stopping.load(Ordering::Acquire) {
            let mut samples = [0.0; 960];
            let digit = LOCAL_DTMF.load(Ordering::Acquire);
            let local_voice = LOCAL_INPUT.load(Ordering::Acquire);
            let local_input = local_voice || digit != 0;
            if local_input {
                LOCAL_RX_KEYED_TURNS.fetch_add(1, Ordering::Relaxed);
            }
            if let Some((row, column)) = b"123A456B789C*0#D"
                .iter()
                .position(|value| usize::from(*value) == digit)
                .map(|position| (position / 4, position % 4))
            {
                let low = [697.0, 770.0, 852.0, 941.0][row];
                let high = [1209.0, 1336.0, 1477.0, 1633.0][column];
                for (index, sample) in samples.iter_mut().enumerate() {
                    let t = (first + index as u64) as f64 / 48000.0;
                    *sample = (0.12 * (std::f64::consts::TAU * low * t).sin()
                        + 0.12 * (std::f64::consts::TAU * high * t).sin())
                        as f32;
                }
            } else if local_voice {
                for (index, sample) in samples.iter_mut().enumerate() {
                    *sample =
                        0.2 * ((first + index as u64) as f32 * std::f32::consts::TAU / 48.0).sin();
                }
            }
            let metadata = abi::UrpNativeReceiveMetadata {
                struct_size: size_of::<abi::UrpNativeReceiveMetadata>() as u32,
                abi_version: 1,
                generation_id: 1,
                first_sample_index: first,
                frame_count: 960,
                raw_receiver_keyed: u32::from(local_input),
                receiver_keyed: u32::from(local_input),
                vad_mode,
                rf_burst_id: u64::from(local_input),
                vad_voice_sample_index: if vad_mode != 0 && local_input {
                    first
                } else {
                    u64::MAX
                },
                vad_preroll_frames,
                vad_voice_active: u32::from(vad_mode != 0 && local_input),
                vad_score: if vad_mode != 0 && local_input {
                    1.0
                } else {
                    0.0
                },
                status: 0,
            };
            let (mut keyed, mut ctcss) = (0, 0);
            unsafe {
                receive(
                    contexts.0 as *mut c_void,
                    &metadata,
                    samples.as_mut_ptr(),
                    960,
                );
                transmit(
                    contexts.1 as *mut c_void,
                    samples.as_mut_ptr(),
                    960,
                    &mut keyed,
                    &mut ctcss,
                );
            }
            if let Some(metadata) = metadata_callback {
                let mut linked = 0;
                assert_eq!(
                    unsafe { metadata(contexts.1 as *mut c_void, 0, &mut linked) },
                    0
                );
                if linked != 0 && keyed != 0 && samples.iter().any(|sample| sample.abs() > 0.0001) {
                    LINKED_OUTPUT_TURNS.fetch_add(1, Ordering::Relaxed);
                }
            }
            let nonzero = samples
                .iter()
                .filter(|sample| sample.abs() > 0.0001)
                .count();
            let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
            let correlation = |lag: usize| {
                samples
                    .iter()
                    .zip(samples.iter().skip(lag))
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
            };
            // The configured fixture tone is 800 Hz at the existing 48-kHz DAC.
            RF_MORSE[RADIO_TURNS.load(Ordering::Relaxed) % RF_MORSE.len()].store(
                energy > 0.01 && correlation(60) > energy * 0.7 && correlation(30) < -energy * 0.7,
                Ordering::Release,
            );
            PUBLIC_PCM_OUTPUT.fetch_add(nonzero, Ordering::Relaxed);
            PUBLIC_OUTPUT.fetch_add(nonzero + usize::from(keyed != 0), Ordering::Relaxed);
            LAST_PTT.store(keyed != 0, Ordering::Release);
            RADIO_TURNS.fetch_add(1, Ordering::Relaxed);
            first += 960;
            // A hardware frame clock includes callback work in its 20-ms
            // budget. Relative sleeps incorrectly slow the simulated device.
            next += Duration::from_millis(20);
            if let Some(late) = Instant::now().checked_duration_since(next) {
                // A missed deadline drops elapsed slots, never an unbounded
                // catch-up burst. Native source coordinates retain that gap.
                let missed = (late.as_millis() / 20 + 1) as u64;
                skipped += missed;
                first += missed * 960;
                next += Duration::from_millis(missed * 20);
            }
            thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        eprintln!(
            "simulated radio clock: callbacks={} skipped={skipped} source_ms={} elapsed_ms={}",
            first / 960 - skipped,
            first / 48,
            started.elapsed().as_millis()
        );
    });
    *RADIO.lock().unwrap() = Some(RadioPump { stop, worker });
    0
}
unsafe extern "C" fn ready(_: *mut c_void, _: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn quiesce(_: *mut c_void, _: *mut c_void) -> i32 {
    if let Some(pump) = RADIO.lock().unwrap().take() {
        pump.stop.store(true, Ordering::Release);
        if pump.worker.join().is_err() {
            RADIO_JOIN_FAILURES.fetch_add(1, Ordering::Relaxed);
            return -1;
        }
    }
    0
}

/// Reuse only the physical-I/O substitution in an isolated full-foreground child.
pub(crate) fn configure_foreground_fixture(host: &mut HostServicesOwner) {
    if std::env::var_os("RPTADV_TEST_FOREGROUND_SIP_RADIO").is_some() {
        install_simulated_radio(host);
    }
}

unsafe extern "C" fn bind_simulated_radio(
    _: *mut c_void,
    peer: *mut c_void,
    radio: *mut c_void,
) -> i32 {
    if peer.is_null() || radio.is_null() || RADIO.lock().unwrap().is_none() {
        -1
    } else {
        0
    }
}
fn install_simulated_radio(host: &mut HostServicesOwner) {
    host.descriptor.peer_bind_radio = Some(bind_simulated_radio);
    host.descriptor.radio_activate = Some(activate);
    host.descriptor.radio_metadata_bind = Some(capture_radio_metadata);
    host.descriptor.radio_ready = Some(ready);
    host.descriptor.radio_quiesce = Some(quiesce);
}

/// This assertion runs inside the child after the real foreground has joined owners.
pub(crate) fn assert_foreground_stopped() {
    assert_radio_stopped(false);
}
fn assert_radio_stopped(voiced: bool) {
    assert!(
        RADIO_TURNS.load(Ordering::Acquire) > 0,
        "enabled radio must service actual callback turns"
    );
    assert_eq!(
        RADIO_JOIN_FAILURES.load(Ordering::Acquire),
        0,
        "physical fixture callback thread must join successfully"
    );
    assert!(
        RADIO.lock().unwrap().is_none(),
        "physical fixture callback pump drained"
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire) > 0,
        voiced,
        "only the authorized voice fixture may key or emit radio audio"
    );
    eprintln!("foreground SIP fixture stopped; authorized voice output: {voiced}");
}

struct Admission<'a> {
    runtime: &'a ProductRuntime,
    client: &'a IaxClient,
    account: &'a PreparedAccount,
    local: SocketAddr,
    accepted: bool,
}
unsafe extern "C" fn authorize(
    _: *mut c_void,
    _: *const abi::rptadv_iax2_provider_incoming_v1,
) -> i32 {
    0
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    metadata: *const abi::rptadv_iax2_provider_incoming_v1,
    raw: *mut c_void,
) -> i32 {
    let context = unsafe { &mut *context.cast::<Admission<'_>>() };
    let Ok(peer) = (unsafe { context.client.clone().adopt_inbound(raw) }) else {
        return -1;
    };
    let metadata = unsafe { &*metadata };
    let Ok(receipt) =
        crate::iax::accounts::call_receipt(metadata.receipt, context.account.generation())
    else {
        drop(peer);
        return 0;
    };
    context.accepted = context
        .runtime
        .incoming_provider(
            ProviderCall { peer, receipt },
            context.account,
            context.local,
        )
        .is_ok();
    // Successful adoption consumed the unique handle even if product admission rejected it.
    0
}

/// Observe actual decoded PCM until the finite private reply has drained.
fn prompt(peer: &mut IaxPeer, stage: &str) -> Vec<f32> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_tone = Instant::now();
    let mut result = Vec::new();
    let mut pcm = [0.0; 4096];
    let mut text = [0; 256];
    loop {
        match peer.poll(&mut pcm, &mut text).unwrap() {
            IaxEvent::Audio(count) => {
                if pcm[..count].iter().any(|sample| sample.abs() > 0.01) {
                    last_tone = Instant::now();
                }
                result.extend_from_slice(&pcm[..count]);
            }
            IaxEvent::None => {}
            event => panic!(
                "unexpected {stage} prompt event: {event:?}; decoded frames={}",
                result.len()
            ),
        }
        if !result.is_empty() && last_tone.elapsed() > Duration::from_millis(400) {
            eprintln!(
                "private {stage} prompt received {} decoded frames",
                result.len()
            );
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "private prompt PCM did not arrive"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Narrow raw Morse envelope check at the configured speed, 800 Hz and 8 kHz.
fn decode_prompt(pcm: &[f32], speed_wpm: u32) -> String {
    let unit = 9600 / speed_wpm as usize;
    let mut runs: Vec<(bool, usize)> = Vec::new();
    for chunk in pcm.chunks(10) {
        let tone = chunk.iter().any(|sample| sample.abs() > 0.01);
        if let Some((_, count)) = runs.last_mut().filter(|(last, _)| *last == tone) {
            *count += chunk.len();
        } else {
            runs.push((tone, chunk.len()));
        }
    }
    let first = runs
        .iter()
        .position(|(tone, _)| *tone)
        .expect("nonempty private audio");
    let last = runs.iter().rposition(|(tone, _)| *tone).unwrap();
    runs[first..=last]
        .iter()
        .map(|(tone, count)| {
            if *tone {
                if *count < 2 * unit { '.' } else { '-' }
            } else if *count > 5 * unit {
                '/'
            } else if *count > 2 * unit {
                ' '
            } else {
                '_'
            }
        })
        .filter(|symbol| *symbol != '_')
        .collect()
}
fn assert_prompt(pcm: &[f32], expected: &str) {
    assert_prompt_at_speed(pcm, expected, 40);
}
fn assert_prompt_at_speed(pcm: &[f32], expected: &str, speed_wpm: u32) {
    assert_eq!(
        decode_prompt(pcm, speed_wpm),
        expected,
        "decoded private Morse reply"
    );
}

#[test]
fn configured_prompt_speed_preserves_symbol_boundaries() {
    for speed in [20, 40] {
        let unit = 9600 / speed as usize;
        for offset in [0, 40, 79] {
            let mut pcm = vec![0.0; offset];
            for (tone, units) in [(true, 1), (false, 3), (true, 3), (false, 7), (true, 1)] {
                pcm.extend(std::iter::repeat_n(
                    if tone { 0.25 } else { 0.0 },
                    unit * units,
                ));
            }
            assert_eq!(decode_prompt(&pcm, speed), ". -/.");
            pcm.extend(std::iter::repeat_n(0.0, unit * 10));
            assert_eq!(decode_prompt(&pcm, speed), ". -/.");
            if speed == 40 {
                assert_prompt(&pcm, ". -/.");
            }
        }
    }
}

fn caller(client: IaxClient, local: SocketAddr, target: SocketAddr) {
    let providers = Box::new(ProviderSet::load().unwrap());
    // The retained box outlives this executor, listener and unique call.
    let provider_ref: &'static ProviderSet = unsafe { &*ptr::from_ref(providers.as_ref()) };
    let executor = provider_ref.iax_executor().unwrap();
    let account = ProviderAccount {
        account: "gateway",
        generation: 1,
        local_binding: "gateway",
        remote: target,
        username: "fixture-user",
        secret: "fixture-secret",
        authentication_timeout_ms: 3000,
    };
    let mut listener = IaxServer::bind_with_accounts(&client, local, &[], &[account]).unwrap();
    listener.enable_async(&executor).unwrap();
    let mut call = listener
        .dial_provider_account(&DialRequest {
            account: "gateway",
            generation: 1,
            destination: "fixture",
            caller_number: "524950",
            caller_name: "Untrusted presentation",
            context: "",
            timeout_ms: 3000,
        })
        .unwrap();
    assert_eq!(call.peer.sample_rate_hz(), 8000);
    let login = prompt(&mut call.peer, "login");
    for digit in b"100#" {
        call.peer.send_digit(*digit).unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    let password = prompt(&mut call.peer, "password");
    assert_prompt(
        &password,
        ".--. .- ... ... .-- --- .-. -../.--. --- ..- -. -..",
    );
    for digit in b"1234#" {
        call.peer.send_digit(*digit).unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    let authenticated = prompt(&mut call.peer, "authenticated");
    for digit in b"*4#*70" {
        call.peer.send_digit(*digit).unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    let status = prompt(&mut call.peer, "status");
    assert_prompt(&status, "-. ---/.-.. .. -. -.- ...");
    assert_prompt(
        &authenticated,
        ".-.. --- --. .. -./--- -.-/..- -. .-.. --- -.-. -.-/... . .--. .- .-. .- - .",
    );
    assert_prompt(&login, "-- .- .. .-.. -... --- -..-/.--. --- ..- -. -..");
}

#[test]
fn real_product_private_call_authenticates_and_returns_status_without_radio_output() {
    const CHILD: &str = "RPTADV_TEST_REAL_PHONE_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host_services::product_phone_tests::real_product_private_call_authenticates_and_returns_status_without_radio_output", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "real product child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Isolated child only: retain ordinary redacted product diagnostics on failure.
    unsafe {
        libc::openlog(
            c"rpt-phone-fixture".as_ptr(),
            libc::LOG_PERROR,
            libc::LOG_USER,
        )
    };
    let directory = std::env::temp_dir().join(format!("rpt-real-phone-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("phone.secret");
    fs::write(&path, "fixture-secret\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let local_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let remote_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = local_socket.local_addr().unwrap();
    let remote = remote_socket.local_addr().unwrap();
    let configuration = format!(
        "[general]\nnode_enabled=yes\n[desk]\nradio_channel=fixture\nduplex=full\n\
         [morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n\
         [phone_account office]\nlocal_node=desk\nenabled=yes\nprotocol=iax\n\
         server={remote}\nusername=fixture-user\nsecrets_file=phone.secret\nregister=no\n{}",
        include_str!("../../../product/src/tests/fixtures/phone_mailbox.conf"),
    );
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt_advanced.conf"),
            resolve_phone_accounts(&configuration).unwrap(),
            &PreparedAccounts::default(),
            |_| panic!("numeric endpoint must not resolve"),
        )
        .unwrap();
    let account = &accounts.as_slice()[0];
    let providers = Box::new(ProviderSet::load().unwrap());
    // Runtime, listeners and private owners are stopped before this provider box drops.
    let provider_ref: &'static ProviderSet = unsafe { &*ptr::from_ref(providers.as_ref()) };
    let descriptors = provider_ref.product_runtime_descriptors().unwrap();
    let mut host = HostServicesOwner::new(
        resolve_radio_nodes(&configuration).unwrap(),
        Some(provider_ref),
        SecretsFile::parse("").unwrap(),
    )
    .unwrap();
    install_simulated_radio(&mut host);
    let mut runtime = unsafe {
        ProductRuntime::start(
            &descriptors.product,
            host,
            descriptors.control,
            descriptors.file,
            descriptors.speech,
            &configuration,
        )
    }
    .unwrap();
    let client = selected_client(Path::new("librptadviax2.so.2"), 1);
    drop(local_socket);
    let profile = ProviderAccount {
        account: "office",
        generation: account.generation(),
        local_binding: "desk",
        remote,
        username: "fixture-user",
        secret: account.secret(),
        authentication_timeout_ms: 3000,
    };
    let mut listener = IaxServer::bind_with_accounts(&client, local, &[], &[profile]).unwrap();
    runtime.enable_listener(&mut listener).unwrap();
    drop(remote_socket);
    let remote_client = client.clone();
    let remote_owner = thread::spawn(move || caller(remote_client, remote, local));
    let mut admission = Admission {
        runtime: &runtime,
        client: &client,
        account,
        local,
        accepted: false,
    };
    let deadline = Instant::now() + Duration::from_secs(50);
    while !remote_owner.is_finished() && Instant::now() < deadline {
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;
        unsafe {
            listener.poll_accounts(
                now_seconds,
                None,
                None,
                Some(authorize),
                Some(accept),
                ptr::from_mut(&mut admission).cast(),
            )
        }
        .unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let admitted = admission.accepted;
    let caller_result = remote_owner.join();
    runtime.stop().unwrap();
    drop(listener);
    UdpSocket::bind(local).unwrap();
    UdpSocket::bind(remote).unwrap();
    unsafe { libc::closelog() };
    assert!(
        RADIO.lock().unwrap().is_none(),
        "radio callback pump drained"
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "private replies never reached RF output"
    );
    fs::remove_dir_all(directory).unwrap();
    assert!(admitted, "real product must adopt its private executor");
    caller_result.unwrap();
}

#[path = "product_sip_tests.rs"]
pub(crate) mod sip;
