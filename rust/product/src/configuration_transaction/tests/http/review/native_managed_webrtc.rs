//! Authenticated WebRTC media on the isolated foreground station.
use super::*;
use rpt_advanced_core::config::resources::candidate::ResourceId;
use rptadv_peer_support::webrtc::{
    self as rtc, CHANNEL_PROTOCOL, ChannelConfig, ChannelKind, Direction, Event, MediaKind, Mid,
    Pt, Reliability, RtpWrite, SdpAnswer, SeqNo, Ssrc, UdpDriver,
    channel::{Control, Coordinate, Lease, Reference},
    opus::{DecodeSpan, Encoder, OpusDecoder, OpusProvider, PACKET_FRAMES, RECEIVE_FRAMES},
};
use std::{f64::consts::TAU, path::PathBuf};

const SUBJECT: &str = "webrtc-fixture-browser";
const TIMING_LIFETIME_MS: u16 = 100;

pub(in super::super) fn run(scenario: &Scenario<'_>, transceive: bool) {
    let (mut provision, address) = provisioning_with_browser(scenario, true);
    provision["webrtc_bind_address"] = json!("127.0.0.1");
    let document = scenario.client.pki.join("managed-webrtc.json");
    fs::write(&document, serde_json::to_vec(&provision).unwrap()).unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    let mut process = managed_asl::start(&document);
    managed_asl::wait_service(&client, address, &mut process);
    let port = create_disabled_port(&client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    put(
        &client,
        &format!("{port}interface/"),
        json!({"audio":{"device_identifier":"3-1"}}),
    );
    put(
        &client,
        &format!("{port}radio/"),
        json!({"duplex_mode":"half"}),
    );
    put(
        &client,
        &format!("{port}rx/signaling/"),
        json!({"squelch":{"source":"sound_chip_gpio","enabled":true}}),
    );
    if transceive {
        put(
            &client,
            &format!("{port}tx/signaling/"),
            json!({"ptt":{"timing":{"settle_ms":0,"hang_ms":0}}}),
        );
    }
    let enabled = edit(
        &client,
        "PUT",
        &port,
        json!({"configuration":{"name":"Radio","inherit_from":"site","enabled":true}}),
    );
    managed_asl::confirmed(
        &client,
        enabled.body["desired_revision"].as_str().unwrap(),
        station,
    );

    let status = port.replacen("/configuration/", "/status/", 1);
    let discovery = client.get(&status);
    assert_eq!(discovery.code, 200);
    let collection = href(&discovery.body, "rpt:webrtc-sessions");
    assert_eq!(client.get(collection).code, 403);
    let peers = format!("{status}peers/");
    let carrier = PathBuf::from(std::env::var_os("RPTADV_TEST_SIP_CARRIER_EDGE").unwrap());
    let ptt = PathBuf::from(std::env::var_os("RPTADV_TEST_SIP_PROGRESS_PTT").unwrap());
    assert!(!carrier.exists());
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = if transceive {
        executor.block_on(exercise_transceive(
            &client, collection, &peers, &carrier, &ptt,
        ))
    } else {
        executor.block_on(receive_only(
            &client, station, collection, &peers, &carrier, &ptt,
        ))
    };
    assert!(live_status::wait_for(&client, &peers, |doc| !webrtc_peer(
        doc
    )));
    assert_eq!(
        browser(&client, "GET", &session, None).body["phase"],
        "closed"
    );
    process.signal("-TERM");
    wait_stopped(&mut process);
    assert!(
        WorkingFiles::open(&scenario.fixture.0)
            .unwrap()
            .try_writer_lease()
            .is_ok()
    );
}

/// The published active revision can precede release of the cold-start writer.
fn create_disabled_port(client: &Client<'_>) -> String {
    let validator = client.etag(PORTS);
    let request = json!({"key":"radio","configuration":{
        "name":"Radio","inherit_from":"site","enabled":false
    }});
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = client.send(
            "POST",
            PORTS,
            Some(("If-Match", &validator)),
            Some(&request),
        );
        if reply.code == 201 {
            client.ready();
            return reply.headers["location"].clone();
        }
        assert!(
            reply.code == 409 && reply.body["error"] == "conflict" && Instant::now() < deadline,
            "initial managed port creation returned {} {}",
            reply.code,
            reply.body
        );
        assert_eq!(
            client.etag(PORTS),
            validator,
            "an unrelated edit changed the cold-start fixture collection"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn href<'a>(doc: &'a Value, relation: &str) -> &'a str {
    doc["_links"][relation]["href"]
        .as_str()
        .expect("discovered HAL relation")
}

fn browser(client: &Client<'_>, method: &str, path: &str, body: Option<&Value>) -> Reply {
    client.send_as(method, path, None, body, Some(SUBJECT))
}

fn webrtc_peer(document: &Value) -> bool {
    document["_embedded"]["item"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|peer| peer["type"] == "webrtc"))
}

