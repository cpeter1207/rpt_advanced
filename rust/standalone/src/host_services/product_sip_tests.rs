//! Same actual product/private source, with an independent literal SIP/RTP caller.
use super::*;
use crate::phone::media::TelephoneEvent;
use crate::phone::media::{AudioCodec, AudioProfile};
use std::collections::BTreeMap;
#[path = "product_sip_outgoing_tests.rs"]
mod outgoing;
#[path = "product_sip_vox_tests.rs"]
mod vox;

struct Caller {
    sip: UdpSocket,
    rtp: UdpSocket,
    target: SocketAddr,
    media: SocketAddr,
    to: String,
    sequence: u16,
    timestamp: u32,
    keepalive: bool,
    profile: AudioProfile,
    events: bool,
    event_clock: u32,
    event_payload: u8,
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
    fn packet_interval(&self) -> Duration {
        let p = self.profile.parameters();
        Duration::from_nanos(u64::from(p.packet_frames) * 1_000_000_000 / u64::from(p.pcm_rate_hz))
    }
    fn packet_ticks(&self) -> u32 {
        let p = self.profile.parameters();
        p.packet_frames * p.rtp_clock_hz / p.pcm_rate_hz
    }
    fn answer_register(&self, fields: &BTreeMap<String, String>, target: SocketAddr) {
        let mut response = "SIP/2.0 200 OK\r\n".to_owned();
        for name in ["via", "from", "to", "call-id", "cseq"] {
            response.push_str(&format!("{name}: {}\r\n", fields[name]));
        }
        if let Some(contact) = fields.get("contact") {
            let expiry = fields
                .get("expires")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(600);
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
        Self::establish_profile(sip, registered, AudioProfile::Mulaw, true)
    }
    fn establish_profile(
        sip: UdpSocket,
        registered: Option<SocketAddr>,
        profile: AudioProfile,
        events: bool,
    ) -> Self {
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
            // Inband digits share the continuous audio source clock. Silence
            // during prompt waits prevents the next digit from being stale RTP.
            keepalive: !events,
            profile,
            events,
            event_clock: profile.parameters().rtp_clock_hz,
            event_payload: if profile.parameters().rtp_clock_hz == 16000 {
                102
            } else {
                101
            },
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
        let offer = crate::phone::media::MediaDescription {
            remote: owner.rtp.local_addr().unwrap(),
            codec: profile,
            receive_payload: profile.payload(),
            send_payload: profile.payload(),
            telephone_events: [8000, 16000].map(|rate| {
                (events && rate == owner.event_clock)
                    .then_some(TelephoneEvent::symmetric(owner.event_payload, rate))
            }),
        }
        .answer(owner.rtp.local_addr().unwrap(), 1)
        .unwrap();
        let offer = std::str::from_utf8(&offer).unwrap();
        owner.request("INVITE", offer);
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
                let selected =
                    crate::phone::media::MediaDescription::parse(body.as_bytes()).unwrap();
                assert_eq!(selected.codec, profile);
                assert_eq!(
                    selected.telephone_events,
                    [8000, 16000].map(|rate| (events && rate == owner.event_clock)
                        .then_some(TelephoneEvent::symmetric(owner.event_payload, rate)))
                );
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
    fn audio(&mut self, codec: &mut AudioCodec, pcm: &[f32]) {
        self.audio_payload(codec, pcm, self.profile.payload());
    }
    fn audio_payload(&mut self, codec: &mut AudioCodec, pcm: &[f32], payload: u8) {
        let mut encoded = vec![0; self.profile.parameters().packet_bytes];
        codec.encode(pcm, &mut encoded).unwrap();
        let mut wire = vec![0; 12 + encoded.len()];
        wire[0] = 0x80;
        wire[1] = payload;
        wire[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        wire[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        wire[8..12].copy_from_slice(&19u32.to_be_bytes());
        wire[12..].copy_from_slice(&encoded);
        self.rtp.send_to(&wire, self.media).unwrap();
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(self.packet_ticks());
    }
    fn prompt(&mut self, codec: &mut AudioCodec, stage: &str) -> Vec<f32> {
        let source_start = self.timestamp;
        let started = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_tone = Instant::now();
        let mut result = Vec::new();
        let mut next_audio = Instant::now();
        loop {
            if self.keepalive && Instant::now() >= next_audio {
                self.audio(
                    codec,
                    &vec![0.0; self.profile.parameters().packet_frames as usize],
                );
                next_audio += self.packet_interval();
            }
            self.service();
            let mut wire = [0; 2048];
            if let Ok((length, source)) = self.rtp.recv_from(&mut wire) {
                assert_eq!(source, self.media);
                assert_eq!(length, 12 + self.profile.parameters().packet_bytes);
                assert_eq!(wire[0], 0x80);
                assert_eq!(wire[1] & 0x7f, self.profile.payload());
                let mut pcm = vec![0.0; self.profile.parameters().packet_frames as usize];
                codec.decode(&wire[12..length], &mut pcm).unwrap();
                if pcm.iter().any(|sample| sample.abs() > 0.01) {
                    last_tone = Instant::now();
                }
                result.extend_from_slice(&pcm);
            }
            if !result.is_empty() && last_tone.elapsed() > Duration::from_millis(400) {
                if !self.events {
                    let ticks = self.timestamp.wrapping_sub(source_start);
                    assert!(
                        ticks > 0,
                        "inband prompt waits retain a progressing RTP source clock"
                    );
                    eprintln!(
                        "inband {stage} source timeline: {} ms, {ticks} negotiated RTP ticks, {} PCM Hz",
                        started.elapsed().as_millis(),
                        self.profile.parameters().pcm_rate_hz
                    );
                }
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "private SIP {stage} prompt PCM did not arrive"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn control_digits(&mut self, codec: &mut AudioCodec, digits: &[u8]) {
        if self.events {
            self.digits(digits);
            return;
        }
        let parameters = self.profile.parameters();
        let frames = parameters.packet_frames as usize;
        let mut pcm = vec![0.0; frames];
        let mut next = Instant::now();
        for digit in digits {
            let index = b"123A456B789C*0#D"
                .iter()
                .position(|symbol| symbol == digit)
                .unwrap();
            let low = [697.0, 770.0, 852.0, 941.0][index / 4];
            let high = [1209.0, 1336.0, 1477.0, 1633.0][index % 4];
            // Existing test tone synthesis, at the negotiated source rate.
            // Keep the same100ms tone and at least100ms gap. The final native
            // packet may contain additional silence; no coding unit is truncated.
            let tone_frames = parameters.pcm_rate_hz as usize / 10;
            let digit_packets = (parameters.pcm_rate_hz as usize / 5).div_ceil(frames);
            for packet in 0..digit_packets {
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    thread::sleep(wait);
                }
                for (sample, value) in pcm.iter_mut().enumerate() {
                    let phase = (packet * frames + sample) as f32 * std::f32::consts::TAU
                        / parameters.pcm_rate_hz as f32;
                    *value = if packet * frames + sample < tone_frames {
                        0.2 * ((phase * low).sin() + (phase * high).sin())
                    } else {
                        0.0
                    };
                }
                self.audio(codec, &pcm);
                self.service();
                next += self.packet_interval();
            }
        }
    }
    fn digits(&mut self, digits: &[u8]) {
        for digit in digits {
            let event = b"0123456789*#ABCD"
                .iter()
                .position(|value| value == digit)
                .unwrap() as u8;
            self.timestamp = self
                .timestamp
                .wrapping_add(self.profile.parameters().rtp_clock_hz / 25);
            // A complete event plus its redundant end must yield one private digit.
            for flags in [10, 0x8a, 0x8a] {
                let mut wire = [0u8; 16];
                wire[0] = 0x80;
                wire[1] = self.event_payload;
                wire[2..4].copy_from_slice(&self.sequence.to_be_bytes());
                wire[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
                wire[8..12].copy_from_slice(&19u32.to_be_bytes());
                let duration = (self.event_clock / 50) as u16;
                wire[12..].copy_from_slice(&[event, flags, (duration >> 8) as u8, duration as u8]);
                self.rtp.send_to(&wire, self.media).unwrap();
                self.sequence = self.sequence.wrapping_add(1);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
pub(crate) fn caller(sip: UdpSocket, registered: Option<SocketAddr>) {
    call_flow(sip, registered, false);
}
fn call_flow(sip: UdpSocket, registered: Option<SocketAddr>, login_only: bool) {
    call_flow_profile(sip, registered, login_only, AudioProfile::Mulaw, true);
}
fn call_flow_profile(
    sip: UdpSocket,
    registered: Option<SocketAddr>,
    login_only: bool,
    profile: AudioProfile,
    events: bool,
) {
    let mut caller = Caller::establish_profile(sip, registered, profile, events);
    let mut codec = AudioCodec::load(profile).unwrap();
    let assert_prompt = |pcm: &[f32], expected: &str| {
        // Envelope-only observation reuses the unchanged 8 kHz Morse assertion.
        let observed: Vec<_> = pcm
            .iter()
            .step_by((profile.parameters().pcm_rate_hz / 8000) as usize)
            .copied()
            .collect();
        super::assert_prompt(&observed, expected);
    };
    let login = caller.prompt(&mut codec, "login");
    if !login_only {
        caller.control_digits(&mut codec, b"100#");
        let password = caller.prompt(&mut codec, "password");
        assert_prompt(
            &password,
            ".--. .- ... ... .-- --- .-. -../.--. --- ..- -. -..",
        );
        caller.control_digits(&mut codec, b"1234#");
        let authenticated = caller.prompt(&mut codec, "authenticated");
        caller.control_digits(&mut codec, b"*4#*70");
        let status = caller.prompt(&mut codec, "status");
        assert_prompt(&status, "-. ---/.-.. .. -. -.- ...");
        assert_prompt(
            &authenticated,
            ".-.. --- --. .. -./--- -.-/..- -. .-.. --- -.-. -.-/... . .--. .- .-. .- - .",
        );
    }
    assert_prompt(&login, "-- .- .. .-.. -... --- -..-/.--. --- ..- -. -..");
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
    product_call(false, false, false);
}
#[test]
fn real_product_zero_mailbox_sip_call_prompts_and_hangs_up_without_radio_output() {
    product_call(true, false, false);
}
#[test]
fn real_product_sip_call_connects_remote_and_transmits_voice() {
    product_call(false, true, false);
}
#[test]
fn real_product_sip_call_attaches_to_existing_ordinary_iax_link() {
    product_call(false, true, true);
}
#[test]
fn real_product_g722_private_call_authenticates_with_events() {
    product_call_profile(false, false, false, AudioProfile::G722, true);
}
#[test]
fn real_product_g722_private_call_authenticates_inband() {
    product_call_profile(false, false, false, AudioProfile::G722, false);
}
fn product_call(no_mailbox: bool, voice: bool, existing: bool) {
    product_call_profile(no_mailbox, voice, existing, AudioProfile::Mulaw, true);
}
fn product_call_profile(
    no_mailbox: bool,
    voice: bool,
    existing: bool,
    profile: AudioProfile,
    events: bool,
) {
    const CHILD: &str = "RPTADV_TEST_REAL_SIP_PHONE_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let test = if profile == AudioProfile::Ilbc30 && events {
            "host_services::product_phone_tests::sip::real_product_ilbc_private_call_authenticates_with_events"
        } else if profile == AudioProfile::Ilbc30 {
            "host_services::product_phone_tests::sip::real_product_ilbc_private_call_authenticates_inband"
        } else if profile == AudioProfile::L16Wideband && events {
            "host_services::product_phone_tests::sip::real_product_l16_private_call_authenticates_with_matching_events"
        } else if profile == AudioProfile::L16Wideband {
            "host_services::product_phone_tests::sip::real_product_l16_private_call_authenticates_inband"
        } else if profile == AudioProfile::G722 && events {
            "host_services::product_phone_tests::sip::real_product_g722_private_call_authenticates_with_events"
        } else if profile == AudioProfile::G722 {
            "host_services::product_phone_tests::sip::real_product_g722_private_call_authenticates_inband"
        } else if existing {
            "host_services::product_phone_tests::sip::real_product_sip_call_attaches_to_existing_ordinary_iax_link"
        } else if voice {
            "host_services::product_phone_tests::sip::real_product_sip_call_connects_remote_and_transmits_voice"
        } else if no_mailbox {
            "host_services::product_phone_tests::sip::real_product_zero_mailbox_sip_call_prompts_and_hangs_up_without_radio_output"
        } else {
            "host_services::product_phone_tests::sip::real_product_private_sip_call_authenticates_and_returns_status_without_radio_output"
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "real SIP product child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Isolated child only: retain ordinary redacted product diagnostics on failure.
    unsafe {
        libc::openlog(
            c"rpt-sip-phone-fixture".as_ptr(),
            libc::LOG_PERROR,
            libc::LOG_USER,
        )
    };
    let directory = std::env::temp_dir().join(format!("rpt-real-sip-phone-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let secret = directory.join("phone.secret");
    fs::write(&secret, "fixture-secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let sip = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut remote = voice.then(vox::Remote::new);
    let configuration = format!(
        "[general]\nnode_enabled=yes\n[desk]\nradio_channel=fixture\nduplex=full\n[morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n[phone_account office]\nlocal_node=desk\nenabled=yes\nprotocol=sip\nserver={}\nusername=fixture-user\nsecrets_file=phone.secret\nregister=yes\n{}",
        sip.local_addr().unwrap(),
        if no_mailbox {
            ""
        } else {
            include_str!("../../../product/src/tests/fixtures/phone_mailbox.conf")
        }
    );
    let configuration = if let Some(remote) = remote.as_ref() {
        let path = directory.join("nodes.conf");
        fs::write(
            &path,
            format!("[extnodes]\n2000=radio@{}/2000,127.0.0.1\n", remote.address),
        )
        .unwrap();
        configuration.replace("[desk]", &format!("[1000]\nlink_lookup_method=file\nlink_static_directory_file={}\nphone_allow_accounts=office", path.display()))
            .replace("local_node=desk", "local_node=1000\nattach_local_nodes=1000")
    } else {
        configuration
    };
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
    if let Some(remote) = remote.as_mut() {
        runtime.enable_listener(&mut remote.server).unwrap();
    }
    if existing {
        let remote = remote.as_mut().unwrap();
        let command = runtime.product_descriptor().link_command.unwrap();
        // This is the same ordinary CLI entry point, before any private call exists.
        let ordinary =
            thread::spawn(move || unsafe { command(c"1000".as_ptr(), 4, c"2000".as_ptr(), 4, 1) });
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ordinary.is_finished() && Instant::now() < deadline {
            remote.pump();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(ordinary.join().unwrap(), 0);
        let mut quiet_since = Instant::now();
        let mut public = PUBLIC_OUTPUT.load(Ordering::Acquire);
        loop {
            remote.pump();
            let current = PUBLIC_OUTPUT.load(Ordering::Acquire);
            if current != public || LAST_PTT.load(Ordering::Acquire) {
                quiet_since = Instant::now();
                public = current;
            }
            if quiet_since.elapsed() >= Duration::from_millis(750) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ordinary connection notice must drain"
            );
            thread::sleep(Duration::from_millis(1));
        }
        // Ordinary public link notices precede the privacy observation interval.
        PUBLIC_OUTPUT.store(0, Ordering::Release);
        remote.clear_voice_observation();
    }
    let observation = remote
        .as_ref()
        .map(|remote| Arc::clone(&remote.observation));
    let caller = thread::spawn(move || {
        if let Some(observation) = observation {
            vox::call(sip, observation, existing);
        } else {
            call_flow_profile(sip, None, no_mailbox, profile, events);
        }
    });
    // Keep the synthetic remote alive for the bounded caller scenario, including
    // its finite LOCAL operation results; an independent cutoff can starve return audio.
    while !caller.is_finished() {
        if let Some(remote) = remote.as_mut() {
            remote.pump();
        }
        while let Some(receipt) = worker.incoming() {
            runtime
                .incoming_sip(receipt)
                .expect("actual product SIP admission");
        }
        thread::sleep(Duration::from_millis(1));
    }
    let result = caller.join();
    if voice {
        unsafe extern "C" fn status(_: *mut c_void, text: *const std::ffi::c_char, length: usize) {
            let text = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) };
            eprintln!("composed link status: {}", String::from_utf8_lossy(text));
        }
        unsafe {
            runtime.product_descriptor().link_status.unwrap()(
                c"1000".as_ptr(),
                4,
                Some(status),
                ptr::null_mut(),
            );
        }
    }
    if result.is_ok() {
        if let Some(remote) = remote.as_mut() {
            // Ending the private caller must not disconnect its established node link.
            let until = Instant::now() + Duration::from_millis(200);
            while Instant::now() < until {
                remote.pump();
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    worker.stop().unwrap();
    runtime.stop().unwrap();
    assert_radio_stopped(voice);
    drop(remote);
    fs::remove_dir_all(directory).unwrap();
    result.unwrap();
}

#[test]
fn real_product_l16_private_call_authenticates_with_matching_events() {
    product_call_profile(false, false, false, AudioProfile::L16Wideband, true);
}
#[test]
fn real_product_l16_private_call_authenticates_inband() {
    product_call_profile(false, false, false, AudioProfile::L16Wideband, false);
}

#[test]
fn real_product_ilbc_private_call_authenticates_with_events() {
    product_call_profile(false, false, false, AudioProfile::Ilbc30, true);
}
#[test]
fn real_product_ilbc_private_call_authenticates_inband() {
    product_call_profile(false, false, false, AudioProfile::Ilbc30, false);
}
#[test]
fn caller_packet_clock_matches_selected_codec_frame_duration() {
    for profile in AudioProfile::ALL {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut caller = Caller {
            sip: UdpSocket::bind("127.0.0.1:0").unwrap(),
            rtp: UdpSocket::bind("127.0.0.1:0").unwrap(),
            target: peer.local_addr().unwrap(),
            media: peer.local_addr().unwrap(),
            to: String::new(),
            sequence: 0,
            timestamp: 0,
            keepalive: false,
            profile,
            events: true,
            event_clock: profile.parameters().rtp_clock_hz,
            event_payload: 101,
        };
        let p = profile.parameters();
        let mut codec = AudioCodec::load(profile).unwrap();
        let mut wire = [0; 2048];
        for expected in [0, p.packet_frames * p.rtp_clock_hz / p.pcm_rate_hz] {
            caller.audio(&mut codec, &vec![0.0; p.packet_frames as usize]);
            let (length, source) = peer.recv_from(&mut wire).unwrap();
            assert_eq!(source, caller.rtp.local_addr().unwrap());
            assert_eq!(length, 12 + p.packet_bytes);
            assert_eq!(
                u32::from_be_bytes(wire[4..8].try_into().unwrap()),
                expected,
                "{profile:?}"
            );
        }
    }
}
