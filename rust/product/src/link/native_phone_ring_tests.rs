//! UDP/codec/jitter through the real private reader, admission and paced ring.
use super::*;
use crate::{
    abi,
    link::session::tests::{phone_source_peer_at_rate, phone_source_peer_inband},
    services::clock_tests,
};
use rpt_advanced_core::{
    config::{PhoneAccountProtocol, PhoneSpeechSettings},
    link::PeerInput as _,
};
use rpt_advanced_standalone::phone::native_test::NativeRtp;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
};

type Audio = unsafe extern "C" fn(
    *mut c_void,
    u64,
    *const f32,
    usize,
    u32,
    *const abi::rptadv_source_birth_v1,
) -> usize;
#[derive(Debug, Copy, Clone)]
struct Credit {
    start: u64,
    offered: usize,
    accepted: usize,
    born: u64,
    epoch: u64,
    synthetic: u32,
}
thread_local! {
    // Same serial thread as the source callback. Never share a DSO handle
    // across threads or retain the product's ephemeral context after a read.
    static RTP: RefCell<Option<NativeRtp>> = const { RefCell::new(None) };
    static FORWARD: Cell<Option<Audio>> = const { Cell::new(None) };
    static CREDITS: RefCell<Vec<Credit>> = const { RefCell::new(Vec::new()) };
}
unsafe extern "C" fn audio(
    ctx: *mut c_void,
    start: u64,
    data: *const f32,
    n: usize,
    synthetic: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    let Some(f) = FORWARD.get() else { return 0 };
    let accepted = unsafe { f(ctx, start, data, n, synthetic, birth) };
    if let Some(b) = unsafe { birth.as_ref() } {
        CREDITS.with_borrow_mut(|c| {
            c.push(Credit {
                start,
                offered: n,
                accepted,
                born: b.born_ns,
                epoch: b.host_clock_epoch,
                synthetic,
            })
        });
    }
    accepted
}
unsafe fn read(
    drained: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    result: *mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    // PeerIo owns a full v1 table and writable result of these exact C types.
    // Substitute only a transparent observer, preserving the actual context,
    // callback arguments, acceptance and every other callback in the table.
    let mut tap = unsafe { sink.read() };
    FORWARD.set(tap.audio_timed);
    tap.audio_timed = Some(audio);
    let status = RTP.with_borrow_mut(|r| {
        r.as_mut().map_or(-1, |rtp| unsafe {
            rtp.read(
                drained,
                std::ptr::from_ref(&tap).cast(),
                size_of_val(&tap),
                result.cast(),
                size_of::<abi::rptadv_peer_source_result_v1>(),
            )
        })
    });
    FORWARD.set(None);
    status
}
const BASE: u64 = 1_000_000_000;
const PERIOD: u64 = 20_010_000;
const ORIGIN: u64 = (1_u64 << 32) + 1000;
fn step(s: &mut PrivatePhoneSession, ns: u64) {
    clock_tests::set(1 + ns);
    s.step(ns / 1_000_000).unwrap();
}
fn send(turn: u64, pcm: &[f32; 160]) {
    RTP.with_borrow_mut(|r| {
        r.as_mut()
            .unwrap()
            .audio(2 + turn as u16, 1000 + turn as u32 * 160, pcm)
            .unwrap()
    });
}
fn observe() -> (rptadv_jitter_client::Observation, u64) {
    RTP.with_borrow(|r| r.as_ref().unwrap().observation().unwrap())
}
fn credits() -> Vec<Credit> {
    CREDITS.with_borrow_mut(std::mem::take)
}
struct Call {
    s: PrivatePhoneSession,
    c: PrivatePhoneControl,
    reply: PeerReply,
}
impl Call {
    fn new(events: bool, authenticated: bool) -> Self {
        RTP.with_borrow_mut(|r| *r = Some(NativeRtp::new(events).unwrap()));
        credits();
        let peer = if events {
            phone_source_peer_at_rate(8000)
        } else {
            phone_source_peer_inband(8000)
        };
        let (mut s, c, state, reply) =
            source_session_with_protocol(peer, authenticated, PhoneAccountProtocol::Sip);
        state.lock().unwrap().native_reader = Some(read);
        s.set_source_clock(clock_tests::clock(1));
        step(&mut s, BASE - 100_000_000);
        assert!(
            !c.media_rearm_ready(),
            "startup cut needs a new collector revision"
        );
        Self { s, c, reply }
    }
    fn collected(&mut self, ns: u64) {
        assert!(self.c.collection_ended(&self.reply));
        step(&mut self.s, ns);
        assert!(self.c.media_rearm_ready());
    }
    fn adopt(&mut self, ns: u64) -> media::PhoneMediaInput {
        let (pending, input) = self
            .c
            .prepare_pending_voiced_media(None, PhoneSpeechSettings::default())
            .unwrap();
        self.c.adopt_media(pending).ok().unwrap();
        step(&mut self.s, ns);
        assert!(input.active());
        input
    }
    fn event_control() -> Self {
        let mut call = Self::new(true, true);
        RTP.with_borrow_mut(|r| r.as_mut().unwrap().digit(1, 0, 5).unwrap());
        step(&mut call.s, BASE - 90_000_000);
        assert_eq!(call.c.digit(), Some(('5', 910)));
        call.collected(BASE - 80_000_000);
        call
    }
    fn event_ready() -> (Self, media::PhoneMediaInput) {
        let mut call = Self::event_control();
        let input = call.adopt(BASE - 70_000_000);
        (call, input)
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        RTP.with_borrow_mut(|r| *r = None);
    }
}
#[test]
fn native_ring_repeated_trims_shorten_paced_output_without_renewing_speech() {
    let (mut call, mut input) = Call::event_ready();
    let generation = media::tests::input_snapshot(&input);
    let mut accepted = 0;
    let mut peak = 0;
    let mut tagged = 0;
    let mut next = ORIGIN;
    let mut trims = Vec::new();
    let mut ns = BASE;
    let mut known_hold = None;
    let clip = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm");
    let mut voiced_output = false;
    for turn in 0..520_u64 {
        let mut pcm = [0.; 160];
        // Real recorded speech, followed by known silence before both trims.
        if turn < 23 {
            for (i, s) in pcm.iter_mut().enumerate() {
                let j = ((turn as usize + 10) * 160 + i) * 12;
                *s = f32::from(i16::from_le_bytes([clip[j], clip[j + 1]])) / 32768.;
            }
        }
        send(turn, &pcm);
        let period = if turn < 50 { PERIOD } else { 19_800_000 };
        for part in [0, period / 2] {
            let now = ns + part;
            let before = input.available();
            step(&mut call.s, now);
            assert_eq!(
                media::tests::input_snapshot(&input),
                generation,
                "cut at packet {turn}"
            );
            let after = input.available();
            let reads = credits();
            assert_eq!(
                after - before,
                reads.iter().map(|c| c.accepted as u64).sum::<u64>()
            );
            let hold = media::tests::hold_snapshot(call.s.media.as_ref().unwrap());
            for c in reads {
                assert_eq!((c.start, c.synthetic, c.accepted), (next, 0, c.offered));
                assert!(c.epoch != 0 && c.born < now);
                let ledger = media::tests::ring_bound(
                    call.s.media.as_ref().unwrap(),
                    c.start + c.accepted as u64 - 1,
                )
                .unwrap();
                assert_eq!(ledger.host_clock_epoch, c.epoch);
                assert!(ledger.not_after_ns <= c.born && now - ledger.not_after_ns < 500_000_000);
                assert!(matches!(c.offered, 80 | 160));
                if c.offered == 80 {
                    assert_eq!(
                        hold,
                        known_hold.unwrap(),
                        "trim cannot renew the last genuine hold"
                    );
                    trims.push((turn, c.start));
                }
                next += c.accepted as u64;
            }
            if turn >= 40 && known_hold.is_none() {
                assert!(hold.0.is_some());
                known_hold = Some(hold);
            }
            accepted += after - before;
            peak = peak.max(after);
            for _ in 0..480 {
                if let Some(sample) = input.render_timed_sample() {
                    if let Some(b) = sample.age_bound {
                        assert!(b.not_after_ns <= now && now - b.not_after_ns < 500_000_000);
                        tagged += 1;
                    }
                    let p = sample.provenance;
                    voiced_output |=
                        sample.sample.abs() > 0.001 && p.is_some_and(|p| p.real && !p.muted);
                    if turn > 150 {
                        assert_eq!(sample.sample, 0.0);
                    }
                }
            }
        }
        ns += period;
    }
    let (j, synthetic) = observe();
    eprintln!(
        "ring: trims={trims:?} accepted={accepted} peak={peak} tagged={tagged} hold={known_hold:?} native={j:?}"
    );
    assert!(voiced_output && tagged > 0 && peak <= 2400);
    assert_eq!(
        (
            trims.len(),
            j.skipped_span,
            j.missing_span,
            j.late_drops,
            j.full_drops,
            synthetic
        ),
        (2, 160, 0, 0, 0, 0)
    );
    assert_eq!(
        accepted + j.skipped_span,
        (520 - u64::from(j.queued_packets)) * 160
    );
}

