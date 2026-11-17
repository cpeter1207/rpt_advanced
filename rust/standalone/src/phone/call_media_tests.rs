//! Real RTP, released codec/jitter, and exact source-sink credit tests.
use super::*;
use crate::phone::media::TelephoneEvent;
use crate::providers::ProviderSet;
use std::{ffi::c_void, net::UdpSocket, ptr, time::Duration};

#[derive(Default)]
struct Sink {
    credit: usize,
    allow_zero: bool,
    refuse_digit: bool,
    audio: Vec<(u64, usize, u64)>,
    pcm: Vec<f32>,
    flags: Vec<u32>,
    gaps: Vec<(u64, u32)>,
    digits: Vec<u32>,
    discontinuities: usize,
    ranges: Vec<(u64, usize)>,
    adaptations: Vec<(u64, u32, usize)>,
    refuse_adaptation: bool,
    refuse_discontinuity: bool,
}
unsafe extern "C" fn audio(
    context: *mut c_void,
    start: u64,
    samples: *const f32,
    count: usize,
    concealed: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.flags.push(concealed);
    let accepted = count.min(sink.credit);
    assert!(
        unsafe { std::slice::from_raw_parts(samples, count) }
            .iter()
            .all(|value| value.is_finite() && (sink.allow_zero || *value != 0.0))
    );
    sink.audio
        .push((start, accepted, unsafe { (*birth).born_ns }));
    sink.ranges.push((start, accepted));
    sink.pcm
        .extend_from_slice(unsafe { std::slice::from_raw_parts(samples, accepted) });
    accepted
}
unsafe extern "C" fn gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    let accepted = count.min(sink.credit as u32);
    sink.gaps.push((start, accepted));
    sink.ranges.push((start, accepted as usize));
    accepted
}
unsafe extern "C" fn digit(context: *mut c_void, symbol: u32) -> i32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    if sink.refuse_digit {
        return 1;
    }
    sink.digits.push(symbol);
    0
}
unsafe extern "C" fn discontinuity(context: *mut c_void) -> i32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.discontinuities += 1;
    i32::from(sink.refuse_discontinuity)
}
fn read(media: &mut Media, sink: &mut Sink, now: u64) -> u32 {
    try_read(media, sink, now).unwrap()
}
fn try_read(media: &mut Media, sink: &mut Sink, now: u64) -> Result<u32, MediaError> {
    let input = abi::rptadv_peer_source_sink_v1 {
        struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
        abi_version: 1,
        context: ptr::from_mut(sink).cast(),
        host_clock_epoch: 7,
        reference_now_ns: now,
        audio_timed: Some(audio),
        gap: Some(gap),
        digit: Some(digit),
        source_discontinuity: Some(discontinuity),
        ..unsafe { std::mem::zeroed() }
    };
    let mut output: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
    unsafe { media.read(0, &input, &mut output) }?;
    assert_eq!(output.state, 1);
    Ok(output.progress)
}
fn packet(sequence: u16, timestamp: u32, payload_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x80, payload_type];
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&19u32.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

async fn fixture_media(
    sender: &UdpSocket,
    jitter: &'static abi::rptadv_jitter_descriptor_v1,
) -> Media {
    let offer = format!(
        "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n",
        sender.local_addr().unwrap().port()
    );
    Media::new(
        offer.as_bytes(),
        1,
        jitter,
        &super::super::codec::load_catalog().unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn source_retains_refused_prefix_birth_digits_and_gaps() {
    let providers = ProviderSet::load().unwrap();
    // The descriptor remains loaded until media has been dropped below.
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let target = media.endpoint.local_addr().unwrap();
    let timestamp = u32::MAX - 79;
    sender
        .send_to(&packet(1, timestamp, 0, &[0xa5; 160]), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let mut sink = Sink::default();
    let born = 1_000_000_000;
    assert_eq!(read(&mut media, &mut sink, born), 0);
    let receipt = media.births.iter().flatten().next().unwrap().1;
    assert!(receipt <= born);
    assert_eq!(read(&mut media, &mut sink, born + 40_000_000), 2);
    assert_eq!(
        media.jitter.put(
            ((1u64 << 32) + u64::from(timestamp)) + 160,
            160,
            born + 41_000_000,
            &[0xa5; 160]
        ),
        Err(rptadv_jitter_client::Error::OperationFailed),
        "a refused decoded decision must remain uncommitted"
    );
    sink.credit = 60;
    assert_eq!(read(&mut media, &mut sink, born + 80_000_000), 2);
    sink.credit = 160;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 1);
    let start = (1u64 << 32) + u64::from(timestamp);
    assert_eq!(
        sink.audio,
        [
            (start, 0, receipt),
            (start, 60, receipt),
            (start + 60, 100, receipt)
        ]
    );
    assert_eq!(sink.flags, [0, 0, 0]);
    let observation = media.observe().unwrap();
    assert_eq!(
        (
            observation.decode_recovery_frames,
            observation.native_plc_frames,
            observation.fallback_frames
        ),
        (0, 0, 0)
    );
    assert_eq!(sink.discontinuities, 1);
    assert_eq!(media.timestamp(80), Ok(start + 160));

    // Repeated event ends remain one digit even while the product refuses it.
    for sequence in [2, 3] {
        sender
            .send_to(&packet(sequence, 160, 101, &[11, 0x8a, 0, 160]), target)
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(2)).await;
    sink.refuse_digit = true;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 2);
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 2);
    sink.refuse_digit = false;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 1);
    assert_eq!(sink.digits, [u32::from(b'#')]);
    media.poll(born + 120_000_000).unwrap();
    assert!(!matches!(media.pending, Some(Pending::Digit(_))));

    media.pending = Some(Pending::Gap {
        start: start + 160,
        count: 160,
    });
    media.offset = 0;
    sink.credit = 0;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 2);
    sink.credit = 80;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 2);
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 1);
    assert_eq!(
        sink.gaps[sink.gaps.len() - 3..],
        [(start + 160, 0), (start + 160, 80), (start + 240, 80)]
    );
    drop(media);
}

