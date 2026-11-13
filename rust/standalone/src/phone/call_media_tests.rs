//! Real RTP, released codec/jitter, and exact source-sink credit tests.
use super::*;
use crate::providers::ProviderSet;
use std::{ffi::c_void, net::UdpSocket, ptr, time::Duration};

#[derive(Default)]
struct Sink {
    credit: usize,
    refuse_digit: bool,
    audio: Vec<(u64, usize, u64)>,
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

#[tokio::test(flavor = "current_thread")]
async fn source_retains_refused_prefix_birth_digits_and_gaps() {
    let providers = ProviderSet::load().unwrap();
    // The descriptor remains loaded until media has been dropped below.
    let jitter = unsafe { &*ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let offer = format!(
        "v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n",
        sender.local_addr().unwrap().port()
    );
    let mut media = Media::new(offer.as_bytes(), 1, jitter).await.unwrap();
    let target = media.endpoint.local_addr().unwrap();
    let timestamp = u32::MAX - 79;
    sender
        .send_to(&packet(1, timestamp, 0, &[0xa5; 160]), target)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let mut sink = Sink::default();
    let born = 1_000_000_000;
    assert_eq!(read(&mut media, &mut sink, born), 0);
    assert_eq!(read(&mut media, &mut sink, born + 40_000_000), 2);
    sink.credit = 60;
    assert_eq!(read(&mut media, &mut sink, born + 80_000_000), 2);
    sink.credit = 160;
    assert_eq!(read(&mut media, &mut sink, born + 120_000_000), 1);
    let start = (1u64 << 32) + u64::from(timestamp);
    assert_eq!(
        sink.audio,
        [(start, 0, born), (start, 60, born), (start + 60, 100, born)]
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
