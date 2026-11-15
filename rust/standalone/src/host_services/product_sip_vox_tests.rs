//! Actual SIP caller -> authenticated controller -> an existing or phone-origin IAX remote.
use super::*;

#[derive(Default)]
pub(super) struct Observation {
    accepted: AtomicBool,
    voiced: AtomicUsize,
    return_requested: AtomicBool,
}
pub(super) struct Remote {
    pub address: SocketAddr,
    pub server: IaxServer,
    pub observation: Arc<Observation>,
    client: IaxClient,
    pending: AtomicUsize,
    peer: Option<IaxPeer>,
    return_packets: usize,
    next_return: Instant,
}
unsafe extern "C" fn allow(
    _: *mut c_void,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
) -> i32 {
    0
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    peer: *mut c_void,
) -> i32 {
    let pending = unsafe { &*context.cast::<AtomicUsize>() };
    if peer.is_null()
        || pending
            .compare_exchange(0, peer as usize, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return -1;
    }
    0
}
impl Remote {
    pub fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        // Full remote catalog exercises the actual ordinary or phone-origin factory.
        let client = IaxClient::load(Path::new("librptadviax2.so.2")).unwrap();
        let server = IaxServer::bind(&client, address, &["2000".into()]).unwrap();
        Self {
            address,
            server,
            client,
            observation: Arc::default(),
            pending: AtomicUsize::new(0),
            peer: None,
            return_packets: 0,
            next_return: Instant::now(),
        }
    }
    pub fn clear_voice_observation(&self) {
        assert!(self.observation.accepted.load(Ordering::Acquire));
        self.observation.voiced.store(0, Ordering::Release);
    }
    pub fn voiced_samples(&self) -> usize {
        self.observation.voiced.load(Ordering::Acquire)
    }
    pub fn request_return(&mut self) {
        assert!(self.observation.accepted.load(Ordering::Acquire));
        self.return_packets = 0;
        self.next_return = Instant::now();
        self.observation
            .return_requested
            .store(true, Ordering::Release);
    }
    pub fn sent_return_packets(&self) -> usize {
        self.return_packets
    }
    pub fn pump(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;
        unsafe {
            self.server
                .poll(
                    now,
                    Some(allow),
                    Some(accept),
                    ptr::from_ref(&self.pending).cast_mut().cast(),
                )
                .unwrap();
        }
        let raw = self.pending.swap(0, Ordering::AcqRel);
        if raw != 0 {
            assert!(self.peer.is_none(), "one ordinary remote call");
            self.peer =
                Some(unsafe { self.client.clone().adopt_inbound(raw as *mut c_void) }.unwrap());
            assert_eq!(self.peer.as_ref().unwrap().sample_rate_hz(), 8000);
            self.observation.accepted.store(true, Ordering::Release);
        }
        if let Some(peer) = self.peer.as_mut() {
            if self.observation.return_requested.load(Ordering::Acquire)
                && self.return_packets < 15
                && Instant::now() >= self.next_return
            {
                let pcm: [f32; 160] = std::array::from_fn(|index| {
                    0.2 * (index as f32 * std::f32::consts::TAU / 8.0).sin()
                });
                let mut packet = [0; 320];
                let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
                result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
                assert_eq!(peer.prepare_audio(&pcm, &mut packet, 0, &mut result), 0);
                assert_eq!((result.consumed_frames, result.packet_frames), (160, 160));
                assert!(
                    peer.send_prepared(
                        &packet[..result.encoded_bytes],
                        result.packet_frames,
                        peer.elapsed_ms()
                    )
                    .unwrap()
                );
                self.return_packets += 1;
                self.next_return = Instant::now() + Duration::from_millis(20);
            }
            let mut audio = [0.0; 4096];
            let mut text = [0; 4096];
            for _ in 0..8 {
                match peer.poll(&mut audio, &mut text).unwrap() {
                    IaxEvent::Audio(count) => {
                        self.observation.voiced.fetch_add(
                            audio[..count]
                                .iter()
                                .filter(|sample| sample.abs() > 0.01)
                                .count(),
                            Ordering::Release,
                        );
                    }
                    IaxEvent::None => break,
                    IaxEvent::Text(_) => {}
                    event => panic!("unexpected remote event {event:?}"),
                }
            }
        }
    }
}
fn media(caller: &mut Caller, codec: &mut AudioCodec, samples: &[f32]) {
    for chunk in samples.chunks(160) {
        let mut pcm = [0.0; 160];
        pcm[..chunk.len()].copy_from_slice(chunk);
        caller.audio(codec, &pcm);
        caller.service();
        thread::sleep(Duration::from_millis(20));
    }
}
pub(super) fn call(sip: UdpSocket, observation: Arc<Observation>, existing: bool) {
    let mut caller = Caller::establish(sip, None);
    caller.keepalive = true;
    let mut codec = AudioCodec::load(AudioProfile::Mulaw).unwrap();
    caller.prompt(&mut codec, "mailbox");
    // Reuse the licensed source fixture and the existing source-rate test sampling.
    let speech: Vec<f32> =
        include_bytes!("../../../product/src/tests/fixtures/phone-speech/yes-48000-s16le.pcm")
            .chunks_exact(12)
            .map(|bytes| f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0)
            .collect();
    media(&mut caller, &mut codec, &speech);
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "unauthenticated voice is private"
    );
    assert_eq!(observation.accepted.load(Ordering::Acquire), existing);
    caller.digits(b"100#");
    caller.prompt(&mut codec, "passcode");
    caller.digits(b"1234#");
    caller.prompt(&mut codec, "authenticated");
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "mailbox, passcode and authentication prompts remain private"
    );
    if !existing {
        caller.digits(b"*4#*32000**#");
        let completed = caller.prompt(&mut codec, "remote connect");
        assert_prompt(
            &completed,
            "-.-. --- -- -- .- -. -../-.-. --- -- .--. .-.. . - . -..",
        );
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            0,
            "phone connect result remains private"
        );
    }
    assert!(observation.accepted.load(Ordering::Acquire));
    media(&mut caller, &mut codec, &speech);
    assert_eq!(
        observation.voiced.load(Ordering::Acquire),
        0,
        "dial alone does not attach phone audio"
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "dial alone does not attach phone audio to local RF"
    );
    caller.digits(if existing { b"*3#" } else { b"#*3#" });
    let completed = caller.prompt(&mut codec, "local attachment");
    assert_prompt(
        &completed,
        "-.-. --- -- -- .- -. -../-.-. --- -- .--. .-.. . - . -..",
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "phone attachment result remains private"
    );
    media(&mut caller, &mut codec, &[0.0; 4000]);
    assert_eq!(
        observation.voiced.load(Ordering::Acquire),
        0,
        "silence does not open VOX"
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        0,
        "silence after the private result does not open radio VOX"
    );
    media(&mut caller, &mut codec, &speech);
    media(&mut caller, &mut codec, &[0.0; 16000]);
    assert!(
        observation.voiced.load(Ordering::Acquire) > 160,
        "real remote must hear qualified speech"
    );
    assert!(
        PUBLIC_OUTPUT.load(Ordering::Acquire) > 0,
        "existing local mix receives qualified voice without private command results"
    );
    assert!(
        !LAST_PTT.load(Ordering::Acquire),
        "silence closes voice/PTT after existing hang"
    );
    // Every queued return packet since the completed attachment prompt must be
    // silent: the destination's own phone source is excluded by mix-minus.
    let mut wire = [0; 2048];
    while let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
        assert_eq!(source, caller.media);
        assert_eq!(length, 172);
        let mut pcm = [0.0; 160];
        codec
            .decode(wire[12..172].try_into().unwrap(), &mut pcm)
            .unwrap();
        assert!(
            pcm.iter().all(|sample| sample.abs() <= 0.01),
            "the phone must not hear its own transmitted voice"
        );
    }
    let remote_before = observation.voiced.load(Ordering::Acquire);
    let radio_before = PUBLIC_OUTPUT.load(Ordering::Acquire);
    caller.digits(b"*4#*70");
    let status = caller.prompt(&mut codec, "attached private status");
    assert!(status.iter().any(|sample| sample.abs() > 0.01));
    caller.digits(b"#");
    media(&mut caller, &mut codec, &[0.0; 4000]);
    assert_eq!(
        observation.voiced.load(Ordering::Acquire),
        remote_before,
        "attached DTMF and the private reply must not reach the remote"
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        radio_before,
        "attached DTMF and the private reply must not reach radio output"
    );
    observation.return_requested.store(true, Ordering::Release);
    let returned = caller.prompt(&mut codec, "remote programme");
    assert!(
        returned.iter().filter(|sample| sample.abs() > 0.01).count() > 160,
        "the same private owner must return actual remote audio to SIP"
    );
    let energy: f32 = returned.iter().map(|value| value * value).sum();
    let correlation = |lag: usize| -> f32 {
        returned
            .iter()
            .zip(returned.iter().skip(lag))
            .map(|(left, right)| left * right)
            .sum()
    };
    assert!(
        correlation(8) > energy * 0.5 && correlation(4) < -energy * 0.5,
        "returned programme must retain the remote's known 1-kHz pattern"
    );
    caller.request("BYE", "");
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let mut packet = [0; 8192];
        if let Ok((count, source)) = caller.sip.recv_from(&mut packet) {
            let (first, values) = fields(&packet[..count]);
            if first.starts_with("SIP/2.0 200 ") && values["cseq"] == "2 BYE" {
                break;
            }
            if first.starts_with("REGISTER ") {
                caller.answer_register(&values, source);
            }
        }
        assert!(Instant::now() < until, "voice call BYE must complete");
        thread::sleep(Duration::from_millis(2));
    }
}