struct Browser {
    driver: UdpDriver,
    media: Mid,
    payload: Pt,
    binding: String,
    session: String,
    close: String,
}

impl Browser {
    fn send(&mut self, kind: ChannelKind, bytes: &[u8]) {
        assert!(
            self.driver
                .write_channel(kind, false, bytes, 4096, 4096)
                .unwrap(),
            "fixture's bounded data channel must accept the message"
        );
    }

    async fn restart(&mut self, client: &Client<'_>) {
        let (offer, pending) = self.driver.restart_offer().unwrap();
        let requested = json!({
            "action":"restart","ice_generation":"1","offer":offer.to_sdp_string()
        });
        let restarted = browser(client, "PUT", &self.session, Some(&requested));
        assert_eq!(restarted.code, 200);
        assert_eq!(restarted.body["ice_generation"], "2");
        assert_eq!(restarted.body["phase"], "pending");
        self.close = href(&restarted.body, "rpt:close").to_owned();
        let candidates = href(&restarted.body, "rpt:webrtc-candidates");
        let stale = json!({"ice_generation":"1","sequence":0,"candidate":null});
        assert_eq!(browser(client, "POST", candidates, Some(&stale)).code, 409);

        let deadline = Instant::now() + Duration::from_secs(5);
        let (answer, remote) = loop {
            let reply = browser(client, "GET", &self.session, None);
            let ice = browser(client, "GET", candidates, None);
            assert_eq!((reply.code, ice.code), (200, 200));
            assert_eq!(reply.body["phase"], "pending");
            assert_eq!(reply.body["ice_generation"], "2");
            if let Some(sdp) = reply.body["answer"].as_str()
                && let Some(remote) = ice.body["candidates"][0]["candidate"].as_str()
            {
                break (SdpAnswer::from_sdp_string(sdp).unwrap(), remote.to_owned());
            }
            assert!(
                Instant::now() < deadline,
                "no answer for the new ICE generation"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        };
        self.driver.accept_restart_answer(pending, answer).unwrap();
        self.driver.add_remote_candidate(&remote).unwrap();
        for (sequence, candidate) in [Some(self.driver.candidate()), None]
            .into_iter()
            .enumerate()
        {
            let work = json!({"ice_generation":"2","sequence":sequence,"candidate":candidate});
            assert_eq!(browser(client, "POST", candidates, Some(&work)).code, 200);
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match self.driver.next_event().await.unwrap() {
                    Event::ChannelData(data) => {
                        if let Some(Control::Ready {
                            binding,
                            ice_generation,
                        }) = Control::decode(&data.data)
                        {
                            assert_eq!(binding, self.binding);
                            assert_eq!(ice_generation, Coordinate(2));
                            assert_eq!(
                                self.driver.bound_channel_kind(data.id),
                                Some(ChannelKind::Control)
                            );
                            assert!(self.driver.connection_state().secure_connected());
                            break;
                        }
                    }
                    Event::ChannelOpen(..)
                    | Event::ChannelClose(..)
                    | Event::Connected
                    | Event::Closed => {
                        panic!("restart replaced the existing secure media association");
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("same controller session acknowledged fresh ICE and station media");
    }

    fn close(self, client: &Client<'_>) -> String {
        let closed = browser(client, "DELETE", &self.close, None);
        assert_eq!(closed.code, 200);
        assert_eq!(closed.body["phase"], "closed");
        assert!(closed.body.get("answer").is_none());
        self.session
    }
}

async fn connect(client: &Client<'_>, collection: &str, direction: Direction) -> Browser {
    assert_eq!(browser(client, "GET", collection, None).code, 200);
    let preference = match direction {
        Direction::RecvOnly => "receive_only",
        Direction::SendRecv => "transceive",
        _ => unreachable!("supported test direction"),
    };

    let mut browser_rtc = rtc::transport(Instant::now());
    let (media, offer, pending) = {
        let mut change = browser_rtc.sdp_api();
        let media = change.add_media(MediaKind::Audio, direction, None, None, None);
        for (label, ordered, reliability) in [
            ("control", true, Reliability::Reliable),
            (
                "timing",
                false,
                Reliability::MaxPacketLifetime {
                    lifetime: TIMING_LIFETIME_MS,
                },
            ),
        ] {
            change.add_channel_with_config(ChannelConfig {
                label: label.into(),
                ordered,
                reliability,
                negotiated: None,
                protocol: CHANNEL_PROTOCOL.into(),
            });
        }
        let (offer, pending) = change.apply().unwrap();
        (media, offer, pending)
    };
    let created = browser(
        client,
        "POST",
        collection,
        Some(&json!({
            "direction":preference,"offer":offer.to_sdp_string()
        })),
    );
    assert_eq!(created.code, 201, "offer rejected");
    assert_eq!(created.headers["cache-control"], "no-store");
    let session = href(&created.body, "self").to_owned();
    assert_eq!(created.headers["location"], session);
    let candidates = href(&created.body, "rpt:webrtc-candidates");
    let close = href(&created.body, "rpt:close").to_owned();
    let found = client.send_as("GET", &session, None, None, Some("other-browser"));
    assert_eq!(found.code, 403);

    let deadline = Instant::now() + Duration::from_secs(5);
    let (answer, remote_candidate) = loop {
        let reply = browser(client, "GET", &session, None);
        let remote = browser(client, "GET", candidates, None);
        assert_eq!(reply.code, 200);
        assert_eq!(remote.code, 200);
        assert_eq!(reply.body["phase"], "pending");
        if let Some(sdp) = reply.body["answer"].as_str()
            && let Some(candidate) = remote.body["candidates"][0]["candidate"].as_str()
            && remote.body["candidates"][1]["sequence"] == 1
        {
            assert!(remote.body["candidates"][1]["candidate"].is_null());
            break (
                SdpAnswer::from_sdp_string(sdp).unwrap(),
                candidate.to_owned(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "no pending answer and complete local trickle ICE"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    browser_rtc
        .sdp_api()
        .accept_answer(pending, answer)
        .unwrap();
    // The offerer added this m-line locally, so it does not receive MediaAdded.
    let negotiated = browser_rtc
        .media(media)
        .is_some_and(|track| track.kind() == MediaKind::Audio && track.direction() == direction);
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut driver = UdpDriver::new(browser_rtc, socket).unwrap();
    driver.add_remote_candidate(&remote_candidate).unwrap();
    for (sequence, candidate) in [Some(driver.candidate()), None].into_iter().enumerate() {
        let reply = browser(
            client,
            "POST",
            candidates,
            Some(&json!({
                "ice_generation":"1","sequence":sequence,"candidate":candidate
            })),
        );
        assert_eq!(reply.code, 200, "remote trickle ICE refused");
    }

    let payload = driver.opus_payload(media);
    let mut channels = [false; 2];
    let mut binding = None;
    // The Product actor grants up to twenty seconds for initial setup.
    let setup = tokio::time::timeout(Duration::from_secs(25), async {
        while !(negotiated
            && payload.is_some()
            && driver.connection_state().secure_connected()
            && channels.into_iter().all(|open| open)
            && binding.is_some())
        {
            match driver.next_event().await.map_err(|_| "driver error")? {
                Event::ChannelOpen(id, _) => match driver.bind_channel(id, TIMING_LIFETIME_MS) {
                    Some(ChannelKind::Control) => channels[0] = true,
                    Some(ChannelKind::Timing) => channels[1] = true,
                    None => return Err("unexpected DCEP channel"),
                },
                Event::ChannelData(message) if !message.binary => {
                    if let Some(Control::Ready {
                        binding: actual,
                        ice_generation,
                    }) = Control::decode(&message.data)
                    {
                        let valid_binding = actual.parse::<ResourceId>().is_ok()
                            && session.trim_end_matches('/').rsplit('/').next()
                                != Some(actual.as_str());
                        if driver.bound_channel_kind(message.id) != Some(ChannelKind::Control)
                            || ice_generation != Coordinate(1)
                            || !valid_binding
                        {
                            return Err("invalid Ready");
                        }
                        binding = Some(actual);
                    }
                }
                Event::Closed => return Err("browser transport closed"),
                _ => {}
            }
        }
        Ok(())
    })
    .await;
    let failure = match setup {
        Ok(Ok(())) => None,
        Ok(Err(reason)) => Some(reason),
        Err(_) => Some("timeout"),
    };
    if let Some(reason) = failure {
        let transport = driver.connection_state();
        let reply = browser(client, "GET", &session, None);
        let phase = match reply.body["phase"].as_str() {
            Some(phase @ ("pending" | "active" | "closed")) => phase,
            _ => "unavailable",
        };
        panic!(
            "WebRTC setup {reason}; ice={:?}, dtls={}, negotiated={}, opus={}, control={}, timing={}, ready={}, hal_http={}, hal_phase={phase}",
            transport.ice,
            transport.dtls_authenticated,
            negotiated,
            payload.is_some(),
            channels[0],
            channels[1],
            binding.is_some(),
            reply.code
        );
    }
    let payload = payload.unwrap();
    Browser {
        driver,
        media,
        payload,
        binding: binding.unwrap(),
        session,
        close,
    }
}

async fn receive_only(
    client: &Client<'_>,
    station: &str,
    collection: &str,
    peers: &str,
    carrier: &Path,
    ptt: &Path,
) -> String {
    let idle_trace = fs::read(ptt).unwrap();
    assert_eq!(idle_trace.last(), Some(&b'0'));
    let mut browser_peer = connect(client, collection, Direction::RecvOnly).await;
    let driver = &mut browser_peer.driver;
    assert!(live_status::wait_for(client, peers, webrtc_peer));
    let attached = Instant::now() + Duration::from_secs(3);
    while browser(client, "GET", &browser_peer.session, None).body["phase"] != "active" {
        assert!(
            Instant::now() < attached,
            "station did not acknowledge the WebRTC attachment"
        );
        let _ = tokio::time::timeout(Duration::from_millis(20), driver.next_event()).await;
    }

    let want_talk = Control::RequestTalk {
        ice_generation: Coordinate(1),
    }
    .encode()
    .unwrap();
    assert!(
        driver
            .write_channel(ChannelKind::Control, false, &want_talk, 1024, 2048)
            .unwrap()
    );
    let idle = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(event) = tokio::time::timeout_at(idle, driver.next_event()).await {
        match event.unwrap() {
            Event::ChannelData(data) => {
                assert!(!matches!(
                    Control::decode(&data.data),
                    Some(Control::TalkGranted { .. })
                ));
            }
            Event::Closed => panic!("receive-only session closed on a talk request"),
            _ => {}
        }
    }
    assert_eq!(fs::read(ptt).unwrap(), idle_trace);
    fs::write(carrier, b"").unwrap();
    let opus = OpusProvider::load().unwrap();
    let mut decode = opus.decoder(opus.encoder_bytes()).unwrap();
    let opus_pt = browser_peer.payload;
    let mut pcm = [0.0; RECEIVE_FRAMES];
    let mut spans = [DecodeSpan::default(); 120];
    let snapshot = format!("/api/v1/status/snapshot/?port={station}&topics=meters");
    let meter = format!(
        "link:webrtc:{}:egress:opus_encoder_input:mono",
        browser_peer.binding
    );
    let mut anchor: Option<Reference> = None;
    let mut rtp_end = None;
    let mut heard_marker = false;
    let mut mapped_end = None;
    let mut next_snapshot = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if mapped_end.is_none() && anchor.is_some() && Instant::now() >= next_snapshot {
            next_snapshot = Instant::now() + Duration::from_millis(60);
            let reply = client.get(&snapshot);
            assert_eq!(reply.code, 200);
            let readings = &reply.body["status"]["ports"][station]["meters"];
            let rf = &readings["local_rx:station_input:mono"];
            let correlation = &readings[&meter]["media_correlation"];
            // The exact anchor may advance ahead of status or vice versa.
            if rf["quality"] == "valid"
                && rf["peak_dbfs"].is_number()
                && rf["rms_dbfs"].is_number()
                && rf["lufs"].is_number()
                && correlation["quality"] == "valid"
                && let (Some(current), Ok(measured)) = (
                    &anchor,
                    serde_json::from_value::<Reference>(correlation["anchor"].clone()),
                )
                && *current == measured
            {
                let window = &correlation["window"];
                let coordinate =
                    |field: &str| window[field].as_str().unwrap().parse::<u64>().unwrap();
                let first = coordinate("first_sample");
                let end = coordinate("end_sample");
                let first_rtp = coordinate("first_rtp_timestamp");
                let end_rtp = coordinate("end_rtp_timestamp");
                assert!(end > first);
                assert_eq!(
                    measured.positions().rtp_window(
                        measured.source_generation.0,
                        measured.epoch.0,
                        first..end,
                    ),
                    Some(first_rtp..end_rtp),
                );
                mapped_end = Some(end_rtp);
            }
        }
        if heard_marker
            && mapped_end
                .zip(rtp_end)
                .is_some_and(|(meter, rtp)| meter <= rtp)
        {
            break;
        }
        let Ok(event) = tokio::time::timeout(Duration::from_millis(20), driver.next_event()).await
        else {
            continue;
        };
        match event.unwrap() {
            Event::ChannelData(data) => {
                if let Some(Control::Anchor { reference }) = Control::decode(&data.data) {
                    assert_eq!(reference.binding, browser_peer.binding);
                    if anchor.as_ref() != Some(&reference) {
                        mapped_end = None;
                        rtp_end = None;
                        anchor = Some(reference);
                    }
                }
            }
            Event::RtpPacket(packet) => {
                assert_eq!(packet.header.payload_type, opus_pt);
                let result = decode
                    .decode(&packet.payload, &mut pcm, &mut spans)
                    .unwrap();
                assert_eq!(result.frames, PACKET_FRAMES as usize);
                if anchor
                    .as_ref()
                    .is_some_and(|a| a.ssrc == *packet.header.ssrc)
                {
                    let end = packet.time.numer() + u64::from(PACKET_FRAMES);
                    rtp_end = Some(rtp_end.map_or(end, |latest: u64| latest.max(end)));
                }
                heard_marker |= marker(&pcm[..result.frames], 1000.0, 48_000.0);
            }
            Event::Closed => panic!("media ended before the RF marker and meter"),
            _ => {}
        }
    }
    assert!(
        heard_marker,
        "simulated 1 kHz RF reached the browser over Opus RTP"
    );
    assert!(
        mapped_end
            .zip(rtp_end)
            .is_some_and(|(meter, rtp)| meter <= rtp),
        "receiver RTP and authenticated HAL meter did not share the in-band anchor",
    );

    let status_root_url = href(&client.get("/api/v1/").body, "rpt:status").to_owned();
    let status_root = client.get(&status_root_url);
    let stream = href(&status_root.body, "rpt:status-stream");
    let chain = format!("link:webrtc:{}:egress", browser_peer.binding);
    let meter = first_status_stream_frame(
        client,
        &format!("{stream}?port={station}&topics=meters"),
        Some(&chain),
    );
    let correlation = &meter["media_correlation"];
    assert_eq!(correlation["binding"], browser_peer.binding);
    assert_eq!(correlation["track"], "audio");
    let previous = anchor.unwrap();
    assert_eq!(
        serde_json::from_value::<Reference>(correlation["anchor"].clone()).unwrap(),
        previous,
    );

    fs::remove_file(carrier).unwrap();
    browser_peer.restart(client).await;
    fs::write(carrier, b"").unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut anchored = false;
        loop {
            match browser_peer.driver.next_event().await.unwrap() {
                Event::ChannelData(data) => {
                    if let Some(Control::Anchor { reference }) = Control::decode(&data.data) {
                        assert_eq!(reference.binding, previous.binding);
                        assert!(reference.epoch.0 > previous.epoch.0);
                        anchored = true;
                    }
                }
                Event::RtpPacket(packet) if anchored => {
                    let result = decode
                        .decode(packet.payload.as_ref(), &mut pcm, &mut spans)
                        .unwrap();
                    if marker(&pcm[..result.frames], 1000.0, 48_000.0) {
                        break;
                    }
                }
                Event::Closed => panic!("restarted controller session lost media"),
                _ => {}
            }
        }
    })
    .await
    .expect("restarted RTC and station endpoint delivered the receiver marker");
    fs::remove_file(carrier).unwrap();
    browser_peer.close(client)
}

const TONE_HZ: f64 = 700.0;
const OPUS_RATE: f64 = 48_000.0;
const RTP_START: u32 = 48_000;
const PACKET_INTERVAL: Duration = Duration::from_millis(20);

struct TalkMedia {
    encoder: Encoder,
    encoded: Vec<u8>,
    pcm: [f32; PACKET_FRAMES as usize],
    position: u64,
}

impl TalkMedia {
    fn new(opus: &OpusProvider) -> Self {
        Self {
            encoder: opus.encoder().unwrap(),
            encoded: vec![0; opus.encoder_bytes()],
            pcm: std::array::from_fn(|i| {
                (TAU * TONE_HZ * i as f64 / OPUS_RATE).sin() as f32 * 0.25
            }),
            position: 0,
        }
    }

    fn send(&mut self, peer: &mut Browser) -> Ssrc {
        let result = self
            .encoder
            .encode(&self.pcm, &mut self.encoded, false)
            .unwrap();
        assert_eq!(result.consumed_frames, self.pcm.len());
        assert_eq!(result.packet_frames, PACKET_FRAMES);
        assert_ne!(result.encoded_bytes, 0);
        let frame = RtpWrite::new(
            peer.payload,
            SeqNo::from(103 + self.position / u64::from(PACKET_FRAMES)),
            RTP_START + u32::try_from(self.position).unwrap(),
            Instant::now(),
            self.encoded[..result.encoded_bytes].to_vec(),
        );
        self.position += u64::from(PACKET_FRAMES);
        peer.driver.write_rtp(peer.media, frame).unwrap()
    }
}

struct ListenerAudio {
    decoder: OpusDecoder,
    pcm: [f32; RECEIVE_FRAMES],
    origins: [DecodeSpan; 48],
}

impl ListenerAudio {
    fn new(opus: &OpusProvider) -> Self {
        let decoder = opus.decoder(opus.encoder_bytes()).unwrap();
        assert!(
            decoder.supports_provenance(),
            "stage the qualified Opus provider"
        );
        Self {
            decoder,
            pcm: [0.0; RECEIVE_FRAMES],
            origins: [DecodeSpan::default(); 48],
        }
    }

    fn heard(&mut self, event: Event, frequency: f64) -> bool {
        let Event::RtpPacket(packet) = event else {
            return false;
        };
        let decoded = self
            .decoder
            .decode(&packet.payload, &mut self.pcm, &mut self.origins)
            .unwrap();
        marker(&self.pcm[..decoded.frames], frequency, OPUS_RATE)
    }
}

/// Drive both active RTC owners on their bounded deadlines while sending simulated media.
async fn service(
    talker: &mut Browser,
    listener: &mut Browser,
    wait: Duration,
    mut on_talker: impl FnMut(Event) -> bool,
    mut on_listener: impl FnMut(Event) -> bool,
) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = talker.driver.next_event() => {
                let event = event.unwrap();
                assert!(!matches!(event, Event::Closed), "talker closed during the fixture");
                if on_talker(event) {
                    break;
                }
            }
            event = listener.driver.next_event() => {
                let event = event.unwrap();
                assert!(!matches!(event, Event::Closed), "independent listener closed");
                if on_listener(event) {
                    break;
                }
            }
        }
    }
}

async fn exercise_transceive(
    client: &Client<'_>,
    collection: &str,
    peers: &str,
    carrier: &Path,
    ptt: &Path,
) -> String {
    let mut listener = connect(client, collection, Direction::RecvOnly).await;
    let mut talker = connect(client, collection, Direction::SendRecv).await;
    assert_ne!(talker.binding, listener.binding);
    let observed_key = |binding: &str| {
        let reply = client.get(peers);
        assert_eq!(reply.code, 200);
        reply.body["_embedded"]["item"]
            .as_array()
            .unwrap()
            .iter()
            .find(|peer| peer["media_binding"] == binding)
            .and_then(|peer| peer["transmit_keyed"].as_bool())
    };
    let opus = OpusProvider::load().unwrap();
    let mut voice = TalkMedia::new(&opus);
    let mut listen_pcm = ListenerAudio::new(&opus);
    let mut self_pcm = ListenerAudio::new(&opus);
    let dac = PathBuf::from(std::env::var_os("RPTADV_TEST_SIP_PROGRESS_PCM").unwrap());
    let idle_trace = fs::read(ptt).unwrap();
    let start_pcm = fs::metadata(&dac).unwrap().len() as usize;
    assert_eq!(idle_trace.last(), Some(&b'0'));

    let ssrc = voice.send(&mut talker);
    talker.send(
        ChannelKind::Control,
        &Control::RequestTalk {
            ice_generation: Coordinate(1),
        }
        .encode()
        .unwrap(),
    );
    let mut granted = None;
    service(
        &mut talker,
        &mut listener,
        Duration::from_secs(3),
        |event| {
            if let Event::ChannelData(data) = event
                && let Some(Control::TalkGranted { grant, epoch }) = Control::decode(&data.data)
            {
                granted = Some((grant, epoch));
                return true;
            }
            false
        },
        |_| false,
    )
    .await;
    let (grant, epoch) = granted.expect("current station transport grants transceive");
    talker.send(
        ChannelKind::Control,
        &Control::Anchor {
            reference: Reference {
                binding: talker.binding.clone(),
                ssrc: *ssrc,
                epoch,
                source_generation: Coordinate(1),
                source_sample: Coordinate(0),
                rtp_timestamp: Coordinate(u64::from(RTP_START)),
                time_basis: None,
                time_ns: None,
                uncertainty_ns: Some(Coordinate(0)),
            },
        }
        .encode()
        .unwrap(),
    );
    // In-band control and timing deliberately have different SCTP delivery
    // order guarantees. Drive the reliable anchor to the receiver first.
    service(
        &mut talker,
        &mut listener,
        Duration::from_millis(40),
        |_| false,
        |_| false,
    )
    .await;
    for _ in 0..5 {
        assert_eq!(voice.send(&mut talker), ssrc);
        service(
            &mut talker,
            &mut listener,
            PACKET_INTERVAL,
            |_| false,
            |event| {
                assert!(
                    !listen_pcm.heard(event, TONE_HZ),
                    "media without a lease reached the listener"
                );
                false
            },
        )
        .await;
        assert_eq!(
            fs::read(ptt).unwrap(),
            idle_trace,
            "a grant and audio do not directly key RF"
        );
    }

    let mut lease = Lease {
        grant,
        epoch,
        sequence: Coordinate(0),
        start: Coordinate(voice.position),
        end: Coordinate(voice.position + OPUS_RATE as u64),
    };
    let mut heard_monitor = false;
    for frame in 0..24 {
        if frame % 2 == 0 {
            lease.sequence.0 += 1;
            talker.send(ChannelKind::Timing, &lease.encode().unwrap());
            // A separate short drive permits the grant's timing renewal to
            // reach the receiver before the following RTP frame.
            service(
                &mut talker,
                &mut listener,
                Duration::from_millis(3),
                |_| false,
                |event| {
                    heard_monitor |= listen_pcm.heard(event, TONE_HZ);
                    false
                },
            )
            .await;
        }
        let _ = voice.send(&mut talker);
        service(
            &mut talker,
            &mut listener,
            PACKET_INTERVAL,
            |event| {
                assert!(
                    !self_pcm.heard(event, TONE_HZ),
                    "the talker must receive a mix-minus"
                );
                false
            },
            |event| {
                heard_monitor |= listen_pcm.heard(event, TONE_HZ);
                false
            },
        )
        .await;
    }
    assert!(
        heard_monitor,
        "an independently attached browser heard real permitted talker RTP"
    );
    assert_eq!(observed_key(&talker.binding), Some(true));
    assert_eq!(observed_key(&listener.binding), None);
    let active = fs::read(ptt).unwrap();
    assert!(
        active[idle_trace.len()..].contains(&b'1'),
        "leased media never keyed simulated RF"
    );
    let transmitted = fs::read(dac).unwrap();
    let samples = transmitted[start_pcm..]
        .chunks_exact(4)
        .map(|word| f32::from_ne_bytes(word.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert!(
        samples
            .chunks_exact(160)
            .any(|block| marker(block, TONE_HZ, 8000.0)),
        "actual decoded Opus tone reached the simulated RF DAC"
    );

    // Keep sending valid RTP within the sample bounds. Lose key-off and
    // provide only the last duplicate renewal until the station expires it.
    for _ in 0..14 {
        talker.send(ChannelKind::Timing, &lease.encode().unwrap());
        let _ = voice.send(&mut talker);
        service(
            &mut talker,
            &mut listener,
            PACKET_INTERVAL,
            |_| false,
            |_| false,
        )
        .await;
    }
    let expired = fs::read(ptt).unwrap();
    assert_eq!(
        expired.last(),
        Some(&b'0'),
        "local lease expiry must release PTT"
    );
    assert_eq!(expired.get(idle_trace.len()..), Some(&b"10"[..]));
    assert_eq!(observed_key(&talker.binding), Some(false));

    // A later sequence for a retired grant still cannot revive it.
    for _ in 0..7 {
        lease.sequence.0 += 1;
        talker.send(ChannelKind::Timing, &lease.encode().unwrap());
        let _ = voice.send(&mut talker);
        service(
            &mut talker,
            &mut listener,
            PACKET_INTERVAL,
            |_| false,
            |event| {
                assert!(
                    !listen_pcm.heard(event, TONE_HZ),
                    "a retired grant leaked to the monitor"
                );
                false
            },
        )
        .await;
    }
    assert_eq!(fs::read(ptt).unwrap(), expired);
    let session = talker.close(client);

    fs::write(carrier, b"").unwrap();
    let mut heard_rf = false;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !heard_rf {
            let event = listener.driver.next_event().await.unwrap();
            assert!(!matches!(event, Event::Closed));
            heard_rf = listen_pcm.heard(event, 1000.0);
        }
    })
    .await
    .expect("the independent receiver still hears RF after the talker closes");
    fs::remove_file(carrier).unwrap();
    let _ = listener.close(client);
    session
}

/// Detect a phase-independent test tone in received Opus or simulated DAC PCM.
fn marker(pcm: &[f32], frequency: f64, rate: f64) -> bool {
    let (sin, cos, energy) =
        pcm.iter()
            .enumerate()
            .fold((0.0, 0.0, 0.0), |sum, (index, sample)| {
                let (sin, cos) = (TAU * index as f64 * frequency / rate).sin_cos();
                let sample = f64::from(*sample);
                (
                    sum.0 + sample * sin,
                    sum.1 + sample * cos,
                    sum.2 + sample * sample,
                )
            });
    let n = pcm.len() as f64;
    energy / n > 0.00001 && 2.0 * (sin * sin + cos * cos) > 0.6 * energy * n
}
