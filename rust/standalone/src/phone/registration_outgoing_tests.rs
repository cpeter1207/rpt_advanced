//! Literal remote endpoint exercising the actual single SIP reactor.
use super::*;
use crate::{
    configuration::ResolvedPhoneAccount,
    phone::{
        media::MediaDescription,
        sip_test_registrar::{header, receive_register, response},
    },
    phone_accounts::AccountPreparer,
    providers::ProviderSet,
};
use std::{fs, net::UdpSocket, os::unix::fs::PermissionsExt};

/// Deterministically suspend the existing reactor at the prepared-offer cut.
static ORIGINATE_PAUSE: std::sync::Mutex<
    Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
> = std::sync::Mutex::new(None);

pub(super) fn before_originate() {
    let pause = ORIGINATE_PAUSE.lock().unwrap().take();
    if let Some((entered, release)) = pause {
        entered.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(3)).unwrap();
    }
}

fn receive(socket: &UdpSocket, wire: &mut [u8]) -> (String, std::net::SocketAddr) {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    receive_register(socket, wire).unwrap()
}
fn await_failure(dial: &mut calls::Dial) {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match dial.try_complete() {
            Err(_) => return,
            Ok(None) => {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(Some(_)) => panic!("refused/cancelled call returned a usable media handle"),
        }
    }
}

