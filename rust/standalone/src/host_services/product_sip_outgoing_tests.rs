//! Actual typed product origination through the registered SIP reactor and radio mixer.
use super::*;
#[path = "product_sip_ivr_decoder.rs"]
mod ivr_decoder;
#[derive(Clone, Copy, PartialEq, Eq)]
enum IvrMode {
    Disabled,
    Events,
    InBand,
    MixedClock,
}

use crate::phone::{
    media::MediaDescription,
    sip_test_registrar::{receive_register, response},
};

fn receive(socket: &UdpSocket) -> (String, SocketAddr) {
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    receive_register(socket, &mut [0; 8192]).expect("bounded outgoing dialog progress")
}
fn command(runtime: &ProductRuntime, operation: u32, call_id: u64) -> (i32, u64) {
    let (account, destination) = if operation == abi::RPTADV_PHONE_DIAL {
        ("office", "sip:called@fixture.invalid")
    } else {
        ("", "")
    };
    let request = abi::rptadv_phone_command_v1 {
        struct_size: size_of::<abi::rptadv_phone_command_v1>() as u32,
        abi_version: abi::RPTADV_PHONE_COMMAND_ABI_VERSION,
        operation,
        account: account.as_ptr().cast(),
        account_length: account.len(),
        destination: destination.as_ptr().cast(),
        destination_length: destination.len(),
        call_id,
    };
    let mut id = 0;
    let result = unsafe {
        runtime
            .product_descriptor()
            .phone_command
            .expect("actual typed product entry")(c"1000".as_ptr(), 4, &request, &mut id)
    };
    (result, id)
}
fn next_call(runtime: &ProductRuntime) -> u64 {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let (result, id) = command(runtime, abi::RPTADV_PHONE_DIAL, 0);
        if result == 0 {
            assert_ne!(id, 0);
            return id;
        }
        assert_eq!((result, id), (-1, 0));
        assert!(
            Instant::now() < until,
            "previous exact claim was not released"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
static ANSWER_ACKS: AtomicUsize = AtomicUsize::new(0);
static SELECTED_PAYLOAD: AtomicUsize = AtomicUsize::new(0);
static RTP_PACKET_TICKS: AtomicUsize = AtomicUsize::new(160);
static SOURCE_DIGITS: AtomicUsize = AtomicUsize::new(0);
static CONTINUOUS_RTP: AtomicBool = AtomicBool::new(false);
static LAST_RTP: Mutex<Option<(u16, u32, u32)>> = Mutex::new(None);

// Successful missing/concealed/reset/control source boundaries retire the phone
// generation. Observe only that scalar fact, forwarding all original credit.
static SOURCE_CUTS: AtomicUsize = AtomicUsize::new(0);
static LAST_MARKED_CUT: AtomicUsize = AtomicUsize::new(0);
static ALLOW_SOURCE_CUT: AtomicBool = AtomicBool::new(false);
// Diagnostic metadata for synthetic silence only; no PCM or control symbols.
static QUIET_PHASE: AtomicUsize = AtomicUsize::new(0);
static QUIET_PACKET: AtomicUsize = AtomicUsize::new(0);
static QUIET_SKIPPED_INPUT: AtomicUsize = AtomicUsize::new(0);
struct SourceCutObserver {
    original: abi::rptadv_peer_source_sink_v1,
}
unsafe extern "C" fn observe_source_audio(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
) -> usize {
    // SAFETY: the synchronous read retains the full copied sink and original context.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted =
        unsafe { original.audio.unwrap()(original.context, start, pcm, count, concealed) };
    if concealed != 0 && accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_audio_timed(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    // SAFETY: borrowed PCM and birth are forwarded unchanged, never inspected or retained.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe {
        original.audio_timed.unwrap()(original.context, start, pcm, count, concealed, birth)
    };
    if concealed != 0 && accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    // SAFETY: this wrapper exists only for the original nonnull callback.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.gap.unwrap()(original.context, start, count) };
    if accepted != 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_discontinuity(context: *mut c_void) -> i32 {
    // SAFETY: forward the serial owner and its exact discontinuity result.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.source_discontinuity.unwrap()(original.context) };
    if accepted == 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_source_digit(context: *mut c_void, symbol: u32) -> i32 {
    // SAFETY: forward the exact symbol; retain only anonymous accepted-cut credit.
    let original = &unsafe { &*context.cast::<SourceCutObserver>() }.original;
    let accepted = unsafe { original.digit.unwrap()(original.context, symbol) };
    if accepted == 0 {
        SOURCE_CUTS.fetch_add(1, Ordering::Release);
        SOURCE_DIGITS.fetch_add(1, Ordering::Release);
    }
    accepted
}
unsafe extern "C" fn observe_peer_read_source(
    context: *mut c_void,
    peer: *mut c_void,
    completion: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    text: abi::rptadv_peer_source_text_v1,
    text_context: *mut c_void,
    output: *mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    if sink.is_null()
        // SAFETY: a supplied sink promises the readable size prefix.
        || unsafe { ptr::addr_of!((*sink).struct_size).read() }
            != size_of::<abi::rptadv_peer_source_sink_v1>() as u32
        // SAFETY: the preceding size check covers this version field.
        || unsafe { ptr::addr_of!((*sink).abi_version).read() } != 1
    {
        // SAFETY: preserve existing validation and all arguments for an older/newer/invalid sink.
        return unsafe {
            crate::host_services::peer_read_source(
                context,
                peer,
                completion,
                sink,
                text,
                text_context,
                output,
            )
        };
    }
    // SAFETY: the checked current prefix covers this Copy record; no pointee is owned.
    let mut trace = SourceCutObserver {
        original: unsafe { sink.read() },
    };
    let mut delegated = trace.original;
    delegated.context = ptr::from_mut(&mut trace).cast();
    if delegated.audio.is_some() {
        delegated.audio = Some(observe_source_audio);
    }
    if delegated.audio_timed.is_some() {
        delegated.audio_timed = Some(observe_source_audio_timed);
    }
    if delegated.gap.is_some() {
        delegated.gap = Some(observe_source_gap);
    }
    if delegated.source_discontinuity.is_some() {
        delegated.source_discontinuity = Some(observe_source_discontinuity);
    }
    if delegated.digit.is_some() {
        delegated.digit = Some(observe_source_digit);
    }
    // SAFETY: each callback restores its original context, arguments and credit.
    // Clock fields, completion, text callback/context and output are unchanged.
    unsafe {
        crate::host_services::peer_read_source(
            context,
            peer,
            completion,
            &delegated,
            text,
            text_context,
            output,
        )
    }
}

// Check every packet, including transition packets. Fresh source credit never
// permits a cut once the measured strict interval has begun.
fn assert_rtp_step(
    previous: (u16, u32, u32),
    current: (u16, u32, u32),
    marker: bool,
    expected: u32,
    allow_cut: bool,
    cuts: usize,
    last_cut: usize,
) -> usize {
    let (sequence, timestamp, ssrc) = previous;
    assert_eq!(
        (current.0, current.2),
        (sequence.wrapping_add(1), ssrc),
        "one accepted RTP sequence and SSRC across source retirement"
    );
    let delta = current.1.wrapping_sub(timestamp);
    if marker || delta != expected {
        assert!(
            allow_cut,
            "healthy negotiated silence must remain continuous: phase={}, packet={}, previous={previous:?}, current={current:?}, marker={marker}, delta={delta}, expected={expected}, cuts={cuts}, last_cut={last_cut}, skipped_input={}",
            QUIET_PHASE.load(Ordering::Acquire),
            QUIET_PACKET.load(Ordering::Acquire),
            QUIET_SKIPPED_INPUT.load(Ordering::Acquire)
        );
        assert!(marker, "source cut must retain the existing reset marker");
        assert!(
            cuts > last_cut,
            "a marker alone is not evidence of an accepted source cut"
        );
        assert!(
            (delta as i32) > 0,
            "retirement cannot replay or rewind source time"
        );
        return cuts;
    }
    last_cut
}

#[test]
fn strict_silence_rejects_even_credited_later_cuts() {
    let previous = (10, 1000, 7);
    assert_eq!(
        assert_rtp_step(previous, (11, 1160, 7), false, 160, false, 20, 20),
        20
    );
    assert_eq!(
        assert_rtp_step(previous, (11, 1472, 7), true, 160, true, 21, 20),
        21
    );
    for (allow, marker, delta, cuts) in [
        (false, true, 472, 21),
        (false, false, 472, 20),
        (true, true, 472, 20),
        (true, false, 472, 21),
        (true, true, 0, 21),
    ] {
        assert!(
            std::panic::catch_unwind(|| {
                assert_rtp_step(
                    previous,
                    (11, 1000 + delta, 7),
                    marker,
                    160,
                    allow,
                    cuts,
                    20,
                )
            })
            .is_err()
        );
    }
    // Fresh transition credit cannot excuse packet loss or rewound source time.
    for (current, marker) in [((12, 1160, 7), false), ((11, 999, 7), true)] {
        assert!(
            std::panic::catch_unwind(|| {
                assert_rtp_step(previous, current, marker, 160, true, 21, 20)
            })
            .is_err()
        );
    }
}

fn observe_rtp(wire: &[u8]) {
    let current = (
        u16::from_be_bytes(wire[2..4].try_into().unwrap()),
        u32::from_be_bytes(wire[4..8].try_into().unwrap()),
        u32::from_be_bytes(wire[8..12].try_into().unwrap()),
    );
    let mut last = LAST_RTP.lock().unwrap();
    if CONTINUOUS_RTP.load(Ordering::Acquire) {
        if let Some(previous) = *last {
            let cut = assert_rtp_step(
                previous,
                current,
                wire[1] & 0x80 != 0,
                RTP_PACKET_TICKS.load(Ordering::Acquire) as u32,
                ALLOW_SOURCE_CUT.load(Ordering::Acquire),
                SOURCE_CUTS.load(Ordering::Acquire),
                LAST_MARKED_CUT.load(Ordering::Acquire),
            );
            LAST_MARKED_CUT.store(cut, Ordering::Release);
        }
    }
    if last.is_none() {
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
    }
    *last = Some(current);
}

fn quiet_media(
    caller: &mut Caller,
    codec: &mut AudioCodec,
    packets: u64,
    rearm_digits: Option<usize>,
) {
    let incoming = rearm_digits.is_some();
    let mut transition = incoming;
    let phase = if incoming { 2 } else { 1 };
    QUIET_PHASE.store(phase, Ordering::Release);
    QUIET_PACKET.store(0, Ordering::Release);
    QUIET_SKIPPED_INPUT.store(0, Ordering::Release);
    let rf_before = PUBLIC_OUTPUT.load(Ordering::Acquire);
    let mut started = Instant::now();
    let transition_deadline = started + Duration::from_secs(3);
    let mut next_input = started;
    let mut wire = [0; 2048];
    // Thirty seconds is a bounded steady-state transport test, not an assumed NAT lifetime.
    let mut packet = 0;
    while packet < packets {
        QUIET_PACKET.store(
            if transition { 0 } else { packet as usize + 1 },
            Ordering::Release,
        );
        let until = if transition {
            transition_deadline
        } else {
            Instant::now() + Duration::from_secs(3)
        };
        let (length, source) = loop {
            if incoming && Instant::now() >= next_input {
                caller.audio_payload(
                    codec,
                    &vec![0.0; caller.profile.parameters().packet_frames as usize],
                    caller.profile.payload(),
                );
                next_input += caller.packet_interval();
                // Preserve source gaps after missed slots, never burst old PCM.
                while next_input <= Instant::now() {
                    QUIET_SKIPPED_INPUT.fetch_add(1, Ordering::Release);
                    caller.timestamp = caller.timestamp.wrapping_add(caller.packet_ticks());
                    next_input += caller.packet_interval();
                }
            }
            service(caller);
            if let Ok(packet) = caller.rtp.recv_from(&mut wire) {
                break packet;
            }
            assert!(
                Instant::now() < until,
                "answered idle call suppressed RTP after its first packet"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(
            (length, source),
            (12 + caller.profile.parameters().packet_bytes, caller.media)
        );
        assert_eq!(
            wire[1] & 0x7f,
            SELECTED_PAYLOAD.load(Ordering::Acquire) as u8
        );
        observe_rtp(&wire[..length]);
        let mut pcm = vec![0.0; caller.profile.parameters().packet_frames as usize];
        codec.decode(&wire[12..length], &mut pcm).unwrap();
        assert!(pcm.iter().all(|sample| sample.abs() < 0.001));
        if transition {
            assert!(
                Instant::now() < transition_deadline,
                "native control rearm did not reach the wire"
            );
            let accepted = SOURCE_DIGITS.load(Ordering::Acquire);
            let expected = rearm_digits.unwrap();
            assert!(
                accepted <= expected,
                "native control was accepted more than once"
            );
            if accepted == expected
                && LAST_MARKED_CUT.load(Ordering::Acquire) == SOURCE_CUTS.load(Ordering::Acquire)
            {
                // A paced packet has crossed the already credited rearm boundary.
                // Keep its RTP history; every following packet is strictly continuous.
                transition = false;
                ALLOW_SOURCE_CUT.store(false, Ordering::Release);
                started = Instant::now();
            }
            continue;
        }
        packet += 1;
        if packet % 50 == 0 {
            // Include ordinary OS scheduling tolerance without accepting a catch-up flood.
            let expected = caller.packet_interval() * u32::try_from(packet).unwrap();
            let elapsed = started.elapsed();
            assert!(elapsed + Duration::from_millis(100) >= expected);
            assert!(elapsed <= expected + Duration::from_millis(500));
        }
    }
    QUIET_PHASE.store(0, Ordering::Release);
    assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        rf_before,
        "transport silence must not key or populate RF"
    );
}

fn service(caller: &Caller) {
    let mut wire = [0; 8192];
    while let Ok((count, source)) = caller.sip.recv_from(&mut wire) {
        let (first, values) = fields(&wire[..count]);
        if first.starts_with("ACK ") {
            assert!(values["cseq"].ends_with(" ACK"));
            ANSWER_ACKS.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        assert!(
            !first.starts_with("BYE "),
            "product unexpectedly closed outgoing voice"
        );
        assert!(
            first.starts_with("OPTIONS ") || first.starts_with("REGISTER "),
            "unexpected dialog message: {first}"
        );
        caller.answer_register(&values, source);
    }
}
fn drain(caller: &Caller, codec: &mut AudioCodec, returned: &mut Vec<f32>) {
    let mut wire = [0; 2048];
    while let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
        assert_eq!(
            source, caller.media,
            "same offered RTP socket owns answered media"
        );
        assert_eq!(length, 12 + caller.profile.parameters().packet_bytes);
        assert_eq!(
            (wire[0], wire[1] & 0x7f),
            (0x80, SELECTED_PAYLOAD.load(Ordering::Acquire) as u8)
        );
        observe_rtp(&wire[..length]);
        let mut pcm = vec![0.0; caller.profile.parameters().packet_frames as usize];
        codec.decode(&wire[12..length], &mut pcm).unwrap();
        returned.extend_from_slice(&pcm);
    }
}
fn media(
    caller: &mut Caller,
    codec: &mut AudioCodec,
    samples: &[f32],
    returned: &mut Vec<f32>,
    mut linked: Option<&mut vox::Remote>,
) {
    let mut next = Instant::now();
    for chunk in samples.chunks(caller.profile.parameters().packet_frames as usize) {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let mut pcm = vec![0.0; caller.profile.parameters().packet_frames as usize];
        pcm[..chunk.len()].copy_from_slice(chunk);
        caller.audio_payload(codec, &pcm, caller.profile.payload());
        if let Some(remote) = linked.as_mut() {
            remote.pump();
        }
        service(caller);
        drain(caller, codec, returned);
        next += caller.packet_interval();
        // This bounded fixture never catches up by emitting a burst of old media.
        // A missed source slot is explicit in its timestamp, not a renewed birth.
        while next <= Instant::now() {
            caller.timestamp = caller.timestamp.wrapping_add(caller.packet_ticks());
            next += caller.packet_interval();
        }
    }
    // Preserve the final packet interval across consecutive media calls too.
    if let Some(wait) = next.checked_duration_since(Instant::now()) {
        thread::sleep(wait);
    }
}

fn radio_digits(digits: &[u8]) {
    // Existing receiver clock consumes real tones and keyed gaps; no direct
    // runtime command or second input queue stands in for the RF decoder.
    LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
    thread::sleep(Duration::from_millis(100));
    for &digit in digits {
        LOCAL_DTMF.store(usize::from(digit), Ordering::Release);
        thread::sleep(Duration::from_millis(160));
        LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
        thread::sleep(Duration::from_millis(120));
    }
    LOCAL_DTMF.store(0, Ordering::Release);
}

#[test]
fn actual_product_originates_two_way_phone_and_releases_the_exact_call() {
    product_outgoing(false, false, false, AudioProfile::Mulaw, false);
}

#[test]
fn actual_radio_dtmf_originates_two_way_phone_and_hangs_up() {
    product_outgoing(true, false, false, AudioProfile::Mulaw, false);
}

#[test]
fn actual_answer_first_utterance_reaches_rf_without_any_receiver_key() {
    product_outgoing(false, true, false, AudioProfile::Mulaw, false);
}

#[test]
fn actual_rf_answer_first_utterance_reaches_rf_without_receiver_rekey() {
    product_outgoing(true, true, false, AudioProfile::Mulaw, false);
}

#[test]
fn actual_answered_quiet_call_opens_symmetric_rtp_before_any_receiver_key() {
    product_outgoing(false, true, true, AudioProfile::Mulaw, false);
}

#[test]
fn actual_alaw_quiet_call_sends_continuous_silence_without_receiver_key() {
    product_outgoing(false, true, true, AudioProfile::Alaw, false);
}

// A symmetric media relay learns only the actual RTP source, never the SDP
// address. It cannot forward the first greeting before that mapping exists.
fn latch(caller: &Caller) -> Option<SocketAddr> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut wire = [0; 2048];
    while Instant::now() < until {
        service(caller);
        if let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
            assert_eq!(length, 12 + caller.profile.parameters().packet_bytes);
            assert_eq!(
                (wire[0], wire[1] & 0x7f),
                (0x80, SELECTED_PAYLOAD.load(Ordering::Acquire) as u8)
            );
            observe_rtp(&wire[..length]);
            return Some(source);
        }
        thread::sleep(Duration::from_millis(1));
    }
    None
}

#[test]
fn actual_outgoing_call_isolates_and_restores_existing_ordinary_link() {
    product_outgoing_with_ivr(
        true,
        false,
        false,
        AudioProfile::Mulaw,
        IvrMode::Events,
        true,
    );
}

fn pump_linked(remote: &mut vox::Remote, duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        remote.pump();
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_linked_quiet(remote: &mut vox::Remote) {
    let deadline = Instant::now() + Duration::from_secs(10);
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
            return;
        }
        assert!(Instant::now() < deadline, "finite LOCAL result must drain");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn actual_radio_digits_reach_telephone_ivr_without_control_or_audio_leak() {
    product_outgoing_with_ivr(
        true,
        true,
        false,
        AudioProfile::Mulaw,
        IvrMode::Events,
        false,
    );
}

#[test]
fn actual_radio_digits_use_private_inband_when_event_payload_is_absent() {
    product_outgoing_with_ivr(
        true,
        true,
        false,
        AudioProfile::Mulaw,
        IvrMode::InBand,
        false,
    );
}

#[test]
fn actual_alaw_radio_digits_reach_telephone_ivr_without_audio_leak() {
    product_outgoing_with_ivr(
        true,
        true,
        false,
        AudioProfile::Alaw,
        IvrMode::Events,
        false,
    );
}

#[test]
fn actual_alaw_radio_digits_use_private_inband() {
    product_outgoing_with_ivr(
        true,
        true,
        false,
        AudioProfile::Alaw,
        IvrMode::InBand,
        false,
    );
}

fn product_outgoing(
    radio: bool,
    immediate: bool,
    latching: bool,
    selected: AudioProfile,
    isolated: bool,
) {
    product_outgoing_with_ivr(
        radio,
        immediate,
        latching,
        selected,
        IvrMode::Disabled,
        isolated,
    );
}

fn product_outgoing_with_ivr(
    radio: bool,
    immediate: bool,
    latching: bool,
    selected: AudioProfile,
    ivr: IvrMode,
    isolated: bool,
) {
    product_outgoing_with_policy(radio, immediate, latching, selected, ivr, isolated, None);
}

#[test]
fn actual_unlocked_phone_bypass_preserves_local_only_playout() {
    product_outgoing_with_policy(
        true,
        false,
        false,
        AudioProfile::Mulaw,
        IvrMode::Disabled,
        true,
        Some(false),
    );
}

#[test]
fn actual_unlocked_phone_bypass_allows_eligible_peer_playout() {
    product_outgoing_with_policy(
        true,
        false,
        false,
        AudioProfile::Mulaw,
        IvrMode::Disabled,
        true,
        Some(true),
    );
}

fn product_outgoing_with_policy(
    radio: bool,
    immediate: bool,
    latching: bool,
    selected: AudioProfile,
    ivr: IvrMode,
    isolated: bool,
    bypass_all: Option<bool>,
) {
    const CHILD: &str = "RPTADV_TEST_OUTGOING_PRODUCT";
    if std::env::var_os(CHILD).is_none() {
        let name = if bypass_all == Some(true) {
            "host_services::product_phone_tests::sip::outgoing::actual_unlocked_phone_bypass_allows_eligible_peer_playout"
        } else if bypass_all == Some(false) {
            "host_services::product_phone_tests::sip::outgoing::actual_unlocked_phone_bypass_preserves_local_only_playout"
        } else if selected == AudioProfile::Ilbc30 {
            if ivr == IvrMode::Events {
                "host_services::product_phone_tests::sip::outgoing::actual_ilbc_radio_digits_preserve_private_native_events"
            } else if ivr == IvrMode::InBand {
                "host_services::product_phone_tests::sip::outgoing::actual_ilbc_radio_digits_preserve_private_inband"
            } else {
                "host_services::product_phone_tests::sip::outgoing::actual_ilbc_quiet_call_preserves_thirty_ms_rtp"
            }
        } else if selected == AudioProfile::L16Wideband {
            match ivr {
                IvrMode::MixedClock => {
                    "host_services::product_phone_tests::sip::outgoing::actual_l16_mixed_event_answer_keeps_native_receive_and_private_inband_transmit"
                }
                IvrMode::Events => {
                    "host_services::product_phone_tests::sip::outgoing::actual_l16_matched_events_preserve_native_digits_and_clock"
                }
                IvrMode::InBand => {
                    "host_services::product_phone_tests::sip::outgoing::actual_l16_absent_events_preserve_private_inband"
                }
                IvrMode::Disabled => {
                    "host_services::product_phone_tests::sip::outgoing::actual_l16_quiet_call_preserves_zero_receiver_key_and_continuous_rtp"
                }
            }
        } else if selected == AudioProfile::Gsm {
            "host_services::product_phone_tests::sip::outgoing::actual_gsm_two_way_phone_preserves_quiet_rtp"
        } else if selected == AudioProfile::G726 {
            "host_services::product_phone_tests::sip::outgoing::actual_g726_two_way_phone_preserves_quiet_rtp"
        } else if selected == AudioProfile::G726Aal2 {
            "host_services::product_phone_tests::sip::outgoing::actual_aal2_g726_two_way_phone_preserves_quiet_rtp"
        } else if selected == AudioProfile::L16Narrowband {
            "host_services::product_phone_tests::sip::outgoing::actual_l16_narrowband_two_way_phone_preserves_quiet_rtp"
        } else if selected == AudioProfile::G722 {
            "host_services::product_phone_tests::sip::outgoing::actual_product_g722_two_way_phone_preserves_wideband_pcm"
        } else if isolated {
            "host_services::product_phone_tests::sip::outgoing::actual_outgoing_call_isolates_and_restores_existing_ordinary_link"
        } else if ivr == IvrMode::InBand && selected == AudioProfile::Alaw {
            "host_services::product_phone_tests::sip::outgoing::actual_alaw_radio_digits_use_private_inband"
        } else if ivr == IvrMode::Events && selected == AudioProfile::Alaw {
            "host_services::product_phone_tests::sip::outgoing::actual_alaw_radio_digits_reach_telephone_ivr_without_audio_leak"
        } else if ivr == IvrMode::InBand {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_digits_use_private_inband_when_event_payload_is_absent"
        } else if ivr == IvrMode::Events {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_digits_reach_telephone_ivr_without_control_or_audio_leak"
        } else if selected == AudioProfile::Alaw {
            "host_services::product_phone_tests::sip::outgoing::actual_alaw_quiet_call_sends_continuous_silence_without_receiver_key"
        } else if latching {
            "host_services::product_phone_tests::sip::outgoing::actual_answered_quiet_call_opens_symmetric_rtp_before_any_receiver_key"
        } else if radio && immediate {
            "host_services::product_phone_tests::sip::outgoing::actual_rf_answer_first_utterance_reaches_rf_without_receiver_rekey"
        } else if immediate {
            "host_services::product_phone_tests::sip::outgoing::actual_answer_first_utterance_reaches_rf_without_any_receiver_key"
        } else if radio {
            "host_services::product_phone_tests::sip::outgoing::actual_radio_dtmf_originates_two_way_phone_and_hangs_up"
        } else {
            "host_services::product_phone_tests::sip::outgoing::actual_product_originates_two_way_phone_and_releases_the_exact_call"
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "outgoing product child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let remote_audio_payload = if selected == AudioProfile::L16Wideband {
        113
    } else {
        selected.payload()
    };
    SELECTED_PAYLOAD.store(usize::from(remote_audio_payload), Ordering::Release);
    RTP_PACKET_TICKS.store(
        (selected.parameters().packet_frames * selected.parameters().rtp_clock_hz
            / selected.parameters().pcm_rate_hz) as usize,
        Ordering::Release,
    );
    let directory =
        std::env::temp_dir().join(format!("rpt-outgoing-product-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("phone.secret"), "synthetic-test-secret\n").unwrap();
    fs::set_permissions(
        directory.join("phone.secret"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let sip = UdpSocket::bind("127.0.0.1:0").unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
    rtp.set_nonblocking(true).unwrap();
    let mut linked = isolated.then(vox::Remote::new);
    // Admission default deliberately differs from the eligible initiating node.
    let configuration = format!(
        "[general]\nnode_enabled=yes\n[1000]\nradio_channel=fixture\nduplex=full\nphone_allow_accounts=office\n[2000]\nnode_enabled=no\n[phone_account office]\nlocal_node=2000\nattach_local_nodes=1000\nenabled=yes\nprotocol=sip\nserver={}\nusername=fixture-user\nsecrets_file=phone.secret\nregister=yes\n",
        sip.local_addr().unwrap()
    );
    let configuration = if radio {
        configuration.replace("[1000]", "[1000]\nlink_command_phone_dial=6\nlink_command_phone_hangup=0\nphone_outgoing_account=office") + "\n[morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n"
    } else {
        configuration
    };
    let configuration = if let Some(all) = bypass_all {
        let digest = include_str!("../../../product/src/tests/fixtures/phone_mailbox.conf")
            .lines()
            .find_map(|line| line.strip_prefix("pin_hash="))
            .unwrap();
        configuration.replace("[1000]", &format!(
            "[1000]\nlink_command_phone_bypass=99\nphone_outgoing_playout={}\ndtmf_admin_unlock_hash={digest}\ndtmf_admin_lock_hash={digest}\ndtmf_admin_timeout_ms=2000",
            if all { "all" } else { "local" },
        ))
    } else {
        configuration
    };
    // IVR and protected commands use the deployed receive reserve.
    let configuration = if immediate || ivr != IvrMode::Disabled || bypass_all.is_some() {
        configuration.replace("[1000]", "[1000]\nfull_duplex=yes\nsquelch_delay_ms=30")
            + "\n[radio]\nradio_duplex_mode=half\n[radio 1000]\nvad_mode=kerchunk\nvad_hang_ms=1000\n"
    } else {
        configuration
    };
    let configuration = if let Some(remote) = linked.as_ref() {
        let path = directory.join("nodes.conf");
        fs::write(
            &path,
            format!("[extnodes]\n2000=radio@{}/2000,127.0.0.1\n", remote.address),
        )
        .unwrap();
        configuration.replace(
            "[1000]",
            &format!(
                "[1000]\nlink_lookup_method=file\nlink_static_directory_file={}",
                path.display()
            ),
        )
    } else {
        configuration
    };
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt.conf"),
            resolve_phone_accounts(&configuration).unwrap(),
            &PreparedAccounts::default(),
            |_| unreachable!(),
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
    host.descriptor.peer_read_source = Some(observe_peer_read_source);
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
    let (register, target) = receive(&sip);
    assert!(register.starts_with("REGISTER "));
    sip.send_to(response(&register, 200, false).as_bytes(), target)
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while worker.observe().unwrap()[0].1.state != 2 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    runtime.publish_sip(Some(&worker), &accounts);
    if let Some(remote) = linked.as_mut() {
        runtime.enable_listener(&mut remote.server).unwrap();
        let link = runtime.product_descriptor().link_command.unwrap();
        let connecting =
            thread::spawn(move || unsafe { link(c"1000".as_ptr(), 4, c"2000".as_ptr(), 4, 1) });
        let deadline = Instant::now() + Duration::from_secs(8);
        while !connecting.is_finished() && Instant::now() < deadline {
            remote.pump();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            connecting.is_finished(),
            "ordinary fixture connect is bounded"
        );
        assert_eq!(connecting.join().unwrap(), 0);
        wait_linked_quiet(remote);
        remote.clear_voice_observation();
        LOCAL_INPUT.store(true, Ordering::Release);
        pump_linked(remote, Duration::from_millis(1000));
        LOCAL_INPUT.store(false, Ordering::Release);
        pump_linked(remote, Duration::from_millis(500));
        assert!(
            remote.voiced_samples() > 160,
            "ordinary outbound works before isolation"
        );
        wait_linked_quiet(remote);
        let before = PUBLIC_OUTPUT.load(Ordering::Acquire);
        remote.request_return();
        pump_linked(remote, Duration::from_millis(1000));
        assert_eq!(remote.sent_return_packets(), 15);
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before + 160,
            "ordinary inbound works before isolation"
        );
        wait_linked_quiet(remote);
        PUBLIC_OUTPUT.store(0, Ordering::Release);
    }
    let mut before_dial_ack = 0;
    let (refused, id) = if radio {
        radio_digits(b"*600123#");
        before_dial_ack = PUBLIC_OUTPUT.load(Ordering::Acquire);
        (0, 0)
    } else {
        let refused = next_call(&runtime);
        let (invite, remote) = receive(&sip);
        assert!(invite.starts_with("INVITE sip:called@fixture.invalid "));
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_DIAL, 0),
            (-1, 0),
            "one pending claim before answer"
        );
        sip.send_to(response(&invite, 486, false).as_bytes(), remote)
            .unwrap();
        assert!(receive(&sip).0.starts_with("ACK "));
        let id = next_call(&runtime);
        assert_ne!(id, refused);
        (refused, id)
    };
    let (invite, remote) = if radio {
        // Move the existing finite-result bound ahead of INVITE, not a guessed sleep.
        sip.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        receive_register(&sip, &mut [0; 8192])
            .expect("finite acknowledgment before outgoing INVITE")
    } else {
        receive(&sip)
    };
    if radio {
        assert!(
            invite.starts_with(&format!("INVITE sip:00123@{} ", sip.local_addr().unwrap())),
            "literal telephone digits retain leading zeros"
        );
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_DIAL, 0),
            (-1, 0),
            "RF command reserved the same pending owner before answer"
        );
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before_dial_ack + 160,
            "finite LOCAL acknowledgment must finish before INVITE"
        );
        let before = PUBLIC_OUTPUT.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            before,
            "acknowledgment still producing after INVITE"
        );
    }
    if let Some(remote) = linked.as_mut() {
        // Drain only already-received pre-cut loopback packets before observing
        // new programme. This is not a downstream network-expiry assertion.
        remote.pump();
        remote.clear_voice_observation();
    }
    let offer =
        MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
    let before_ringing = PUBLIC_OUTPUT.load(Ordering::Acquire);
    sip.send_to(response(&invite, 180, false).as_bytes(), remote)
        .unwrap();
    if radio {
        let until = Instant::now() + Duration::from_secs(3);
        while PUBLIC_OUTPUT.load(Ordering::Acquire) <= before_ringing + 160
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before_ringing + 160,
            "actual180 must produce LOCAL ringing before answer"
        );
    }
    assert!(worker.incoming().is_none());
    let event_clock = if ivr == IvrMode::MixedClock {
        8000
    } else {
        selected.parameters().rtp_clock_hz
    };
    let local_event_payload = if event_clock == 16000 { 102 } else { 101 };
    let remote_event_payload = if selected == AudioProfile::L16Wideband {
        if event_clock == 16000 { 121 } else { 120 }
    } else {
        local_event_payload
    };
    let answer = MediaDescription {
        codec: selected,
        receive_payload: remote_audio_payload,
        send_payload: remote_audio_payload,
        telephone_events: [8000, 16000].map(|rate| {
            (ivr != IvrMode::InBand && rate == event_clock)
                .then_some(TelephoneEvent::symmetric(remote_event_payload, rate))
        }),
        ..offer.clone()
    }
    .answer(rtp.local_addr().unwrap(), 99)
    .unwrap();
    let accepted = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:called@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        sip.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    let rx_keys_at_answer = LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire);
    let linked_before_answer = LINKED_OUTPUT_TURNS.load(Ordering::Acquire);
    sip.send_to(accepted.as_bytes(), remote).unwrap();
    if !immediate {
        assert!(receive(&sip).0.starts_with("ACK "));
    }
    sip.set_nonblocking(true).unwrap();
    let mut caller = Caller {
        sip,
        rtp,
        target,
        media: offer.remote,
        to: String::new(),
        sequence: 0,
        timestamp: 0,
        keepalive: false,
        profile: selected,
        events: ivr != IvrMode::InBand,
        event_clock,
        event_payload: local_event_payload,
    };
    let mut codec = AudioCodec::load(selected).unwrap();
    let mut returned = Vec::new();
    if latching {
        let learned = latch(&caller);
        if learned.is_none() {
            assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
            // Negative control reproduces the reported unlock without allowing
            // it to satisfy the initial, no-RX media requirement.
            LOCAL_INPUT.store(true, Ordering::Release);
            let after_rx = latch(&caller);
            LOCAL_INPUT.store(false, Ordering::Release);
            assert_eq!(
                after_rx,
                Some(offer.remote),
                "receiver key must demonstrate the same socket can reach the relay"
            );
            panic!(
                "quiet answered call emitted no RTP; relay learned the source only after receiver key"
            );
        }
        assert_eq!(learned, Some(offer.remote));
        assert_eq!(LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire), 0);
        caller.media = learned.unwrap();
        CONTINUOUS_RTP.store(true, Ordering::Release);
        let packets = Duration::from_secs(30)
            .as_nanos()
            .div_ceil(caller.packet_interval().as_nanos()) as u64;
        quiet_media(&mut caller, &mut codec, packets, None);
        // An initial admission cut cannot excuse a later unrelated reset.
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
        ALLOW_SOURCE_CUT.store(true, Ordering::Release);
    }
    if !immediate {
        media(
            &mut caller,
            &mut codec,
            &vec![0.0; selected.parameters().pcm_rate_hz as usize / 2],
            &mut returned,
            linked.as_mut(),
        );
    }
    if !radio {
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            0,
            "no login prompt or automatic RF output"
        );
    }
    if radio && !immediate {
        let after_answer = PUBLIC_OUTPUT.load(Ordering::Acquire);
        media(
            &mut caller,
            &mut codec,
            &vec![0.0; selected.parameters().pcm_rate_hz as usize / 5],
            &mut returned,
            linked.as_mut(),
        );
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            after_answer,
            "ringback or a repeated acknowledgment continued after answer"
        );
    }
    if let Some(remote) = linked.as_mut() {
        remote.request_return();
        let before = PUBLIC_OUTPUT.load(Ordering::Acquire);
        media(
            &mut caller,
            &mut codec,
            &vec![0.0; selected.parameters().pcm_rate_hz as usize],
            &mut returned,
            Some(&mut *remote),
        );
        assert_eq!(remote.sent_return_packets(), 15);
        assert_eq!(
            PUBLIC_OUTPUT.load(Ordering::Acquire),
            before,
            "isolated ordinary inbound cannot key or feed local RF"
        );
        assert!(
            returned.iter().all(|sample| sample.abs() <= 0.01),
            "isolated ordinary inbound cannot reach the telephone"
        );
    }
    let before_voice = PUBLIC_OUTPUT.load(Ordering::Acquire);
    let speech: Vec<f32> =
        include_bytes!("../../../product/src/tests/fixtures/phone-speech/yes-48000-s16le.pcm")
            .chunks_exact((48000 / selected.parameters().pcm_rate_hz * 2) as usize)
            .map(|bytes| f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0)
            .collect();
    media(
        &mut caller,
        &mut codec,
        &speech,
        &mut returned,
        linked.as_mut(),
    );
    media(
        &mut caller,
        &mut codec,
        &vec![0.0; selected.parameters().pcm_rate_hz as usize * 2],
        &mut returned,
        linked.as_mut(),
    );
    if immediate {
        assert_eq!(
            ANSWER_ACKS.load(Ordering::Acquire),
            1,
            "answer ACK is serviced while the immediate greeting is sent"
        );
        assert_eq!(
            LOCAL_RX_KEYED_TURNS.load(Ordering::Acquire),
            rx_keys_at_answer,
            "first callee utterance must not depend on any receiver rekey after answer"
        );
        if !radio {
            assert_eq!(rx_keys_at_answer, 0);
        }
        assert!(
            LINKED_OUTPUT_TURNS.load(Ordering::Acquire) > linked_before_answer,
            "actual phone contribution must become active in keyed RF output before any receiver rekey"
        );
    }
    assert!(
        PUBLIC_OUTPUT.load(Ordering::Acquire) > before_voice + 160,
        "called-party speech reaches actual node RF mix through VOX"
    );
    assert!(
        !LAST_PTT.load(Ordering::Acquire),
        "existing VOX/hang releases PTT"
    );
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "callee hears neither self echo nor incoming login prompts"
    );
    LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
    let radio_before = PUBLIC_OUTPUT.load(Ordering::Acquire);
    let digits_before = SOURCE_DIGITS.load(Ordering::Acquire);
    caller.digits(b"*4#*70");
    media(
        &mut caller,
        &mut codec,
        &vec![0.0; selected.parameters().pcm_rate_hz as usize / 2],
        &mut returned,
        linked.as_mut(),
    );
    assert_eq!(
        PUBLIC_OUTPUT.load(Ordering::Acquire),
        radio_before,
        "callee DTMF grants no node control"
    );
    if selected == AudioProfile::L16Wideband {
        assert_eq!(
            SOURCE_DIGITS.load(Ordering::Acquire) - digits_before,
            if ivr == IvrMode::InBand { 0 } else { 6 },
            "negotiated native RX credits each completed key once without outgoing-role authority"
        );
    }
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "no private control result for outgoing role"
    );
    assert!(worker.incoming().is_none());
    returned.clear();
    if latching {
        // Continue the established incoming source with real silence instead of
        // introducing missing RTP during this healthy post-rearm cadence phase.
        quiet_media(&mut caller, &mut codec, 100, Some(digits_before + 6));
        LAST_MARKED_CUT.store(SOURCE_CUTS.load(Ordering::Acquire), Ordering::Release);
        ALLOW_SOURCE_CUT.store(true, Ordering::Release);
    }
    assert_radio_return(&mut caller, &mut codec, linked.as_mut());
    if let Some(remote) = linked.as_ref() {
        assert_eq!(
            remote.voiced_samples(),
            0,
            "neither callee nor local RF programme may reach the isolated ordinary link"
        );
    }
    CONTINUOUS_RTP.store(false, Ordering::Release);
    if ivr != IvrMode::Disabled {
        telephone_ivr(&mut caller, &mut codec, ivr, linked.as_mut());
        // Finite private digit playback must yield to the retained programme.
        assert_radio_return(&mut caller, &mut codec, linked.as_mut());
        if let Some(remote) = linked.as_ref() {
            assert_eq!(
                remote.voiced_samples(),
                0,
                "IVR and resumed RF audio remain isolated"
            );
        }
    }
    if let Some(all) = bypass_all {
        assert_call_bypass(
            &mut caller,
            &mut codec,
            linked.as_mut().unwrap(),
            &speech,
            all,
        );
    }
    if radio {
        radio_digits(b"*0#");
    } else {
        assert_eq!(
            command(&runtime, abi::RPTADV_PHONE_HANGUP, refused),
            (-1, 0),
            "stale id cannot end successor"
        );
        assert_eq!(command(&runtime, abi::RPTADV_PHONE_HANGUP, id), (0, id));
    }
    caller.sip.set_nonblocking(false).unwrap();
    let (bye, remote) = receive(&caller.sip);
    assert!(bye.starts_with("BYE "));
    caller
        .sip
        .send_to(response(&bye, 200, false).as_bytes(), remote)
        .unwrap();
    assert!(worker.incoming().is_none());
    if let Some(remote) = linked.as_mut() {
        // Exact hangup restores the same transport after a fresh graph cut.
        wait_linked_quiet(remote);
        remote.clear_voice_observation();
        LOCAL_INPUT.store(true, Ordering::Release);
        pump_linked(remote, Duration::from_millis(1000));
        LOCAL_INPUT.store(false, Ordering::Release);
        pump_linked(remote, Duration::from_millis(500));
        assert!(
            remote.voiced_samples() > 160,
            "ordinary outbound restored after exact hangup"
        );
        wait_linked_quiet(remote);
        let before = PUBLIC_OUTPUT.load(Ordering::Acquire);
        remote.request_return();
        pump_linked(remote, Duration::from_millis(1000));
        assert_eq!(remote.sent_return_packets(), 15);
        assert!(
            PUBLIC_OUTPUT.load(Ordering::Acquire) > before + 160,
            "ordinary inbound restored after exact hangup"
        );
    }
    if radio {
        runtime.stop().unwrap();
        worker.stop().unwrap();
        assert_radio_stopped(true);
        fs::remove_dir_all(directory).unwrap();
        return;
    }
    // This synthetic telephone reuses its socket, so discard only the ended
    // dialog's already-received packets before selecting the new offer endpoint.
    while caller.rtp.recv_from(&mut [0; 2048]).is_ok() {}
    // Establish another exact call, then let the telephone end the dialog.
    let remote_ended = next_call(&runtime);
    assert_ne!(remote_ended, id);
    let (invite, remote) = receive(&caller.sip);
    let next_offer =
        MediaDescription::parse(invite.split_once("\r\n\r\n").unwrap().1.as_bytes()).unwrap();
    let answer = MediaDescription {
        codec: selected,
        receive_payload: remote_audio_payload,
        send_payload: remote_audio_payload,
        telephone_events: [8000, 16000].map(|rate| {
            (ivr != IvrMode::InBand && rate == event_clock)
                .then_some(TelephoneEvent::symmetric(remote_event_payload, rate))
        }),
        ..next_offer.clone()
    }
    .answer(caller.rtp.local_addr().unwrap(), 100)
    .unwrap();
    let accepted = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n", &format!(
        "Contact: <sip:called@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        caller.sip.local_addr().unwrap(), answer.len(), std::str::from_utf8(&answer).unwrap()));
    caller.sip.send_to(accepted.as_bytes(), remote).unwrap();
    assert!(receive(&caller.sip).0.starts_with("ACK "));
    caller.sip.set_nonblocking(true).unwrap();
    caller.media = next_offer.remote;
    caller.sequence = 0;
    caller.timestamp = 0;
    // Service real source callbacks before the remote close, without a login.
    media(
        &mut caller,
        &mut codec,
        &vec![0.0; selected.parameters().pcm_rate_hz as usize / 5],
        &mut Vec::new(),
        linked.as_mut(),
    );
    let (_, dialog) = fields(invite.as_bytes());
    let contact = crate::phone::sip_test_registrar::header(&invite, "Contact")
        .trim_start_matches('<')
        .split('>')
        .next()
        .unwrap();
    let bye = format!(
        "BYE {contact} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-outgoing-peer-bye;rport\r\nMax-Forwards: 70\r\nFrom: {};tag=fixture\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 BYE\r\nContent-Length: 0\r\n\r\n",
        caller.sip.local_addr().unwrap(),
        dialog["to"],
        dialog["from"],
        dialog["call-id"]
    );
    caller.sip.send_to(bye.as_bytes(), remote).unwrap();
    caller.sip.set_nonblocking(false).unwrap();
    let completed = receive(&caller.sip).0;
    assert!(
        completed.starts_with("SIP/2.0 200 ") && fields(completed.as_bytes()).1["cseq"] == "1 BYE"
    );
    let final_id = next_call(&runtime);
    assert_ne!(
        final_id, remote_ended,
        "remote BYE releases its exact active claim"
    );
    let (invite, remote) = receive(&caller.sip);
    caller
        .sip
        .send_to(response(&invite, 486, false).as_bytes(), remote)
        .unwrap();
    assert!(receive(&caller.sip).0.starts_with("ACK "));
    runtime.stop().unwrap();
    worker.stop().unwrap();
    assert_radio_stopped(true);
    fs::remove_dir_all(directory).unwrap();
}