fn drain_to(input: &mut media::PhoneMediaInput, target: u64) {
    for _ in 0..12_000 {
        if input.available() <= target {
            break;
        }
        let _ = input.render_timed_sample();
    }
    assert!(input.available() <= target);
}
/// Preserve the 44th full decision's native advance but stop the consumer
/// exactly at the full product ring. Only the consumer will create credit.
fn stalled(discard: u64) -> (Call, media::PhoneMediaInput, u64, Credit) {
    stalled_with_pcm(discard, false)
}
fn stalled_with_pcm(discard: u64, speech: bool) -> (Call, media::PhoneMediaInput, u64, Credit) {
    let mut call = Call::event_control();
    for turn in 0..discard {
        send(turn, &[0.; 160]);
        for part in [0, PERIOD / 2] {
            step(&mut call.s, BASE + turn * PERIOD + part);
        }
    }
    let mut input = call.adopt(BASE + (discard - 1) * PERIOD + PERIOD / 2);
    credits();
    call.c.set_attached(true);
    for turn in discard..=45_u64 {
        let mut pcm = [0.; 160];
        if speech && turn >= 17 {
            let clip = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm");
            for (i, sample) in pcm.iter_mut().enumerate() {
                let j = ((turn as usize - 17) * 160 + i) * 12;
                *sample = f32::from(i16::from_le_bytes([clip[j], clip[j + 1]])) / 32768.;
            }
        }
        send(turn, &pcm);
        for part in [0, PERIOD / 2] {
            let now = BASE + turn * PERIOD + part;
            step(&mut call.s, now);
            let seen = credits();
            if let Some(c) = seen.last().filter(|c| c.offered == 80) {
                assert_eq!(
                    input.available(),
                    2400,
                    "retained turn={turn}, credit={c:?}"
                );
                return (call, input, now, *c);
            }
            if observe().0.skipped_span == 0 && input.available() > 2240 {
                drain_to(&mut input, 2240);
            }
        }
    }
    panic!("native half-packet did not encounter a full ring")
}
#[test]
fn native_ring_stalled_suffix_preserves_receipt_and_partial_credit() {
    for (discard, want) in [(4, 0), (1, 32)] {
        let (mut call, mut input, now, first) = stalled(discard);
        assert_eq!((observe().0.skipped_span, first.accepted), (80, want));
        step(&mut call.s, now + 1_000_000);
        let refused = credits()[0];
        assert_eq!(
            (
                refused.start,
                refused.offered,
                refused.accepted,
                refused.born,
                refused.epoch
            ),
            (
                first.start + want as u64,
                80 - want,
                0,
                first.born,
                first.epoch
            )
        );
        assert_eq!(input.available(), 2400);
        drain_to(&mut input, 2300);
        let before = input.available();
        step(&mut call.s, now + 2_000_000);
        let resumed = credits()[0];
        assert_eq!(
            (
                resumed.start,
                resumed.offered,
                resumed.accepted,
                resumed.born,
                resumed.epoch
            ),
            (
                refused.start,
                refused.offered,
                refused.offered,
                first.born,
                first.epoch
            )
        );
        assert_eq!(input.available(), before + resumed.accepted as u64);
        assert!(input.active());
        assert_eq!((observe().0.skipped_span, observe().1), (80, 0));
    }
}
#[test]
fn native_ring_retired_partial_suffix_never_enters_replacement() {
    for action in ["detach", "replace", "end", "clock"] {
        let (mut call, mut old, now, first) = stalled(1);
        assert_eq!(first.accepted, 32);
        if action == "clock" {
            // Production clock binding rejects a new epoch on the same owner;
            // the old retained decision cannot reach a new epoch or sink.
            let replacement_clock = clock_tests::clock(1);
            call.s.set_source_clock(replacement_clock);
            assert_eq!(call.s.step(now / 1_000_000), Err(Error::Hangup));
            assert!(credits().is_empty() && !call.reply.phone_media_authorized());
        } else if action == "end" {
            call.reply.retire();
            assert_eq!(call.s.step(now / 1_000_000), Err(Error::Hangup));
            assert!(credits().is_empty() && !call.reply.phone_media_authorized());
        } else {
            if action == "detach" {
                call.c.set_attached(false);
            }
            call.c.retire_media();
            assert!(
                call.reply.phone_media_authorized() && !call.reply.ended(),
                "revoking the selected generation is not a mailbox logout"
            );
            if action == "replace" {
                let generation = media::tests::input_snapshot(&old).0;
                let replacement = call.adopt(now + 1_000_000);
                assert_ne!(media::tests::input_snapshot(&replacement).0, generation);
                assert_eq!(
                    replacement.available(),
                    0,
                    "old suffix never lands in new ring"
                );
            } else {
                step(&mut call.s, now + 1_000_000);
                assert!(!call.c.media_occupied());
            }
            let discarded = credits()[0];
            assert_eq!(
                (
                    discarded.start,
                    discarded.offered,
                    discarded.accepted,
                    discarded.born
                ),
                (first.start + 32, 48, 48, first.born)
            );
        }
        assert!(!old.active());
        assert!(old.render_timed_sample().is_none());
    }
}
#[test]
fn native_ring_unauthenticated_source_cannot_prepare_programme() {
    let mut call = Call::new(true, false);
    assert!(matches!(
        call.c
            .prepare_pending_voiced_media(None, PhoneSpeechSettings::default()),
        Err(Error::Admission)
    ));
}

