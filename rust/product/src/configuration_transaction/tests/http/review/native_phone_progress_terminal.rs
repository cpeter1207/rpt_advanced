//! Terminal signaling, exact failure speech and a subsequent intentional cancellation.
use super::*;

pub(in super::super) struct Observation {
    busy_ack: bool,
    busy_text: bool,
    audible: bool,
    cancel: bool,
    final_ack: bool,
    cancel_quiet: bool,
    early_pcm: bool,
    receive_only: bool,
    answered: bool,
    fresh_pcm: bool,
    uplink: bool,
    retired: bool,
}

impl Observation {
    pub(in super::super) fn verify(self) {
        eprintln!(
            "PHONE_TERMINAL_CLOSED busy_ack={} busy_text={} local_busy_pcm={} cancel={} final_ack={} cancel_quiet={} withdrew=true joined=true writer=true",
            self.busy_ack,
            self.busy_text,
            self.audible,
            self.cancel,
            self.final_ack,
            self.cancel_quiet
        );
        eprintln!(
            "PHONE_PROGRESS_ANSWER_CLOSED early_pcm={} receive_only={} answered={} fresh_pcm={} uplink={} retired={}",
            self.early_pcm,
            self.receive_only,
            self.answered,
            self.fresh_pcm,
            self.uplink,
            self.retired
        );
        assert!(
            self.early_pcm
                && self.receive_only
                && self.answered
                && self.fresh_pcm
                && self.uplink
                && self.retired,
            "the same provisional dialog must transfer to fresh established bidirectional media"
        );
        assert!(
            self.busy_ack && self.busy_text && self.audible,
            "486 must speak the Fluent busy text on the originating DAC"
        );
        assert!(
            self.cancel && self.final_ack && self.cancel_quiet,
            "intentional hangup must cancel the exact pending call quietly"
        );
    }
}

pub(in super::super) fn voice(root: &Path) -> PathBuf {
    let voice = root.join("progress-80.onnx");
    fs::write(&voice, b"synthetic voice\n").unwrap();
    fs::write(
        root.join("progress-80.onnx.json"),
        br#"{"audio":{"sample_rate":8000}}"#,
    )
    .unwrap();
    fs::write(voice.with_extension("requests"), b"").unwrap();
    voice
}

fn response(socket: &UdpSocket, call: &Invite, status: u16) {
    let (head, _) = call
        .answer
        .split_once("Content-Type: application/sdp")
        .unwrap();
    let head = head.replacen("SIP/2.0 200 ", &format!("SIP/2.0 {status} "), 1);
    socket
        .send_to(
            format!("{head}Content-Length: 0\r\n\r\n").as_bytes(),
            call.source,
        )
        .unwrap();
}

fn wire(socket: &UdpSocket, call: &Invite, method: &str) -> Option<String> {
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut bytes = [0; 8192];
    let (msg, from) = registrar::receive_with_keepalives(socket, &mut bytes, |from| {
        assert_eq!(from, call.source)
    })
    .ok()?;
    let from_tag = registrar::header(&msg, "From");
    let to = registrar::header(&msg, "To");
    let expected = if method == "CANCEL" {
        call.to.split(";tag=").next()?
    } else {
        call.to.as_str()
    };
    (from == call.source
        && msg.starts_with(&format!("{method} "))
        && registrar::header(&msg, "Call-ID") == call.call_id
        && from_tag == call.from
        && to == expected
        && registrar::header(&msg, "CSeq") == format!("{} {method}", call.sequence))
    .then_some(msg)
}

fn marked(pcm: &[f32], hz: f64) -> bool {
    let mut count = 0;
    for block in pcm.chunks_exact(800) {
        let power: f64 = block.iter().map(|v| f64::from(*v).powi(2)).sum();
        let tone = power > 0.01
            && power_share(block, hz, power) > 0.72
            && power_share(block, 500.0, power) < 0.04;
        count = if tone { count + 1 } else { 0 };
        if count == 3 {
            return true;
        }
    }
    false
}