fn assert_radio_return(
    caller: &mut Caller,
    codec: &mut AudioCodec,
    mut linked: Option<&mut vox::Remote>,
) {
    let mut returned = Vec::new();
    let pcm_rate = caller.profile.parameters().pcm_rate_hz as usize;
    LOCAL_INPUT.store(true, Ordering::Release);
    media(
        caller,
        codec,
        &vec![0.0; pcm_rate],
        &mut returned,
        linked.as_deref_mut(),
    );
    LOCAL_INPUT.store(false, Ordering::Release);
    media(
        caller,
        codec,
        &vec![0.0; pcm_rate / 2],
        &mut returned,
        linked,
    );
    let energy: f32 = returned.iter().map(|sample| sample * sample).sum();
    let correlation = |lag: usize| {
        returned
            .iter()
            .zip(returned.iter().skip(lag))
            .map(|(a, b)| a * b)
            .sum::<f32>()
    };
    assert!(
        returned.iter().filter(|sample| sample.abs() > 0.01).count()
            > caller.profile.parameters().packet_frames as usize,
        "actual local RF input returns to the called telephone"
    );
    assert!(
        correlation(pcm_rate / 1000) > energy * 0.5 && correlation(pcm_rate / 2000) < -energy * 0.5,
        "known1kHz return pattern, not a status prompt"
    );
}