/// Observe the detector's own lookback release and authority cut on PCM. No
/// RFC4733 mapping is negotiated: both collector revisions come from the wire
/// codec's decoded dual tones, not native symbols or a test-side source input.
#[test]
fn native_ring_inband_lookback_across_trim_retires_the_real_generation() {
    for first in [84, 160] {
        let mut call = Call::new(false, true);
        let mut input: Option<media::PhoneMediaInput> = None;
        let mut digits = Vec::new();
        let mut ranges = Vec::new();
        let mut trimmed = None;
        let mut cut = None;
        let mut heard_speech = false;
        let clip = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm");
        for turn in 0..72_u64 {
            let mut pcm = [0.; 160];
            if turn < 7 || (41..50).contains(&turn) {
                let hz = if turn < 7 { 770. } else { 852. };
                for (i, s) in pcm.iter_mut().enumerate() {
                    if turn == 41 && i < first {
                        continue;
                    }
                    let p = (turn * 160 + i as u64) as f32 * std::f32::consts::TAU / 8000.;
                    *s = 0.25 * ((p * hz).sin() + (p * 1336.).sin());
                }
            } else if (22..35).contains(&turn) {
                for (i, s) in pcm.iter_mut().enumerate() {
                    let j = ((turn as usize + 1) * 160 + i) * 12;
                    *s = f32::from(i16::from_le_bytes([clip[j], clip[j + 1]])) / 32768.;
                }
            }
            send(turn, &pcm);
            for part in [0, PERIOD / 2] {
                let now = BASE + turn * PERIOD + part;
                let previously = input.as_ref().is_some_and(|i| i.active());
                step(&mut call.s, now);
                let reads = credits();
                if let Some(c) = reads.iter().find(|c| c.offered == 80) {
                    trimmed = Some(c.start);
                    assert_eq!(
                        input.as_ref().unwrap().active(),
                        first == 160,
                        "only real tone onset may retire this generation"
                    );
                }
                if turn >= 40 && !reads.is_empty() {
                    ranges.extend(call.s.source.as_ref().unwrap().detected_mute_ranges());
                }
                if previously && !input.as_ref().unwrap().active() {
                    cut = Some((turn, call.s.source_end));
                }
                if let Some((digit, _)) = call.c.digit() {
                    digits.push(digit);
                    if digits.len() == 1 {
                        assert_eq!(digit, '5');
                        assert!(call.c.collection_ended(&call.reply));
                    }
                }
                if digits == ['5'] && input.is_none() && call.c.media_rearm_ready() {
                    input = Some(call.adopt(now));
                }
                if let Some(input) = input.as_mut() {
                    for _ in 0..480 {
                        if let Some(sample) = input.render_timed_sample() {
                            if let Some(p) = sample.provenance {
                                heard_speech |= p.real && !p.muted && sample.sample.abs() > 0.001;
                                if p.position.is_some_and(|x| x.frame >= ORIGIN + 41 * 160) {
                                    assert!(sample.sample == 0.0 || p.muted);
                                }
                            }
                        }
                    }
                }
            }
        }
        let trim = trimmed.expect("verified packet suffix");
        assert_eq!(digits, ['5', '8']);
        assert!(heard_speech);
        assert_eq!((observe().0.skipped_span, observe().1), (80, 0));
        let (_, end) = cut.expect("real detector onset retired this generation");
        if first == 84 {
            assert_eq!(end, trim);
            assert!(
                ranges.contains(&(trim - 46, trim + 240)),
                "later detector lookback crosses the actual splice"
            );
        } else {
            assert_eq!(end, trim + 400);
            assert_eq!(
                ranges.first(),
                Some(&(trim + 56, trim + 400)),
                "lookback into the retained suffix must not use the wire coordinate"
            );
        }
        assert!(!call.c.media_rearm_ready() && !call.c.media_occupied());
        assert!(input.unwrap().render_timed_sample().is_none());
        eprintln!("inband: onset_phase={first} trim={trim} onset_end={end} symbols={digits:?}");
    }
}

/// The alternative native cadence advances over a whole packet. Even though its
/// cause is known, it is not the in-packet trim classification tested above.
#[test]
fn native_ring_full_packet_skip_cannot_preserve_the_old_generation() {
    let (mut call, mut input) = Call::event_ready();
    for turn in 0..48 {
        send(turn, &[0.; 160]);
        for part in [0, 10_000_000] {
            step(&mut call.s, BASE + turn * 20_000_000 + part);
            for _ in 0..480 {
                let _ = input.render_timed_sample();
            }
        }
    }
    assert_eq!(observe().0.skipped_span, 160);
    assert!(!input.active() && input.render_timed_sample().is_none());
    assert!(!call.c.media_rearm_ready() && !call.reply.ended());
}

#[path = "native_phone_expiry_tests.rs"]
mod expiry;
