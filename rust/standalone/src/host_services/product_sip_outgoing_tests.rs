//! Actual typed product origination through the registered SIP reactor and radio mixer.
use super::*;
#[path = "product_sip_ivr_decoder.rs"]
mod ivr_decoder;
#[derive(Clone, Copy, PartialEq, Eq)]
enum IvrMode {
    Disabled,
    Events,
    InBand,
}

use crate::phone::{
    media::MediaDescription,
    sip_test_registrar::{receive_register, response},
};

fn receive(socket: &UdpSocket) -> (String, SocketAddr) {
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    receive_register(socket, &mut [0; 8192]).expect("bounded outgoing dialog progress")
}
fn command(runtime: &ProductRuntime, operation: u32, call_id: u64) -> (i32, u64) {
    let (account, destination) = if operation == abi::RPTADV_PHONE_DIAL {
        ("office", "sip:called@fixture.invalid")
    } else {
        ("", "")
    };
    let request = abi::rptadv_phone_command_v1 {
        struct_size: size_of::<abi::rptadv_phone_command_v1>() as u32,
        abi_version: abi::RPTADV_PHONE_COMMAND_ABI_VERSION,
        operation,
        account: account.as_ptr().cast(),
        account_length: account.len(),
        destination: destination.as_ptr().cast(),
        destination_length: destination.len(),
        call_id,
    };
    let mut id = 0;
    let result = unsafe {
        runtime
            .product_descriptor()
            .phone_command
            .expect("actual typed product entry")(c"1000".as_ptr(), 4, &request, &mut id)
    };
    (result, id)
}
fn next_call(runtime: &ProductRuntime) -> u64 {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let (result, id) = command(runtime, abi::RPTADV_PHONE_DIAL, 0);
        if result == 0 {
            assert_ne!(id, 0);
            return id;
        }
        assert_eq!((result, id), (-1, 0));
        assert!(
            Instant::now() < until,
            "previous exact claim was not released"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
static ANSWER_ACKS: AtomicUsize = AtomicUsize::new(0);
static SELECTED_PAYLOAD: AtomicUsize = AtomicUsize::new(0);
static CONTINUOUS_RTP: AtomicBool = AtomicBool::new(false);
static LAST_RTP: Mutex<Option<(u16, u32, u32)>> = Mutex::new(None);

// Successful missing/concealed/reset/control source boundaries retire the phone
// generation. Observe only that scalar fact, forwarding all original credit.
static SOURCE_CUTS: AtomicUsize = AtomicUsize::new(0);
static LAST_MARKED_CUT: AtomicUsize = AtomicUsize::new(0);
static ALLOW_SOURCE_CUT: AtomicBool = AtomicBool::new(false);
struct SourceCutObserver {
    original: abi::rptadv_peer_source_sink_v1,
}
unsafe extern "C" fn observe_source_audio(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
) -> usize {
    // SAFETY: the synchronous read retains the full copied sink and original context.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted =
        unsafe { original.audio.unwrap()(original.context, start, pcm, count, concealed) };
    if concealed != 0 && accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_audio_timed(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    // SAFETY: borrowed PCM and birth are forwarded unchanged, never inspected or retained.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe {
        original.audio_timed.unwrap()(original.context, start, pcm, count, concealed, birth)
    };
    if concealed != 0 && accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    // SAFETY: this wrapper exists only for the original nonnull callback.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.gap.unwrap()(original.context, start, count) };
    if accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_discontinuity(context: *mut c_void) -> i32 {
    // SAFETY: forward the serial owner and its exact discontinuity result.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.source_discontinuity.unwrap()(original.context) };
    if accepted == 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_digit(context: *mut c_void, symbol: u32) -> i32 {
    // SAFETY: forward the exact symbol; retain only anonymous accepted-cut credit.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.digit.unwrap()(original.context, symbol) };
    if accepted == 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_peer_read_source(
    context: *mut c_void,
    peer: *mut c_void,
    completion: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    text: abi::rptadv_peer_source_text_v1,
    text_context: *mut c_void,
    output: *mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    if sink.is_null()
        // SAFETY: a supplied sink promises the readable size prefix.
        || unsafe { ptr::addr_of!((*sink).struct_size).read() }
            != size_of::<abi::rptadv_peer_source_sink_v1>() as u32
        // SAFETY: the preceding size check covers this version field.
        || unsafe { ptr::addr_of!((*sink).abi_version).read() } != 1
    {
        // SAFETY: preserve existing validation and all arguments for an older/newer/invalid sink.
        return unsafe {
            crate::host_services::peer_read_source(
                context,
                peer,
                completion,
                sink,
                text,
                text_context,
                output,
            )
        };
    }
    // SAFETY: the checked current prefix covers this Copy record; no pointee is owned.
    let mut trace = SourceCutObserver {
        original: unsafe { sink.read() },
    };
    let mut delegated = trace.original;
    delegated.context = ptr::from_mut(&mut trace).cast();
    if delegated.audio.is_some() {
        delegated.audio = Some(observe_source_audio);
    }
    if delegated.audio_timed.is_some() {
        delegated.audio_timed = Some(observe_source_audio_timed);
    }
    if delegated.gap.is_some() {
        delegated.gap = Some(observe_source_gap);
    }
    if delegated.source_discontinuity.is_some() {
        delegated.source_discontinuity = Some(observe_source_discontinuity);
    }
    if delegated.digit.is_some() {
        delegated.digit = Some(observe_source_digit);
    }
    // SAFETY: each callback restores its original context, arguments and credit.
    // Clock fields, completion, text callback/context and output are unchanged.
    unsafe {
        crate::host_services::peer_read_source(
            context,
            peer,
            completion,
            &delegated,
            text,
            text_context,
            output,
        )
    }
}

fn observe_rtp(wire: &[u8]) {
    let current = (
        u16::from_be_bytes(wire[2..4].try_into().unwrap()),
        u32::from_be_bytes(wire[4..8].try_into().unwrap()),
        u32::from_be_bytes(wire[8..12].try_into().unwrap()),
    );
    let mut last = LAST_RTP.lock().unwrap();
    if CONTINUOUS_RTP.load(Ordering::Acquire) {
        if let Some((sequence, timestamp, ssrc)) = *last {
            assert_eq!(
                (current.0, current.2),
                (sequence.wrapping_add(1), ssrc),
                "one accepted RTP sequence and SSRC across source retirement"
            );
            let delta = current.1.wrapping_sub(timestamp);
            if wire[1] & 0x80 != 0 || delta != 160 {
                assert!(
                    ALLOW_SOURCE_CUT.load(Ordering::Acquire),
                    "healthy negotiated20ms silence must remain continuous"
                );
                assert_ne!(
                    wire[1] & 0x80,
                    0,
                    "source cut must retain the existing reset marker"
                );
                let cuts = SOURCE_CUTS.load(Ordering::Acquire);
                assert!(
                    cuts > LAST_MARKED_CUT.swap(cuts, Ordering::AcqRel),
                    "a marker alone is not evidence of an accepted source cut"
                );
                assert!(
                    (delta as i32) > 0,
                    "retirement cannot replay or rewind source time"
                );
            }
        }
    }
    if last.is_none() {
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
    }
    *last = Some(current);
}

fn quiet_media(caller: &mut Caller, codec: &mut G711Codec, packets: u64, incoming: bool) {
    let rf_before = PUBLIC_OUTPUT.load(Ordering::Acquire);
    let started = Instant::now();
    let mut next_input = started;
    let mut wire = [0; 2048];
    // Thirty seconds is a bounded steady-state transport test, not an assumed NAT lifetime.
    for packet in 1..=packets {
        let until = Instant::now() + Duration::from_secs(3);
        let (length, source) = loop {
            if incoming && Instant::now() >= next_input {
                caller.audio_payload(
                    codec,
                    &[0.0; 160],
                    SELECTED_PAYLOAD.load(Ordering::Acquire) as u8,
                );
                next_input += Duration::from_millis(20);
                // Preserve source gaps after missed slots, never burst old PCM.
                while next_input <= Instant::now() {
                    caller.timestamp = caller.timestamp.wrapping_add(160);
                    next_input += Duration::from_millis(20);
                }
            }
            service(caller);
            if let Ok(packet) = caller.rtp.recv_from(&mut wire) {
                break packet;
            }
            assert!(
                Instant::now() < until,
                "answered idle call suppressed RTP after its first packet"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!((length, source), (172, caller.media));
        assert_eq!(
            wire[1] & 0x7f,
            SELECTED_PAYLOAD.load(Ordering::Acquire) as u8
        );
        observe_rtp(&wire[..length]);
        let mut pcm = [0.0; 160];
        codec
            .decode(wire[12..172].try_into().unwrap(), &mut pcm)
            .unwrap();
        assert!(pcm.iter().all(|sample| sample.abs() < 0.001));
        if packet % 50 == 0 {
            // Include ordinary OS scheduling tolerance without accepting a catch-up flood.
            let expected = Duration::from_millis(packet * 20);
            let elapsed = started.elapsed();
            assert!(elapsed + Duration::from_millis(100) >= expected);
            assert!(elapsed <= expected + Duration::from_millis(500));
        }
    }
    assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        rf_before,
        "transport silence must not key or populate RF"
    );
}

fn service(caller: &Caller) {
    let mut wire = [0; 8192];
    while let Ok((count, source)) = caller.sip.recv_from(&mut wire) {
        let (first, values) = fields(&wire[..count]);
        if first.starts_with("ACK ") {
            assert!(values["cseq"].ends_with(" ACK"));
            ANSWER_ACKS.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        assert!(
            !first.starts_with("BYE "),
            "product unexpectedly closed outgoing voice"
        );
        assert!(
            first.starts_with("OPTIONS ") || first.starts_with("REGISTER "),
            "unexpected dialog message: {first}"
        );
        caller.answer_register(&values, source);
    }
}
fn drain(caller: &Caller, codec: &mut G711Codec, returned: &mut Vec<f32>) {
    let mut wire = [0; 2048];
    while let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
        assert_eq!(
            source, caller.media,
            "same offered RTP socket owns answered media"
        );
        assert_eq!(length, 172);
        assert_eq!(
            (wire[0], wire[1] & 0x7f),
            (0x80, SELECTED_PAYLOAD.load(Ordering::Acquire) as u8)
        );
        observe_rtp(&wire[..length]);
        let mut pcm = [0.0; 160];
        codec
            .decode(wire[12..172].try_into().unwrap(), &mut pcm)
            .unwrap();
        returned.extend_from_slice(&pcm);
    }
}
fn media(caller: &mut Caller, codec: &mut G711Codec, samples: &[f32], returned: &mut Vec<f32>) {
    let mut next = Instant::now();
    for chunk in samples.chunks(160) {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let mut pcm = [0.0; 160];
        pcm[..chunk.len()].copy_from_slice(chunk);
        caller.audio_payload(codec, &pcm, SELECTED_PAYLOAD.load(Ordering::Acquire) as u8);
        service(caller);
        drain(caller, codec, returned);
        next += Duration::from_millis(20);
        // This bounded fixture never catches up by emitting a burst of old media.
        // A missed source slot is explicit in its timestamp, not a renewed birth.
        while next <= Instant::now() {
            caller.timestamp = caller.timestamp.wrapping_add(160);
            next += Duration::from_millis(20);
        }
    }
    // Preserve the final packet interval across consecutive media calls too.
    if let Some(wait) = next.checked_duration_since(Instant::now()) {
        thread::sleep(wait);
    }
}

fn radio_digits(digits: &[u8]) {
    // Existing receiver clock consumes real tones and keyed gaps; no direct
    // runtime command or second input queue stands in for the RF decoder.
    LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
    thread::sleep(Duration::from_millis(100));
    for &digit in digits {
        LOCAL_DTMF.store(usize::from(digit), Ordering::Release);
        thread::sleep(Duration::from_millis(160));
        LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
        thread::sleep(Duration::from_millis(120));
    }
    LOCAL_DTMF.store(0, Ordering::Release);
}

#[test]
fn actual_product_originates_two_way_phone_and_releases_the_exact_call() {
    product_outgoing(false, false, false, G711::Mulaw);
}

#[test]
fn actual_radio_dtmf_originates_two_way_phone_and_hangs_up() {
    product_outgoing(true, false, false, G711::Mulaw);
}

#[test]
fn actual_answer_first_utterance_reaches_rf_without_any_receiver_key() {
    product_outgoing(false, true, false, G711::Mulaw);
}

#[test]
fn actual_rf_answer_first_utterance_reaches_rf_without_receiver_rekey() {
    product_outgoing(true, true, false, G711::Mulaw);
}

#[test]
fn actual_answered_quiet_call_opens_symmetric_rtp_before_any_receiver_key() {
    product_outgoing(false, true, true, G711::Mulaw);
}

#[test]
fn actual_alaw_quiet_call_sends_continuous_silence_without_receiver_key() {
    product_outgoing(false, true, true, G711::Alaw);
}

// A symmetric media relay learns only the actual RTP source, never the SDP
// address. It cannot forward the first greeting before that mapping exists.
fn latch(caller: &Caller) -> Option<SocketAddr> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut wire = [0; 2048];
    while Instant::now() < until {
        service(caller);
        if let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
            assert_eq!(length, 172);
            assert_eq!(
                (wire[0], wire[1] & 0x7f),
                (0x80, SELECTED_PAYLOAD.load(Ordering::Acquire) as u8)
            );
            observe_rtp(&wire[..length]);
            return Some(source);
        }
        thread::sleep(Duration::from_millis(1));
    }
    None
}

#[test]
fn actual_radio_digits_reach_telephone_ivr_without_control_or_audio_leak() {
    product_outgoing_with_ivr(true, true, false, G711::Mulaw, IvrMode::Events);
}

#[test]
fn actual_radio_digits_use_private_inband_when_event_payload_is_absent() {
    product_outgoing_with_ivr(true, true, false, G711::Mulaw, IvrMode::InBand);
}

fn product_outgoing(radio: bool, immediate: bool, latching: bool, selected: G711) {
    product_outgoing_with_ivr(radio, immediate, latching, selected, IvrMode::Disabled);
}

fn product_outgoing_with_ivr(
    radio: bool,
    immediate: bool,
    latching: bool,
    selected: G711,
    ivr: IvrMode,
) {
    const CHILD: &str = "RPTADV_TEST_OUTGOING_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let name = if ivr == IvrMode::InBand {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_digits_use_private_inband_when_event_payload_is_absent"
        } else if ivr == IvrMode::Events {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_digits_reach_telephone_ivr_without_control_or_audio_leak"
        } else if selected == G711::Alaw {
            "host_services::product_phone_tests::sip::outgoing::actual_alaw_quiet_call_sends_continuous_silence_without_receiver_key"
        } else if latching {
            "host_services::product_phone_tests::sip::outgoing::actual_answered_quiet_call_opens_symmetric_rtp_before_any_receiver_key"
        } else if radio && immediate {
            "host_services::product_phone_tests::sip::outgoing::actual_rf_answer_first_utterance_reaches_rf_without_receiver_rekey"
        } else if immediate {
            "host_services::product_phone_tests::sip::outgoing::actual_answer_first_utterance_reaches_rf_without_any_receiver_key"
        } else if radio {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_dtmf_originates_two_way_phone_and_hangs_up"
        } else {
            "host_services::product_phone_tests::sip::outgoing::actual_product_originates_two_way_phone_and_releases_the_exact_call"
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "outgoing product child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    SELECTED_PAYLOAD.store(
        if selected == G711::Mulaw { 0 } else { 8 },
        Ordering::Release,
    );
    let directory =
        std::env::temp_dir().join(format!("rpt-outgoing-product-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("phone.secret"), "synthetic-test-secret\n").unwrap();
    fs::set_permissions(
        directory.join("phone.secret"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let sip = UdpSocket::bind("127.0.0.1:0").unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
    rtp.set_nonblocking(true).unwrap();
    // Admission default deliberately differs from the eligible initiating node.
    let configuration = format!(
        "[general]\nnode_enabled=yes\n[1000]\nradio_channel=fixture\nduplex=full\nphone_allow_accounts=office\n[2000]\nnode_enabled=no\n[phone_account office]\nlocal_node=2000\nattach_local_nodes=1000\nenabled=yes\nprotocol=sip\nserver={}\nusername=fixture-user\nsecrets_file=phone.secret\nregister=yes\n",
        sip.local_addr().unwrap()
    );
    let configuration = if radio {
        configuration.replace("[1000]", "[1000]\nlink_command_phone_dial=6\nlink_command_phone_hangup=0\nphone_outgoing_account=office") + "\n[morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n"
    } else {
        configuration
    };
    let configuration = if immediate {
        configuration.replace("[1000]", "[1000]\nfull_duplex=yes\nsquelch_delay_ms=30")
            + "\n[radio]\nradio_duplex_mode=half\n[radio 1000]\nvad_mode=kerchunk\nvad_hang_ms=1000\n"
    } else {
        configuration
    };
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt.conf"),
            resolve_phone_accounts(&configuration).unwrap(),
            &PreparedAccounts::default(),
            |_| unreachable!(),
        )
        .unwrap();
    let providers = Box::new(ProviderSet::load().unwrap());
    let provider_ref: &'static ProviderSet = unsafe { &*ptr::from_ref(providers.as_ref()) };
    let descriptors = provider_ref.product_runtime_descriptors().unwrap();
    let mut host = HostServicesOwner::new(
        resolve_radio_nodes(&configuration).unwrap(),
        Some(provider_ref),
        SecretsFile::parse("").unwrap(),
    )
    .unwrap();
    install_simulated_radio(&mut host);
    host.descriptor.peer_read_source = Some(observe_peer_read_source);
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
    let mut worker =
        crate::phone::registration::Worker::from_provider(provider_ref.sip_provider().unwrap())
            .unwrap();
    worker.prepare(&accounts).unwrap().commit().unwrap();
    let (register, target) = receive(&sip);
    assert!(register.starts_with("REGISTER "));
    sip.send_to(response(&register, 200, false).as_bytes(), target)
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while worker.observe().unwrap()[0].1.state != 2 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    runtime.publish_sip(Some(&worker), &accounts);
    let mut before_dial_ack = 0;
    let (refused, id) = if radio {
        radio_digits(b"*600123#");
        before_dial_ack = PUBLIC_OUTPUT.load(Ordering::Acquire);
        (0, 0)
    } else {
        let refused = next_call(&runtime);
        let (invite, remote) = receive(&sip);
        assert!(invite.starts_with("INVITE sip:called@fixture.invalid "));
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_DIAL, 0),
            (-1, 0),
            "one pending claim before answer"
        );
        sip.send_to(response(&invite, 486, false).as_bytes(), remote)
            .unwrap();
        assert!(receive(&sip).0.starts_with("ACK "));
        let id = next_call(&runtime);
        assert_ne!(id, refused);
        (refused, id)
    };
    let (invite, remote) = if radio {
        // Move the existing finite-result bound ahead of INVITE, not a guessed sleep.
        sip.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        receive_register(&sip, &mut [0; 8192])
            .expect("finite acknowledgment before outgoing INVITE")
    } else {
        receive(&sip)
    };
    if radio {
        assert!(
            invite.starts_with(&format!("INVITE sip:00123@{} ", sip.local_addr().unwrap())),
            "literal telephone digits retain leading zeros"
        );
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_DIAL, 0),
            (-1, 0),
            "RF command reserved the same pending owner before answer"
        );
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before_dial_ack + 160,
            "finite LOCAL acknowledgment must finish before INVITE"
        );
        let before = PUBLIC_OUTPUT.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            before,
            "acknowledgment still producing after INVITE"
        );
    }
    let offer =
        MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
    let before_ringing = PUBLIC_OUTPUT.load(Ordering::Acquire);
    sip.send_to(response(&invite, 180, false).as_bytes(), remote)
        .unwrap();
    if radio {
        let until = Instant::now() + Duration::from_secs(3);
        while PUBLIC_OUTPUT.load(Ordering::Acquire) <= before_ringing + 160
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before_ringing + 160,
            "actual180 must produce LOCAL ringing before answer"
        );
    }
    assert!(worker.incoming().is_none());
    let answer = MediaDescription {
        codec: selected,
        telephone_event: if ivr == IvrMode::InBand {
            None
        } else {
            offer.telephone_event
        },
        ..offer.clone()
    }
    .answer(rtp.local_addr().unwrap(), 99)
    .unwrap();
    let accepted = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:called@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        sip.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    let rx_keys_at_answer = LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire);
    let linked_before_answer = LINKED_OUTPUT_TURNS.load(Ordering::Acquire);
    sip.send_to(accepted.as_bytes(), remote).unwrap();
    if !immediate {
        assert!(receive(&sip).0.starts_with("ACK "));
    }
    sip.set_nonblocking(true).unwrap();
    let mut caller = Caller {
        sip,
        rtp,
        target,
        media: offer.remote,
        to: String::new(),
        sequence: 0,
        timestamp: 0,
        keepalive: false,
    };
    let mut codec = G711Codec::load(selected).unwrap();
    let mut returned = Vec::new();
    if latching {
        let learned = latch(&caller);
        if learned.is_none() {
            assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
            // Negative control reproduces the reported unlock without allowing
            // it to satisfy the initial, no-RX media requirement.
            LOCAL_INPUT.store(true, Ordering::Release);
            let after_rx = latch(&caller);
            LOCAL_INPUT.store(false, Ordering::Release);
            assert_eq!(
                after_rx,
                Some(offer.remote),
                "receiver key must demonstrate the same socket can reach the relay"
            );
            panic!(
                "quiet answered call emitted no RTP; relay learned the source only after receiver key"
            );
        }
        assert_eq!(learned, Some(offer.remote));
        assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
        caller.media = learned.unwrap();
        CONTINUOUS_RTP.store(true, Ordering::Release);
        quiet_media(&mut caller, &mut codec, 1500, false);
        // An initial admission cut cannot excuse a later unrelated reset.
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
        ALLOW_SOURCE_CUT.store(true, Ordering::Release);
    }
    if !immediate {
        media(&mut caller, &mut codec, &[0.0; 4000], &mut returned);
    }
    if !radio {
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            0,
            "no login prompt or automatic RF output"
        );
    }
    if radio && !immediate {
        let after_answer = PUBLIC_OUTPUT.load(Ordering::Acquire);
        media(&mut caller, &mut codec, &[0.0; 1600], &mut returned);
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            after_answer,
            "ringback or a repeated acknowledgment continued after answer"
        );
    }
    let before_voice = PUBLIC_OUTPUT.load(Ordering::Acquire);
    let speech: Vec<f32> =
        include_bytes!("../../../product/src/tests/fixtures/phone-speech/yes-48000-s16le.pcm")
            .chunks_exact(12)
            .map(|bytes| f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0)
            .collect();
    media(&mut caller, &mut codec, &speech, &mut returned);
    media(&mut caller, &mut codec, &[0.0; 16000], &mut returned);
    if immediate {
        assert_eq!(
            ANSWER_ACKS.load(Ordering::Acquire),
            1,
            "answer ACK is serviced while the immediate greeting is sent"
        );
        assert_eq!(
            LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire),
            rx_keys_at_answer,
            "first callee utterance must not depend on any receiver rekey after answer"
        );
        if !radio {
            assert_eq!(rx_keys_at_answer, 0);
        }
        assert!(
            LINKED_OUTPUT_TURNS.load(Ordering::Acquire) > linked_before_answer,
            "actual phone contribution must become active in keyed RF output before any receiver rekey"
        );
    }
    assert!(
        PUBLIC_OUTPUT.load(Ordering::Acquire) > before_voice + 160,
        "called-party speech reaches actual node RF mix through VOX"
    );
    assert!(
        !LAST_PTT.load(Ordering::Acquire),
        "existing VOX/hang releases PTT"
    );
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "callee hears neither self echo nor incoming login prompts"
    );
    LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
    let radio_before = PUBLIC_OUTPUT.load(Ordering::Acquire);
    caller.digits(b"*4#*70");
    media(&mut caller, &mut codec, &[0.0; 4000], &mut returned);
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        radio_before,
        "callee DTMF grants no node control"
    );
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "no private control result for outgoing role"
    );
    assert!(worker.incoming().is_none());
    returned.clear();
    if latching {
        // Continue the established incoming source with real silence instead of
        // introducing missing RTP during this healthy post-rearm cadence phase.
        ALLOW_SOURCE_CUT.store(false, Ordering::Release);
        quiet_media(&mut caller, &mut codec, 100, true);
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
        ALLOW_SOURCE_CUT.store(true, Ordering::Release);
    }
    LOCAL_INPUT.store(true, Ordering::Release);
    media(&mut caller, &mut codec, &[0.0; 8000], &mut returned);
    LOCAL_INPUT.store(false, Ordering::Release);
    media(&mut caller, &mut codec, &[0.0; 4000], &mut returned);
    let energy: f32 = returned.iter().map(|sample| sample * sample).sum();
    let correlation = |lag: usize| {
        returned
            .iter()
            .zip(returned.iter().skip(lag))
            .map(|(a, b)| a * b)
            .sum::<f32>()
    };
    assert!(
        returned.iter().filter(|sample| sample.abs() > 0.01).count() > 160,
        "actual local RF input returns to the called telephone"
    );
    assert!(
        correlation(8) > energy * 0.5 && correlation(4) < -energy * 0.5,
        "known1kHz return pattern, not a status prompt"
    );
    CONTINUOUS_RTP.store(false, Ordering::Release);
    if ivr != IvrMode::Disabled {
        telephone_ivr(&mut caller, &mut codec, ivr);
    }
    if radio {
        radio_digits(b"*0#");
    } else {
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_HANGUP, refused),
            (-1, 0),
            "stale id cannot end successor"
        );
        assert_eq!(command(&runtime, abi::RPTADV_PHONE_HANGUP, id), (0, id));
    }
    caller.sip.set_nonblocking(false).unwrap();
    let (bye, remote) = receive(&caller.sip);
    assert!(bye.starts_with("BYE "));
    caller
        .sip
        .send_to(response(&bye, 200, false).as_bytes(), remote)
        .unwrap();
    assert!(worker.incoming().is_none());
    if radio {
        runtime.stop().unwrap();
        worker.stop().unwrap();
        assert_radio_stopped(true);
        fs::remove_dir_all(directory).unwrap();
        return;
    }
    // This synthetic telephone reuses its socket, so discard only the ended
    // dialog's already-received packets before selecting the new offer endpoint.
    while caller.rtp.recv_from(&mut [0; 2048]).is_ok() {}
    // Establish another exact call, then let the telephone end the dialog.
    let remote_ended = next_call(&runtime);
    assert_ne!(remote_ended, id);
    let (invite, remote) = receive(&caller.sip);
    let next_offer =
        MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
    let answer = MediaDescription {
        codec: selected,
        ..next_offer.clone()
    }
    .answer(caller.rtp.local_addr().unwrap(), 100)
    .unwrap();
    let accepted = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:called@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        caller.sip.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    caller.sip.send_to(accepted.as_bytes(), remote).unwrap();
    assert!(receive(&caller.sip).0.starts_with("ACK "));
    caller.sip.set_nonblocking(true).unwrap();
    caller.media = next_offer.remote;
    caller.sequence = 0;
    caller.timestamp = 0;
    // Service real source callbacks before the remote close, without a login.
    media(&mut caller, &mut codec, &[0.0; 1600], &mut Vec::new());
    let (_, dialog) = fields(invite.as_bytes());
    let contact = crate::phone::sip_test_registrar::header(&invite, "Contact")
        .trim_start_matches('<')
        .split('>')
        .next()
        .unwrap();
    let bye = format!(
        "BYE {contact} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-outgoing-peer-bye;rport\r\nMax-Forwards: 70\r\nFrom: {};tag=fixture\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 BYE\r\nContent-Length: 0\r\n\r\n",
        caller.sip.local_addr().unwrap(),
        dialog["to"],
        dialog["from"],
        dialog["call-id"]
    );
    caller.sip.send_to(bye.as_bytes(), remote).unwrap();
    caller.sip.set_nonblocking(false).unwrap();
    let completed = receive(&caller.sip).0;
    assert!(
        completed.starts_with("SIP/2.0 200 ") && fields(completed.as_bytes()).1["cseq"] == "1 BYE"
    );
    let final_id = next_call(&runtime);
    assert_ne!(
        final_id, remote_ended,
        "remote BYE releases its exact active claim"
    );
    let (invite, remote) = receive(&caller.sip);
    caller
        .sip
        .send_to(response(&invite, 486, false).as_bytes(), remote)
        .unwrap();
    assert!(receive(&caller.sip).0.starts_with("ACK "));
    runtime.stop().unwrap();
    worker.stop().unwrap();
    assert_radio_stopped(true);
    fs::remove_dir_all(directory).unwrap();
}

