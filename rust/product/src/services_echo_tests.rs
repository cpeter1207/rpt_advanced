//! Foreign Echo source records are validated before accepting PCM credit or text.
use super::*;

unsafe extern "C" fn info_fault(
    mode: *mut c_void,
    peer: *const c_void,
    output: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    unsafe { echo_info(ptr::null_mut(), peer, output) };
    let output = unsafe { &mut *output };
    let bytes = |data: &'static [u8]| abi::rptadv_peer_bytes_v1 {
        data: data.as_ptr(),
        length: data.len(),
    };
    match mode.addr() {
        0 => (),
        1 => return -1,
        2 => output.struct_size = 0,
        3 => output.abi_version = 0,
        4 => output.protocol = 9,
        5 => output.remote = bytes(&[0xff]),
        6 => output.remote.length = 33,
        7 => output.remote.data = ptr::null(),
        8 => output.remote = bytes(b"000123"),
        9 => output.local.data = ptr::null(),
        10 => output.local.length = 0,
        11 => output.local = bytes(b"local"),
        12 => output.capabilities = 32,
        13 => output.account_generation = 0,
        14 => output.qso_generation = 0,
        15 => output.callsign.length = 0,
        16 => output.callsign = bytes(b"W1!XYZ"),
        17 => output.capabilities = 27,
        18 => output.capabilities = 7,
        19 => output.capabilities = 19,
        20 => output.protocol = 1,
        21 => output.capabilities = 21,
        22 => output.callsign.length = 65,
        23 => output.callsign.data = ptr::null(),
        24 => output.callsign = bytes(&[0xff]),
        25 => output.local.length = 33,
        26 => output.remote = bytes(b"0"),
        27..=30 => {
            output.protocol = 1;
            output.capabilities = [1, 3, 0, 2][mode.addr() - 27];
        }
        31 | 32 => (),
        _ => unreachable!(),
    }
    0
}

#[test]
fn copied_peer_identity_requires_canonical_names_and_complete_per_peer_capabilities() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for mode in 0..=32 {
        let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
        let services = host_services(|table| {
            *table = prepared.services.0;
            table.context = ptr::without_provenance_mut(mode);
            table.peer_info = Some(info_fault);
            table.peer_read_source = Some(read_fault);
            table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
            table.peer_dial = Some(peer_dial_mode);
            match mode {
                27 | 28 => {
                    table.peer_prepare_audio = None;
                    table.peer_reset_encoder = None;
                    table.peer_send_prepared = None;
                    table.peer_elapsed_ms = None;
                    table.peer_step_wake = None;
                    table.peer_set_deadline = None;
                    table.peer_read_source = None;
                    if mode == 27 {
                        table.peer_step_attach = None;
                        table.peer_step_stop = None;
                        table.peer_step_detach = None;
                    }
                }
                29 => table.peer_write = None,
                31 => table.peer_read_source = None,
                32 => table.peer_send_human_text = None,
                _ => (),
            }
        });
        let handle = crate::fixture::peer();
        match unsafe { services.peer(handle) } {
            Ok(mut peer) => {
                assert!(matches!(mode, 0 | 19), "invalid record admitted: {mode}");
                assert_eq!(peer.callsign(), "W1XYZ-R");
                assert_eq!(peer.generations(), (3, 4));
                assert!(peer.source_media());
                assert_eq!(peer.can_forward_text(), mode == 0);
                if mode == 19 {
                    assert!(matches!(
                        peer.send_human_text("W1XYZ-R", "Hello"),
                        Err(Error::Operation)
                    ));
                }
            }
            Err(error) => {
                assert!(!matches!(mode, 0 | 19), "valid record rejected: {mode}");
                assert!(matches!(error, Error::Admission));
                unsafe { services.0.peer_destroy.unwrap()(services.0.context, handle) };
                let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
                assert!(matches!(
                    services.dial("EchoLink/123456", "1000", 960, || true),
                    Err(Error::Admission)
                ));
                assert_eq!(
                    crate::fixture::PEER_DROPS.load(Ordering::Acquire),
                    before + 1,
                    "rejected owned dial leaked its handle: {mode}"
                );
            }
        }
    }
    let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
    let mut table = prepared.services.0;
    table.peer_step_attach = None;
    table.peer_step_stop = None;
    table.peer_step_detach = None;
    assert!(
        matches!(unsafe { HostServices::open(&table) }, Err(Error::Admission)),
        "complete codec egress still requires its serial owner"
    );
    let mut table = prepared.services.0;
    table.peer_prepare_audio = None;
    table.peer_reset_encoder = None;
    table.peer_send_prepared = None;
    table.peer_elapsed_ms = None;
    table.peer_step_wake = None;
    table.peer_set_deadline = None;
    assert!(table.peer_write.is_some());
    table.peer_read_source = Some(read_fault);
    assert!(
        matches!(unsafe { HostServices::open(&table) }, Err(Error::Admission)),
        "source reads require the complete codec egress group"
    );
    let services = host_services(|table| table.echo_account_status = None);
    assert_eq!(services.echo_account_status("1000"), Ok(None));
}

