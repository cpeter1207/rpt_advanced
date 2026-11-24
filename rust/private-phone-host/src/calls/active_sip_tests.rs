//! Exercise the selected account, one real SIP dialog and its negotiated RTP socket.
use super::{Transport, source_capture::{Audio, read}};
use crate::{phone::{AccountRow, PreparedAccount, Protocol}, selection::{NativePaths, PhoneHost}};
use std::{net::{SocketAddr, UdpSocket}, path::PathBuf,
    sync::{Arc, mpsc}, thread, time::{Duration, Instant}};

#[path = "../../tests/support/sip_registrar.rs"]
mod registrar;
#[path = "../../tests/support/sip_digest.rs"]
mod digest;

fn drive(host: &PhoneHost, ticket: usize, reverse: bool) {
    let end = Instant::now() + Duration::from_secs(4);
    while !host.drive(ticket, reverse).unwrap() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(2));
    }
}
fn sdp_request(socket: &UdpSocket, address: SocketAddr, method: &str, to: &str, body: &str) {
    let port = socket.local_addr().unwrap().port();
    let cseq = if method == "BYE" { 2 } else { 1 };
    let header = if body.is_empty() { "" } else { "Content-Type: application/sdp\r\n" };
    let packet = format!("{method} sip:desk@{address} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK-policy-{method};rport\r\nMax-Forwards: 70\r\nFrom: <sip:caller@fixture.invalid>;tag=policy\r\nTo: {to}\r\nCall-ID: policy@fixture.invalid\r\nCSeq: {cseq} {method}\r\nContact: <sip:caller@127.0.0.1:{port}>\r\n{header}Content-Length: {}\r\n\r\n{body}", body.len());
    socket.send_to(packet.as_bytes(), address).unwrap();
}

