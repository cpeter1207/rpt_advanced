//! One complete registered command through the actual standalone SIP provider.
use super::{managed_asl, phone_caller::Caller, private_phone::{register, registrar}, *};
use std::{io::ErrorKind, net::{SocketAddr, UdpSocket}, panic::{AssertUnwindSafe, catch_unwind, resume_unwind}};

const MAPPING: &str = "fixture-dial";
const CONTROL_CODE: &str = "*508";
const DIAL_CODE: &str = "*509";
const NUMBER: &str = "01852";

struct Invite {
    source: SocketAddr,
    target: String,
    call_id: String,
    sequence: String,
    answer: String,
    from: String,
    to: String,
    media: SocketAddr,
}

fn await_invite(socket: &UdpSocket, media: &UdpSocket, invite: &mut Option<Invite>) {
    socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let mut wire = [0; 8192];
    let (request, source) = match registrar::receive_register(socket, &mut wire) {
        Ok(received) => received,
        Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => return,
        Err(error) => panic!("SIP fixture receive failed: {error}"),
    };
    assert!(request.starts_with("INVITE "));
    let authority = socket.local_addr().unwrap();
    let media_port = media.local_addr().unwrap().port();
    let description = format!("v=0\r\no=test 1 1 IN IP4 127.0.0.1\r\ns=private\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {media_port} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n");
    let answer = registrar::response(&request, 200, false).replace("Content-Length: 0\r\n\r\n",
        &format!("Contact: <sip:recipient@{authority}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{description}", description.len()));
    let address = request.lines().find_map(|line| line.strip_prefix("c=IN IP4 ")).unwrap()
        .parse::<std::net::Ipv4Addr>().unwrap();
    let port = request.lines().find_map(|line| line.strip_prefix("m=audio ")).unwrap()
        .split_ascii_whitespace().next().unwrap().parse::<u16>().unwrap();
    assert!(address.is_loopback());
    *invite = Some(Invite {
        source,
        target: request.split_ascii_whitespace().nth(1).unwrap().to_owned(),
        call_id: registrar::header(&request, "Call-ID").to_owned(),
        sequence: registrar::header(&request, "CSeq").split_ascii_whitespace().next().unwrap().to_owned(),
        from: registrar::header(&request, "From").to_owned(),
        to: registrar::header(&answer, "To").to_owned(),
        answer,
        media: (address, port).into(),
    });
    socket.send_to(invite.as_ref().unwrap().answer.as_bytes(), source).unwrap();
}

fn exercise_dialog(socket: &UdpSocket, media: UdpSocket, invite: &Invite, witness: &Path, forbidden: &str)
    -> [bool; 5] {
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut wire = [0; 8192];
    let ack = registrar::receive_register(socket, &mut wire).is_ok_and(|(packet, source)| {
        packet.starts_with("ACK ") && source == invite.source
            && registrar::header(&packet, "Call-ID") == invite.call_id
            && registrar::header(&packet, "CSeq").split_ascii_whitespace().next() == Some(invite.sequence.as_str())
    });
    if !ack { return [false; 5]; }
    let mut caller = Caller::answered(socket, invite.source, media, invite.media, Some(101));
    let station_pcm = caller.station_audio();
    // Establish the station source above before injecting a distinct return waveform.
    assert_ne!(fs::read_to_string(witness).as_deref().ok(), Some("500Hz\n"));
    caller.audio(65, false);
    let phone_pcm = fs::read_to_string(witness).as_deref().ok() == Some("500Hz\n");
    caller.digits(forbidden);
    let contact = invite.source;
    let authority = socket.local_addr().unwrap();
    let bye = format!("BYE sip:desk@{contact} SIP/2.0\r\nVia: SIP/2.0/UDP {authority};branch=z9hG4bK-a5-end;rport\r\nMax-Forwards: 70\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 BYE\r\nContent-Length: 0\r\n\r\n",
        invite.to, invite.from, invite.call_id);
    socket.send_to(bye.as_bytes(), contact).unwrap();
    let mut media_retired = false;
    let mut command_isolated = true;
    let deadline = Instant::now() + Duration::from_secs(4);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if remaining.is_zero() { break; }
        socket.set_read_timeout(Some(remaining)).unwrap();
        let Ok((packet, source)) = registrar::receive_register(socket, &mut wire) else { break };
        assert_eq!(source, contact);
        let same = registrar::header(&packet, "Call-ID") == invite.call_id;
        if same && packet.starts_with("SIP/2.0 200 ") && registrar::header(&packet, "CSeq") == "1 BYE" {
            media_retired = caller.media_quiet();
            break;
        }
        if packet.starts_with("BYE ") {
            socket.send_to(registrar::response(&packet, 200, false).as_bytes(), source).unwrap();
        } else if packet.starts_with("INVITE ") && !same {
            command_isolated = false;
            socket.send_to(registrar::response(&packet, 486, false).as_bytes(), source).unwrap();
        }
    }
    [ack, station_pcm, phone_pcm, command_isolated, media_retired]
}