#[test]
fn outgoing_reactor_adopts_validated_answer_without_incoming_authority() {
    let directory =
        std::env::temp_dir().join(format!("rpt-sip-outgoing-reactor-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("secret"), "synthetic-test-secret\n").unwrap();
    fs::set_permissions(directory.join("secret"), fs::Permissions::from_mode(0o600)).unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
    rtp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt.conf"),
            vec![ResolvedPhoneAccount {
                id: "outgoing".into(),
                local_node: "desk".into(),
                protocol: PhoneAccountProtocol::Sip,
                enabled: true,
                server: socket.local_addr().unwrap().to_string(),
                username: "synthetic".into(),
                secrets_file: "secret".into(),
                register: true,
                registration_interval_s: 60,
                authentication_timeout_ms: 2000,
            }],
            &PreparedAccounts::default(),
            |_| unreachable!(),
        )
        .unwrap();
    let provider = std::env::var("RPTADV_SIP_TEST_PROVIDER").unwrap();
    let mut worker = Worker::start(Path::new(&provider)).unwrap();
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*std::ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    worker.prepare(&accounts).unwrap().commit().unwrap();
    let mut wire = [0; 8192];
    let (register, remote) = receive(&socket, &mut wire);
    assert!(register.starts_with("REGISTER "));
    socket
        .send_to(response(&register, 200, false).as_bytes(), remote)
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(2);
    while worker.observe().unwrap()[0].1.state != 2 {
        assert!(Instant::now() < limit);
        std::thread::sleep(Duration::from_millis(2));
    }
    bounded_originator_admission(&accounts.as_slice()[0], jitter);
    let mut dial = worker
        .client()
        .originate(
            &accounts.as_slice()[0],
            "desk",
            "sip:fixture@example.invalid".into(),
            jitter,
        )
        .expect("registered reactor must originate without an incoming receipt");
    let (invite, remote) = receive(&socket, &mut wire);
    assert!(invite.starts_with("INVITE sip:fixture@example.invalid "));
    let offer =
        MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
    assert_eq!(
        dial.provisional_status(),
        0,
        "no invented ringing before signaling"
    );
    socket
        .send_to(response(&invite, 180, false).as_bytes(), remote)
        .unwrap();
    let ringing_deadline = Instant::now() + Duration::from_secs(1);
    while dial.provisional_status() != 180 && Instant::now() < ringing_deadline {
        assert!(dial.try_complete().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        dial.provisional_status(),
        180,
        "real180 must cross the pending owner boundary"
    );
    assert!(dial.try_complete().unwrap().is_none());
    assert!(
        worker.incoming().is_none(),
        "outgoing recipient must never enter mailbox admission"
    );
    let answer = offer.answer(rtp.local_addr().unwrap(), 99).unwrap();
    let base = response(&invite, 200, false);
    let accepted = base.replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:fixture@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        socket.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    socket.send_to(accepted.as_bytes(), remote).unwrap();
    let (ack, _) = receive(&socket, &mut wire);
    assert!(ack.starts_with("ACK "));
    let limit = Instant::now() + Duration::from_secs(2);
    let handle = loop {
        if let Some(handle) = dial.try_complete().unwrap() {
            break handle;
        }
        assert!(Instant::now() < limit);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(worker.incoming().is_none());
    assert_eq!(
        dial.provisional_status(),
        0,
        "answered call is no longer ringing"
    );
    assert_eq!(handle.shared.local, "desk");
    assert!(handle.shared.live());
    {
        let mut media = handle.shared.media.lock().unwrap();
        media.reset().unwrap();
        let deadline = media.elapsed_ms() + 100;
        assert_eq!(media.send(&[0xa5; 160], 0, deadline).unwrap(), 0);
    }
    let (count, source) = rtp.recv_from(&mut wire).unwrap();
    assert_eq!(
        source, offer.remote,
        "answered call must keep the advertised port"
    );
    assert_eq!(count, 172);
    drop(handle);
    let (bye, remote) = receive(&socket, &mut wire);
    assert!(bye.starts_with("BYE "));
    socket
        .send_to(response(&bye, 200, false).as_bytes(), remote)
        .unwrap();
    host_factory_roundtrip(&worker, &providers, &accounts, &socket, &rtp);
    // More than the native slot count proves refusal releases terminal custody.
    for _ in 0..9 {
        let mut refused = worker
            .client()
            .originate(
                &accounts.as_slice()[0],
                "desk",
                "sip:fixture@example.invalid".into(),
                jitter,
            )
            .unwrap();
        let (request, remote) = receive(&socket, &mut wire);
        assert!(request.starts_with("INVITE "));
        socket
            .send_to(response(&request, 486, false).as_bytes(), remote)
            .unwrap();
        let (ack, _) = receive(&socket, &mut wire);
        assert!(ack.starts_with("ACK "));
        await_failure(&mut refused);
        assert!(worker.incoming().is_none());
    }
    // Unsupported media is rejected before a usable Handle, then ACK/BYE retires the dialog.
    let mut unsupported = worker
        .client()
        .originate(
            &accounts.as_slice()[0],
            "desk",
            "sip:fixture@example.invalid".into(),
            jitter,
        )
        .unwrap();
    let (request, remote) = receive(&socket, &mut wire);
    let unsupported_sdp = "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 9999 RTP/AVP 96\r\na=rtpmap:96 opus/48000/2\r\na=sendrecv\r\n";
    let bad_answer = response(&request, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:fixture@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        socket.local_addr().unwrap(), unsupported_sdp.len(), unsupported_sdp));
    socket.send_to(bad_answer.as_bytes(), remote).unwrap();
    let (ack, _) = receive(&socket, &mut wire);
    assert!(ack.starts_with("ACK "));
    let (bye, remote) = receive(&socket, &mut wire);
    assert!(bye.starts_with("BYE "));
    socket
        .send_to(response(&bye, 200, false).as_bytes(), remote)
        .unwrap();
    await_failure(&mut unsupported);
    let cancelled = worker
        .client()
        .originate(
            &accounts.as_slice()[0],
            "desk",
            "sip:fixture@example.invalid".into(),
            jitter,
        )
        .unwrap();
    let (request, remote) = receive(&socket, &mut wire);
    socket
        .send_to(response(&request, 180, false).as_bytes(), remote)
        .unwrap();
    drop(cancelled);
    let (cancel, remote) = receive(&socket, &mut wire);
    assert!(cancel.starts_with("CANCEL "));
    socket
        .send_to(response(&cancel, 200, false).as_bytes(), remote)
        .unwrap();
    socket
        .send_to(response(&request, 487, false).as_bytes(), remote)
        .unwrap();
    let (ack, _) = receive(&socket, &mut wire);
    assert!(ack.starts_with("ACK "));
    let mut retiring = worker
        .client()
        .originate(
            &accounts.as_slice()[0],
            "desk",
            "sip:fixture@example.invalid".into(),
            jitter,
        )
        .unwrap();
    let (request, remote) = receive(&socket, &mut wire);
    socket
        .send_to(response(&request, 180, false).as_bytes(), remote)
        .unwrap();
    worker
        .prepare(&PreparedAccounts::default())
        .unwrap()
        .commit()
        .unwrap();
    let mut removed = false;
    let mut cancelled = false;
    let mut acknowledged = false;
    while !(removed && cancelled && acknowledged) {
        let (packet, remote) = receive(&socket, &mut wire);
        if packet.starts_with("REGISTER ") {
            assert!(!removed, "one account removal transaction");
            assert_eq!(header(&packet, "Expires"), "0");
            socket
                .send_to(response(&packet, 200, false).as_bytes(), remote)
                .unwrap();
            removed = true;
        } else if packet.starts_with("CANCEL ") {
            assert!(!cancelled, "one pending call cancellation transaction");
            socket
                .send_to(response(&packet, 200, false).as_bytes(), remote)
                .unwrap();
            socket
                .send_to(response(&request, 487, false).as_bytes(), remote)
                .unwrap();
            cancelled = true;
        } else {
            assert!(packet.starts_with("ACK "));
            acknowledged = true;
        }
    }
    await_failure(&mut retiring);
    assert!(worker.incoming().is_none());
    worker.stop().unwrap();
    drop(dial);
    fs::remove_dir_all(directory).unwrap();
}

/// Queue refusal retains one claim; cancellation never needs a second queue slot.
fn bounded_originator_admission(
    account: &crate::phone_accounts::PreparedAccount,
    jitter: &'static crate::abi::rptadv_jitter_descriptor_v1,
) {
    for cancel_before_space in [true, false] {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        assert!(sender.try_send(Command::Stop).is_ok());
        let client = calls::Client::new(sender);
        let mut dial = client
            .originate(
                account,
                "desk",
                "sip:fixture@example.invalid".into(),
                jitter,
            )
            .unwrap();
        assert!(dial.try_complete().unwrap().is_none());
        if cancel_before_space {
            drop(dial);
            assert!(matches!(receiver.try_recv(), Ok(Command::Stop)));
            assert!(receiver.try_recv().is_err());
        } else {
            assert!(matches!(receiver.try_recv(), Ok(Command::Stop)));
            assert!(dial.try_complete().unwrap().is_none());
            let Ok(Command::Call(calls::Command::Originate(
                _,
                _,
                _,
                _,
                _,
                close,
                _progress,
                _reply,
            ))) = receiver.try_recv()
            else {
                panic!("the exact retained originate command must be submitted once");
            };
            assert!(!close.load(std::sync::atomic::Ordering::Acquire));
            drop(dial);
            assert!(close.load(std::sync::atomic::Ordering::Acquire));
            assert!(receiver.try_recv().is_err());
        }
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    assert!(
        calls::Client::new(sender)
            .originate(
                account,
                "desk",
                "sip:fixture@example.invalid".into(),
                jitter
            )
            .is_err()
    );
}

/// The host callback waits off-reactor while the same live reactor serves SIP.
fn host_factory_roundtrip(
    worker: &Worker,
    providers: &ProviderSet,
    accounts: &PreparedAccounts,
    socket: &UdpSocket,
    rtp: &UdpSocket,
) {
    use crate::host_services::HostServicesOwner;
    use std::{
        ffi::c_void,
        ptr,
        sync::atomic::{AtomicBool, AtomicU32, Ordering},
    };
    // The borrowed provider lease outlives this host and every scoped callback.
    let providers = unsafe { &*ptr::from_ref(providers) };
    let host = HostServicesOwner::new(
        vec![],
        Some(providers),
        crate::secrets::SecretsFile::parse("").unwrap(),
    )
    .unwrap();
    host.publish_sip(Some(worker), accounts);
    let api = host.descriptor();
    let dial = api
        .peer_dial_phone_progress
        .expect("standalone exposes the optional telephone factory");
    let context = api.context as usize;
    let current = AtomicBool::new(true);
    let progress = AtomicU32::new(777);
    unsafe extern "C" fn report_progress(context: *mut c_void, status: u32) {
        unsafe { &*context.cast::<AtomicU32>() }.store(status, Ordering::Release);
    }
    let progress_context = ptr::from_ref(&progress) as usize;
    unsafe extern "C" fn is_current(context: *mut c_void) -> u32 {
        u32::from(unsafe { &*context.cast::<AtomicBool>() }.load(Ordering::Acquire))
    }
    let current_context = ptr::from_ref(&current) as usize;
    let call = || {
        let mut peer = ptr::null_mut();
        let status = unsafe {
            dial(
                context as *mut c_void,
                c"outgoing".as_ptr(),
                8,
                c"second-node".as_ptr(),
                11,
                c"sip:fixture@example.invalid".as_ptr(),
                27,
                960,
                Some(is_current),
                current_context as *mut c_void,
                Some(report_progress),
                progress_context as *mut c_void,
                &mut peer,
            )
        };
        (status, peer as usize)
    };
    // A staged transaction blocks new calls but preserves the committed cohort.
    host.set_sip_dial_admission(false);
    assert_eq!(call(), (-1, 0));
    assert_eq!(
        progress.load(Ordering::Acquire),
        0,
        "early refusal clears progress too"
    );
    host.set_sip_dial_admission(true);
    // First request validates the answered socket and existing PHONE owner.
    std::thread::scope(|scope| {
        let task = scope.spawn(call);
        let mut wire = [0; 8192];
        let (invite, remote) = receive(socket, &mut wire);
        assert!(invite.starts_with("INVITE "));
        socket
            .send_to(response(&invite, 180, false).as_bytes(), remote)
            .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while progress.load(Ordering::Acquire) != 180 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(progress.load(Ordering::Acquire), 180);

        let offer =
            MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
        let answer = offer.answer(rtp.local_addr().unwrap(), 101).unwrap();
        let accepted = response(&invite,200,false).replace("Content-Length: 0\r\n\r\n", &format!(
            "Contact: <sip:fixture@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            socket.local_addr().unwrap(),answer.len(),std::str::from_utf8(&answer).unwrap()));
        socket.send_to(accepted.as_bytes(), remote).unwrap();
        assert!(receive(socket, &mut wire).0.starts_with("ACK "));
        let (status, raw) = task.join().unwrap();
        assert_eq!(status, 0);
        assert_ne!(raw, 0);
        let mut info = crate::abi::rptadv_peer_info_v1 {
            struct_size: std::mem::size_of::<crate::abi::rptadv_peer_info_v1>() as u32,
            abi_version: 1,
            ..unsafe { std::mem::zeroed() }
        };
        assert_eq!(
            unsafe { api.peer_info.unwrap()(api.context, raw as *mut c_void, &mut info) },
            0
        );
        assert_eq!(info.protocol, 3);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(info.local.data.cast::<u8>(), info.local.length) },
            b"second-node"
        );
        assert!(worker.incoming().is_none());
        unsafe { api.peer_destroy.unwrap()(api.context, raw as *mut c_void) };
        let (bye, remote) = receive(socket, &mut wire);
        assert!(bye.starts_with("BYE "));
        socket
            .send_to(response(&bye, 200, false).as_bytes(), remote)
            .unwrap();
    });
    // Cancellation during queued/prepared admission must not emit an INVITE.
    for remove in [false, true] {
        current.store(true, Ordering::Release);
        host.publish_sip(Some(worker), accounts);
        std::thread::scope(|scope| {
            let (entered, admission) = std::sync::mpsc::sync_channel(1);
            let (release, resume) = std::sync::mpsc::sync_channel(1);
            *ORIGINATE_PAUSE.lock().unwrap() = Some((entered, resume));
            let task = scope.spawn(call);
            admission.recv_timeout(Duration::from_secs(2)).unwrap();
            if remove {
                host.publish_sip(None, &PreparedAccounts::default());
            } else {
                current.store(false, Ordering::Release);
            }
            let deadline = Instant::now() + Duration::from_millis(200);
            while !task.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            let cancelled_before_release = task.is_finished();
            release.send(()).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let mut wire = [0; 8192];
            // The existing registrar helper answers OPTIONS but preserves this
            // fixed receive deadline, so keepalive is not mistaken for dialing.
            let unexpected = match receive_register(socket, &mut wire) {
                Ok((packet, remote)) => {
                    assert!(
                        packet.starts_with("INVITE "),
                        "unexpected SIP method {}",
                        packet.split_ascii_whitespace().next().unwrap_or("empty")
                    );
                    // Complete the old defective path before recording its failure.
                    socket
                        .send_to(response(&packet, 486, false).as_bytes(), remote)
                        .unwrap();
                    let mut acknowledged = false;
                    for _ in 0..2 {
                        let (packet, remote) = receive(socket, &mut wire);
                        if packet.starts_with("CANCEL ") {
                            socket
                                .send_to(response(&packet, 200, false).as_bytes(), remote)
                                .unwrap();
                        } else {
                            assert!(packet.starts_with("ACK "));
                            acknowledged = true;
                            break;
                        }
                    }
                    assert!(acknowledged);
                    true
                }
                Err(error) => {
                    assert!(matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ));
                    false
                }
            };
            let (status, raw) = task.join().unwrap();
            assert_ne!(status, 0);
            assert_eq!(raw, 0);
            assert!(
                !unexpected,
                "cancelled prepared admission emitted an INVITE"
            );
            assert!(
                cancelled_before_release,
                "host cancellation waited for reactor admission"
            );
        });
    }
    // Cancellation and committed account removal must close the same pending call.
    for remove in [false, true] {
        current.store(true, Ordering::Release);
        host.publish_sip(Some(worker), accounts);
        std::thread::scope(|scope| {
            let task = scope.spawn(call);
            let mut wire = [0; 8192];
            let (invite, remote) = receive(socket, &mut wire);
            assert!(invite.starts_with("INVITE "));
            socket
                .send_to(response(&invite, 180, false).as_bytes(), remote)
                .unwrap();
            if remove {
                host.publish_sip(None, &PreparedAccounts::default());
            } else {
                current.store(false, Ordering::Release);
            }
            let (cancel, remote) = receive(socket, &mut wire);
            assert!(cancel.starts_with("CANCEL "));
            socket
                .send_to(response(&cancel, 200, false).as_bytes(), remote)
                .unwrap();
            socket
                .send_to(response(&invite, 487, false).as_bytes(), remote)
                .unwrap();
            assert!(receive(socket, &mut wire).0.starts_with("ACK "));
            let (status, raw) = task.join().unwrap();
            assert_ne!(status, 0);
            assert_eq!(raw, 0);
        });
    }
    // An old account binding cannot initiate after its committed publication ends.
    current.store(true, Ordering::Release);
    assert_eq!(call(), (-1, 0));
}
