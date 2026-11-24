//! Registered reverse patch on the actual standalone foreground using the simulated station.
use super::{
    managed_asl, phone_caller::Caller, private_phone::{register, registrar}, *,
};
use std::{
    net::{SocketAddr, UdpSocket},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
};

const DIALOG: u32 = 1;

/// Answer registration keepalives while the media test owns the call.
/// Join the reader before the caller resumes signaling on the same socket.
fn with_keepalives<T>(
    socket: &UdpSocket,
    contact: SocketAddr,
    measure: impl FnOnce() -> T,
) -> (T, usize) {
    let timeout = socket.read_timeout().unwrap();
    let (measured, serviced) = thread::scope(|scope| {
        let (stop, running) = std::sync::mpsc::channel::<()>();
        let reader = scope.spawn(move || {
            let mut bytes = [0; 8192];
            let mut answered = 0;
            while matches!(
                running.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ) {
                socket
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                match registrar::receive_with_keepalives(socket, &mut bytes, |source| {
                    assert_eq!(source, contact);
                    answered += 1;
                }) {
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => panic!("incoming SIP keepalive read failed: {error}"),
                    Ok((_, source)) => {
                        assert_eq!(source, contact);
                        panic!("unexpected incoming SIP signaling during the media check");
                    }
                }
            }
            answered
        });
        let measured = catch_unwind(AssertUnwindSafe(measure));
        drop(stop);
        (measured, reader.join())
    });
    socket.set_read_timeout(timeout).unwrap();
    let answered = serviced.unwrap_or_else(|failure| resume_unwind(failure));
    (
        measured.unwrap_or_else(|failure| resume_unwind(failure)),
        answered,
    )
}

/// Finish the registered account even when the fixture's call assertion unwinds.
fn withdraw(socket: &UdpSocket, contact: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(12);
    let call_id = format!("grant{DIALOG}@fixture.invalid");
    let mut wire = [0; 8192];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())
            .expect("incoming SIP account did not withdraw");
        socket.set_read_timeout(Some(remaining)).unwrap();
        let (request, source) = registrar::receive_register(socket, &mut wire).unwrap();
        assert_eq!(source, contact, "SIP account changed its signaling socket");
        if request.starts_with("REGISTER ") {
            assert_eq!(registrar::header(&request, "Expires"), "0");
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), source).unwrap();
            return;
        }
        assert_eq!(registrar::header(&request, "Call-ID"), call_id);
        if request.starts_with("BYE ") {
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), source).unwrap();
        } else {
            assert!(request.starts_with("SIP/2.0 "));
        }
    }
}

pub(in super::super) fn run(scenario: &Scenario<'_>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    let authority = socket.local_addr().unwrap();
    let (provision, address) = provisioning(scenario);
    let document = scenario.client.pki.join("managed-phone-incoming.json");
    fs::write(&document, serde_json::to_vec(&provision).unwrap()).unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let client = Client { pki: scenario.client.pki, port: address.port(), proxy: None };
    let witness = std::path::PathBuf::from(
        std::env::var_os("RPTADV_TEST_PHONE_AUDIO").expect("isolated station programme witness"),
    );
    let mut process = managed_asl::start(&document);
    managed_asl::wait_service(&client, address, &mut process);
    let port = port(&client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    put(&client, &format!("{port}interface/"), json!({"audio":{"device_identifier":"3-1"}}));
    put(&client, &format!("{port}radio/"), json!({"duplex_mode":"full"}));
    // The same synthetic mailbox and PIN used by the in-process incoming success fixture.
    edit(&client, "POST", &format!("{port}mailboxes/"), json!({"key":"741","configuration":{
        "first_name":"Test","callsign":"N0TEST",
        "pin_hash":"$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw"
    }}));
    edit(&client, "POST", &format!("{port}protocols/phone-patch/"), json!({"configuration":{
        "provider":{"protocol":"sip","server":"127.0.0.1","port":authority.port(),"username":"desk"},
        "credentials":{"password":"fixture-only"},"vad":{"enabled":false}
    }}));

    let (enabled, contact) = thread::scope(|scope| {
        let peer = scope.spawn(|| {
            let (request, contact) = register(&socket);
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), contact).unwrap();
            contact
        });
        let result = client.edit("PUT", &port, Some(&json!({"configuration":{
            "name":"Radio","inherit_from":"site","enabled":true
        }})));
        (result, peer.join().unwrap())
    });

    let checked = catch_unwind(AssertUnwindSafe(|| {
        assert_eq!(enabled.code, 202, "incoming SIP radio activation");
        managed_asl::confirmed(&client, enabled.body["desired_revision"].as_str().unwrap(), station);
        let mut caller = Caller::connect(&socket, contact, DIALOG, true);
        let ((station_to_phone, phone_to_station), options_answered) =
            with_keepalives(&socket, contact, || {
                assert!(
                    caller.receive_private_audio() > 0,
                    "expected incoming login prompt"
                );
                caller.digits("741#1234#");
                caller.digits("*3#");

                // The independent station tone must arrive before the caller supplies its own waveform.
                let station_to_phone = caller.fresh_station_audio();
                assert!(
                    fs::read_to_string(&witness).unwrap().is_empty(),
                    "caller tone witness was already set"
                );
                caller.audio(65, false);
                let phone_to_station = fs::read_to_string(&witness).unwrap() == "500Hz\n";
                (station_to_phone, phone_to_station)
            });
        eprintln!(
            "INCOMING_SIP_PCM station_to_phone={station_to_phone} phone_to_station={phone_to_station} options_answered={options_answered}"
        );
        caller.hangup();
        (station_to_phone, phone_to_station)
    }));

    process.signal("-TERM");
    withdraw(&socket, contact);
    wait_stopped(&mut process);
    assert!(WorkingFiles::open(&scenario.fixture.0).unwrap().try_writer_lease().is_ok());
    let (station_to_phone, phone_to_station) =
        checked.unwrap_or_else(|failure| resume_unwind(failure));
    eprintln!("INCOMING_SIP_CLOSED digest_registered=true confirmed=true invite_200=true ack=true station_to_phone={station_to_phone} phone_to_station={phone_to_station} bye_200=true withdrawal_200=true foreground_joined=true writer_released=true");
    assert!(station_to_phone && phone_to_station, "attached incoming SIP must exchange both PCM directions");
}