fn withdraw(socket: &UdpSocket, contact: SocketAddr, invite: Option<&Invite>) {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut wire = [0; 8192];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())
            .expect("SIP account did not withdraw");
        socket.set_read_timeout(Some(remaining)).unwrap();
        let (request, source) = registrar::receive_register(socket, &mut wire).unwrap();
        assert_eq!(source, contact, "SIP account changed its signaling socket");
        if request.starts_with("REGISTER ") {
            assert_eq!(registrar::header(&request, "Expires"), "0");
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), source).unwrap();
            return;
        }
        let same_transaction = invite.is_some_and(|i| registrar::header(&request, "Call-ID") == i.call_id);
        assert!(same_transaction, "SIP fixture observed a second transaction");
        if request.starts_with("INVITE ") {
            socket.send_to(invite.unwrap().answer.as_bytes(), source).unwrap();
        } else if request.starts_with("BYE ") {
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), source).unwrap();
        } else {
            assert!(request.starts_with("ACK ") || request.starts_with("SIP/2.0 "));
        }
    }
}

pub(in super::super) fn run(scenario: &Scenario<'_>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let authority = socket.local_addr().unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    let (provision, address) = provisioning(scenario);
    let document = scenario.client.pki.join("managed-phone-dial.json");
    fs::write(&document, serde_json::to_vec(&provision).unwrap()).unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let client = Client { pki: scenario.client.pki, port: address.port(), proxy: None };
    let witness = std::path::PathBuf::from(std::env::var_os("RPTADV_TEST_PHONE_AUDIO")
        .expect("isolated station programme witness"));
    let mut process = managed_asl::start(&document);
    managed_asl::wait_service(&client, address, &mut process);
    let port = port(&client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    put(&client, &format!("{port}interface/"), json!({"audio":{"device_identifier":"3-1"}}));
    put(&client, &format!("{port}radio/"), json!({"duplex_mode":"full"}));
    edit(&client, "POST", &format!("{port}protocols/phone-patch/"), json!({"configuration":{
        "provider":{"protocol":"sip","server":"127.0.0.1","port":authority.port(),"username":"desk"},
        "credentials":{"password":"fixture-only"},"vad":{"enabled":false}
    }}));

    let (enabled, contact) = thread::scope(|scope| {
        let registrar = scope.spawn(|| {
            let (request, contact) = register(&socket);
            socket.send_to(registrar::response(&request, 200, false).as_bytes(), contact).unwrap();
            contact
        });
        let result = client.edit("PUT", &port, Some(&json!({"configuration":{
            "name":"Radio","inherit_from":"site","enabled":true
        }})));
        (result, registrar.join().unwrap())
    });

    let mut invite = None;
    let checked = catch_unwind(AssertUnwindSafe(|| {
        assert_eq!(enabled.code, 202, "SIP radio activation");
        managed_asl::confirmed(&client, enabled.body["desired_revision"].as_str().unwrap(), station);
        let settings = format!("{port}command/settings/");
        let effective_path = effective(&settings);
        let prior = client.get(&effective_path);
        assert_eq!(prior.code, 200);
        let before = &prior.body["effective"];
        for mapping in before["mappings"].as_object().unwrap().values() {
            let existing = mapping["command"].as_str().unwrap();
            assert!(existing.is_empty() || [CONTROL_CODE, DIAL_CODE].iter().all(|code|
                !code.starts_with(existing) && !existing.starts_with(*code)));
        }
        let terminator = before["terminator"].as_str().unwrap();
        assert_eq!(terminator.len(), 1);
        assert!(!DIAL_CODE.contains(terminator) && !NUMBER.contains(terminator));
        let authored = |action, code| json!({"configuration":{"mappings":{
            (MAPPING):{"action":action,"command":code,"protected":false}
        }}});
        edit(&client, "PUT", &settings, authored("link_status", CONTROL_CODE));
        let control = client.get(&effective_path);
        assert_eq!(control.code, 200);
        assert_eq!(control.body["effective"]["mappings"][MAPPING]["action"], "link_status");
        assert_eq!(control.body["effective"]["mappings"][MAPPING]["command"], CONTROL_CODE);

        let mapped = client.edit("PUT", &settings, Some(&authored("phone_dial", DIAL_CODE)));
        let health = "/api/v1/health/";
        let mut active = client.get(health).body["status"]["active_revision"].clone();
        let mut selected = false;
        if let Some(wanted) = mapped.body["desired_revision"].as_str().filter(|_| mapped.code == 202) {
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                let status = client.get(health).body["status"].clone();
                active = status["active_revision"].clone();
                selected = active == wanted
                    && status["desired_revision"] == wanted && status["historical_activation"] == "confirmed";
                if selected || status["historical_activation"] == "failed" || Instant::now() >= until {
                    break;
                }
                process.running(until);
            }
        }

        let view = client.get(&effective_path);
        let mapping = &view.body["effective"]["mappings"][MAPPING];
        let effective = view.code == 200 && mapping["action"] == "phone_dial" && mapping["command"] == DIAL_CODE;
        let submitted = operation(&client, json!({"submitted":true,"targets":[station],
            "expected_active_revision":active,
            "action":{"action":"execute_dtmf","digits":format!("{DIAL_CODE}{NUMBER}{terminator}")}}));
        let receipt = &submitted.body["receipt"]["outcomes"][0];
        let admitted = receipt["target"] == station && receipt["outcome"] == "admitted";
        let media = UdpSocket::bind("127.0.0.1:0").unwrap();
        await_invite(&socket, &media, &mut invite);
        let media = invite.as_ref().map_or([false; 5], |call| {
            exercise_dialog(&socket, media, call, &witness, &format!("{DIAL_CODE}{NUMBER}{terminator}"))
        });
        (mapped.code, selected, view.code, effective, submitted.code, admitted, media)
    }));

    process.signal("-TERM");
    withdraw(&socket, contact, invite.as_ref());
    wait_stopped(&mut process);
    assert!(WorkingFiles::open(&scenario.fixture.0).unwrap().try_writer_lease().is_ok());
    let (mapping_http, selected, effective_http, effective, work_http, admitted,
        [ack, station_pcm, phone_pcm, command_isolated, media_retired]) =
        checked.unwrap_or_else(|failure| resume_unwind(failure));
    let reached_recipient = invite.as_ref().is_some_and(|call| {
        call.source == contact && call.target == format!("sip:{NUMBER}@{authority}")
    });
    eprintln!(
        "A5_SIP_CLOSED digest_registered=true withdrawal_200=true foreground_joined=true writer_released=true mapping_http={mapping_http} selected={selected} effective_http={effective_http} effective={effective} operation_http={work_http} admitted={admitted} invite={} invite_matches={reached_recipient}",
        invite.is_some()
    );
    eprintln!("A5_SIP_MEDIA ack={ack} station_to_phone={station_pcm} phone_to_station={phone_pcm} command_isolated={command_isolated} media_retired={media_retired}");
    assert!(mapping_http == 202 && selected && effective && admitted && reached_recipient,
        "A5 completed phone_dial mapping must originate exactly one INVITE for the complete number on the selected SIP account");
    assert!(ack && station_pcm && phone_pcm && command_isolated && media_retired,
        "A5 answered SIP must exchange both PCM directions, deny called-party commands and retire media on BYE");
}