fn telephone_ivr(
    caller: &mut Caller,
    codec: &mut AudioCodec,
    mode: IvrMode,
    mut linked: Option<&mut vox::Remote>,
) {
    let input = b"12***4**#";
    let expected = b"12*4#";
    let start = Instant::now();
    let duration = Duration::from_millis(100 + input.len() as u64 * 280 + 1000);
    let mut next = start;
    let mut last = None;
    let mut events: Vec<(u32, u8, u16, u8)> = Vec::new();
    let mut decoder = ivr_decoder::Decoder::new(caller.profile.parameters().pcm_rate_hz);
    let mut decoded = Vec::with_capacity(16);
    let mut maximum_audio = 0.0f32;
    let mut nonzero_by_slot = [0usize; 12];
    let before_pcm = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    while start.elapsed() < duration {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let elapsed = start.elapsed().as_millis() as usize;
        let symbol = if elapsed < 100 {
            b' '
        } else {
            let index = (elapsed - 100) / 280;
            if index >= input.len() {
                0
            } else if (elapsed - 100) % 280 < 160 {
                input[index]
            } else {
                b' '
            }
        };
        LOCAL_DTMF.store(usize::from(symbol), Ordering::Release);
        caller.audio_payload(
            codec,
            &vec![0.0; caller.profile.parameters().packet_frames as usize],
            caller.profile.payload(),
        );
        if let Some(remote) = linked.as_mut() {
            // The existing ordinary transport stays live, but any DTMF event
            // is rejected by Remote::pump and all received PCM must be silent.
            remote.pump();
            assert_eq!(
                remote.voiced_samples(),
                0,
                "private telephone IVR leaked to the ordinary peer"
            );
        }
        service(caller);
        let mut wire = [0; 2048];
        while let Ok((length, source)) = caller.rtp.recv_from(&mut wire) {
            assert_eq!(source, caller.media);
            assert_eq!(wire[0], 0x80);
            let sequence = u16::from_be_bytes(wire[2..4].try_into().unwrap());
            let timestamp = u32::from_be_bytes(wire[4..8].try_into().unwrap());
            let ssrc = u32::from_be_bytes(wire[8..12].try_into().unwrap());
            if let Some((previous, original_ssrc)) = last {
                assert_eq!(sequence, u16::wrapping_add(previous, 1));
                assert_eq!(ssrc, original_ssrc);
            }
            last = Some((sequence, ssrc));
            if wire[1] & 0x7f != SELECTED_PAYLOAD.load(Ordering::Acquire) as u8 {
                let event_payload = if caller.profile == AudioProfile::L16Wideband {
                    121
                } else {
                    101
                };
                assert_eq!(wire[1] & 0x7f, event_payload);
                assert_eq!(length, 16);
                let ended = wire[13] & 0x80 != 0;
                let duration = u16::from_be_bytes(wire[14..16].try_into().unwrap());
                if events.last().is_none_or(|event| event.0 != timestamp) {
                    assert!(wire[1] & 0x80 != 0, "event onset marker");
                    if let Some(previous) = events.last() {
                        assert_eq!(previous.3, 3);
                    }
                    events.push((timestamp, wire[12], 0, 0));
                } else {
                    assert_eq!(wire[1] & 0x80, 0);
                }
                let event = events.last_mut().unwrap();
                assert_eq!(event.1, wire[12]);
                assert!(
                    duration >= event.2
                        && u32::from(duration) <= caller.profile.parameters().rtp_clock_hz / 5
                );
                event.2 = duration;
                if ended {
                    assert_eq!(
                        u32::from(duration),
                        caller.profile.parameters().rtp_clock_hz / 5
                    );
                    event.3 += 1;
                }
                assert!(event.3 <= 3);
            } else {
                assert_eq!(length, 12 + caller.profile.parameters().packet_bytes);
                assert_eq!(
                    wire[1] & 0x7f,
                    SELECTED_PAYLOAD.load(Ordering::Acquire) as u8
                );
                let mut pcm = vec![0.0; caller.profile.parameters().packet_frames as usize];
                codec.decode(&wire[12..length], &mut pcm).unwrap();
                if pcm.iter().any(|sample| sample.abs() > 0.01) {
                    nonzero_by_slot[(elapsed / 280).min(11)] += 1;
                }
                for sample in &pcm {
                    maximum_audio = maximum_audio.max(sample.abs());
                }
                decoder.process(&mut pcm, &mut decoded);
            }
        }
        next += caller.packet_interval();
        while next <= Instant::now() {
            caller.timestamp = caller.timestamp.wrapping_add(caller.packet_ticks());
            next += caller.packet_interval();
        }
    }
    LOCAL_DTMF.store(0, Ordering::Release);
    let symbols: Vec<u8> = events
        .iter()
        .map(|event| b"0123456789*#ABCD"[usize::from(event.1)])
        .collect();
    if mode == IvrMode::Events {
        assert_eq!(
            symbols, expected,
            "actual RF parser and SIP event destination"
        );
        assert!(events.iter().all(|event| event.3 == 3));
        assert!(
            decoded.is_empty(),
            "negotiated events must not duplicate as in-band digits"
        );
        assert!(
            maximum_audio <= 0.01,
            "RF DTMF leaked into telephone programme instead of negotiated events: max={maximum_audio}, nonzero_by_slot={nonzero_by_slot:?}"
        );
    } else {
        assert!(
            events.is_empty(),
            "unnegotiated telephone-event payload was sent"
        );
        assert_eq!(
            decoded, expected,
            "released detector decodes the private regenerated in-band digits"
        );
    }
    // Actual RF input may still key the local transmitter. Count audible PCM,
    // not the older combined PCM/PTT counter, to reject regenerated feedback.
    assert_eq!(
        PUBLIC_PCM_OUTPUT.load(Ordering::Acquire),
        before_pcm,
        "IVR digits created RF results/audio"
    );
}