async fn signed_offset(offset: i32) {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let target = media.endpoint.local_addr().unwrap();
    let born = 1_000_000_000;
    let start = (1u64 << 32) + 1000;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    sender
        .send_to(&packet(1, 1000, 0, &[0xa5; 160]), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(read(&mut media, &mut sink, born), 0);
    let second = (1160i32 + offset) as u32;
    let mut payload = [0xb5; 160];
    payload[..40].fill(0xa5);
    sender
        .send_to(&packet(2, second, 0, &payload), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(read(&mut media, &mut sink, born + 10_000_000), 0);
    let first_birth = media.births[0].unwrap().1;
    let second_birth = media.births[1].unwrap().1;
    assert!(first_birth <= born && second_birth <= born + 10_000_000);
    assert_eq!(read(&mut media, &mut sink, born + 40_000_000), 1);
    assert_eq!(sink.audio, [(start, 160, first_birth)]);
    sink.audio.clear();
    sink.pcm.clear();
    sink.credit = 0;
    assert_eq!(read(&mut media, &mut sink, born + 60_000_000), 2);
    if offset > 0 {
        sink.credit = 20;
        assert_eq!(read(&mut media, &mut sink, born + 70_000_000), 2);
        assert_eq!(read(&mut media, &mut sink, born + 80_000_000), 1);
        assert_eq!(
            sink.gaps,
            [(start + 160, 0), (start + 160, 20), (start + 180, 20)]
        );
        assert!(
            sink.audio.is_empty(),
            "missing prefix precedes the retained PCM"
        );
        sink.credit = 50;
        assert_eq!(read(&mut media, &mut sink, born + 90_000_000), 2);
        sink.credit = 160;
        assert_eq!(read(&mut media, &mut sink, born + 100_000_000), 1);
        assert_eq!(
            sink.audio,
            [
                (start + 200, 50, second_birth),
                (start + 250, 110, second_birth)
            ]
        );
    } else {
        sink.credit = 50;
        assert_eq!(read(&mut media, &mut sink, born + 70_000_000), 2);
        sink.credit = 160;
        assert_eq!(read(&mut media, &mut sink, born + 80_000_000), 1);
        assert!(
            sink.gaps.is_empty(),
            "already released samples are trimmed, not republished"
        );
        assert_eq!(
            sink.audio,
            [
                (start + 160, 0, second_birth),
                (start + 160, 50, second_birth),
                (start + 210, 70, second_birth)
            ]
        );
    }
    let mut reference = [0.0; 160];
    let mut codec = AudioCodec::load(super::super::media::AudioProfile::Mulaw).unwrap();
    codec.decode(&payload, &mut reference).unwrap();
    assert_eq!(sink.pcm, reference[if offset < 0 { 40 } else { 0 }..]);
}
#[tokio::test(flavor = "current_thread")]
async fn positive_jitter_offset_preserves_missing_prefix_and_packet_birth() {
    signed_offset(40).await;
}
#[tokio::test(flavor = "current_thread")]
async fn negative_jitter_offset_trims_overlap_without_losing_the_tail() {
    signed_offset(-40).await;
}

#[tokio::test(flavor = "current_thread")]
async fn jitter_delay_insertion_does_not_publish_a_source_gap() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let target = media.endpoint.local_addr().unwrap();
    let born = 1_000_000_000;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    sender
        .send_to(&packet(1, 1000, 0, &[0xa5; 160]), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    read(&mut media, &mut sink, born);
    read(&mut media, &mut sink, born + 40_000_000);
    sender
        .send_to(&packet(2, 1160, 0, &[0xa5; 160]), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    read(&mut media, &mut sink, born + 95_000_000);
    read(&mut media, &mut sink, born + 95_000_000);
    assert!(
        media.observe().unwrap().jitter.insertion_span > 0,
        "exercise an actual owned-provider insertion"
    );
    assert!(
        sink.gaps.is_empty(),
        "insertion delays playout but must not advance source coordinates"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn socket_backlog_keeps_receipt_age_before_dequeue_and_refused_credit() {
    for refused in [false, true] {
        let providers = ProviderSet::load().unwrap();
        let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let clock = std::time::Instant::now();
        let now = || 1_000_000_000 + clock.elapsed().as_nanos() as u64;
        let mut media = fixture_media(&sender, jitter).await;
        let target = media.endpoint.local_addr().unwrap();
        let mut sink = Sink::default();
        let timestamp = if refused {
            sender
                .send_to(&packet(1, 1000, 0, &[0xa5; 160]), target)
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
            read(&mut media, &mut sink, now());
            tokio::time::sleep(Duration::from_millis(40)).await;
            read(&mut media, &mut sink, now());
            assert!(matches!(media.pending, Some(Pending::Audio { .. })));
            1160
        } else {
            1000
        };
        sender
            .send_to(&packet(2, timestamp, 0, &[0xa5; 160]), target)
            .unwrap();
        // No owner reads this queued packet while the product retains its prefix.
        let queued_before = now();
        tokio::time::sleep(Duration::from_millis(250)).await;
        if refused {
            read(&mut media, &mut sink, now());
            assert!(matches!(media.pending, Some(Pending::Audio { .. })));
            sink.credit = 160;
            read(&mut media, &mut sink, now());
        }
        read(&mut media, &mut sink, now());
        let timestamp = (1u64 << 32) + u64::from(timestamp);
        let (_, born) = media
            .births
            .iter()
            .flatten()
            .find(|(start, _)| *start == timestamp)
            .copied()
            .unwrap();
        assert!(
            born <= queued_before,
            "dequeue must not rejuvenate a queued packet"
        );
        assert!(
            now() - born >= 250_000_000,
            "backpressure keeps original age"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_socket_receipt_mapping_is_a_hard_discontinuity() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    sender
        .send_to(
            &packet(1, 1000, 0, &[0xa5; 160]),
            media.endpoint.local_addr().unwrap(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    read(&mut media, &mut sink, 0);
    read(&mut media, &mut sink, 40_000_000);
    assert!(sink.audio.is_empty());
    assert_eq!(sink.gaps, [((1u64 << 32) + 1000, 160)]);
    assert_eq!(sink.discontinuities, 2, "unknown receipt is not timed loss");
}

#[tokio::test(flavor = "current_thread")]
async fn adaptive_delay_reduction_preserves_exact_source_gap_credit() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 40,
        ..Sink::default()
    };
    let origin = 1u64 << 32;
    // Use the real jitter's deterministic early-arrival trace. RTP parsing and
    // receipt retention are exercised separately; no provider decision is faked.
    for index in 0..100u64 {
        let timestamp = origin + 1000 + index * 160;
        let born = 1_000_000_000 + index * 20_000_000;
        media.clock = Some((timestamp as u32, timestamp));
        assert_eq!(
            media
                .jitter
                .put(timestamp, 160, born, &[0xa5; 160])
                .unwrap(),
            Admission::Accepted
        );
        media.births[index as usize % 64] = Some((timestamp, born));
        let now = born + 40_000_000;
        read(&mut media, &mut sink, now);
        if matches!(media.pending, Some(Pending::Audio { prefix, .. }) if prefix > 0) {
            let before = sink.audio.len();
            sink.credit = 0;
            assert_eq!(read(&mut media, &mut sink, now), 2);
            assert_eq!(
                sink.audio.len(),
                before,
                "refused gap cannot release later PCM"
            );
            sink.credit = 40;
        }
        while media.pending.is_some() {
            read(&mut media, &mut sink, now);
        }
    }
    assert!(
        media.observe().unwrap().jitter.skipped_span > 0,
        "exercise actual adaptive delay reduction"
    );
    assert!(
        sink.discontinuities > 1,
        "adaptation is a hard source discontinuity, not timed PLC"
    );
    let mut end = origin + 1000;
    for (start, count) in sink.ranges {
        assert_eq!(
            start, end,
            "every skipped interval must precede later PCM as credited gap"
        );
        end += count as u64;
    }
    assert!(
        !sink.gaps.is_empty(),
        "adaptation must be represented as source loss"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn outgoing_offer_keeps_one_port_selects_answer_and_discards_preanswer_packets() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let route = UdpSocket::bind("127.0.0.1:0").unwrap();
    let recipient = UdpSocket::bind("127.0.0.1:0").unwrap();
    recipient.set_nonblocking(true).unwrap();
    let offer = Offer::new(
        route.local_addr().unwrap(),
        77,
        Arc::new(super::super::codec::load_catalog().unwrap()),
    )
    .await
    .expect("outgoing call must reserve its RTP port before INVITE");
    let description = MediaDescription::parse(offer.description()).unwrap();
    let offered = description.remote;
    let text = std::str::from_utf8(offer.description()).unwrap();
    assert!(text.contains("RTP/AVP 0 8 9 3 96 97 98 99 100 101 102"));
    assert_eq!(
        description.telephone_events,
        [Some(TelephoneEvent::symmetric(101, 8000)), None]
    );
    // This came from the temporary route before final-answer adoption, not the recipient.
    route
        .send_to(&packet(1, 100, 0, &[0xa5; 160]), offered)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let answer = format!(
        "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n",
        recipient.local_addr().unwrap().port()
    );
    let mut media = offer.accept(answer.as_bytes(), jitter).await.unwrap();
    assert_eq!(
        media.endpoint.local_addr().unwrap(),
        offered,
        "answer must not replace the advertised socket"
    );
    media.poll(1_000_000_000).unwrap();
    assert!(
        media.births.iter().all(Option::is_none),
        "pre-answer datagram entered the selected media source"
    );
    media.reset().unwrap();
    assert_eq!(
        media
            .send(&[0xd5; 160], 0, media.elapsed_ms() + 100)
            .unwrap(),
        0
    );
    let mut wire = [0; 2048];
    let (count, source) = recipient.recv_from(&mut wire).unwrap();
    assert_eq!(source, offered);
    assert_eq!(count, 172);
    assert_eq!(
        wire[1] & 0x7f,
        8,
        "answer-selected G.711 codec controls RTP payload"
    );
    recipient
        .send_to(&packet(2, 260, 8, &[0xd5; 160]), offered)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    media.poll(1_010_000_000).unwrap();
    assert!(media.births.iter().any(Option::is_some));
}

#[tokio::test(flavor = "current_thread")]
async fn outgoing_offer_refuses_unoffered_events_and_uncleared_receive_backlog() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let route = UdpSocket::bind("127.0.0.1:0").unwrap();
    for crowded in [false, true] {
        let offer = Offer::new(
            route.local_addr().unwrap(),
            78,
            Arc::new(super::super::codec::load_catalog().unwrap()),
        )
        .await
        .unwrap();
        let target = MediaDescription::parse(offer.description()).unwrap().remote;
        if crowded {
            for sequence in 0..80 {
                route
                    .send_to(&packet(sequence, 100, 0, &[0xa5; 160]), target)
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let answer = MediaDescription {
            remote: route.local_addr().unwrap(),
            codec: super::super::media::AudioProfile::Mulaw,
            receive_payload: super::super::media::AudioProfile::Mulaw.payload(),
            send_payload: super::super::media::AudioProfile::Mulaw.payload(),
            telephone_events: [
                Some(TelephoneEvent::symmetric(
                    if crowded { 101 } else { 102 },
                    8000,
                )),
                None,
            ],
        }
        .answer(route.local_addr().unwrap(), 78)
        .unwrap();
        assert!(
            offer.accept(&answer, jitter).await.is_err(),
            "an unoffered event map or an unproven empty cut must not activate media"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn outgoing_answer_cannot_reinterpret_an_advertised_format_as_inband_fallback() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let recipient = UdpSocket::bind("127.0.0.1:0").unwrap();
    let valid = format!(
        "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n",
        recipient.local_addr().unwrap().port()
    );
    for (from, to) in [
        ("telephone-event/8000", "telephone-event/16000"),
        ("telephone-event/8000", "PCMU/8000"),
        ("RTP/AVP 0 101", "RTP/AVP 0 101 110"),
        ("a=fmtp:101 0-15", "a=fmtp:101 0-9"),
    ] {
        let offer = Offer::new(
            recipient.local_addr().unwrap(),
            79,
            Arc::new(super::super::codec::load_catalog().unwrap()),
        )
        .await
        .unwrap();
        assert!(
            offer
                .accept(valid.replace(from, to).as_bytes(), jitter)
                .await
                .is_err(),
            "unoffered answer format must be refused, not silently treated as inband DTMF"
        );
    }
    let offer = Offer::new(
        recipient.local_addr().unwrap(),
        80,
        Arc::new(super::super::codec::load_catalog().unwrap()),
    )
    .await
    .unwrap();
    let no_events = valid
        .replace("RTP/AVP 0 101", "RTP/AVP 0")
        .replace("a=rtpmap:101 telephone-event/8000\r\n", "")
        .replace("a=fmtp:101 0-15\r\n", "");
    assert!(
        offer
            .accept(no_events.as_bytes(), jitter)
            .await
            .unwrap()
            .digit_capabilities
            == 64,
        "genuine omission of events retains the existing inband fallback"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn g722_source_credit_uses_pcm_clock_and_keeps_receipt_birth_through_wrap() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let offer = format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 9 101\r\na=rtpmap:9 G722/8000\r\na=rtpmap:101 telephone-event/8000\r\na=ptime:20\r\n",
        sender.local_addr().unwrap().port()
    );
    let mut media = Media::new(
        offer.as_bytes(),
        1,
        jitter,
        &super::super::codec::load_catalog().unwrap(),
    )
    .await
    .unwrap();
    let target = media.endpoint.local_addr().unwrap();
    let mut encoder = AudioCodec::load(super::super::media::AudioProfile::G722).unwrap();
    let mut payload = [0; 160];
    encoder.encode(&[0.125; 320], &mut payload).unwrap();
    let timestamp = u32::MAX - 79;
    sender
        .send_to(&packet(1, timestamp, 9, &payload), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let mut sink = Sink {
        allow_zero: true,
        ..Default::default()
    };
    let born = 1_000_000_000;
    assert_eq!(read(&mut media, &mut sink, born), 0);
    let receipt = media.births.iter().flatten().next().unwrap().1;
    assert!(receipt <= born);
    assert_eq!(read(&mut media, &mut sink, born + 40_000_000), 2);
    sink.credit = 120;
    assert_eq!(read(&mut media, &mut sink, born + 80_000_000), 2);
    sink.credit = 320;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 1);
    let start = ((1u64 << 32) + u64::from(timestamp)) * 2;
    assert_eq!(
        sink.audio,
        [
            (start, 0, receipt),
            (start, 120, receipt),
            (start + 120, 200, receipt)
        ]
    );
    assert_eq!(media.timestamp(80), Ok(start + 320));
    assert_eq!(sink.pcm.len(), 320);
    media.reset().unwrap();
    assert_eq!(media.send(&payload, 20, media.elapsed_ms() + 100), Ok(0));
    let mut wire = [0; 2048];
    let count = sender.recv(&mut wire).unwrap();
    assert_eq!(count, 172);
    assert_eq!(wire[1] & 0x7f, 9);
    assert_eq!(u32::from_be_bytes(wire[4..8].try_into().unwrap()), 160);
}

#[tokio::test(flavor = "current_thread")]
async fn variable_source_spans_keep_original_birth_across_refused_prefixes() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    for (frames, born) in [(80_usize, 1_000_000_000), (240, 1_000_000_000_000)] {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let target = media.endpoint.local_addr().unwrap();
        sender
            .send_to(&packet(1, 1000, 0, &vec![0xa5; frames]), target)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let mut sink = Sink::default();
        assert_eq!(read(&mut media, &mut sink, born), 0);
        let receipt = media
            .births
            .iter()
            .flatten()
            .next()
            .expect("valid variable packet admitted")
            .1;
        assert!(receipt <= born);
        assert_eq!(read(&mut media, &mut sink, born + 40_000_000), 2);
        let storage = media.pcm.as_ptr();
        sink.credit = frames / 3;
        assert_eq!(read(&mut media, &mut sink, born + 50_000_000), 2);
        sink.credit = frames;
        assert_eq!(read(&mut media, &mut sink, born + 60_000_000), 1);
        let start = (1_u64 << 32) + 1000;
        assert_eq!(
            sink.audio,
            [
                (start, 0, receipt),
                (start, frames / 3, receipt),
                (start + (frames / 3) as u64, frames - frames / 3, receipt),
            ]
        );
        assert_eq!(sink.pcm.len(), frames);
        assert_eq!(media.source_end, Some(start + frames as u64));
        assert_eq!(media.pcm.as_ptr(), storage);
        assert!(sink.gaps.is_empty());
        assert_eq!(sink.discontinuities, 1);
        drop(media);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn variable_packet_offsets_preserve_exact_gap_overlap_and_birth() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    for offset in [-40_i32, 40] {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let target = media.endpoint.local_addr().unwrap();
        let now = 1_000_000_000;
        let start = (1_u64 << 32) + 1000;
        let mut sink = Sink {
            credit: 240,
            ..Sink::default()
        };
        sender
            .send_to(&packet(1, 1000, 0, &[0xa5; 80]), target)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(read(&mut media, &mut sink, now), 0);
        sender
            .send_to(&packet(2, (1080 + offset) as u32, 0, &[0xb5; 240]), target)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(read(&mut media, &mut sink, now + 10_000_000), 0);
        let first_birth = media.births[0].expect("first10ms packet admitted").1;
        let second_birth = media.births[1].expect("second30ms packet admitted").1;
        assert!(first_birth <= now && second_birth <= now + 10_000_000);
        assert_eq!(read(&mut media, &mut sink, now + 40_000_000), 1);
        assert_eq!(sink.audio, [(start, 80, first_birth)]);
        sink.audio.clear();
        sink.pcm.clear();
        sink.credit = 0;
        assert_eq!(read(&mut media, &mut sink, now + 60_000_000), 2);
        if offset > 0 {
            sink.credit = 40;
            assert_eq!(read(&mut media, &mut sink, now + 70_000_000), 1);
            assert_eq!(sink.gaps, [(start + 80, 0), (start + 80, 40)]);
            assert!(sink.audio.is_empty());
            sink.credit = 50;
            assert_eq!(read(&mut media, &mut sink, now + 80_000_000), 2);
            sink.credit = 240;
            assert_eq!(read(&mut media, &mut sink, now + 90_000_000), 1);
            assert_eq!(
                sink.audio,
                [
                    (start + 120, 50, second_birth),
                    (start + 170, 190, second_birth)
                ]
            );
        } else {
            sink.credit = 50;
            assert_eq!(read(&mut media, &mut sink, now + 70_000_000), 2);
            sink.credit = 240;
            assert_eq!(read(&mut media, &mut sink, now + 80_000_000), 1);
            assert!(sink.gaps.is_empty());
            assert_eq!(
                sink.audio,
                [
                    (start + 80, 0, second_birth),
                    (start + 80, 50, second_birth),
                    (start + 130, 150, second_birth)
                ]
            );
        }
        let mut expected = [0.0; 240];
        media.codec.decode(&[0xb5; 240], &mut expected).unwrap();
        assert_eq!(sink.pcm, expected[if offset < 0 { 40 } else { 0 }..]);
        assert_eq!(media.source_end, Some(start + (80 + offset + 240) as u64));
        drop(media);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn recovered_pcm_keeps_concealed_flag_and_samples_across_refusals() {
    use super::super::media::AudioProfile;
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let description = MediaDescription {
        remote: sender.local_addr().unwrap(),
        codec: AudioProfile::Ilbc30,
        receive_payload: 100,
        send_payload: 100,
        telephone_events: [None, None],
    };
    let endpoint = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), description.clone(), 1, 0)
        .await
        .unwrap();
    let catalog = super::super::codec::load_catalog().unwrap();
    let mut media =
        Media::from_endpoint(endpoint, description, Vec::new(), jitter, &catalog).unwrap();
    let mut packet = [0; 50];
    media.codec.encode(&[0.25; 240], &mut packet).unwrap();
    packet[49] |= 1;
    let mut reference = AudioCodec::load(AudioProfile::Ilbc30).unwrap();
    let mut expected = [0.0; 240];
    reference.decode(&packet, &mut expected).unwrap();
    media.packet[..50].copy_from_slice(&packet);
    media.last_birth = Some(5);
    media.pending = Some(Pending::Audio {
        start: 100,
        count: 240,
        born: 10,
        prefix: 0,
        decode: Some((50, 0)),
        pcm_start: 0,
    });
    let mut sink = Sink {
        allow_zero: true,
        ..Sink::default()
    };
    assert_eq!(read(&mut media, &mut sink, 20), 2);
    let retained = media.pcm.to_vec();
    sink.credit = 70;
    assert_eq!(read(&mut media, &mut sink, 20), 2);
    sink.credit = 240;
    assert_eq!(read(&mut media, &mut sink, 20), 1);
    assert_eq!(sink.flags, [1, 1, 1]);
    assert_eq!(sink.audio, [(100, 0, 5), (100, 70, 5), (170, 170, 5)]);
    assert_eq!(sink.pcm, expected);
    assert_eq!(&*media.pcm, retained);
    assert_eq!(
        media.last_birth,
        Some(5),
        "damaged packet receipt cannot renew source age"
    );
    assert_eq!(media.observe().unwrap().decode_recovery_frames, 240);
}

#[test]
fn scripted_recovery_reaches_sink_with_one_decision_across_credit() {
    const CHILD: &str = "RPTADV_TEST_RECOVERY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let providers = ProviderSet::load().unwrap();
            let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
            let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
            let mut media = fixture_media(&sender, jitter).await;
            sender
                .send_to(
                    &packet(1, 1000, 0, &[0xa5; 160]),
                    media.endpoint.local_addr().unwrap(),
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let mut sink = Sink::default();
            let now = 1_000_000_000;
            if std::env::var(CHILD).unwrap() == "recovered" {
                media.last_birth = Some(now - 10_000_000);
            }
            assert_eq!(read(&mut media, &mut sink, now), 0);
            if std::env::var(CHILD).unwrap() == "cancelled" {
                assert_eq!(read(&mut media, &mut sink, now + 40_000_000), 2);
                assert!(media.pending.is_some());
                let library =
                    unsafe { libloading::Library::new("librptadv_codec_g711.so.1") }.unwrap();
                drop(media);
                let live = unsafe {
                    library
                        .get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_codec_live_handles\0")
                }
                .unwrap();
                assert_eq!(unsafe { live() }, 0);
                let mut replacement = fixture_media(&sender, jitter).await;
                assert!(replacement.codec.decoded_spans().is_empty());
                sink.credit = 160;
                assert_eq!(read(&mut replacement, &mut sink, now + 80_000_000), 0);
                assert!(
                    sink.pcm.is_empty(),
                    "replacement cannot replay cancelled retained PCM"
                );
                return;
            }
            if std::env::var(CHILD).unwrap() == "failed" {
                for _ in 0..2 {
                    assert_eq!(
                        try_read(&mut media, &mut sink, now + 40_000_000),
                        Err(MediaError::Codec)
                    );
                    assert!(sink.audio.is_empty() && sink.pcm.is_empty() && sink.gaps.is_empty());
                }
                let library =
                    unsafe { libloading::Library::new("librptadv_codec_g711.so.1") }.unwrap();
                let decoded = unsafe {
                    library.get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_decode_calls\0")
                }
                .unwrap();
                let concealed = unsafe {
                    library.get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_conceal_calls\0")
                }
                .unwrap();
                assert_eq!(unsafe { decoded() }, 1);
                assert_eq!(unsafe { concealed() }, 0);
                return;
            }
            if std::env::var(CHILD).unwrap() == "missing" {
                assert_eq!(read(&mut media, &mut sink, now + 40_000_000), 2);
                sink.credit = 80;
                assert_eq!(read(&mut media, &mut sink, now + 60_000_000), 2);
                assert_eq!(read(&mut media, &mut sink, now + 80_000_000), 1);
                assert!(sink.flags.is_empty());
                assert_eq!(sink.gaps.iter().map(|(_, count)| *count).sum::<u32>(), 160);
                let observation = media.observe().unwrap();
                assert_eq!(
                    (
                        observation.decode_recovery_frames,
                        observation.native_plc_frames,
                        observation.fallback_frames
                    ),
                    (160, 0, 160)
                );
                return;
            }
            sender
                .send_to(
                    &packet(2, 1120, 0, &[0xa5; 160]),
                    media.endpoint.local_addr().unwrap(),
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
            assert_eq!(read(&mut media, &mut sink, now + 10_000_000), 0);
            assert_eq!(read(&mut media, &mut sink, now + 40_000_000), 2);
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 160);
            sink.credit = 60;
            assert_eq!(read(&mut media, &mut sink, now + 60_000_000), 2);
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 160);
            sink.credit = 160;
            assert_eq!(read(&mut media, &mut sink, now + 80_000_000), 1);
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 160);
            assert_eq!(sink.flags, [1, 1, 1]);
            assert_eq!(sink.pcm, vec![0.25; 160]);
            sink.credit = 0;
            assert_eq!(read(&mut media, &mut sink, now + 100_000_000), 2);
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 320);
            sink.credit = 50;
            assert_eq!(read(&mut media, &mut sink, now + 120_000_000), 2);
            sink.credit = 160;
            assert_eq!(read(&mut media, &mut sink, now + 140_000_000), 1);
            assert_eq!(sink.flags, [1, 1, 1, 1, 1, 1]);
            assert_eq!(sink.pcm, vec![0.25; 280]);
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 320);
            media.reset().unwrap();
            assert_eq!(media.observe().unwrap().decode_recovery_frames, 320);
            let library = unsafe { libloading::Library::new("librptadv_codec_g711.so.1") }.unwrap();
            let decoded = unsafe {
                library.get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_decode_calls\0")
            }
            .unwrap();
            let concealed = unsafe {
                library.get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_conceal_calls\0")
            }
            .unwrap();
            assert_eq!(
                unsafe { decoded() },
                2,
                "two retained packets, not one decode per retry"
            );
            assert_eq!(
                unsafe { concealed() },
                0,
                "successful native recovery never conceals twice"
            );
            let fresh = fixture_media(&sender, jitter).await;
            assert_eq!(fresh.observe().unwrap().decode_recovery_frames, 0);
        });
        return;
    }
    let directory = std::env::temp_dir().join(format!("rptadv-recovery-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let source =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phone_codec.c");
    let output = std::process::Command::new("timeout")
        .args([
            "15", "cc", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-I",
        ])
        .arg(
            std::env::var_os("RPTADV_CODEC_INCLUDE")
                .unwrap_or_else(|| "/usr/include/rptadv-codec".into()),
        )
        .arg(source)
        .args(["-ldl", "-o"])
        .arg(directory.join("librptadv_codec_g711.so.1"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let provider = unsafe {
        let library = libloading::Library::new("librptadv_codec_g711.so.1").unwrap();
        let entry = library
            .get::<unsafe extern "C" fn()>(b"rptadv_codec_descriptor_v1\0")
            .unwrap();
        let mut info: libc::Dl_info = std::mem::zeroed();
        assert_ne!(libc::dladdr(*entry as *const libc::c_void, &mut info), 0);
        std::fs::canonicalize(std::ffi::CStr::from_ptr(info.dli_fname).to_str().unwrap()).unwrap()
    };
    let mut paths = vec![directory.clone()];
    if let Some(original) = std::env::var_os("LD_LIBRARY_PATH") {
        paths.extend(std::env::split_paths(&original));
    }
    for (mode, fault) in [
        ("recovered", "native_recovered"),
        ("missing", "native_recovered"),
        ("cancelled", "native_recovered"),
        ("failed", "malformed_recover"),
        ("failed", "span_origin"),
        ("failed", "span_offset"),
        ("failed", "span_count"),
    ] {
        let child = std::process::Command::new("timeout").arg("15").arg(std::env::current_exe().unwrap())
            .args(["--exact", "phone::call_media::tests::scripted_recovery_reaches_sink_with_one_decision_across_credit", "--test-threads=1", "--nocapture"])
            .env(CHILD, mode).env("RPTADV_TEST_CODEC_FAULT", fault)
            .env("RPTADV_TEST_REAL_CODEC", &provider).env("LD_LIBRARY_PATH", std::env::join_paths(&paths).unwrap())
            .output().unwrap();
        assert!(
            child.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn observation_exposes_jitter_drops_and_preserves_call_boundary() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    assert_eq!(
        media.jitter.put(1000, 160, 0, &[1]).unwrap(),
        Admission::Accepted
    );
    assert_eq!(
        media.jitter.put(1000, 160, 0, &[1]).unwrap(),
        Admission::Duplicate
    );
    for i in 1..64 {
        assert_eq!(
            media.jitter.put(1000 + i * 160, 160, 0, &[1]).unwrap(),
            Admission::Accepted
        );
    }
    assert_eq!(
        media.jitter.put(1000 + 64 * 160, 160, 0, &[1]).unwrap(),
        Admission::Full
    );
    let before = media.observe().unwrap();
    assert_eq!(
        (before.jitter.duplicate_drops, before.jitter.full_drops),
        (1, 1)
    );
    assert_eq!(
        media
            .jitter
            .get(40_000_000, 160, &mut media.packet)
            .unwrap()
            .kind,
        Kind::Packet
    );
    media.jitter.advance(40_000_000, 0).unwrap();
    assert_eq!(
        media.jitter.put(700, 160, 100_000_000, &[1]).unwrap(),
        Admission::Late
    );
    assert_eq!(media.observe().unwrap().jitter.late_drops, 1);
    media.reset().unwrap(); // Transmit reset must not erase receive-call observations.
    let after = media.observe().unwrap();
    assert_eq!(
        (
            after.jitter.duplicate_drops,
            after.jitter.full_drops,
            after.jitter.late_drops
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            after.decode_recovery_frames,
            after.native_plc_frames,
            after.fallback_frames
        ),
        (0, 0, 0)
    );
    let fresh = fixture_media(&sender, jitter).await.observe().unwrap();
    assert_eq!(
        (
            fresh.jitter.duplicate_drops,
            fresh.jitter.full_drops,
            fresh.jitter.late_drops
        ),
        (0, 0, 0)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn observation_distinguishes_missing_from_insertion() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    media.jitter.put(1000, 160, 0, &[1]).unwrap();
    media.jitter.put(1320, 160, 20_000_000, &[3]).unwrap();
    assert_eq!(
        media
            .jitter
            .get(40_000_000, 160, &mut media.packet)
            .unwrap()
            .kind,
        Kind::Packet
    );
    media.jitter.advance(40_000_000, 0).unwrap();
    let decision = media
        .jitter
        .get(60_000_000, 160, &mut media.packet)
        .unwrap();
    assert_eq!(decision.kind, Kind::Missing);
    let snapshot = media.observe().unwrap();
    assert_eq!(snapshot.jitter.missing_span, u64::from(decision.span));
    assert_eq!(snapshot.jitter.insertion_span, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn native_loss_runs_once_and_fallback_is_not_stacked() {
    use super::super::media::AudioProfile;
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let catalog = super::super::codec::load_catalog().unwrap();
    for profile in AudioProfile::ALL {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let description = MediaDescription {
            remote: sender.local_addr().unwrap(),
            codec: profile,
            receive_payload: profile.payload(),
            send_payload: profile.payload(),
            telephone_events: [None, None],
        };
        let endpoint = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), description.clone(), 1, 0)
            .await
            .unwrap();
        let mut media =
            Media::from_endpoint(endpoint, description, Vec::new(), jitter, &catalog).unwrap();
        let frames = profile.parameters().packet_frames as usize;
        let mut encoded = vec![0; profile.parameters().packet_bytes];
        let mut encoder = AudioCodec::load(profile).unwrap();
        encoder.encode(&vec![0.25; frames], &mut encoded).unwrap();
        let start = 1u64 << 32;
        let born = 1_000_000_000;
        media.clock = Some((0, start));
        media
            .jitter
            .put(start, frames as u32, born, &encoded)
            .unwrap();
        media.births[0] = Some((start, born));
        let mut sink = Sink {
            credit: frames,
            allow_zero: true,
            ..Default::default()
        };
        read(&mut media, &mut sink, born + 40_000_000);
        assert_eq!(media.source_end, Some(start + frames as u64), "{profile:?}");
        sink.credit = 0;
        read(&mut media, &mut sink, born + 100_000_000);
        let observation = media.observe().unwrap();
        let mut reference = AudioCodec::load(profile).unwrap();
        let mut reference_pcm = vec![0.0; frames];
        reference.decode(&encoded, &mut reference_pcm).unwrap();
        let native = reference
            .conceal(frames as u32, &mut reference_pcm)
            .unwrap()
            == DecodeProvenance::Concealed;
        assert_eq!(
            observation.native_plc_frames,
            if native { frames as u64 } else { 0 },
            "{profile:?}"
        );
        assert_eq!(
            observation.fallback_frames,
            if native { 0 } else { frames as u64 },
            "{profile:?}"
        );
        assert_eq!(
            observation.native_plc_frames + observation.fallback_frames,
            frames as u64,
            "{profile:?}"
        );
        for _ in 0..3 {
            read(&mut media, &mut sink, born + 100_000_000);
        }
        let again = media.observe().unwrap();
        assert_eq!(
            (again.native_plc_frames, again.fallback_frames),
            (observation.native_plc_frames, observation.fallback_frames)
        );
        sink.credit = 1;
        while media.pending.is_some() {
            read(&mut media, &mut sink, born + 100_000_000);
        }
        assert_eq!(media.source_end, Some(start + 2 * frames as u64));
        if observation.native_plc_frames > 0 {
            assert!(sink.flags[1..].iter().all(|flag| *flag == 1), "{profile:?}");
            assert!(sink.gaps.is_empty(), "native PLC must not stack fallback");
            assert_eq!(
                &sink.pcm[frames..],
                &reference_pcm,
                "native retained PCM changed on retry"
            );
        } else {
            assert_eq!(
                sink.gaps.iter().map(|(_, n)| u64::from(*n)).sum::<u64>(),
                frames as u64
            );
        }
        let next = start + 2 * frames as u64;
        let later = born + 100_000_000;
        media
            .jitter
            .put(next, frames as u32, later, &encoded)
            .unwrap();
        media.births[1] = Some((next, later));
        sink.credit = frames;
        let before = sink.pcm.len();
        read(&mut media, &mut sink, later + 40_000_000);
        while media.pending.is_some() {
            read(&mut media, &mut sink, later + 40_000_000);
        }
        assert_eq!(media.source_end, Some(next + frames as u64));
        assert_eq!(
            sink.flags.last(),
            Some(&0),
            "real audio resumes after native/fallback"
        );
        assert_eq!(sink.pcm.len() - before, frames);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ilbc_prefix_uses_bounded_scratch_before_decoding_retained_packet() {
    use super::super::media::AudioProfile;
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let catalog = super::super::codec::load_catalog().unwrap();
    for prefix in [80u32, 240, 480] {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let description = MediaDescription {
            remote: sender.local_addr().unwrap(),
            codec: AudioProfile::Ilbc30,
            receive_payload: AudioProfile::Ilbc30.payload(),
            send_payload: AudioProfile::Ilbc30.payload(),
            telephone_events: [None, None],
        };
        let endpoint = RtpEndpoint::bind("127.0.0.1:0".parse().unwrap(), description.clone(), 1, 0)
            .await
            .unwrap();
        let mut media =
            Media::from_endpoint(endpoint, description, Vec::new(), jitter, &catalog).unwrap();
        let mut encoder = AudioCodec::load(AudioProfile::Ilbc30).unwrap();
        let mut encoded = [0; 50];
        encoder.encode(&[0.25; 240], &mut encoded).unwrap();
        media.packet[..50].copy_from_slice(&encoded);
        media.pending = Some(Pending::Audio {
            start: 1000,
            count: 240,
            born: 1_000_000_000,
            prefix: 0,
            pcm_start: 0,
            decode: Some((50, 0)),
        });
        let mut sink = Sink {
            credit: 240,
            allow_zero: true,
            ..Default::default()
        };
        read(&mut media, &mut sink, 1_100_000_000);
        assert_eq!(media.source_end, Some(1240));
        media.offset = 0;
        media.pending = Some(Pending::Audio {
            start: 1240 + u64::from(prefix),
            count: 240,
            born: 1_010_000_000,
            prefix,
            pcm_start: 0,
            decode: Some((50, 0)),
        });
        sink.credit = 0;
        read(&mut media, &mut sink, 1_100_000_000);
        let first = media.observe().unwrap();
        for _ in 0..3 {
            read(&mut media, &mut sink, 1_100_000_000);
        }
        let after = media.observe().unwrap();
        assert_eq!(
            (after.native_plc_frames, after.fallback_frames),
            (first.native_plc_frames, first.fallback_frames)
        );
        assert!(
            matches!(
                media.pending,
                Some(Pending::Audio {
                    decode: Some(_),
                    ..
                })
            ),
            "packet must still be encoded while PLC owns scratch"
        );
        sink.credit = 17;
        while media.pending.is_some() {
            read(&mut media, &mut sink, 1_100_000_000);
        }
        let after = media.observe().unwrap();
        assert_eq!(
            after.native_plc_frames,
            if prefix == 80 { 0 } else { u64::from(prefix) }
        );
        assert_eq!(after.fallback_frames, if prefix == 80 { 80 } else { 0 });
        assert_eq!(media.source_end, Some(1480 + u64::from(prefix)));
        let mut reference = AudioCodec::load(AudioProfile::Ilbc30).unwrap();
        let mut expected = [0.0; 240];
        reference.decode(&encoded, &mut expected).unwrap();
        for _ in 0..prefix / 240 {
            reference.conceal(240, &mut expected).unwrap();
        }
        reference.decode(&encoded, &mut expected).unwrap();
        assert_eq!(
            &sink.pcm[sink.pcm.len() - 240..],
            &expected,
            "prefix PLC must not overwrite or predecode retained packet"
        );
    }
}

#[test]
fn retained_mixed_map_slices_refusal_partial_boundary_and_cross_boundary_overlap() {
    let spans = [
        DecodeSpan {
            offset_frames: 0,
            frame_count: 2,
            origin: 1,
        },
        DecodeSpan {
            offset_frames: 2,
            frame_count: 3,
            origin: 0,
        },
        DecodeSpan {
            offset_frames: 5,
            frame_count: 2,
            origin: 1,
        },
    ];
    let before = spans.map(|s| (s.offset_frames, s.frame_count, s.origin));
    let pcm = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7];
    // The artificial map tests the cursor, not newly permitted SIP aggregates.
    for (overlap, credits, expected) in [
        (
            0,
            vec![0, 1, 1, 3, 2],
            vec![(0, 0, 1), (0, 1, 1), (1, 1, 1), (2, 3, 0), (5, 2, 1)],
        ),
        (3, vec![1, 9, 9], vec![(3, 1, 0), (4, 1, 0), (5, 2, 1)]),
        (5, vec![2], vec![(5, 2, 1)]),
    ] {
        let mut offset = overlap;
        let mut actual = Vec::new();
        let mut accepted_pcm = Vec::new();
        for credit in credits {
            let (remaining, origin) = retained_span(&spans, offset).unwrap();
            let accepted = credit.min(remaining);
            actual.push((
                offset,
                accepted,
                u32::from(origin == DecodeProvenance::Concealed),
            ));
            accepted_pcm.extend_from_slice(&pcm[offset..offset + accepted]);
            offset += accepted;
        }
        assert_eq!(actual, expected);
        assert_eq!(accepted_pcm, pcm[overlap..]);
        assert_eq!(offset, 7);
    }
    assert_eq!(retained_span(&spans, 7), Err(MediaError::Codec));
    assert_eq!(
        spans.map(|s| (s.offset_frames, s.frame_count, s.origin)),
        before
    );
}

/// The pre-extension ABI allocation with exactly one appended optional callback.
/// This layout deliberately works before production exposes the new field.
#[repr(C)]
struct CausalSink {
    struct_size: u32,
    abi_version: u32,
    context: *mut c_void,
    audio: abi::rptadv_peer_source_audio_v1,
    gap: abi::rptadv_peer_source_gap_v1,
    host_clock_epoch: u64,
    reference_now_ns: u64,
    audio_timed: abi::rptadv_peer_source_audio_timed_v1,
    source_discontinuity: abi::rptadv_peer_source_discontinuity_v1,
    digit: abi::rptadv_peer_source_digit_v1,
    source_adaptation: Option<unsafe extern "C" fn(*mut c_void, u64, u32) -> i32>,
}
unsafe extern "C" fn adaptation(context: *mut c_void, start: u64, count: u32) -> i32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.adaptations.push((start, count, sink.ranges.len()));
    i32::from(sink.refuse_adaptation)
}
fn causal_callbacks(sink: &mut Sink, now: u64) -> CausalSink {
    CausalSink {
        struct_size: size_of::<CausalSink>() as u32,
        abi_version: 1,
        context: ptr::from_mut(sink).cast(),
        audio: None,
        gap: Some(gap),
        host_clock_epoch: 7,
        reference_now_ns: now,
        audio_timed: Some(audio),
        source_discontinuity: Some(discontinuity),
        digit: Some(digit),
        source_adaptation: Some(adaptation),
    }
}
fn causal_read(media: &mut Media, sink: &mut Sink, now: u64) -> u32 {
    let input = causal_callbacks(sink, now);
    let mut output = unsafe { std::mem::zeroed() };
    unsafe { media.read(0, ptr::from_ref(&input).cast(), &mut output) }.unwrap();
    output.progress
}

#[tokio::test(flavor = "current_thread")]
async fn steady_cadence_media_adaptation_survives_real_put() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let target = media.endpoint.local_addr().unwrap();
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    let origin = (1u64 << 32) + 1000;
    let base = 1_000_000_000;
    for ms in (0..=900u64).step_by(10) {
        if ms % 20 == 0 {
            sender
                .send_to(
                    &packet((ms / 20) as u16, 1000 + ms as u32 * 8, 0, &[0xa5; 160]),
                    target,
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        if ms == 900 {
            let before = media.jitter.observe().unwrap();
            assert_eq!(before.skipped_span, 160);
            assert_eq!(media.source_end, Some(origin + 860 * 8));
            let packet900 = origin + 900 * 8;
            assert!(
                media
                    .births
                    .iter()
                    .flatten()
                    .all(|(timestamp, _)| *timestamp != packet900)
            );
            sink.refuse_adaptation = true;
            let ranges = sink.ranges.len();
            assert_eq!(causal_read(&mut media, &mut sink, base + ms * 1_000_000), 2);
            // Packet900 has entered via Media::poll before its get. The put may
            // retire skipped packets; gauges are deliberately not authority.
            let after = media.jitter.observe().unwrap();
            let admitted_birth = media
                .births
                .iter()
                .flatten()
                .find(|(timestamp, _)| *timestamp == packet900)
                .map(|(_, birth)| *birth);
            assert!(
                admitted_birth.is_some(),
                "packet900 must be admitted by the production put before get900"
            );
            assert!(admitted_birth.unwrap() <= base + ms * 1_000_000);
            // Exact pinned trace oracle, not production cause authority: the
            // accepted put cleans skipped packet860 before get returns packet880.
            assert_eq!(
                (
                    before.queued_packets,
                    before.buffered_span,
                    before.late_drops
                ),
                (2, 320, 0)
            );
            assert_eq!(
                (after.queued_packets, after.buffered_span, after.late_drops),
                (1, 160, 1)
            );
            assert_eq!(sink.adaptations, [(origin + 860 * 8, 160, ranges)]);
            assert_eq!(
                sink.ranges.len(),
                ranges,
                "refusal precedes all gap/PCM credit"
            );
            assert_eq!(causal_read(&mut media, &mut sink, base + ms * 1_000_000), 2);
            assert_eq!(sink.adaptations[1], sink.adaptations[0]);
            sink.refuse_adaptation = false;
            assert_eq!(causal_read(&mut media, &mut sink, base + ms * 1_000_000), 1);
            assert_eq!(sink.adaptations[2], sink.adaptations[0]);
            assert_eq!(sink.gaps, [(origin + 860 * 8, 160)]);
            assert_eq!(sink.discontinuities, 1, "startup alone is generic");
            assert_eq!(causal_read(&mut media, &mut sink, base + ms * 1_000_000), 1);
            assert_eq!(sink.adaptations.len(), 3, "completed cut is not repeated");
            eprintln!(
                "real Media gate cut={:?} before={before:?} after={after:?}",
                sink.adaptations[0]
            );
        } else {
            for _ in 0..8 {
                if causal_read(&mut media, &mut sink, base + ms * 1_000_000) == 0 {
                    break;
                }
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn old_layout_and_short_field_sinks_use_only_the_readable_sip_prefix() {
    use std::alloc::{Layout, alloc, dealloc};
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    for size in [7, 15, 23, 31, 39, 47, 55, 63, 71, 72, 73, 79, 80] {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let mut sink = Sink::default();
        let input = CausalSink {
            struct_size: size as u32,
            abi_version: 1,
            context: ptr::from_mut(&mut sink).cast(),
            audio: None,
            gap: Some(gap),
            host_clock_epoch: 7,
            reference_now_ns: 1_000_000_000,
            audio_timed: Some(audio),
            source_discontinuity: Some(discontinuity),
            digit: Some(digit),
            source_adaptation: None,
        };
        let layout = Layout::from_size_align(size, std::mem::align_of::<CausalSink>()).unwrap();
        let allocation = unsafe { alloc(layout) };
        assert!(!allocation.is_null());
        unsafe { ptr::copy_nonoverlapping(ptr::from_ref(&input).cast::<u8>(), allocation, size) };
        let mut output = unsafe { std::mem::zeroed() };
        let result = unsafe { media.read(0, allocation.cast(), &mut output) };
        unsafe { dealloc(allocation, layout) };
        assert_eq!(result.is_ok(), size >= 72, "actual allocation size {size}");
        assert_eq!(sink.discontinuities, usize::from(size >= 72));
        assert!(sink.adaptations.is_empty());
    }
}

/// Run the same production UDP cadence only as far as its completed native cut.
async fn real_native_cut(media: &mut Media, sender: &UdpSocket, sink: &mut Sink) {
    real_native_cut_with_last(media, sender, sink, true).await;
}
async fn real_native_cut_with_last(
    media: &mut Media,
    sender: &UdpSocket,
    sink: &mut Sink,
    last: bool,
) {
    let target = media.endpoint.local_addr().unwrap();
    for ms in (0..=880u64).step_by(10) {
        if ms % 20 == 0 && (ms != 880 || last) {
            sender
                .send_to(
                    &packet((ms / 20) as u16, 1000 + ms as u32 * 8, 0, &[0xa5; 160]),
                    target,
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        for _ in 0..8 {
            if causal_read(media, sink, 1_000_000_000 + ms * 1_000_000) == 0 {
                break;
            }
        }
    }
    let witness = media.adaptation_witness.expect("completed native skip");
    assert_eq!(
        (witness.start, witness.count),
        ((1u64 << 32) + 1000 + 860 * 8, 160)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn proved_native_adaptation_is_ordered_before_exact_gap_credit() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    real_native_cut(&mut media, &sender, &mut sink).await;
    let cut = media.source_end.unwrap();
    sink.credit = 0;
    assert_eq!(causal_read(&mut media, &mut sink, 1_900_000_000), 2);
    assert_eq!(sink.adaptations.len(), 1);
    assert_eq!(sink.gaps, [(cut, 0)]);
    let audio = sink.audio.len();
    sink.credit = 40;
    for offset in [0, 40, 80, 120] {
        let progress = causal_read(&mut media, &mut sink, 1_900_000_000);
        assert_eq!(progress, if offset == 120 { 1 } else { 2 });
        assert_eq!(sink.gaps.last(), Some(&(cut + offset, 40)));
        assert_eq!(sink.audio.len(), audio);
    }
    assert_eq!(
        sink.adaptations.len(),
        1,
        "completed reset is never repeated with partial gap credit"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ambiguous_or_unknown_source_cut_never_claims_adaptation() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    for defect in 0..5 {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let mut sink = Sink {
            credit: 160,
            ..Sink::default()
        };
        real_native_cut(&mut media, &sender, &mut sink).await;
        match defect {
            0 => media.adaptation_witness = None, // Known positive prefix alone is not proof.
            1 => media.births.fill(None),         // A real skip plus unknown receipt is generic.
            2 => media.adaptation_witness.as_mut().unwrap().count += 1,
            3 => media.source_end = media.source_end.map(|n| n - 1),
            4 => {
                // New generic/startup discontinuity dominates a simultaneous native witness.
                media.discontinuity = true;
            }
            _ => unreachable!(),
        }
        for _ in 0..8 {
            if causal_read(&mut media, &mut sink, 1_900_000_000) == 0 {
                break;
            }
        }
        assert!(sink.adaptations.is_empty(), "ambiguous case {defect}");
        assert!(sink.discontinuities > 1);
        assert!(!sink.gaps.is_empty());
    }
}

type NativeObserve =
    unsafe extern "C" fn(*const c_void, *mut abi::rptadv_jitter_observation_v1) -> i32;
thread_local! {
    static OBSERVATION_FAULT: std::cell::Cell<(Option<NativeObserve>, u8)> = const { std::cell::Cell::new((None, 0)) };
}
unsafe extern "C" fn fault_observe(
    context: *const c_void,
    output: *mut abi::rptadv_jitter_observation_v1,
) -> i32 {
    let (original, fault) = OBSERVATION_FAULT.get();
    let result = unsafe { original.unwrap()(context, output) };
    if result != 0 {
        return result;
    }
    let output = unsafe { &mut *output };
    match fault {
        1 => return -1,
        2 => output.struct_size -= 1,
        3 => output.skipped_span -= 1,
        4 => output.late_drops = u64::MAX,
        5 => output.missing_span += 1,
        6 => output.insertion_span += 1,
        7 => output.limit_resets += 1,
        8 => output.overload_span += 1,
        9 => output.duplicate_drops += 1,
        10 => output.full_drops += 1,
        11 => output.overload_drops += 1,
        12 => output.skipped_span = u64::MAX,
        _ => {}
    }
    0
}
#[tokio::test(flavor = "current_thread")]
async fn adaptation_witness_has_one_packet_lifetime() {
    let providers = ProviderSet::load().unwrap();
    let mut descriptor = *providers.jitter_descriptor().unwrap();
    OBSERVATION_FAULT.set((descriptor.observe, 0));
    descriptor.observe = Some(fault_observe);
    let jitter = unsafe { &*ptr::from_ref(&descriptor) };
    for fault in 0..=12 {
        OBSERVATION_FAULT.set((OBSERVATION_FAULT.get().0, 0));
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let mut sink = Sink {
            credit: 160,
            ..Sink::default()
        };
        real_native_cut(&mut media, &sender, &mut sink).await;
        // NotDue is an allowed operation; it must not destroy an intact witness.
        assert_eq!(causal_read(&mut media, &mut sink, 1_890_000_000), 0);
        assert!(media.adaptation_witness.is_some());
        if fault == 0 {
            // A duplicate received by production Media::poll invalidates authority.
            sender
                .send_to(
                    &packet(80, 1000 + 880 * 8, 0, &[0xa5; 160]),
                    media.endpoint.local_addr().unwrap(),
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        } else {
            OBSERVATION_FAULT.set((OBSERVATION_FAULT.get().0, fault));
        }
        causal_read(&mut media, &mut sink, 1_900_000_000);
        assert!(media.adaptation_witness.is_none(), "fault {fault}");
        assert!(sink.adaptations.is_empty(), "fault {fault}");
        assert_eq!(sink.discontinuities, 2);
    }
    OBSERVATION_FAULT.set((OBSERVATION_FAULT.get().0, 0));
}

#[tokio::test(flavor = "current_thread")]
async fn native_non_packet_then_put_resync_cannot_restore_witness() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    real_native_cut_with_last(&mut media, &sender, &mut sink, false).await;
    let skipped = media.observe().unwrap().jitter.skipped_span;
    for ms in (900..=1500u64).step_by(20) {
        for _ in 0..8 {
            if causal_read(&mut media, &mut sink, 1_000_000_000 + ms * 1_000_000) == 0 {
                break;
            }
        }
        assert!(
            media.adaptation_witness.is_none(),
            "first non-Packet invalidates before later puts"
        );
    }
    let before = media.observe().unwrap().jitter;
    assert!(
        before.missing_span >= 21 * 80,
        "exercise >20 unsuccessful native fetches"
    );
    sender
        .send_to(
            &packet(100, 1000 + 1520 * 8, 0, &[0xa5; 160]),
            media.endpoint.local_addr().unwrap(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    for ms in [1520, 1540, 1560] {
        for _ in 0..8 {
            if causal_read(&mut media, &mut sink, 1_000_000_000 + ms * 1_000_000) == 0 {
                break;
            }
        }
        assert!(media.adaptation_witness.is_none());
    }
    assert!(sink.adaptations.is_empty());
    assert_eq!(media.observe().unwrap().jitter.skipped_span, skipped);
    eprintln!(
        "non-Packet resync before={before:?} after={:?}",
        media.observe().unwrap().jitter
    );
}

#[tokio::test(flavor = "current_thread")]
async fn refused_legacy_cut_cannot_upgrade_to_adaptation() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    real_native_cut(&mut media, &sender, &mut sink).await;
    sink.refuse_discontinuity = true;
    assert_eq!(read(&mut media, &mut sink, 1_900_000_000), 2);
    let credits = sink.ranges.len();
    assert_eq!(causal_read(&mut media, &mut sink, 1_900_000_000), 2);
    assert!(
        sink.adaptations.is_empty(),
        "refused generic cause cannot upgrade on a later read"
    );
    assert_eq!(sink.ranges.len(), credits);
    sink.refuse_discontinuity = false;
    assert_eq!(causal_read(&mut media, &mut sink, 1_900_000_000), 1);
    assert!(sink.adaptations.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_packet_invalidates_native_witness() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    real_native_cut(&mut media, &sender, &mut sink).await;
    sender
        .send_to(&[0], media.endpoint.local_addr().unwrap())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    causal_read(&mut media, &mut sink, 1_900_000_000);
    assert!(
        sink.adaptations.is_empty(),
        "malformed input must invalidate native cause"
    );
    assert_eq!(sink.discontinuities, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn refused_native_cut_cannot_downgrade_to_generic() {
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    let mut sink = Sink {
        credit: 160,
        ..Sink::default()
    };
    real_native_cut(&mut media, &sender, &mut sink).await;
    sink.refuse_adaptation = true;
    assert_eq!(causal_read(&mut media, &mut sink, 1_900_000_000), 2);
    let credits = sink.ranges.len();
    assert_eq!(
        try_read(&mut media, &mut sink, 1_900_000_000),
        Err(MediaError::SourceContract)
    );
    assert_eq!(sink.ranges.len(), credits);
    assert_eq!(sink.discontinuities, 1);
}

#[test]
fn source_cause_media_state_size() {
    // Exact pre-change field layout, compiled by the same target/compiler.
    #[allow(dead_code)]
    struct BaselineLayout {
        endpoint: RtpEndpoint,
        codec: AudioCodec,
        profile: AudioParameters,
        jitter: Jitter,
        decode_recovery_frames: u64,
        native_plc_frames: u64,
        fallback_frames: u64,
        clock: Option<(u32, u64)>,
        births: [Option<(u64, u64)>; 64],
        birth_slot: usize,
        pcm: Box<[f32]>,
        packet: Box<[u8]>,
        pending: Option<Pending>,
        offset: usize,
        loss: Option<Loss>,
        // Original receipt history only; synthetic samples never refresh this lease.
        last_birth: Option<u64>,
        // End of source media actually credited by product, independent of jitter adaptation.
        source_end: Option<u64>,
        decision_pending: bool,
        host_epoch: Option<u64>,
        discontinuity: bool,
        epoch: Option<u64>,
        marker: bool,
        answer: Vec<u8>,
        pub(crate) digit_capabilities: u32,
    }
    eprintln!(
        "Media baseline={} current={} delta={}",
        size_of::<BaselineLayout>(),
        size_of::<Media>(),
        size_of::<Media>() as isize - size_of::<BaselineLayout>() as isize
    );
}

thread_local! {
    static OPERATION_FAULT: std::cell::Cell<(*const abi::rptadv_jitter_descriptor_v1, u8)> =
        const { std::cell::Cell::new((ptr::null(), 0)) };
}
unsafe extern "C" fn fault_put(
    context: *mut c_void,
    packet: *const abi::rptadv_jitter_packet_v1,
) -> i32 {
    let (api, fault) = OPERATION_FAULT.get();
    match fault {
        1 => 3, // Full, even without useful observation change.
        2 => -1,
        3 => 2, // Late.
        4 => 1, // Duplicate.
        _ => unsafe { (*api).put.unwrap()(context, packet) },
    }
}
unsafe extern "C" fn fault_get(
    context: *mut c_void,
    now: u64,
    span: u32,
    payload: *mut u8,
    capacity: usize,
    output: *mut abi::rptadv_jitter_result_v1,
) -> i32 {
    let (api, fault) = OPERATION_FAULT.get();
    let result = unsafe { (*api).get.unwrap()(context, now, span, payload, capacity, output) };
    if result == 0 {
        let output = unsafe { &mut *output };
        match fault {
            5 => {
                output.kind = 1;
                output.length = 0;
            } // Missing.
            6 => {
                output.kind = 2;
                output.length = 0;
            } // Insertion.
            7 => output.start_offset = -40,
            8 => output.timestamp += 1, // Not the exact known successor.
            _ => {}
        }
    }
    result
}
#[tokio::test(flavor = "current_thread")]
async fn rejected_put_nonpacket_overlap_reset_and_second_advance_discard_witness() {
    let providers = ProviderSet::load().unwrap();
    let original = providers.jitter_descriptor().unwrap();
    let mut descriptor = *original;
    descriptor.put = Some(fault_put);
    descriptor.get = Some(fault_get);
    let jitter = unsafe { &*ptr::from_ref(&descriptor) };
    for fault in 1..=10 {
        OPERATION_FAULT.set((ptr::from_ref(original), 0));
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let mut sink = Sink {
            credit: 160,
            ..Sink::default()
        };
        real_native_cut(&mut media, &sender, &mut sink).await;
        let end = media.source_end;
        let gaps = sink.gaps.len();
        OPERATION_FAULT.set((ptr::from_ref(original), fault));
        if fault <= 4 {
            let result = media.put_source((1u64 << 32) + 1000 + 900 * 8, 160, 1_900_000_000, 160);
            assert!(result.is_err() || result.unwrap() != Admission::Accepted);
            assert!(media.adaptation_witness.is_none());
        } else if fault == 9 {
            // No second native advance can carry the previous proof forward.
            assert!(media.advance_source(1_890_000_000).is_err());
            assert!(media.adaptation_witness.is_none());
        } else if fault == 10 {
            media.jitter.reset().unwrap();
        }
        causal_read(&mut media, &mut sink, 1_900_000_000);
        assert!(media.adaptation_witness.is_none(), "fault {fault}");
        assert!(sink.adaptations.is_empty(), "fault {fault}");
        if fault == 6 {
            assert_eq!(media.source_end, end);
            assert_eq!(
                sink.gaps.len(),
                gaps,
                "insertion must not manufacture source loss"
            );
        }
    }
    OPERATION_FAULT.set((ptr::null(), 0));
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_source_read_error_discards_pending_native_proof() {
    unsafe extern "C" fn reject(context: *mut c_void, start: u64, count: u32) -> i32 {
        unsafe { adaptation(context, start, count) };
        -1
    }
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    for fault in 0..7 {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let mut sink = Sink {
            credit: 160,
            ..Sink::default()
        };
        real_native_cut(&mut media, &sender, &mut sink).await;
        // Preserve the original pre-offer case as well as the independently
        // reviewed case where cause already moved from witness to refused cut.
        if fault != 6 {
            sink.refuse_adaptation = true;
            assert_eq!(causal_read(&mut media, &mut sink, 1_900_000_000), 2);
            assert!(media.adaptation_cut.is_some() && media.adaptation_offered);
        }
        let mut input = causal_callbacks(&mut sink, 1_900_000_000);
        let completion = match fault {
            0 | 6 => 1,
            1 => {
                input.struct_size = 71;
                0
            }
            2 => {
                input.host_clock_epoch = 8;
                0
            }
            3 => {
                input.source_adaptation = None;
                0
            }
            4 => {
                input.source_adaptation = Some(reject);
                0
            }
            5 => {
                input.audio_timed = None;
                0
            }
            _ => unreachable!(),
        };
        let mut output = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { media.read(completion, ptr::from_ref(&input).cast(), &mut output) },
            Err(MediaError::SourceContract),
            "initial terminal error {fault}"
        );
        let counts = (
            sink.adaptations.len(),
            sink.discontinuities,
            sink.gaps.len(),
            sink.audio.len(),
            sink.ranges.len(),
            sink.digits.len(),
        );
        sink.refuse_adaptation = false;
        let valid = causal_callbacks(&mut sink, 1_900_000_000);
        for _ in 0..2 {
            assert_eq!(
                unsafe { media.read(0, ptr::from_ref(&valid).cast(), &mut output) },
                Err(MediaError::SourceContract),
                "terminal reread must reject without callbacks: {fault}"
            );
            assert_eq!(
                (
                    sink.adaptations.len(),
                    sink.discontinuities,
                    sink.gaps.len(),
                    sink.audio.len(),
                    sink.ranges.len(),
                    sink.digits.len()
                ),
                counts,
                "no native/generic/gap/PCM/digit dispatch after terminal error {fault}"
            );
            assert_eq!(
                (
                    output.progress,
                    output.activity,
                    output.finish_pending,
                    output.state
                ),
                (0, 0, 0, 3)
            );
        }
        assert!(media.adaptation_witness.is_none() && media.completed_packet_end.is_none());
        assert!(media.adaptation_cut.is_none() && !media.adaptation_offered);
    }
}
