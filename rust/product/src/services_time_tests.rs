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

#[test]
fn phone_source_digit_credit_is_ordered_private_and_retained_between_polls() {
    unsafe extern "C" fn info(
        _: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { super::phone::info(ptr::null_mut(), peer, output) };
        unsafe { (*output).capabilities = 51 };
        0
    }
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
        assert!(
            sink.struct_size as usize
                >= std::mem::offset_of!(abi::rptadv_peer_source_sink_v1, digit)
                    + size_of::<abi::rptadv_peer_source_digit_v1>()
        );
        let digit = sink.digit.expect("phone source digit capability");
        let rejected = context.addr() == 0;
        let pcm = [0.5; 4];
        assert_eq!(
            unsafe { digit(sink.context, u32::from(b'5')) },
            i32::from(rejected)
        );
        assert_eq!(
            unsafe { sink.audio.unwrap()(sink.context, 12, pcm.as_ptr(), pcm.len(), 0) },
            if rejected { 0 } else { 4 }
        );
        0
    }
    let (prepared, _) = crate::link::session::tests::phone_source_peer();
    let mut services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    let mut digits = Vec::new();
    let mut audio = 0;
    for accept in [false, true] {
        peer.services.0.context = ptr::without_provenance_mut(usize::from(accept));
        peer.read_source(SourceCompletion::Pending, |input| match input {
            SourceInput::Digit(digit) => {
                if accept {
                    digits.push(digit);
                }
                usize::from(accept)
            }
            SourceInput::Audio { samples, .. } => {
                audio += samples.len();
                samples.len()
            }
            _ => panic!("unexpected source event"),
        })
        .unwrap();
        assert_eq!(digits.len(), usize::from(accept));
        assert_eq!(audio, if accept { 4 } else { 0 });
    }
    assert_eq!(digits, ['5']);
    // Never offer phone-origin digits to the ordinary Echo/ASL source owner.
    unsafe extern "C" fn without_digits(
        _: *mut c_void,
        _: *mut c_void,
        _: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        _: abi::rptadv_peer_source_text_v1,
        _: *mut c_void,
        _: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        assert!(unsafe { (*sink).digit }.is_none());
        0
    }
    services.0.peer_info = Some(echo_info);
    services.0.peer_read_source = Some(without_digits);
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    assert_eq!(peer.identity().protocol(), ProtocolId::EchoLink);
    peer.read_source(SourceCompletion::Pending, |_| {
        panic!("no public digit callback")
    })
    .unwrap();
}

#[test]
fn phone_source_digit_rejects_invalid_symbols_and_invalid_acceptance() {
    unsafe extern "C" fn info(
        _: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { super::phone::info(ptr::null_mut(), peer, output) };
        unsafe { (*output).capabilities = 51 };
        0
    }
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
        let symbol = match context.addr() {
            0 => 0,
            1 => u32::from(b'a'),
            2 => 0x1_0035,
            _ => u32::from(b'5'),
        };
        if context.addr() == 5 {
            assert_eq!(unsafe { sink.digit.unwrap()(sink.context, symbol) }, 1);
            assert_eq!(
                unsafe { sink.digit.unwrap()(sink.context, u32::from(b'6')) },
                -1
            );
        } else {
            assert_eq!(unsafe { sink.digit.unwrap()(sink.context, symbol) }, -1);
        }
        let pcm = [0.5];
        assert_eq!(
            unsafe { sink.audio.unwrap()(sink.context, 0, pcm.as_ptr(), 1, 0) },
            0
        );
        0
    }
    let (prepared, _) = crate::link::session::tests::phone_source_peer();
    for mode in 0..=5 {
        let services = host_services(|table| {
            *table = prepared.services.0;
            table.context = ptr::without_provenance_mut(mode);
            table.peer_info = Some(info);
            table.peer_read_source = Some(read);
            table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
        });
        let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
        assert_eq!(
            peer.read_source(SourceCompletion::Pending, |event| {
                assert!(matches!(event, SourceInput::Digit('5')));
                match mode {
                    3 => 2,
                    4 => panic!("rejected dispatch panic"),
                    5 => 0,
                    _ => panic!("invalid symbol must not reach dispatch"),
                }
            })
            .err(),
            Some(Error::InvalidFrame),
            "mode {mode}",
        );
    }
}

#[test]
fn source_reset_callback_overcredit_fails_closed() {
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
        assert_eq!(
            unsafe { sink.source_discontinuity.unwrap()(sink.context) },
            -1
        );
        0
    }
    let (prepared, _) = crate::link::session::tests::source_peer();
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(echo_info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    assert_eq!(
        peer.read_source(SourceCompletion::Pending, |input| {
            assert!(matches!(input, SourceInput::Discontinuity));
            2
        })
        .err(),
        Some(Error::InvalidFrame)
    );
}