fn telephone_ivr(caller: &mut Caller, codec: &mut G711Codec, mode: IvrMode) {
    let input = b"12***4**#";
    let expected = b"12*4#";
    let start = Instant::now();
    let duration = Duration::from_millis(100 + input.len() as u64 * 280 + 1000);
    let mut next = start;
    let mut last = None;
    let mut events: Vec<(u32, u8, u16, u8)> = Vec::new();
    let mut decoder = ivr_decoder::Decoder::new();
    let mut decoded = Vec::with_capacity(16);
    let mut maximum_audio = 0.0f32;
    let mut nonzero_by_slot = [0usize; 12];
    let before_pcm = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    while start.elapsed() < duration {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let elapsed = start.elapsed().as_millis() as usize;
        let symbol = if elapsed < 100 {
            b' '
        } else {
            let index = (elapsed - 100) / 280;
            if index >= input.len() {
                0
            } else if (elapsed - 100) % 280 < 160 {
                input[index]
            } else {
                b' '
            }
        };
        LOCAL_DTMF.store(usize::from(symbol), Ordering::Release);
        caller.audio_payload(
            codec,
            &[0.0; 160],
            SELECTED_PAYLOAD.load(Ordering::Acquire) as u8,
        );
        service(caller);
        let mut wire = [0; 2048];
        while let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
            assert_eq!(source, caller.media);
            assert_eq!(wire[0], 0x80);
            let sequence = u16::from_be_bytes(wire[2..4].try_into().unwrap());
            let timestamp = u32::from_be_bytes(wire[4..8].try_into().unwrap());
            let ssrc = u32::from_be_bytes(wire[8..12].try_into().unwrap());
            if let Some((previous, original_ssrc)) = last {
                assert_eq!(sequence, u16::wrapping_add(previous, 1));
                assert_eq!(ssrc, original_ssrc);
            }
            last = Some((sequence, ssrc));
            if wire[1] & 0x7f == 101 {
                assert_eq!(length, 16);
                let ended = wire[13] & 0x80 != 0;
                let duration = u16::from_be_bytes(wire[14..16].try_into().unwrap());
                if events.last().is_none_or(|event| event.0 != timestamp) {
                    assert!(wire[1] & 0x80 != 0, "event onset marker");
                    if let Some(previous) = events.last() {
                        assert_eq!(previous.3, 3);
                    }
                    events.push((timestamp, wire[12], 0, 0));
                } else {
                    assert_eq!(wire[1] & 0x80, 0);
                }
                let event = events.last_mut().unwrap();
                assert_eq!(event.1, wire[12]);
                assert!(duration >= event.2 && duration <= 1600);
                event.2 = duration;
                if ended {
                    assert_eq!(duration, 1600);
                    event.3 += 1;
                }
                assert!(event.3 <= 3);
            } else {
                assert_eq!(length, 172);
                assert_eq!(
                    wire[1] & 0x7f,
                    SELECTED_PAYLOAD.load(Ordering::Acquire) as u8
                );
                let mut pcm = [0.0; 160];
                codec
                    .decode(wire[12..172].try_into().unwrap(), &mut pcm)
                    .unwrap();
                if pcm.iter().any(|sample| sample.abs() > 0.01) {
                    nonzero_by_slot[(elapsed / 280).min(11)] += 1;
                }
                for sample in pcm {
                    maximum_audio = maximum_audio.max(sample.abs());
                }
                decoder.process(&mut pcm, &mut decoded);
            }
        }
        next += Duration::from_millis(20);
        while next <= Instant::now() {
            caller.timestamp = caller.timestamp.wrapping_add(160);
            next += Duration::from_millis(20);
        }
    }
    LOCAL_DTMF.store(0, Ordering::Release);
    let symbols: Vec<u8> = events
        .iter()
        .map(|event| b"0123456789*#ABCD"[usize::from(event.1)])
        .collect();
    if mode == IvrMode::Events {
        assert_eq!(
            symbols, expected,
            "actual RF parser and SIP event destination"
        );
        assert!(events.iter().all(|event| event.3 == 3));
        assert!(
            decoded.is_empty(),
            "negotiated events must not duplicate as in-band digits"
        );
        assert!(
            maximum_audio <= 0.01,
            "RF DTMF leaked into telephone programme instead of negotiated events: max={maximum_audio}, nonzero_by_slot={nonzero_by_slot:?}"
        );
    } else {
        assert!(
            events.is_empty(),
            "unnegotiated telephone-event payload was sent"
        );
        assert_eq!(
            decoded, expected,
            "released detector decodes the private regenerated in-band digits"
        );
    }
    // Actual RF input may still key the local transmitter. Count audible PCM,
    // not the older combined PCM/PTT counter, to reject regenerated feedback.
    assert_eq!(
        PUBLIC_PCM_OUTPUT.load(Ordering::Acquire),
        before_pcm,
        "IVR digits created RF results/audio"
    );
}
