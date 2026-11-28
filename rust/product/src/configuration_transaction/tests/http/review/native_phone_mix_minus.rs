//! Distinct per-dialog PCM witnesses on the existing loopback SIP fixture.
use super::{Caller, Duration, Instant, SocketAddr, UdpSocket};
use std::{io::ErrorKind, path::Path, thread};

const CALLER_B: usize = 500;
const CALLER_A: usize = 650;
const PROMPT: usize = 800;
const STATION: usize = 1_000;
const BINS: [usize; 4] = [CALLER_B, CALLER_A, PROMPT, STATION];
const FRAMES: usize = 160;
const WITNESS_LEVEL: f64 = 500.0;

#[derive(Default)]
struct Witness {
    frames: usize,
    hits: [usize; 4],
    longest: [usize; 4],
}

impl Witness {
    fn hits(&self, frequency: usize) -> usize {
        self.hits[BINS.iter().position(|bin| *bin == frequency).unwrap()]
    }

    fn hear(&self, frequency: usize) -> bool {
        self.hits(frequency) >= 12
    }

    fn excludes(&self, frequencies: &[usize]) -> bool {
        frequencies
            .iter()
            .all(|frequency| self.hits(*frequency) == 0)
    }

    fn excludes_sustained(&self, frequencies: &[usize]) -> bool {
        BINS.iter()
            .zip(self.longest)
            .all(|(bin, run)| !frequencies.contains(bin) || run < 3)
    }
}

fn receive(media: &UdpSocket, remote: SocketAddr, score_from: Instant, until: Instant) -> Witness {
    let coefficients = BINS.map(|frequency| {
        std::array::from_fn::<_, FRAMES, _>(|index| {
            let radians = (index * frequency) as f64 * std::f64::consts::TAU / 8_000.0;
            radians.sin_cos()
        })
    });
    let mut found = Witness::default();
    let mut previous = None;
    let mut run = [0; 4];
    let mut wire = [0; 2048];
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if left.is_zero() {
            break;
        }
        media.set_read_timeout(Some(left)).unwrap();
        let (size, source) = match media.recv_from(&mut wire) {
            Ok(packet) => packet,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                break;
            }
            Err(error) => panic!("private mix witness RTP read failed: {error}"),
        };
        assert_eq!(source, remote);
        assert_eq!(size, 12 + FRAMES);
        assert_eq!(wire[0], 0x80);
        assert_eq!(wire[1] & 0x7f, 0);
        let sequence = u16::from_be_bytes(wire[2..4].try_into().unwrap());
        let ssrc = u32::from_be_bytes(wire[8..12].try_into().unwrap());
        if let Some((prior, stream)) = previous {
            assert_eq!(ssrc, stream);
            assert!((1..=u16::MAX / 2).contains(&sequence.wrapping_sub(prior)));
        }
        previous = Some((sequence, ssrc));
        if Instant::now() < score_from {
            continue;
        }

        let pcm: [f64; FRAMES] = std::array::from_fn(|index| {
            let code = !wire[12 + index];
            let magnitude =
                f64::from(((i32::from(code & 15) * 8 + 132) << ((code >> 4) & 7)) - 132);
            if code & 0x80 == 0 {
                magnitude
            } else {
                -magnitude
            }
        });
        found.frames += 1;
        for (index, waveform) in coefficients.iter().enumerate() {
            let (mut sine, mut cosine) = (0.0_f64, 0.0_f64);
            for (sample, (sin, cos)) in pcm.iter().zip(waveform) {
                sine += sample * sin;
                cosine += sample * cos;
            }
            let audible = sine.hypot(cosine) * 2.0 >= WITNESS_LEVEL * FRAMES as f64;
            found.hits[index] += usize::from(audible);
            run[index] = if audible { run[index] + 1 } else { 0 };
            found.longest[index] = found.longest[index].max(run[index]);
        }
    }
    found
}

fn transmit(caller: &mut Caller<'_>, frequency: Option<usize>, until: Instant) {
    if let Some(frequency) = frequency {
        let pcmu: [u8; FRAMES] = std::array::from_fn(|sample| {
            if sample * frequency % 8_000 < 4_000 {
                0xa2
            } else {
                0x22
            }
        });
        while Instant::now() < until {
            caller.packet(0, false, caller.timestamp, &pcmu);
            caller.timestamp = caller.timestamp.wrapping_add(FRAMES as u32);
        }
    } else {
        while Instant::now() < until {
            caller.audio(1, true);
        }
    }
}

