//! Qualification of existing alpha12 behavior, not a new jitter implementation.
//! Released source 4e7026c5234a7c7fb61907bbe56fbaef3a18963b:
//! client.rs::configure_jitter selects the phone profile by real provider ownership;
//! server::prepare_dial and server_provider::prepare_poll select FormatCatalog::phone.
//! Both real loopback call directions use the host's unchanged configure wrapper.
use super::*;
use crate::iax::{
    IaxClient, IaxServer,
    accounts::{DialRequest, ProviderAccount},
};
use std::{
    net::UdpSocket,
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct JitterEvidence {
    real: Option<abi::rptadv_jitter_descriptor_v1>,
    configurations: Vec<abi::rptadv_jitter_config_v1>,
    fail_next: bool,
    created: usize,
    destroyed: usize,
}
thread_local! { static JITTER: RefCell<JitterEvidence> = RefCell::default(); }

unsafe extern "C" fn create_jitter(
    config: *const abi::rptadv_jitter_config_v1,
    output: *mut *mut c_void,
) -> i32 {
    let create = JITTER.with_borrow_mut(|evidence| {
        evidence.configurations.push(unsafe { *config });
        if std::mem::take(&mut evidence.fail_next) {
            None
        } else {
            evidence.real.unwrap().create
        }
    });
    let Some(create) = create else {
        unsafe { output.write(ptr::null_mut()) };
        return -1;
    };
    let status = unsafe { create(config, output) };
    if status == 0 {
        JITTER.with_borrow_mut(|evidence| evidence.created += 1);
    }
    status
}
unsafe extern "C" fn destroy_jitter(context: *mut c_void) {
    let destroy = JITTER.with_borrow_mut(|evidence| {
        evidence.destroyed += 1;
        evidence.real.unwrap().destroy.unwrap()
    });
    unsafe { destroy(context) };
}

fn provider(account: &PreparedAccount) -> ProviderAccount<'_> {
    ProviderAccount {
        account: &account.settings().id,
        generation: account.generation(),
        local_binding: &account.settings().local_node,
        remote: account.remote(),
        username: &account.settings().username,
        secret: account.secret(),
        authentication_timeout_ms: 3000,
    }
}

