use super::*;

#[test]
fn source_clock_keeps_one_origin_and_identity_until_its_owner_ends() {
    use std::time::{Duration, Instant};
    let origin = Instant::now() - Duration::from_secs(1);
    let clock = SourceTimeClock::new(origin).unwrap();
    let initial = clock.reference();
    assert_ne!(initial.host_clock_epoch, 0);
    assert!(initial.now_ns >= 1_000_000_000);
    let retained = clock;
    let next = retained.reference();
    assert_eq!(initial.host_clock_epoch, next.host_clock_epoch);
    assert!(next.now_ns >= initial.now_ns);
    let other_owner = SourceTimeClock::new(origin).unwrap().reference();
    assert_ne!(initial.host_clock_epoch, other_owner.host_clock_epoch);
    let future = SourceTimeClock::new(Instant::now() + Duration::from_secs(3600)).unwrap();
    assert_eq!(future.reference().host_clock_epoch, 0);
    assert_eq!(future.reference().now_ns, 0);
}

#[test]
fn timed_source_retains_original_birth_across_rejected_and_partial_offers() {
    unsafe extern "C" fn read(
        _: *mut c_void,
        _: *mut c_void,
        _: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        _: abi::rptadv_peer_source_text_v1,
        _: *mut c_void,
        _: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        let sink = unsafe { &*sink };
        assert_eq!(sink.host_clock_epoch, 77);
        assert_eq!(sink.reference_now_ns, 1000);
        let birth = abi::rptadv_source_birth_v1 {
            struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
            abi_version: 1,
            host_clock_epoch: 77,
            born_ns: 650,
        };
        let pcm = [0.25; 4];
        let audio = sink.audio_timed.unwrap();
        // Rejected and partially accepted retries carry the same packet birth.
        assert_eq!(
            unsafe { audio(sink.context, 12, pcm.as_ptr(), 4, 0, &birth) },
            0
        );
        assert_eq!(
            unsafe { audio(sink.context, 12, pcm.as_ptr(), 4, 0, &birth) },
            2
        );
        assert_eq!(
            unsafe { audio(sink.context, 14, pcm.as_ptr().add(2), 2, 0, &birth) },
            2
        );
        0
    }
    let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(echo_info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    let mut offered = Vec::new();
    peer.read_source_at(
        SourceCompletion::Pending,
        SourceTimeReference {
            host_clock_epoch: 77,
            now_ns: 1000,
        },
        |input| {
            let SourceInput::Audio {
                start,
                samples,
                birth,
                ..
            } = input
            else {
                panic!("expected audio")
            };
            assert_eq!(
                birth,
                Some(SourceBirth {
                    host_clock_epoch: 77,
                    born_ns: 650
                })
            );
            offered.push((start, samples.len()));
            if offered.len() == 1 { 0 } else { 2 }
        },
    )
    .unwrap();
    assert_eq!(offered, [(12, 4), (12, 4), (14, 2)]);
}

#[test]
fn timed_source_rejects_malformed_stale_and_future_clock_records() {
    unsafe extern "C" fn read(
        context: *mut c_void,
        _: *mut c_void,
        _: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        _: abi::rptadv_peer_source_text_v1,
        _: *mut c_void,
        _: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        let sink = unsafe { &*sink };
        let pcm = [0.25; 2];
        unsafe { sink.audio_timed.unwrap()(sink.context, 0, pcm.as_ptr(), 2, 0, context.cast()) };
        0
    }
    let valid = abi::rptadv_source_birth_v1 {
        struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
        abi_version: 1,
        host_clock_epoch: 77,
        born_ns: 650,
    };
    for case in 0..7 {
        let mut birth = valid;
        match case {
            0 => {}
            1 => birth.struct_size -= 1,
            2 => birth.abi_version = 2,
            3 => birth.host_clock_epoch = 0,
            4 => birth.host_clock_epoch = 76,
            5 => birth.born_ns = 1001,
            _ => {} // A legacy or unqualified provider may omit the record.
        }
        let context = if case == 6 {
            ptr::null_mut()
        } else {
            ptr::from_mut(&mut birth).cast()
        };
        let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
        let services = host_services(|table| {
            *table = prepared.services.0;
            table.context = context;
            table.peer_info = Some(echo_info);
            table.peer_read_source = Some(read);
            table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
        });
        let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
        let mut seen = Vec::new();
        let result = peer.read_source_at(
            SourceCompletion::Pending,
            SourceTimeReference {
                host_clock_epoch: 77,
                now_ns: 1000,
            },
            |input| {
                let SourceInput::Audio { birth, .. } = input else {
                    panic!("expected audio")
                };
                seen.push(birth);
                2
            },
        );
        if case == 0 || case == 6 {
            assert!(result.is_ok());
            assert_eq!(
                seen,
                [if case == 0 {
                    Some(SourceBirth {
                        host_clock_epoch: 77,
                        born_ns: 650,
                    })
                } else {
                    None
                }]
            );
        } else {
            assert!(matches!(result, Err(Error::InvalidFrame)), "case {case}");
            assert!(
                seen.is_empty(),
                "invalid birth must never enter the media graph"
            );
        }
    }
}

#[test]
fn source_reset_receipt_blocks_fresh_pcm_until_actual_retirement() {
    use std::cell::Cell;
    unsafe extern "C" fn read(
        context: *mut c_void,
        _: *mut c_void,
        _: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        _: abi::rptadv_peer_source_text_v1,
        _: *mut c_void,
        _: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        let sink = unsafe { &*sink };
        let reset = sink.source_discontinuity.unwrap();
        let turn = unsafe { &*context.cast::<Cell<u32>>() };
        let current = turn.get();
        turn.set(current + 1);
        if current == 0 {
            assert_eq!(unsafe { reset(sink.context) }, 1);
            return 0;
        }
        let pcm = [0.25; 2];
        if current == 1 {
            assert_eq!(
                unsafe { sink.audio.unwrap()(sink.context, 0, pcm.as_ptr(), 2, 0) },
                0
            );
            assert_eq!(unsafe { sink.gap.unwrap()(sink.context, 0, 2) }, 0);
            assert_eq!(unsafe { reset(sink.context) }, 0);
        }
        assert_eq!(
            unsafe { sink.audio.unwrap()(sink.context, 0, pcm.as_ptr(), 2, 0) },
            2
        );
        0
    }
    let mut turn = Cell::new(0);
    let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.context = ptr::from_mut(&mut turn).cast();
        table.peer_info = Some(echo_info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    let mut resets = 0;
    let mut audio = 0;
    for _ in 0..3 {
        peer.read_source(SourceCompletion::Pending, |input| match input {
            SourceInput::Discontinuity => {
                resets += 1;
                usize::from(resets > 1)
            }
            SourceInput::Audio { .. } => {
                audio += 1;
                2
            }
            _ => panic!("unretired loss must stay provider-owned"),
        })
        .unwrap();
    }
    assert_eq!((resets, audio), (2, 2));
}
