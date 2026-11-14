//! Real RTP, released codec/jitter, and exact source-sink credit tests.
use super::*;
use crate::providers::ProviderSet;
use std::{ffi::c_void, net::UdpSocket, ptr, time::Duration};

#[derive(Default)]
struct Sink {
    credit: usize,
    refuse_digit: bool,
    audio: Vec<(u64, usize, u64)>,
    pcm: Vec<f32>,
    gaps: Vec<(u64, u32)>,
    digits: Vec<u32>,
    discontinuities: usize,
}
unsafe extern "C" fn audio(
    context: *mut c_void,
    start: u64,
    samples: *const f32,
    count: usize,
    _: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    let accepted = count.min(sink.credit);
    assert!(
        unsafe { std::slice::from_raw_parts(samples, count) }
            .iter()
            .all(|value| value.is_finite() && *value != 0.0)
    );
    sink.audio
        .push((start, accepted, unsafe { (*birth).born_ns }));
    sink.pcm
        .extend_from_slice(unsafe { std::slice::from_raw_parts(samples, accepted) });
    accepted
}
unsafe extern "C" fn gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    let accepted = count.min(sink.credit as u32);
    sink.gaps.push((start, accepted));
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
    Media::new(offer.as_bytes(), 1, jitter).await.unwrap()
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
    let mut codec = G711Codec::load(super::super::media::G711::Mulaw).unwrap();
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
        media.jitter.observe().unwrap().insertion_span > 0,
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
