//! Timed RTP contracts test framing and socket admission independently.
use super::*;

fn egress() -> RtpEgress {
    RtpEgress {
        header: rtp::header::Header {
            version: 2,
            payload_type: 0,
            sequence_number: u16::MAX,
            timestamp: 77,
            ssrc: 0x12345678,
            ..Default::default()
        },
        pending: None,
        epoch: 1,
    }
}

fn audio(timestamp: u32) -> TimedAudio<'static> {
    TimedAudio {
        payload: &[0xff; 160],
        marker: true,
        timestamp,
        deadline_ms: 200,
        epoch: 1,
    }
}

#[test]
fn first_send_and_source_gap_keep_original_rtp_timestamps() {
    let mut sender = egress();
    for (sequence, timestamp) in [(u16::MAX, u32::MAX - 79), (0, 400)] {
        let mut received = Vec::new();
        assert_eq!(
            sender.send(audio(timestamp), 10, |wire| {
                received.extend_from_slice(wire);
                Ok(wire.len())
            }),
            Ok(SendStatus::Sent)
        );
        assert_eq!(
            &received[..4],
            &[0x80, 0x80, (sequence >> 8) as u8, sequence as u8]
        );
        assert_eq!(&received[4..8], &timestamp.to_be_bytes());
        assert_eq!(&received[8..12], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&received[12..], &[0xff; 160]);
    }
}

#[test]
fn elapsed_deadline_never_calls_the_socket() {
    let mut sender = egress();
    assert_eq!(
        sender.send(audio(77), 200, |_| panic!("expired media reached socket")),
        Ok(SendStatus::Expired)
    );
}

#[test]
fn blocked_submission_keeps_wire_identity_and_rejects_replacement() {
    let mut sender = egress();
    let original = audio(77);
    let mut first_wire = Vec::new();
    assert_eq!(
        sender.send(original, 0, |wire| {
            first_wire.extend_from_slice(wire);
            Err(std::io::ErrorKind::WouldBlock.into())
        }),
        Ok(SendStatus::WouldBlock)
    );
    for changed in [
        TimedAudio {
            payload: &[0xd5; 160],
            ..original
        },
        TimedAudio {
            timestamp: 237,
            ..original
        },
        TimedAudio {
            marker: false,
            ..original
        },
        TimedAudio {
            deadline_ms: 201,
            ..original
        },
    ] {
        assert_eq!(
            sender.send(changed, 100, |_| panic!("replacement reached socket")),
            Err(MediaError::InvalidPacket)
        );
    }
    assert_eq!(
        sender.send(original, 199, |wire| {
            assert_eq!(wire, first_wire);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}

#[test]
fn a_new_submission_cannot_extend_the_maximum_age_budget() {
    let mut sender = egress();
    let request = TimedAudio {
        deadline_ms: 211,
        ..audio(77)
    };
    assert_eq!(
        sender.send(request, 10, |_| panic!("overlong deadline reached socket")),
        Err(MediaError::InvalidPacket)
    );
}

#[test]
fn expiry_discards_pending_bytes_without_compressing_the_source_gap() {
    let mut sender = egress();
    assert_eq!(
        sender.send(audio(77), 0, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(
        sender.send(audio(77), 200, |_| panic!("expired retry reached socket")),
        Ok(SendStatus::Expired)
    );
    let next = TimedAudio {
        timestamp: 2477,
        deadline_ms: 400,
        ..audio(77)
    };
    assert_eq!(
        sender.send(next, 200, |wire| {
            assert_eq!(&wire[2..4], &[0xff, 0xff]);
            assert_eq!(&wire[4..8], &[0, 0, 9, 173]);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}

#[test]
fn retirement_fences_old_work_without_discarding_the_new_generation() {
    let mut sender = egress();
    let old = audio(77);
    assert_eq!(
        sender.send(old, 0, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(sender.retire(), Ok(2));
    let next = TimedAudio {
        epoch: 2,
        timestamp: 477,
        ..old
    };
    assert_eq!(
        sender.send(next, 10, |_| Err(std::io::ErrorKind::WouldBlock.into())),
        Ok(SendStatus::WouldBlock)
    );
    assert_eq!(
        sender.send(old, 20, |_| panic!("retired media reached socket")),
        Ok(SendStatus::Expired)
    );
    assert_eq!(
        sender.send(next, 30, |wire| {
            assert_eq!(&wire[2..8], &[0xff, 0xff, 0, 0, 1, 221]);
            Ok(wire.len())
        }),
        Ok(SendStatus::Sent)
    );
}