pub(in super::super) fn exercise(
    (client, station, active): (&Client<'_>, &str, &Value),
    terminator: &str,
    socket: &UdpSocket,
    invite: &mut Option<Invite>,
    voice: &Path,
    probes: Probes,
) -> Observation {
    let command = |digits: String| {
        let reply = operation(
            client,
            json!({"submitted":true,"targets":[station],
            "expected_active_revision":active,"action":{"action":"execute_dtmf","digits":digits}}),
        );
        let outcome = &reply.body["receipt"]["outcomes"][0];
        assert!(
            reply.code == 200 && outcome["outcome"] == "admitted" && outcome["target"] == station
        );
    };
    let dial = || command(format!("{DIAL_CODE}{NUMBER}{terminator}"));
    let first = invite.as_ref().unwrap();
    let old_id = first.call_id.clone();
    let position = || fs::metadata(&probes.dac).unwrap().len();
    let beginning = position();
    let requests = voice.with_extension("requests");
    let prior = fs::read(&requests).unwrap();
    response(socket, first, 486);
    let busy_ack = wire(socket, first, "ACK").is_some();
    let deadline = Instant::now() + Duration::from_secs(5);
    let audible = loop {
        if marked(&channel(&probes.dac, beginning, position()), 1_000.0) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let spoken = fs::read(&requests).unwrap();
    let busy_text = spoken.strip_prefix(prior.as_slice()) == Some(b"busy\n".as_slice());

    let quiet = Probes::ready();
    let edges = fs::read(&quiet.ptt).unwrap();
    let start = position();
    dial();
    let media = UdpSocket::bind("127.0.0.1:0").unwrap();
    let source = first.source;
    *invite = None;
    await_invite(socket, &media, invite, false);
    let pending = invite.as_ref().unwrap();
    assert_eq!(pending.source, source);
    assert_ne!(pending.call_id, old_id);
    assert_eq!(
        pending.target,
        format!("sip:{NUMBER}@{}", socket.local_addr().unwrap())
    );
    response(socket, pending, 100);
    command(format!("{}{terminator}", hangup::CODE));
    let cancelled = wire(socket, pending, "CANCEL");
    if let Some(request) = &cancelled {
        socket
            .send_to(registrar::response(request, 200, false).as_bytes(), source)
            .unwrap();
    }
    response(socket, pending, 487);
    let final_ack = wire(socket, pending, "ACK").is_some();
    thread::sleep(Duration::from_secs(2));
    let end = position();
    let cancel_quiet = end >= start + 64_000
        && fs::read(&requests).unwrap() == spoken
        && fs::read(&quiet.ptt).unwrap() == edges
        && channel(&probes.dac, start, end)
            .iter()
            .all(|v| v.abs() < 0.001);
    // A third dialog negotiates one source and keeps its original offered socket.
    // Only the final 200 unlocks uplink. Use different payloads on opposite sides
    // of the cut so a queued provisional suffix cannot satisfy the established oracle.
    let old_id = pending.call_id.clone();
    dial();
    let media = UdpSocket::bind("127.0.0.1:0").unwrap();
    *invite = None;
    await_invite(socket, &media, invite, false);
    let final_call = invite.as_ref().unwrap();
    assert_eq!(final_call.source, source);
    assert_ne!(final_call.call_id, old_id);
    assert_eq!(
        final_call.target,
        format!("sip:{NUMBER}@{}", socket.local_addr().unwrap())
    );
    let early = final_call
        .answer
        .replacen("SIP/2.0 200 ", "SIP/2.0 183 ", 1);
    socket.send_to(early.as_bytes(), source).unwrap();
    let mut caller = Caller::answered(
        socket,
        source,
        media.try_clone().unwrap(),
        final_call.media,
        Some(101),
    );
    let start = position();
    caller.audio(70, false);
    let early_pcm = sustained(&channel(&probes.dac, start, position()), true);
    media
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let before_answer = media
        .recv_from(&mut [0u8; 2048])
        .is_err_and(|e| matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut));
    socket
        .send_to(final_call.answer.as_bytes(), source)
        .unwrap();
    let answered = wire(socket, final_call, "ACK").is_some();
    let uplink = caller.idle_rtp();
    caller.resume_audio_clock();
    let start = position();
    caller.established_800hz(70);
    let fresh_pcm = marked(&channel(&probes.dac, start, position()), 800.0);
    let mut hangup = hangup::Observation::default();
    hangup.request(client, station, active, terminator, socket, final_call);
    let ended = hangup.command_ended_dialog();
    let retired = caller.media_quiet();
    Observation {
        early_pcm,
        receive_only: before_answer,
        answered,
        fresh_pcm,
        uplink,
        retired: ended && retired,
        busy_ack,
        busy_text,
        audible,
        cancel: cancelled.is_some(),
        final_ack,
        cancel_quiet,
    }
}
