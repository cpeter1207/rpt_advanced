use crate::{
    Error, codec,
    connection::Connection,
    fixture::{host, reset},
};

#[test]
fn radio_start_failure_retains_unique_channel_cleanup_and_readiness_is_bounded() {
    reset();
    let mut radio = Connection::open(c"usb").unwrap().into_radio(960).unwrap();
    host(|s| s.failure = 30);
    assert_eq!(radio.start(c"usb"), Err(Error::Call));
    host(|s| s.failure = 0);
    assert_eq!(radio.start(c"usb"), Ok(()));
    assert_eq!(radio.ready(), Ok(false));
    host(|s| s.failure = 34);
    assert_eq!(radio.ready(), Ok(true));
    host(|s| s.failure = 31);
    assert_eq!(radio.ready(), Err(Error::Hangup));
    drop(radio);
    host(|s| s.clean());
}

#[test]
fn missing_and_mismatched_codec_cache_entries_are_skipped_and_invalid_bounds_release_channel() {
    reset();
    for missing in [true, false] {
        host(|s| {
            s.missing_cache = missing.then_some(3);
            s.rates[3] = if missing { 8000 } else { 4000 };
        });
        let formats = codec::candidates().unwrap();
        assert_eq!(
            formats.iter().map(|f| f.rate()).collect::<Vec<_>>(),
            vec![48000, 16000, 16000, 16000]
        );
        drop(formats);
        host(|s| s.clean());
    }
    for maximum in [0, usize::MAX] {
        assert!(matches!(
            Connection::open(c"usb").unwrap().into_radio(maximum),
            Err(Error::InvalidFrame)
        ));
        host(|s| s.clean());
    }
}

#[test]
fn preparation_allocation_failure_releases_formats_and_reserved_radio() {
    reset();
    for bytes in [
        9 * size_of::<(u32, bool, codec::Format)>(),
        5 * size_of::<codec::Format>(),
    ] {
        assert!(matches!(
            crate::fixture::fail_allocation(bytes, codec::candidates),
            Err(Error::Allocation)
        ));
        host(|state| state.clean());
    }
    let connection = Connection::open(c"usb").unwrap();
    assert!(matches!(
        crate::fixture::fail_allocation(7 * size_of::<f32>(), || connection.into_radio(7)),
        Err(Error::Allocation)
    ));
    host(|state| state.clean());
}

#[test]
fn radio_ignores_other_control_and_propagates_control_edge_indication_failure() {
    reset();
    let mut radio = Connection::open(c"usb").unwrap().into_radio(8).unwrap();
    host(|state| state.control(crate::bindings::AST_CONTROL_HANGUP as i32));
    radio.exchange(|_, _| panic!("unhandled control")).unwrap();
    host(|state| {
        state.control(0);
        state.edit_frame(|frame| frame.frametype = crate::bindings::AST_FRAME_TEXT);
    });
    radio.exchange(|_, _| panic!("unhandled text")).unwrap();
    host(|state| {
        state.failure = 7;
        state.control(crate::bindings::AST_CONTROL_RADIO_KEY as i32);
    });
    assert_eq!(
        radio.exchange(|receiving, samples| {
            assert!(receiving);
            assert!(samples.is_empty());
            true
        }),
        Err(Error::Indication)
    );
    drop(radio);
    host(|state| state.clean());
}

#[test]
fn mismatched_wire_format_rate_is_not_admitted_from_the_registry() {
    reset();
    host(|state| state.failure = 32);
    let formats = codec::candidates().unwrap();
    assert_eq!(
        formats.iter().map(codec::Format::rate).collect::<Vec<_>>(),
        [48000, 16000, 16000, 8000]
    );
    drop(formats);
    host(|state| state.clean());
}

#[test]
fn non_audio_registry_entries_are_not_offered_to_peers() {
    reset();
    host(|state| state.codecs[4].type_ = crate::bindings::AST_MEDIA_TYPE_VIDEO);
    let formats = codec::candidates().unwrap();
    assert_eq!(
        formats.iter().map(codec::Format::rate).collect::<Vec<_>>(),
        vec![48000, 16000, 16000, 16000, 8000]
    );
    drop(formats);
    host(|state| state.clean());
}