fn observe(
    a: &mut Caller<'_>,
    b: &mut Caller<'_>,
    a_tone: Option<usize>,
    b_tone: Option<usize>,
) -> (Witness, Witness) {
    let a_media = a.media.try_clone().unwrap();
    let b_media = b.media.try_clone().unwrap();
    let (a_remote, b_remote) = (a.destination, b.destination);
    Caller::drain_prompt_backlog(&a_media, a_remote);
    Caller::drain_prompt_backlog(&b_media, b_remote);
    a.resume_audio_clock();
    b.resume_audio_clock();
    // A genuine source may need up to 500 ms to reach stable playout.
    // Compare sustained simultaneous audio after starting the caller tones.
    let score_from = Instant::now()
        + if a_tone.is_some() {
            Duration::from_millis(600)
        } else {
            Duration::ZERO
        };
    let until = score_from + Duration::from_millis(1_600);
    thread::scope(|scope| {
        scope.spawn(move || transmit(a, a_tone, until));
        scope.spawn(move || transmit(b, b_tone, until));
        let listener = scope.spawn(move || receive(&a_media, a_remote, score_from, until));
        let b_wire = receive(&b_media, b_remote, score_from, until);
        (listener.join().unwrap(), b_wire)
    })
}

impl Caller<'_> {
    /// Keep both dialogs on an independently clocked RTP wire through all phases.
    pub(in super::super) fn two_caller_mix_minus(
        &mut self,
        b: &mut Self,
        rf_witness: &Path,
    ) -> bool {
        self.during(true, || b.audible_greeting());
        let (shared, private) = observe(self, b, None, Some(CALLER_B));
        let rf_quiet_before_login = std::fs::read_to_string(rf_witness).unwrap().is_empty();
        // A partial Morse mark can project into another bin for one block.
        let private_prompt = private.hear(PROMPT)
            && private.excludes_sustained(&[CALLER_B, STATION])
            && shared.hear(STATION)
            && shared.excludes(&[PROMPT, CALLER_B])
            && rf_quiet_before_login;

        self.during(true, || {
            b.drain_private_prompt(Duration::from_secs(20));
            b.digits("741#");
            b.drain_private_prompt(Duration::from_secs(20));
            b.digits("1234#");
            assert!(
                b.fresh_station_audio(),
                "authenticated muted caller must receive station programme after its private prompt"
            );
        });
        let (shared_muted, muted) = observe(self, b, None, Some(CALLER_B));
        let verified_muted = shared_muted.hear(STATION)
            && shared_muted.excludes(&[CALLER_B, PROMPT])
            && muted.hear(STATION)
            && muted.excludes(&[CALLER_B, PROMPT])
            && std::fs::read_to_string(rf_witness).unwrap().is_empty();

        self.during(true, || b.digits("*99#"));
        let b_attached = self.during(true, || b.fresh_station_audio());
        let (a_mix, b_mix) = observe(self, b, Some(CALLER_A), Some(CALLER_B));
        let a_hears_b = a_mix.hear(CALLER_B);
        let b_hears_a = b_mix.hear(CALLER_A);
        let a_minus_self = a_mix.hear(STATION) && a_mix.excludes(&[CALLER_A, PROMPT]);
        let b_minus_self = b_mix.hear(STATION) && b_mix.excludes(&[CALLER_B, PROMPT]);
        let rf_b_after_attach = std::fs::read_to_string(rf_witness).unwrap() == "500Hz\n";
        eprintln!(
            "PRIVATE_CALL_MIX private_prompt={private_prompt} verified_muted={verified_muted} b_attached={b_attached} a_hears_b={a_hears_b} b_hears_a={b_hears_a} a_minus_self={a_minus_self} b_minus_self={b_minus_self} rf_b_after_attach={rf_b_after_attach} shared_prompt_frames={} private_prompt_frames={}",
            shared.frames, private.frames
        );
        private_prompt
            && verified_muted
            && b_attached
            && a_hears_b
            && b_hears_a
            && a_minus_self
            && b_minus_self
            && rf_b_after_attach
    }
}
