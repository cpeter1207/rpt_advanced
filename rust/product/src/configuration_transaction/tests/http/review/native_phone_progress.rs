//! Observe provisional SIP through the same foreground, physical callbacks and dialog.
use super::*;
use std::path::PathBuf;

#[path = "native_phone_progress_terminal.rs"]
pub(super) mod terminal;

pub(super) struct Probes {
    dac: PathBuf,
    ptt: PathBuf,
    start: usize,
}

pub(super) struct Observation {
    pre_ring_idle: bool,
    ringing: bool,
    ring_keyed: bool,
    original: bool,
    receive_only: bool,
    refused_ack: bool,
}

impl Observation {
    pub(super) fn verify(self) {
        eprintln!(
            "PHONE_PROGRESS_CLOSED pre_ring_idle={} ringing={} ring_keyed={} original={} receive_only={} refused_ack={} withdrawal=true joined=true writer=true",
            self.pre_ring_idle,
            self.ringing,
            self.ring_keyed,
            self.original,
            self.receive_only,
            self.refused_ack
        );
        assert!(
            self.refused_ack,
            "the selected pending dialog must complete its refusal/ACK"
        );
        assert!(
            self.receive_only,
            "a provisional call transmitted to its remote RTP port"
        );
        assert!(
            self.pre_ring_idle,
            "100 and silent 183 must leave local PTT and programme idle"
        );
        assert!(
            self.ringing && self.ring_keyed && self.original,
            "actual 180 must key local ringback and original provisional RTP must replace it"
        );
    }
}

fn channel(path: &Path, begin: u64, end: u64) -> Vec<f32> {
    let bytes = fs::read(path).unwrap();
    assert!(end >= begin && end <= bytes.len() as u64 && begin % 4 == 0 && end % 4 == 0);
    bytes[begin as usize..end as usize]
        .chunks_exact(4)
        .map(|word| f32::from_ne_bytes(word.try_into().unwrap()))
        .collect()
}

// At 8 kHz a 100 ms window has orthogonal bins for 440, 480 and 500 Hz.
fn power_share(block: &[f32], hz: f64, total: f64) -> f64 {
    let mut real = 0.0;
    let mut imag = 0.0;
    for (n, sample) in block.iter().enumerate() {
        let angle = std::f64::consts::TAU * hz * n as f64 / 8000.0;
        real += f64::from(*sample) * angle.cos();
        imag += f64::from(*sample) * angle.sin();
    }
    2.0 * (real * real + imag * imag) / (block.len() as f64 * total)
}

fn sustained(samples: &[f32], original: bool) -> bool {
    let mut consecutive = 0;
    for block in samples.chunks_exact(800) {
        let total: f64 = block.iter().map(|v| f64::from(*v).powi(2)).sum();
        let matches = if total.is_finite() && total > 0.0008 {
            let low = power_share(block, 440.0, total);
            let high = power_share(block, 480.0, total);
            let provider = power_share(block, 500.0, total);
            if original {
                provider > 0.65 && low + high < 0.03
            } else {
                low > 0.12 && high > 0.12 && low + high > 0.7 && provider < 0.03
            }
        } else {
            false
        };
        consecutive = if matches { consecutive + 1 } else { 0 };
        if consecutive == 3 {
            return true;
        }
    }
    false
}

impl Probes {
    pub(super) fn ready() -> Self {
        let dac = PathBuf::from(std::env::var_os("RPTADV_TEST_SIP_PROGRESS_PCM").unwrap());
        let ptt = PathBuf::from(std::env::var_os("RPTADV_TEST_SIP_PROGRESS_PTT").unwrap());
        let limit = Instant::now() + Duration::from_secs(8);
        loop {
            let initial = fs::read(&ptt).unwrap();
            let begin = fs::metadata(&dac).unwrap().len();
            thread::sleep(Duration::from_millis(400));
            let end = fs::metadata(&dac).unwrap().len();
            if initial.last() == Some(&b'0')
                && initial == fs::read(&ptt).unwrap()
                && end >= begin + 8000
                && channel(&dac, begin, end).iter().all(|v| v.abs() < 0.001)
            {
                return Self {
                    dac,
                    ptt,
                    start: initial.len(),
                };
            }
            assert!(
                Instant::now() < limit,
                "station needs a quiet observed DAC and deasserted PTT"
            );
        }
    }

    pub(super) fn exercise(
        &self,
        socket: &UdpSocket,
        media: UdpSocket,
        call: &Invite,
    ) -> Observation {
        let signaling = |code: u16, sdp: bool| {
            let response = call
                .answer
                .replacen("SIP/2.0 200 ", &format!("SIP/2.0 {code} "), 1);
            let response = if sdp {
                response
            } else {
                let (headers, _) = response
                    .split_once("Content-Type: application/sdp")
                    .unwrap();
                format!("{headers}Content-Length: 0\r\n\r\n")
            };
            socket.send_to(response.as_bytes(), call.source).unwrap();
        };
        let position = || fs::metadata(&self.dac).unwrap().len();
        let before = position();
        signaling(100, false);
        thread::sleep(Duration::from_millis(250));
        signaling(183, true);
        thread::sleep(Duration::from_millis(650));
        let after_sdp = position();
        let edges = fs::read(&self.ptt).unwrap();
        let idle = after_sdp >= before + 16000
            && edges.len() == self.start
            && edges.last() == Some(&b'0')
            && channel(&self.dac, before, after_sdp)
                .iter()
                .all(|v| v.abs() < 0.001);
        signaling(180, false);
        let until = Instant::now() + Duration::from_secs(2);
        let ring = loop {
            thread::sleep(Duration::from_millis(100));
            if sustained(&channel(&self.dac, after_sdp, position()), false) {
                break true;
            }
            if Instant::now() >= until {
                break false;
            }
        };
        let keyed = fs::read(&self.ptt)
            .unwrap()
            .get(self.start..)
            .is_some_and(|edges| edges.contains(&b'1'));
        let first_rtp = position();
        let mut remote = Caller::answered(
            socket,
            call.source,
            media.try_clone().unwrap(),
            call.media,
            Some(101),
        );
        remote.audio(75, false);
        let original = sustained(&channel(&self.dac, first_rtp, position()), true);
        // This socket was bound before the offer. Check every datagram accumulated since INVITE.
        media
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut packet = [0; 2048];
        let receive_only = media
            .recv_from(&mut packet)
            .is_err_and(|e| matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock));
        signaling(486, false);
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut wire = [0; 8192];
        let refused_ack =
            registrar::receive_register(socket, &mut wire).is_ok_and(|(msg, source)| {
                source == call.source
                    && msg.starts_with("ACK ")
                    && registrar::header(&msg, "Call-ID") == call.call_id
                    && registrar::header(&msg, "CSeq") == format!("{} ACK", call.sequence)
                    && registrar::header(&msg, "From") == call.from
                    && registrar::header(&msg, "To") == call.to
            });
        Observation {
            pre_ring_idle: idle,
            ringing: ring,
            ring_keyed: keyed,
            original,
            receive_only,
            refused_ack,
        }
    }
}