#[test]
fn translators_release_paths_and_consumed_frames_including_buffered_packets() {
    reset();
    let formats = codec::candidates().unwrap();
    host(|s| s.failure = 13);
    assert!(matches!(
        codec::Translator::new(&formats[0], &formats[1]),
        Err(Error::Translation)
    ));
    let mut identity = codec::Translator::new(&formats[0], &formats[0]).unwrap();
    host(|s| s.voice(vec![0; 2], 0));
    let read = || {
        crate::radio::Frame(
            std::ptr::NonNull::new(unsafe { crate::bindings::ast_read(std::ptr::null_mut()) })
                .unwrap(),
        )
    };
    drop(identity.translate(read()).unwrap());
    host(|s| {
        s.failure = 0;
        assert_eq!(s.translators, 0);
    });
    let mut translator = codec::Translator::new(&formats[0], &formats[1]).unwrap();
    for buffered in [false, true] {
        host(|s| {
            s.buffered = buffered;
            s.voice(vec![0; 2], 0);
        });
        let translated = translator.translate(read());
        assert_eq!(translated.is_none(), buffered);
        drop(translated);
    }
    drop(translator);
    drop(identity);
    drop(formats);
    host(|s| {
        assert_eq!(s.freed, 3);
        s.clean();
    });
}

#[test]
fn reservation_releases_all_owned_objects_at_every_failure() {
    reset();
    for (failure, error) in [
        (1, Error::MissingTechnology),
        (2, Error::MissingFormat),
        (3, Error::UnsupportedFormat),
        (4, Error::Reservation),
        (5, Error::ChannelFormat),
        (6, Error::ChannelFormat),
        (23, Error::MissingFormat),
        (24, Error::UnsupportedFormat),
        (25, Error::UnsupportedFormat),
        (26, Error::MissingFormat),
    ] {
        host(|s| s.failure = failure);
        assert!(matches!(Connection::open(c"usb"), Err(actual) if actual == error));
        host(|s| s.clean());
    }
    host(|s| {
        s.clean();
        s.failure = 0;
    });
    let connection = Connection::open(c"usb").unwrap();
    host(|s| {
        assert_eq!(s.channels, 1);
        assert!(s.indications.is_empty());
    });
    drop(connection);
    host(|s| s.clean());
}

#[test]
fn reservation_uses_an_owned_offer_after_technology_lookup() {
    reset();
    host(|state| state.technology_unloaded = true);
    let connection = Connection::open(c"usb").unwrap();
    drop(connection);
    host(|state| state.clean());
}

#[test]
fn reservation_rejects_lower_rate_and_compressed_channel_native_formats() {
    for (native, rate) in [(1, 16000), (3, 8000), (1, 48000)] {
        reset();
        host(|state| {
            state.native = native;
            state.rates[native] = rate;
            state.technology_unloaded = true;
        });
        assert!(matches!(
            Connection::open(c"usb"),
            Err(Error::UnsupportedFormat)
        ));
        host(|state| state.clean());
    }
}

