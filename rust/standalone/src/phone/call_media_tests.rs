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
    unsafe {
        (*context.cast::<Sink>()).discontinuities += 1;
    }
    0
}
fn read(media: &mut Media, sink: &mut Sink, now: u64) -> u32 {
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
    unsafe { media.read(0, &input, &mut output) }.unwrap();
    assert_eq!(output.state, 1);
    output.progress
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
async fn unavailable_socket_receipt_mapping_uses_existing_source_gap() {
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
    for frames in [80_usize, 240] {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut media = fixture_media(&sender, jitter).await;
        let target = media.endpoint.local_addr().unwrap();
        sender
            .send_to(&packet(1, 1000, 0, &vec![0xa5; frames]), target)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let mut sink = Sink::default();
        let born = 1_000_000_000;
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
    let providers = ProviderSet::load().unwrap();
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut media = fixture_media(&sender, jitter).await;
    media.pcm[..4].copy_from_slice(&[0.1, 0.2, 0.3, 0.4]);
    media.pending = Some(Pending::Audio {
        start: 100,
        count: 4,
        born: 10,
        prefix: 0,
        provenance: DecodeProvenance::Concealed,
    });
    let mut sink = Sink::default();
    assert_eq!(read(&mut media, &mut sink, 20), 2);
    sink.credit = 2;
    assert_eq!(read(&mut media, &mut sink, 20), 2);
    assert_eq!(read(&mut media, &mut sink, 20), 1);
    assert_eq!(sink.flags, [1, 1, 1]);
    assert_eq!(sink.pcm, [0.1, 0.2, 0.3, 0.4]);
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
            assert_eq!(read(&mut media, &mut sink, now), 0);
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
                    (0, 0, 0)
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
        ("recovered", "malformed_recover"),
        ("missing", "malformed_missing"),
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