unsafe extern "C" fn read_fault(
    mode: *mut c_void,
    _: *mut c_void,
    _: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    text: abi::rptadv_peer_source_text_v1,
    context: *mut c_void,
    output: *mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    let mode = mode.addr();
    let sink = unsafe { &*sink };
    let output = unsafe { &mut *output };
    let pcm = [0.25; 4];
    let bytes = |data: &'static [u8]| abi::rptadv_peer_bytes_v1 {
        data: data.as_ptr(),
        length: data.len(),
    };
    match mode {
        0 => assert_eq!(
            unsafe { sink.audio.unwrap()(sink.context, 0, ptr::null(), 0, 0) },
            0
        ),
        1..=6 => {
            let (start, data, count, concealed) = match mode {
                1 => (0, pcm.as_ptr(), 4097, 0),
                2 => (0, ptr::null(), 1, 0),
                3 => (0, pcm.as_ptr(), 1, 2),
                4 => (u64::MAX, pcm.as_ptr(), 1, 0),
                _ => (0, pcm.as_ptr(), 1, 0),
            };
            assert_eq!(
                unsafe { sink.audio.unwrap()(sink.context, start, data, count, concealed) },
                0
            );
        }
        7 => assert_eq!(unsafe { sink.gap.unwrap()(sink.context, u64::MAX, 1) }, 0),
        8..=15 => {
            let mut sender = bytes(b"W1XYZ-R");
            let mut body = bytes(b"Hello");
            match mode {
                8 => sender.length = 0,
                9 => sender.length = 65,
                10 => sender.data = ptr::null(),
                11 => sender = bytes(&[0xff]),
                12 => body.length = 0,
                13 => body.length = 4097,
                14 => body.data = ptr::null(),
                _ => body = bytes(&[0xff]),
            }
            unsafe { text.unwrap()(context, sender, body) };
        }
        16 | 17 => {
            unsafe { text.unwrap()(context, bytes(b"W1XYZ-R"), bytes(b"Hello")) };
            if mode == 16 {
                unsafe { text.unwrap()(context, bytes(b"W1XYZ-R"), bytes(b"Duplicate")) };
            }
        }
        18 => return -1,
        19 => output.struct_size = 0,
        20 => output.abi_version = 0,
        21 => output.progress = 4,
        22 => output.activity = 3,
        23 => output.finish_pending = 2,
        24 => output.state = 4,
        25 => output.represented_node_number = 123456,
        _ => unreachable!(),
    }
    0
}

#[test]
fn malformed_source_records_or_failed_dispatch_never_acknowledge_unaccepted_media() {
    for mode in 0..=25 {
        let (prepared, _) = crate::link::session::tests::peer_with_egress(8000, true, true);
        let services = host_services(|table| {
            *table = prepared.services.0;
            table.context = ptr::without_provenance_mut(mode);
            table.peer_info = Some(echo_info);
            table.peer_read_source = Some(read_fault);
            table.peer_destroy = unsafe { (*crate::fixture::host_descriptor()).peer_destroy };
        });
        let mut peer = unsafe { services.peer(crate::fixture::peer()) }.unwrap();
        let mut dispatches = 0;
        let result = peer.read_source(SourceCompletion::Pending, |input| {
            dispatches += 1;
            match mode {
                0 => {
                    assert!(matches!(input, SourceInput::Audio { samples: [], .. }));
                    0
                }
                5 => 2, // More than the single offered sample is a contract violation.
                6 | 17 => panic!("source dispatch failed"),
                _ => panic!("malformed source reached dispatch: {mode}"),
            }
        });
        if mode == 0 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::InvalidFrame)), "case {mode}");
        }
        assert_eq!(
            dispatches,
            usize::from(matches!(mode, 0 | 5 | 6 | 17)),
            "case {mode}"
        );
    }
}