/// Exercise protected bypass through real RF tones while the same media owners
/// continue paced RTP and ordinary control; no direct command shortcut.
fn assert_call_bypass(
    caller: &mut Caller,
    codec: &mut AudioCodec,
    remote: &mut vox::Remote,
    speech: &[f32],
    all: bool,
) {
    let mut returned = Vec::new();
    let before = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
    media(
        caller,
        codec,
        &[0.0; 800],
        &mut returned,
        Some(&mut *remote),
    );
    for &digit in b"*8001234#*99#" {
        LOCAL_DTMF.store(usize::from(digit), Ordering::Release);
        media(
            caller,
            codec,
            &[0.0; 1280],
            &mut returned,
            Some(&mut *remote),
        );
        LOCAL_DTMF.store(usize::from(b' '), Ordering::Release);
        media(
            caller,
            codec,
            &[0.0; 960],
            &mut returned,
            Some(&mut *remote),
        );
    }
    LOCAL_DTMF.store(0, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut quiet_since = Instant::now();
    let mut public = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    loop {
        media(
            caller,
            codec,
            &[0.0; 160],
            &mut returned,
            Some(&mut *remote),
        );
        let current = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
        if current != public || LAST_PTT.load(Ordering::Acquire) {
            public = current;
            quiet_since = Instant::now();
        }
        if current > before && quiet_since.elapsed() >= Duration::from_millis(750) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "finite protected-command result must drain"
        );
    }
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "unlock/bypass and LOCAL result cannot reach the callee as PCM"
    );
    assert_eq!(
        remote.voiced_samples(),
        0,
        "unlock/bypass and LOCAL result cannot leak to the restored ordinary peer"
    );
    remote.clear_voice_observation();
    returned.clear();
    let before = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    media(caller, codec, speech, &mut returned, Some(&mut *remote));
    media(
        caller,
        codec,
        &[0.0; 16000],
        &mut returned,
        Some(&mut *remote),
    );
    assert!(
        PUBLIC_PCM_OUTPUT.load(Ordering::Acquire) > before + 160,
        "bypassed telephone speech still reaches local RF"
    );
    assert_eq!(
        remote.voiced_samples() > 160,
        all,
        "configured outgoing audience alone selects eligible ordinary peers"
    );
    if !all {
        assert_eq!(
            remote.voiced_samples(),
            0,
            "local-only means no callee PCM to peers"
        );
    }
    assert!(
        returned.iter().all(|sample| sample.abs() <= 0.01),
        "bypass preserves telephone mix-minus"
    );
    // The two-second source grant has expired during result/speech playback.
    // The applied bypass remains attached to the call, not that grant timer.
    remote.clear_voice_observation();
    returned.clear();
    let before = PUBLIC_PCM_OUTPUT.load(Ordering::Acquire);
    remote.request_return();
    media(
        caller,
        codec,
        &[0.0; 8000],
        &mut returned,
        Some(&mut *remote),
    );
    assert_eq!(remote.sent_return_packets(), 15);
    assert!(
        PUBLIC_PCM_OUTPUT.load(Ordering::Acquire) > before + 160,
        "the same ordinary input is restored by call-scoped bypass"
    );
    assert!(
        returned.iter().any(|sample| sample.abs() > 0.01),
        "ordinary programme reaches the call after bypass"
    );
    remote.clear_voice_observation();
    assert_radio_return(caller, codec, Some(&mut *remote));
    assert!(
        remote.voiced_samples() > 160,
        "local-only telephone audience does not disable ordinary RF forwarding"
    );
}

