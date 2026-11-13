//! Same actual product/private source, with an independent literal SIP/RTP caller.
use super::*;
use crate::phone::media::{G711, G711Codec};
use std::collections::BTreeMap;

struct Caller {
    sip: UdpSocket,
    rtp: UdpSocket,
    target: SocketAddr,
    media: SocketAddr,
    to: String,
    sequence: u16,
    timestamp: u32,
}

fn fields(packet: &[u8]) -> (String, BTreeMap<String, String>) {
    let text = std::str::from_utf8(packet).unwrap();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap().to_owned();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            let name = match name.to_ascii_lowercase().as_str() {
                "v" => "via".to_owned(),
                "f" => "from".to_owned(),
                "t" => "to".to_owned(),
                "i" => "call-id".to_owned(),
                _ => name.to_ascii_lowercase(),
            };
            (name, value.trim().to_owned())
        })
        .collect();
    (first, headers)
}
impl Caller {
    fn answer_register(&self, fields: &BTreeMap<String, String>, target: SocketAddr) {
        let mut response = "SIP/2.0 200 OK\r\n".to_owned();
        for name in ["via", "from", "to", "call-id", "cseq"] {
            response.push_str(&format!("{name}: {}\r\n", fields[name]));
        }
        if let Some(contact) = fields.get("contact") {
            let expiry = if fields.get("expires").is_some_and(|value| value == "0") {
                0
            } else {
                600
            };
            response.push_str(&format!("Contact: {contact};expires={expiry}\r\n"));
        }
        response.push_str("Content-Length: 0\r\n\r\n");
        self.sip.send_to(response.as_bytes(), target).unwrap();
    }
    fn request(&self, method: &str, body: &str) {
        let port = self.sip.local_addr().unwrap().port();
        let cseq = if method == "BYE" { 2 } else { 1 };
        let mut wire = format!(
            "{method} sip:fixture-user@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK-product-{method};rport\r\nMax-Forwards: 70\r\nFrom: <sip:untrusted@fixture.invalid>;tag=fixture\r\nTo: {}\r\nCall-ID: literal-private-call@fixture.invalid\r\nCSeq: {cseq} {method}\r\nContact: <sip:caller@127.0.0.1:{port}>\r\n",
            self.to
        );
        if !body.is_empty() {
            wire.push_str("Content-Type: application/sdp\r\n");
        }
        wire.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        self.sip.send_to(wire.as_bytes(), self.target).unwrap();
    }
    fn establish(sip: UdpSocket, registered: Option<SocketAddr>) -> Self {
        sip.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
        rtp.set_nonblocking(true).unwrap();
        let placeholder = "127.0.0.1:1".parse().unwrap();
        let mut owner = Self {
            sip,
            rtp,
            target: registered.unwrap_or(placeholder),
            media: placeholder,
            to: "<sip:fixture-user@127.0.0.1>".into(),
            sequence: 0,
            timestamp: 0,
        };
        let mut packet = [0; 8192];
        if registered.is_none() {
            let (count, target) = owner.sip.recv_from(&mut packet).unwrap();
            let (first, values) = fields(&packet[..count]);
            assert!(first.starts_with("REGISTER "));
            owner.target = target;
            owner.answer_register(&values, target);
            // Let the existing account owner adopt its registration acknowledgement.
            thread::sleep(Duration::from_millis(30));
        }
        let port = owner.rtp.local_addr().unwrap().port();
        let offer = format!(
            "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n"
        );
        owner.request("INVITE", &offer);
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let (count, source) = owner
                .sip
                .recv_from(&mut packet)
                .expect("actual product did not answer incoming SIP call");
            let (first, values) = fields(&packet[..count]);
            if ["REGISTER ", "OPTIONS "]
                .iter()
                .any(|method| first.starts_with(method))
            {
                owner.answer_register(&values, source);
            } else if first.starts_with("SIP/2.0 200 ") {
                assert_eq!(values["cseq"], "1 INVITE");
                owner.to = values["to"].clone();
                let body = std::str::from_utf8(&packet[..count])
                    .unwrap()
                    .split_once("\r\n\r\n")
                    .unwrap()
                    .1;
                let address = body
                    .lines()
                    .find_map(|line| line.strip_prefix("c=IN IP4 "))
                    .unwrap();
                let media = body
                    .lines()
                    .find_map(|line| line.strip_prefix("m=audio "))
                    .unwrap();
                let port: u16 = media.split_whitespace().next().unwrap().parse().unwrap();
                assert!(body.contains("PCMU/8000") && body.contains("telephone-event/8000"));
                owner.media = SocketAddr::new(address.trim().parse().unwrap(), port);
                owner.request("ACK", "");
                owner.sip.set_nonblocking(true).unwrap();
                return owner;
            } else {
                assert!(first.starts_with("SIP/2.0 100 "), "call rejected: {first}");
            }
            assert!(Instant::now() < deadline, "no explicit SDP answer");
        }
    }
    fn service(&self) {
        let mut packet = [0; 8192];
        if let Ok((count, address)) = self.sip.recv_from(&mut packet) {
            let (first, values) = fields(&packet[..count]);
            if ["REGISTER ", "OPTIONS ", "BYE "]
                .iter()
                .any(|method| first.starts_with(method))
            {
                self.answer_register(&values, address);
            }
        }
    }
    fn prompt(&self, codec: &mut G711Codec, stage: &str) -> Vec<f32> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_tone = Instant::now();
        let mut result = Vec::new();
        loop {
            self.service();
            let mut wire = [0; 2048];
            if let Ok((length, source)) = self.rtp.recv_from(&mut wire) {
                assert_eq!(source, self.media);
                assert_eq!(length, 172);
                assert_eq!(wire[0], 0x80);
                assert_eq!(wire[1] & 0x7f, 0);
                let mut pcm = [0.0; 160];
                codec
                    .decode(wire[12..172].try_into().unwrap(), &mut pcm)
                    .unwrap();
                if pcm.iter().any(|sample| sample.abs() > 0.01) {
                    last_tone = Instant::now();
                }
                result.extend_from_slice(&pcm);
            }
            if !result.is_empty() && last_tone.elapsed() > Duration::from_millis(400) {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "private SIP {stage} prompt PCM did not arrive"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn digits(&mut self, digits: &[u8]) {
        for digit in digits {
            let event = b"0123456789*#ABCD"
                .iter()
                .position(|value| value == digit)
                .unwrap() as u8;
            self.timestamp += 320;
            // A complete event plus its redundant end must yield one private digit.
            for flags in [10, 0x8a, 0x8a] {
                let mut wire = [0u8; 16];
                wire[0] = 0x80;
                wire[1] = 101;
                wire[2..4].copy_from_slice(&self.sequence.to_be_bytes());
                wire[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
                wire[8..12].copy_from_slice(&19u32.to_be_bytes());
                wire[12..].copy_from_slice(&[event, flags, 0, 160]);
                self.rtp.send_to(&wire, self.media).unwrap();
                self.sequence = self.sequence.wrapping_add(1);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
pub(crate) fn caller(sip: UdpSocket, registered: Option<SocketAddr>) {
    let mut caller = Caller::establish(sip, registered);
    let mut codec = G711Codec::load(G711::Mulaw).unwrap();
    let login = caller.prompt(&mut codec, "login");
    caller.digits(b"100#1234#");
    let authenticated = caller.prompt(&mut codec, "authenticated");
    caller.digits(b"*4#*70");
    let status = caller.prompt(&mut codec, "status");
    assert_prompt(&status, "-. ---/.-.. .. -. -.- ...");
    assert_prompt(
        &authenticated,
        ".-.. --- --. .. -./--- -.-/..- -. .-.. --- -.-. -.-/... . .--. .- .-. .- - .",
    );
    assert_prompt(
        &login,
        "-- .- .. .-.. -... --- -..-/.--. --- ..- -. -../.--. .. -./.--. --- ..- -. -..",
    );
    caller.request("BYE", "");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut packet = [0; 8192];
    loop {
        if let Ok((count, source)) = caller.sip.recv_from(&mut packet) {
            let (first, values) = fields(&packet[..count]);
            if first.starts_with("SIP/2.0 200 ") && values["cseq"] == "2 BYE" {
                break;
            }
            if first.starts_with("REGISTER ") {
                caller.answer_register(&values, source);
            }
        }
        assert!(
            Instant::now() < deadline,
            "private SIP BYE was not acknowledged"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn real_product_private_sip_call_authenticates_and_returns_status_without_radio_output() {
    const CHILD: &str = "RPTADV_TEST_REAL_SIP_PHONE_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host_services::product_phone_tests::sip::real_product_private_sip_call_authenticates_and_returns_status_without_radio_output", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "real SIP product child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory = std::env::temp_dir().join(format!("rpt-real-sip-phone-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let secret = directory.join("phone.secret");
    fs::write(&secret, "fixture-secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let sip = UdpSocket::bind("127.0.0.1:0").unwrap();
    let configuration = format!(
        "[general]\nnode_enabled=yes\n[desk]\nradio_channel=fixture\nduplex=full\n[morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n[phone_account office]\nlocal_node=desk\nenabled=yes\nprotocol=sip\nserver={}\nusername=fixture-user\nsecrets_file=phone.secret\nregister=yes\n{}",
        sip.local_addr().unwrap(),
        include_str!("../../../product/src/tests/fixtures/phone_mailbox.conf")
    );
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt_advanced.conf"),
            resolve_phone_accounts(&configuration).unwrap(),
            &PreparedAccounts::default(),
            |_| panic!("numeric endpoint must not resolve"),
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
            descriptors.product,
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
    let caller = thread::spawn(move || caller(sip, None));
    let deadline = Instant::now() + Duration::from_secs(50);
    while !caller.is_finished() && Instant::now() < deadline {
        while let Some(receipt) = worker.incoming() {
            runtime
                .incoming_sip(receipt)
                .expect("actual product SIP admission");
        }
        thread::sleep(Duration::from_millis(1));
    }
    let result = caller.join();
    worker.stop().unwrap();
    runtime.stop().unwrap();
    assert_foreground_stopped();
    fs::remove_dir_all(directory).unwrap();
    result.unwrap();
}