/// Exercise only the actual preparation wrapper. No peer executor is attached.
fn qualify(
    owner: &HostServicesOwner,
    account: &PreparedAccount,
    call: ProviderCall,
    local: SocketAddr,
    format: u64,
) {
    assert_eq!(call.receipt.format, format);
    let handle = Handle {
        owner,
        raw: owner.provider_peer_handle(call, account, local).unwrap(),
    };
    let identity = metadata(&handle);
    assert_eq!((identity.protocol, identity.capabilities), (3, 51));
    assert_eq!(identity.account_generation, account.generation());
    unsafe extern "C" fn step(context: *mut c_void) -> i32 {
        unsafe { &*context.cast::<AtomicU64>() }.fetch_add(1, Ordering::Relaxed);
        -1
    }
    let stepped = AtomicU64::new(0);
    let mut registration = ptr::dangling_mut::<c_void>();
    let api = owner.descriptor();
    let status = unsafe {
        api.peer_step_attach.unwrap()(
            api.context,
            handle.raw,
            Some(step),
            ptr::from_ref(&stepped).cast_mut().cast(),
            &mut registration,
        )
    };
    if !registration.is_null() {
        // Even a regression must quiesce before borrowed fixture context drops.
        unsafe {
            api.peer_step_stop.unwrap()(api.context, registration);
            api.peer_step_detach.unwrap()(api.context, registration);
        }
    }
    assert_eq!(status, -1);
    assert!(registration.is_null());
    assert_eq!(stepped.load(Ordering::Relaxed), 0);
    let ingress = owner.context.ingress.as_ref().unwrap();
    let mut observed = *ingress.jitter;
    JITTER.with_borrow_mut(|evidence| {
        *evidence = JitterEvidence {
            real: Some(observed),
            fail_next: true,
            configurations: Vec::with_capacity(2),
            ..JitterEvidence::default()
        };
    });
    observed.create = Some(create_jitter);
    observed.destroy = Some(destroy_jitter);
    let raw = unsafe { crate::host_services::iax_peer_ref(handle.raw) }
        .unwrap()
        .peer
        .raw_handle();
    assert!(
        unsafe {
            ingress
                .executor
                .configure_jitter(raw, ptr::from_ref(&observed).cast())
        }
        .is_err()
    );
    JITTER.with_borrow(|evidence| assert_eq!((evidence.created, evidence.destroyed), (0, 0)));
    // Failed preparation must leave this unique peer usable, with unchanged identity.
    assert_eq!(metadata(&handle).qso_generation, identity.qso_generation);
    unsafe {
        ingress
            .executor
            .configure_jitter(raw, ptr::from_ref(&observed).cast())
    }
    .unwrap();
    // A second owner is refused before another native jitter context is created.
    assert!(
        unsafe {
            ingress
                .executor
                .configure_jitter(raw, ptr::from_ref(&observed).cast())
        }
        .is_err()
    );
    let mut observation: abi::rptadv_iax2_peer_observation_v1 = unsafe { std::mem::zeroed() };
    observation.struct_size = size_of::<abi::rptadv_iax2_peer_observation_v1>() as u32;
    observation.abi_version = 1;
    assert_eq!(
        unsafe {
            ingress
                .executor
                .observe(raw, ptr::from_mut(&mut observation).cast())
        },
        0
    );
    assert_eq!(observation.jitter_delay_samples, 320);
    JITTER.with_borrow(|evidence| {
        assert_eq!(evidence.configurations.len(), 2);
        for config in &evidence.configurations {
            assert_eq!((config.clock_rate_hz, config.concealment_span), (8000, 160));
            assert_eq!(
                (
                    config.minimum_delay,
                    config.initial_delay,
                    config.maximum_delay
                ),
                (160, 320, 800)
            );
            assert_eq!(
                (config.maximum_packets, config.maximum_payload_bytes),
                (64, 160)
            );
        }
        assert_eq!((evidence.created, evidence.destroyed), (1, 0));
    });
    drop(handle);
    JITTER.with_borrow(|evidence| assert_eq!((evidence.created, evidence.destroyed), (1, 1)));
}

struct Incoming {
    client: IaxClient,
    account: Arc<PreparedAccount>,
    local: SocketAddr,
    format: u64,
    call: Option<ProviderCall>,
    authorized: usize,
}
impl Incoming {
    fn matches(&self, metadata: &abi::rptadv_iax2_provider_incoming_v1) -> bool {
        let receipt = metadata.receipt;
        let endpoint = |value: abi::rptadv_iax2_endpoint_v1, expected: SocketAddr| {
            value.family == 4
                && value.port == expected.port()
                && value.address[..4] == [127, 0, 0, 1]
                && value.address[4..] == [0; 12]
                && value.flowinfo == 0
                && value.scope_id == 0
        };
        let text = |value: abi::rptadv_iax2_bytes_v1, expected: &str| {
            !value.data.is_null()
                && value.length == expected.len()
                && unsafe { std::slice::from_raw_parts(value.data, value.length) }
                    == expected.as_bytes()
        };
        metadata.struct_size as usize == size_of::<abi::rptadv_iax2_provider_incoming_v1>()
            && metadata.abi_version == 1
            && receipt.struct_size as usize
                == size_of::<abi::rptadv_iax2_provider_call_receipt_v1>()
            && receipt.abi_version == 1
            && receipt.account_generation == self.account.generation()
            && receipt.call_generation != 0
            && receipt.format == self.format
            && (1..=32767).contains(&receipt.local_call)
            && (1..=32767).contains(&receipt.remote_call)
            && endpoint(receipt.local, self.local)
            && endpoint(receipt.remote, self.account.remote())
            && text(metadata.account, &self.account.settings().id)
            && text(metadata.local_binding, &self.account.settings().local_node)
    }
}
unsafe extern "C" fn authorize(
    context: *mut c_void,
    metadata: *const abi::rptadv_iax2_provider_incoming_v1,
) -> i32 {
    let incoming = unsafe { &mut *context.cast::<Incoming>() };
    let Some(metadata) = (unsafe { metadata.as_ref() }) else {
        return -1;
    };
    if !incoming.matches(metadata) {
        return -1;
    }
    incoming.authorized += 1;
    0
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    metadata: *const abi::rptadv_iax2_provider_incoming_v1,
    raw: *mut c_void,
) -> i32 {
    let incoming = unsafe { &mut *context.cast::<Incoming>() };
    let Some(metadata) = (unsafe { metadata.as_ref() }) else {
        return -1;
    };
    if incoming.call.is_some() || incoming.authorized != 1 || !incoming.matches(metadata) {
        return -1;
    }
    // Test-only receipt copying after real provider authentication. The accepted
    // callback transfers this raw handle once; no production admission is added.
    let Ok(peer) = (unsafe { incoming.client.clone().adopt_inbound(raw) }) else {
        return -1;
    };
    let receipt = metadata.receipt;
    incoming.call = Some(ProviderCall {
        peer,
        receipt: CallReceipt {
            account_generation: receipt.account_generation,
            call_generation: receipt.call_generation,
            format: receipt.format,
            local_call: receipt.local_call,
            remote_call: receipt.remote_call,
            local: incoming.local,
            remote: incoming.account.remote(),
        },
    });
    0
}