#[test]
fn registry_orders_linear_first_and_preserves_ties_duplicates_and_same_rate_paths() {
    reset();
    for (duplicate, blocked, expected) in [
        (false, None, vec![0, 2, 1, 7, 3]),
        (true, None, vec![0, 2, 2, 1, 3]),
        (false, Some((2, true)), vec![0, 2, 3]),
        (false, Some((2, false)), vec![0, 2, 3]),
    ] {
        host(|s| {
            s.duplicate = duplicate;
            s.blocked = blocked;
        });
        let formats = codec::candidates().unwrap();
        let expected_pointers = host(|s| {
            expected
                .iter()
                .map(|i| s.format(*i) as usize)
                .collect::<Vec<_>>()
        });
        host(|s| s.offers.clear());
        for format in &formats {
            drop(format.offer().unwrap());
        }
        host(|s| assert_eq!(s.offers, expected_pointers));
        assert_eq!(formats[0].rate(), 48000);
        drop(formats);
        host(|s| s.clean());
    }
    host(|s| {
        s.blocked = None;
        s.missing_linear = true;
    });
    assert!(matches!(codec::candidates(), Err(Error::NoCandidates)));
    host(|s| {
        s.clean();
        s.missing_linear = false;
        s.failure = 10;
    });
    assert!(matches!(codec::candidates(), Err(Error::NoCandidates)));
    host(|s| {
        s.clean();
        s.failure = 0;
    });
    let formats = codec::candidates().unwrap();
    for (failure, error) in [(11, Error::Allocation), (12, Error::Offer)] {
        host(|s| s.failure = failure);
        assert!(matches!(formats[0].offer(), Err(actual) if actual == error));
    }
    drop(formats);
    host(|s| s.clean());
}

#[test]
fn radio_tracks_actual_samples_carrier_and_ptt_without_allocating_output_frames() {
    reset();
    let mut radio = Connection::open(c"usb").unwrap().into_radio(1024).unwrap();
    let mut total = 0;
    for count in [1, 7, 960, 1024] {
        host(|s| s.voice(vec![16384; count], 0));
        radio
            .exchange(|receiving, pcm| {
                assert!(!receiving);
                total += pcm.len();
                assert!(pcm.iter().all(|v| *v == 0.5));
                pcm.fill(-0.5);
                false
            })
            .unwrap();
        host(|s| assert_eq!(s.writes.last().unwrap(), &vec![-16384; count]));
    }
    for (condition, receiving) in [(12, true), (12, true), (13, false)] {
        host(|s| s.control(condition));
        radio
            .exchange(|rx, pcm| {
                assert_eq!(rx, receiving);
                assert!(pcm.is_empty());
                rx
            })
            .unwrap();
    }
    host(|s| {
        s.control(4);
        s.voice(vec![0; 3], 0);
    });
    radio
        .exchange(|_, _| panic!("unrelated control advanced controller"))
        .unwrap();
    radio
        .exchange(|_, pcm| {
            assert_eq!(pcm, &[0.0; 3]);
            false
        })
        .unwrap();
    assert_eq!(total, 1992);
    host(|s| {
        assert_eq!(s.indications, vec![12, 13]);
        assert_eq!(s.writes.len(), 5);
        assert_eq!(s.freed, 9);
    });
    assert_eq!(radio.exchange(|_, _| false), Err(Error::Hangup));
    drop(radio);
    host(|s| s.clean());
}

#[test]
fn rejected_frames_do_not_advance_controller_and_ptt_failures_retry_before_write() {
    reset();
    let mut radio = Connection::open(c"usb").unwrap().into_radio(8).unwrap();
    for malformed in 1..=6 {
        host(|s| s.voice(vec![0; 8], malformed));
        assert_eq!(
            radio.exchange(|_, _| panic!("invalid frame advanced controller")),
            Err(Error::InvalidFrame)
        );
    }
    host(|s| s.voice(vec![0; 9], 0));
    assert_eq!(
        radio.exchange(|_, _| panic!("oversize advanced controller")),
        Err(Error::InvalidFrame)
    );
    for (failure, expected) in [
        (7, Err(Error::Indication)),
        (8, Err(Error::Write)),
        (0, Ok(())),
    ] {
        host(|s| {
            s.failure = failure;
            s.voice(vec![0; 2], 0);
        });
        assert_eq!(
            radio.exchange(|_, pcm| {
                pcm.fill(2.0);
                true
            }),
            expected
        );
    }
    host(|s| {
        assert_eq!(s.indications, vec![12, 12]);
        assert_eq!(s.writes, vec![vec![32767; 2]; 2]);
        assert_eq!(s.freed, 10);
    });
    drop(radio);
    host(|s| {
        assert_eq!(s.indications, vec![12, 12, 13]);
        s.clean();
    });
}