#[test]
fn actual_product_g722_two_way_phone_preserves_wideband_pcm() {
    product_outgoing(false, true, true, AudioProfile::G722, false);
}

#[test]
fn actual_l16_mixed_event_answer_keeps_native_receive_and_private_inband_transmit() {
    product_outgoing_with_policy(
        true,
        false,
        false,
        AudioProfile::L16Wideband,
        IvrMode::MixedClock,
        true,
        None,
    );
}
#[test]
fn actual_l16_matched_events_preserve_native_digits_and_clock() {
    product_outgoing_with_policy(
        true,
        false,
        false,
        AudioProfile::L16Wideband,
        IvrMode::Events,
        true,
        None,
    );
}
#[test]
fn actual_l16_absent_events_preserve_private_inband() {
    product_outgoing_with_policy(
        true,
        false,
        false,
        AudioProfile::L16Wideband,
        IvrMode::InBand,
        true,
        None,
    );
}

#[test]
fn actual_l16_quiet_call_preserves_zero_receiver_key_and_continuous_rtp() {
    // Quiet/latch requires no earlier RF activity; isolation fixtures deliberately
    // key RF before origination to prove the ordinary peer works before the cut.
    product_outgoing(false, true, true, AudioProfile::L16Wideband, false);
}

#[test]
fn actual_gsm_two_way_phone_preserves_quiet_rtp() {
    product_outgoing(false, true, true, AudioProfile::Gsm, false);
}

#[test]
fn actual_g726_two_way_phone_preserves_quiet_rtp() {
    product_outgoing(false, true, true, AudioProfile::G726, false);
}

#[test]
fn actual_aal2_g726_two_way_phone_preserves_quiet_rtp() {
    product_outgoing(false, true, true, AudioProfile::G726Aal2, false);
}

#[test]
fn actual_l16_narrowband_two_way_phone_preserves_quiet_rtp() {
    product_outgoing(false, true, true, AudioProfile::L16Narrowband, false);
}

#[test]
fn actual_ilbc_quiet_call_preserves_thirty_ms_rtp() {
    product_outgoing(false, true, true, AudioProfile::Ilbc30, false);
}
#[test]
fn actual_ilbc_radio_digits_preserve_private_native_events() {
    product_outgoing_with_ivr(
        true,
        false,
        false,
        AudioProfile::Ilbc30,
        IvrMode::Events,
        true,
    );
}
#[test]
fn actual_ilbc_radio_digits_preserve_private_inband() {
    product_outgoing_with_ivr(
        true,
        false,
        false,
        AudioProfile::Ilbc30,
        IvrMode::InBand,
        true,
    );
}