#[test]
fn reset_retry_keeps_exact_identity_across_reads() {
    #[derive(Default)]
    struct Provider {
        turn: usize,
        mode: u8,
        callbacks: Vec<i32>,
        credits: Vec<u32>,
    }
    unsafe extern "C" fn read(
        context: *mut c_void,
        _: *mut c_void,
        _: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        _: abi::rptadv_peer_source_text_v1,
        _: *mut c_void,
        _: *mut abi::rptadv_peer_source_result_v1,
    ) -> i32 {
        let state = unsafe { &mut *context.cast::<Provider>() };
        let sink = unsafe { &*sink };
        let Some(adaptation) = sink.source_adaptation else {
            return 0;
        };
        let result = if (state.mode == 3 && state.turn == 0) || (state.mode == 2 && state.turn != 0)
        {
            unsafe { sink.source_discontinuity.unwrap()(sink.context) }
        } else {
            let start = if state.mode == 5 {
                u64::MAX
            } else if state.turn != 0 && state.mode == 1 {
                11
            } else {
                10
            };
            let count = if state.mode == 4 {
                0
            } else if state.mode == 6 && state.turn != 0 {
                159
            } else {
                160
            };
            unsafe { adaptation(sink.context, start, count) }
        };
        state.callbacks.push(result);
        state
            .credits
            .push(unsafe { sink.gap.unwrap()(sink.context, 170, 5) });
        state.turn += 1;
        0
    }
    for mode in 0..=6 {
        let mut provider = Provider {
            mode,
            callbacks: Vec::with_capacity(2),
            credits: Vec::with_capacity(2),
            ..Provider::default()
        };
        let (prepared, _) = crate::link::session::tests::source_peer();
        let services = host_services(|table| {
            *table = prepared.services.0;
            table.context = ptr::from_mut(&mut provider).cast();
            table.peer_info = Some(echo_info);
            table.peer_read_source = Some(read);
            table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
        });
        let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
        let mut attempts = 0;
        let mut consume = |input: SourceInput<'_>| match input {
            SourceInput::Discontinuity => {
                attempts += 1;
                usize::from(attempts > 1)
            }
            SourceInput::Gap { count, .. } => count,
            // Exhaustive public consumer remains unchanged by the private cause.
            SourceInput::Audio { .. } | SourceInput::Digit(_) | SourceInput::Text { .. } => 0,
        };
        let (first, allocations, frees) = crate::fixture::heap_operations(|| {
            peer.read_source(SourceCompletion::Pending, &mut consume)
        });
        assert_eq!(
            (allocations, frees),
            (0, 0),
            "first cut has no heap operations"
        );
        if mode == 4 || mode == 5 {
            assert_eq!(first.err(), Some(Error::InvalidFrame));
            assert_eq!(provider.callbacks, [-1]);
            assert_eq!(provider.credits, [0]);
            continue;
        }
        first.unwrap();
        assert_eq!(
            provider.callbacks,
            [1],
            "a valid cause must be dispatched and retained"
        );
        let (second, allocations, frees) = crate::fixture::heap_operations(|| {
            peer.read_source(SourceCompletion::Pending, &mut consume)
        });
        assert_eq!((allocations, frees), (0, 0), "retry has no heap operations");
        if mode == 0 {
            second.unwrap();
            assert_eq!(provider.callbacks, [1, 0]);
            assert_eq!(provider.credits, [0, 5]);
            assert_eq!(attempts, 2);
        } else {
            assert_eq!(second.err(), Some(Error::InvalidFrame));
            assert_eq!(provider.callbacks, [1, -1]);
            assert_eq!(provider.credits, [0, 0]);
            assert_eq!(attempts, 1, "a changed identity must never reach the owner");
        }
    }
}

#[test]
fn private_causal_dispatch_keeps_exact_interval_without_public_enum_change() {
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
        unsafe { sink.source_adaptation.unwrap()(sink.context, 123, 160) }
    }
    let (prepared, _) = crate::link::session::tests::source_peer();
    let services = host_services(|table| {
        *table = prepared.services.0;
        table.peer_info = Some(echo_info);
        table.peer_read_source = Some(read);
        table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
    });
    let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
    let mut cut = None;
    let (result, allocations, frees) = crate::fixture::heap_operations(|| {
        peer.read_source_causal_at(
            SourceCompletion::Pending,
            SourceTimeReference::default(),
            |event| {
                if let SourceDispatch::AdaptationCut { start, count } = event {
                    cut = Some((start, count));
                }
                1
            },
        )
    });
    result.unwrap();
    assert_eq!(cut, Some((123, 160)));
    assert_eq!((allocations, frees), (0, 0));
    eprintln!(
        "SourceReset={} Option<SourceReset>={} PeerIo={}",
        size_of::<SourceReset>(),
        size_of::<Option<SourceReset>>(),
        size_of::<PeerIo>()
    );
}

#[test]
fn source_cause_peerio_state_size() {
    // Exact pre-change field layout, compiled by the same target/compiler.
    #[allow(dead_code)]
    struct BaselineLayout {
        services: HostServices,
        handle: NonNull<c_void>,
        identity: PeerIdentity,
        local: String,
        callsign: String,
        capabilities: u32,
        account_generation: u64,
        qso_generation: u64,
        provider_account: Option<PhoneAccountId>,
        source_reset_pending: bool,
        source_digit_pending: Option<char>,
    }
    eprintln!(
        "PeerIo baseline={} current={} delta={}",
        size_of::<BaselineLayout>(),
        size_of::<PeerIo>(),
        size_of::<PeerIo>() as isize - size_of::<BaselineLayout>() as isize
    );
}