#[test]
fn real_provider_calls_prepare_phone_jitter_without_activating_private_executor() {
    for (profile, format) in [(1, 4), (2, 8)] {
        let left_reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let right_reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let left = left_reservation.local_addr().unwrap();
        let right = right_reservation.local_addr().unwrap();
        let client = tests::selected_client(Path::new("librptadviax2.so.2"), profile);
        let outgoing_client = client.clone();
        let right_account = account_at(left);
        let host = owner();
        drop(right_reservation);
        let mut listener =
            IaxServer::bind_with_accounts(&client, right, &[], &[provider(&right_account)])
                .unwrap();
        host.enable_listener(&mut listener).unwrap();
        let mut incoming = Incoming {
            client,
            account: Arc::clone(&right_account),
            local: right,
            format,
            call: None,
            authorized: 0,
        };
        drop(left_reservation);
        // Fixture-only thread: synchronous dial and the opposite listener must
        // advance concurrently. Production gains no per-call thread or scheduler.
        let dial = thread::spawn(move || {
            let account = account_at(right);
            let host = owner();
            let mut listener =
                IaxServer::bind_with_accounts(&outgoing_client, left, &[], &[provider(&account)])
                    .unwrap();
            host.enable_listener(&mut listener).unwrap();
            let call = listener
                .dial_provider_account(&DialRequest {
                    account: &account.settings().id,
                    generation: account.generation(),
                    destination: "fixture",
                    caller_number: "not-an-identity",
                    caller_name: "Fixture",
                    context: "",
                    timeout_ms: 3000,
                })
                .unwrap();
            qualify(&host, &account, call, left, format);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while incoming.call.is_none() && !dial.is_finished() && Instant::now() < deadline {
            unsafe {
                listener.poll_accounts(
                    0,
                    None,
                    None,
                    Some(authorize),
                    Some(accept),
                    ptr::from_mut(&mut incoming).cast(),
                )
            }
            .unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        assert_eq!(incoming.authorized, 1);
        let call = incoming.call.take().expect("real incoming provider call");
        qualify(&host, &right_account, call, right, format);
    }
}

/// Exercise the real DSO and UDP ACK window while the public host refuses digits.
#[test]
fn real_provider_ack_progress_continues_behind_a_refused_private_digit() {
    use std::sync::mpsc;
    let left_reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let right_reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let left = left_reservation.local_addr().unwrap();
    let right = right_reservation.local_addr().unwrap();
    let client = tests::selected_client(Path::new("librptadviax2.so.2"), 1);
    let outgoing_client = client.clone();
    let account = account_at(left);
    let host = owner();
    drop(right_reservation);
    let mut listener =
        IaxServer::bind_with_accounts(&client, right, &[], &[provider(&account)]).unwrap();
    host.enable_listener(&mut listener).unwrap();
    let mut incoming = Incoming {
        client,
        account: Arc::clone(&account),
        local: right,
        format: 4,
        call: None,
        authorized: 0,
    };
    let (command, commands) = mpsc::channel();
    let (result, results) = mpsc::channel();
    drop(left_reservation);
    let dial = thread::spawn(move || {
        let account = account_at(right);
        let host = owner();
        let mut listener =
            IaxServer::bind_with_accounts(&outgoing_client, left, &[], &[provider(&account)])
                .unwrap();
        host.enable_listener(&mut listener).unwrap();
        let mut call = listener
            .dial_provider_account(&DialRequest {
                account: &account.settings().id,
                generation: account.generation(),
                destination: "fixture",
                caller_number: "",
                caller_name: "",
                context: "",
                timeout_ms: 3000,
            })
            .unwrap();
        let ingress = host.context.ingress.as_ref().unwrap();
        unsafe {
            ingress
                .executor
                .configure_jitter(call.peer.raw_handle(), ptr::from_ref(ingress.jitter).cast())
        }
        .unwrap();
        call.peer.send_digit(b'5').unwrap();
        let started = Instant::now();
        let mut acknowledged = 0;
        let mut second = false;
        let mut completed = false;
        loop {
            match commands.try_recv() {
                Ok(false) => break,
                Ok(true) => {
                    call.peer.send_digit(b'6').unwrap();
                    second = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "private ACK progress timed out"
            );
            let event = call
                .peer
                .poll_source(
                    &mut [0.0; 160],
                    19,
                    started.elapsed().as_nanos() as u64 + 1_000_000_000,
                    acknowledged,
                )
                .unwrap();
            if event.discontinuity {
                acknowledged = event.media_epoch;
            }
            let mut observation: abi::rptadv_iax2_peer_observation_v1 =
                unsafe { std::mem::zeroed() };
            observation.struct_size = size_of::<abi::rptadv_iax2_peer_observation_v1>() as u32;
            observation.abi_version = 1;
            assert_eq!(
                unsafe {
                    ingress.executor.observe(
                        call.peer.raw_handle(),
                        ptr::from_mut(&mut observation).cast(),
                    )
                },
                0
            );
            if second && !completed && observation.egress_reliable_pending == 0 {
                result.send(()).unwrap();
                completed = true;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    let started = Instant::now();
    while incoming.call.is_none() && started.elapsed() < Duration::from_secs(5) {
        unsafe {
            listener.poll_accounts(
                0,
                None,
                None,
                Some(authorize),
                Some(accept),
                ptr::from_mut(&mut incoming).cast(),
            )
        }
        .unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let call = incoming
        .call
        .take()
        .expect("authenticated real provider call");
    assert_eq!(incoming.authorized, 1);
    let handle = Handle {
        owner: &host,
        raw: host.provider_peer_handle(call, &account, right).unwrap(),
    };
    let ingress = host.context.ingress.as_ref().unwrap();
    let raw = unsafe { crate::host_services::iax_peer_ref(handle.raw) }
        .unwrap()
        .peer
        .raw_handle();
    unsafe {
        ingress
            .executor
            .configure_jitter(raw, ptr::from_ref(ingress.jitter).cast())
    }
    .unwrap();
    let mut sink = Sink {
        digit_status: Some(1),
        ..Sink::default()
    };
    let now = || started.elapsed().as_nanos() as u64 + 1_000_000_000;
    while sink.digit_offers.is_empty() && started.elapsed() < Duration::from_secs(5) {
        read(&handle, Some(&mut sink), now());
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sink.digit_offers, [u32::from(b'5')]);
    command.send(true).unwrap();
    let mut acked = false;
    while started.elapsed() < Duration::from_secs(5) {
        read(&handle, Some(&mut sink), now());
        if results.try_recv().is_ok() {
            acked = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        acked,
        "refused first digit stalled the later reliable frame's ACK"
    );
    assert!(
        sink.digit_offers
            .iter()
            .all(|symbol| *symbol == u32::from(b'5'))
    );
    sink.digit_offers.clear();
    sink.digit_status = Some(0);
    while sink.digit_offers.len() < 2 && started.elapsed() < Duration::from_secs(5) {
        read(&handle, Some(&mut sink), now());
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sink.digit_offers, [u32::from(b'5'), u32::from(b'6')]);
    command.send(false).unwrap();
    dial.join().unwrap();
    drop(handle);
}