#[test]
fn active_dialog_keeps_queued_rtp_pcm_and_private_identity_through_policy_edits() {
    let signaling = UdpSocket::bind("127.0.0.1:0").unwrap();
    signaling.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let upstream = signaling.local_addr().unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let path = |name, fallback| std::env::var_os(name).map_or_else(|| PathBuf::from(fallback), PathBuf::from);
    let host = PhoneHost::new(NativePaths {
        sip: path("RPTADV_SIP_TEST_PROVIDER", "librptadv_sip_adapter.so.1"),
        jitter: path("RPTADV_JITTER_TEST_PROVIDER", "librptadv_jitter_adapter.so.1"),
        iax: "/absent/iax".into(), g711: "/absent/iax-codec".into(),
    }, Arc::new(move |call| { assert!(tx.try_send(call).is_ok()); }));
    let row = |jitter_ms| PreparedAccount::copy(AccountRow { port_id: [1; 16], account_id: [2; 16],
        generation: 7, protocol: Protocol::Sip, server: "127.0.0.1", port: upstream.port(),
        username: "desk", credential: b"fixture-only", jitter_ms }).unwrap();
    let ticket = host.prepare(vec![row([30,85,140])]).unwrap();
    assert!(!host.drive(ticket, false).unwrap());
    let mut bytes = [0; 8192];
    let (first, contact) = registrar::receive_register(&signaling, &mut bytes).unwrap();
    signaling.send_to(registrar::response(&first,401,true).as_bytes(), contact).unwrap();
    let (digest, source) = registrar::receive_register(&signaling,&mut bytes).unwrap();
    assert_eq!(source,contact);
    digest::check(registrar::header(&digest,"Authorization"));
    signaling.send_to(registrar::response(&digest,200,false).as_bytes(),source).unwrap();
    drive(&host, ticket, false);
    host.release(ticket).unwrap();
    let body = format!("v=0\r\no=test 1 1 IN IP4 127.0.0.1\r\ns=policy\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=ptime:20\r\na=sendrecv\r\n", rtp.local_addr().unwrap().port());
    sdp_request(&signaling,contact,"INVITE","<sip:desk@fixture.invalid>",&body);
    let call = rx.recv_timeout(Duration::from_secs(4)).expect("authenticated INVITE");
    assert_eq!(call.source(), upstream);
    // No telephone-event payload was negotiated in this dialog's actual SDP.
    // The grouped frontend metadata must preserve the native receive mode.
    let mut info: crate::abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    info.struct_size = std::mem::size_of_val(&info) as u32;
    assert_eq!(unsafe { crate::frontend::info(&call, &mut info) }, 0);
    assert_eq!(info.capabilities, 51 | 64, "PCMU stream without telephone-event requires in-band digit admission");
    let generations = call.generations();
    assert_eq!(generations.0, 7);
    call.answer_permit().answer().unwrap();
    let response = loop {
        let (message, remote) = registrar::receive_register(&signaling,&mut bytes).unwrap();
        assert_eq!(remote,contact);
        assert!(!message.starts_with("REGISTER "));
        if message.starts_with("SIP/2.0 200 ") { break message; }
    };
    let to = registrar::header(&response,"To").to_string();
    sdp_request(&signaling,contact,"ACK",&to,"");
    let media_port: u16 = response.lines().find_map(|line| line.strip_prefix("m=audio "))
        .unwrap().split_whitespace().next().unwrap().parse().unwrap();
    let timeline = Instant::now();
    while !call.established() {
        assert!(timeline.elapsed() < Duration::from_secs(4));
        thread::sleep(Duration::from_millis(2));
    }
    let observe = || {
        let Transport::Sip(sip) = &call.0 else { panic!("wrong transport") };
        sip.handle.shared.media.lock().unwrap().observe().unwrap()
    };
    let send = |sequence: u16| {
        let mut packet = [0x99_u8; 172];
        packet[0..2].copy_from_slice(&[0x80, 0]);
        packet[2..4].copy_from_slice(&sequence.to_be_bytes());
        packet[4..8].copy_from_slice(&(u32::from(sequence) * 160).to_be_bytes());
        packet[8..12].copy_from_slice(&0x4065_1001_u32.to_be_bytes());
        rtp.send_to(&packet, ("127.0.0.1", media_port)).unwrap();
    };
    let mut capture = Audio::default();
    for seq in 1..=4 { send(seq); }
    let end = Instant::now() + Duration::from_secs(1);
    while observe().jitter.queued_packets < 4 {
        read(&call,&mut capture).unwrap();
        assert!(Instant::now() < end, "actual RTP ingress did not queue four packets");
        thread::sleep(Duration::from_millis(1));
    }
    let before = observe();
    assert_eq!(before.jitter.delay,680);
    let changed = host.prepare(vec![row([10,20,40])]).unwrap();
    drive(&host,changed,false);
    let after = observe();
    assert_eq!(after.jitter.queued_packets,before.jitter.queued_packets);
    assert_eq!(after.jitter.limit_resets,before.jitter.limit_resets);
    let paced = Instant::now();
    for seq in 5..=20 {
        send(seq);
        let due = paced + Duration::from_millis(u64::from(seq-4)*20);
        while Instant::now() < due {
            read(&call,&mut capture).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
    }
    let forward = observe();
    assert!(forward.jitter.delay <= 320, "retained native delay {}",forward.jitter.delay);
    assert!(capture.qualified >= 8*160);
    assert_eq!(forward.jitter.limit_resets,before.jitter.limit_resets);
    assert_eq!(forward.jitter.overload_drops,before.jitter.overload_drops);
    let credited = capture.qualified;
    drive(&host,changed,true);
    host.release(changed).unwrap();
    assert_eq!(call.generations(),generations);
    assert!(call.established());
    assert_eq!(host.observe(&[1;16],&[2;16],7).unwrap(),2);
    let before_loss = observe();
    let previous_concealed = capture.concealed;
    let previous_gaps = capture.gaps;
    for seq in 21..=31 {
        // Withhold exactly one packet after rollback. Keep RTP sequence, media
        // timestamp and source socket intact through subsequent genuine speech.
        if seq != 23 { send(seq); }
        let until = Instant::now()+Duration::from_millis(20);
        while Instant::now() < until {
            read(&call,&mut capture).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(capture.qualified >= credited+4*160);
    let final_state = observe();
    assert!(final_state.jitter.missing_span > before_loss.jitter.missing_span);
    let plc = final_state.native_plc_frames - before_loss.native_plc_frames;
    let fallback = final_state.fallback_frames - before_loss.fallback_frames;
    assert!(plc + fallback >= 160, "missing unit has no codec provenance");
    assert_eq!(plc, (capture.concealed - previous_concealed) as u64);
    assert_eq!(fallback, u64::from(capture.gaps - previous_gaps));
    assert_eq!(capture.gaps as u64 + capture.concealed as u64,160);
    assert_eq!(capture.original,capture.qualified);
    assert_eq!(final_state.jitter.limit_resets,before.jitter.limit_resets);
    assert_eq!(final_state.jitter.overload_drops,before.jitter.overload_drops);
    eprintln!("active SIP call {:?} queued {} delay {} -> {} original {} qualified {} concealed {} gaps {} discontinuities {} native PLC {}",
        generations,before.jitter.queued_packets,before.jitter.delay,forward.jitter.delay,
        capture.original,capture.qualified,capture.concealed,capture.gaps,capture.resets,final_state.native_plc_frames);
    sdp_request(&signaling,contact,"BYE",&to,"");
    let end = Instant::now()+Duration::from_secs(4);
    while call.live() { assert!(Instant::now()<end); thread::sleep(Duration::from_millis(2)); }
    drop(call);
    let withdraw = host.prepare(Vec::new()).unwrap();
    assert!(!host.drive(withdraw,false).unwrap());
    loop {
        let (message, remote) = registrar::receive_register(&signaling,&mut bytes).unwrap();
        assert_eq!(remote,contact);
        if message.starts_with("REGISTER ") {
            assert_eq!(registrar::header(&message,"Expires"),"0");
            signaling.send_to(registrar::response(&message,200,false).as_bytes(),remote).unwrap();
            break;
        }
    }
    drive(&host,withdraw,false);
    host.release(withdraw).unwrap();
    let end = Instant::now()+Duration::from_secs(4);
    while !host.stop().unwrap() { assert!(Instant::now()<end); thread::sleep(Duration::from_millis(2)); }
}
