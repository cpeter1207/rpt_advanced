//! Actual typed product origination through the registered SIP reactor and radio mixer.
use super::*;
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
fn service(caller: &Caller) {
    let mut wire = [0; 8192];
    while let Ok((count, source)) = caller.sip.recv_from(&mut wire) {
        let (first, values) = fields(&wire[..count]);
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
        assert_eq!((wire[0], wire[1] & 0x7f), (0x80, 0));
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
        caller.audio(codec, &pcm);
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
    product_outgoing(false);
}

#[test]
fn actual_radio_dtmf_originates_two_way_phone_and_hangs_up() {
    product_outgoing(true);
}

fn product_outgoing(radio: bool) {
    const CHILD: &str = "RPTADV_TEST_OUTGOING_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let name = if radio {
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
    let answer = offer.answer(rtp.local_addr().unwrap(), 99).unwrap();
    let accepted = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:called@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        sip.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    sip.send_to(accepted.as_bytes(), remote).unwrap();
    assert!(receive(&sip).0.starts_with("ACK "));
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
    let mut codec = G711Codec::load(G711::Mulaw).unwrap();
    let mut returned = Vec::new();
    media(&mut caller, &mut codec, &[0.0; 4000], &mut returned);
    if !radio {
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            0,
            "no login prompt or automatic RF output"
        );
    }
    if radio {
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
    let answer = next_offer
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
