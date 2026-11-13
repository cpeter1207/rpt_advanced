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
static PUBLIC_OUTPUT: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn activate(
    _: *mut c_void,
    _: *mut c_void,
    receive: abi::rptadv_radio_receive_v3,
    receive_context: *mut c_void,
    transmit: abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    let (Some(receive), Some(transmit)) = (receive, transmit) else {
        return -1;
    };
    let contexts = (receive_context as usize, transmit_context as usize);
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        let mut first = 0;
        while !stopping.load(Ordering::Acquire) {
            let mut samples = [0.0; 960];
            let metadata = abi::UrpNativeReceiveMetadata {
                struct_size: size_of::<abi::UrpNativeReceiveMetadata>() as u32,
                abi_version: 1,
                generation_id: 1,
                first_sample_index: first,
                frame_count: 960,
                raw_receiver_keyed: 0,
                receiver_keyed: 0,
                vad_mode: 0,
                rf_burst_id: 0,
                vad_voice_sample_index: u64::MAX,
                vad_preroll_frames: 0,
                vad_voice_active: 0,
                vad_score: 0.0,
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
            PUBLIC_OUTPUT.fetch_add(
                samples
                    .iter()
                    .filter(|sample| sample.abs() > 0.0001)
                    .count()
                    + usize::from(keyed != 0),
                Ordering::Relaxed,
            );
            first += 960;
            thread::sleep(Duration::from_millis(20));
        }
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
        let _ = pump.worker.join();
    }
    0
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

/// Narrow Morse envelope check: 40 WPM, 800 Hz, 8 kHz G.711 loopback.
fn decode_prompt(pcm: &[f32]) -> String {
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
                if *count < 480 { '.' } else { '-' }
            } else if *count > 1200 {
                '/'
            } else if *count > 480 {
                ' '
            } else {
                '_'
            }
        })
        .filter(|symbol| *symbol != '_')
        .collect()
}
fn assert_prompt(pcm: &[f32], expected: &str) {
    assert_eq!(decode_prompt(pcm), expected, "decoded private Morse reply");
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
    for digit in b"100#1234#" {
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
    assert_prompt(
        &login,
        "-- .- .. .-.. -... --- -..-/.--. --- ..- -. -../.--. .. -./.--. --- ..- -. -..",
    );
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
    host.descriptor.radio_activate = Some(activate);
    host.descriptor.radio_ready = Some(ready);
    host.descriptor.radio_quiesce = Some(quiesce);
    let mut runtime = unsafe {
        ProductRuntime::start(
            descriptors.product,
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
