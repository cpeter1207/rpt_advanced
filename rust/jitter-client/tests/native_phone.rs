#![cfg(target_os = "linux")]

use rptadv_jitter_client::{Admission, Jitter, Kind};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    path::PathBuf,
};

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

fn with_provider(check: impl FnOnce(*const c_void)) {
    let directory = std::env::var_os("RPT_ADVANCED_LIBDIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let path = CString::new(
        directory
            .join("librptadv_jitter_adapter.so.1")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    // SAFETY: the path and symbol strings remain live; this setup-only test
    // retains the library until both exclusive contexts have been destroyed.
    let library = unsafe { dlopen(path.as_ptr(), 2) };
    assert!(!library.is_null(), "load required native provider {path:?}");
    let symbol = unsafe { dlsym(library, c"rptadv_jitter_adapter_descriptor_v1".as_ptr()) };
    assert!(!symbol.is_null());
    let descriptor: unsafe extern "C" fn() -> *const c_void =
        unsafe { std::mem::transmute(symbol) };
    check(unsafe { descriptor() });
    assert_eq!(unsafe { dlclose(library) }, 0);
}

#[test]
fn phone_profile_interoperates_with_the_versioned_native_jitter_provider() {
    with_provider(|descriptor| {
        let mut phone = unsafe { Jitter::phone(descriptor) }.unwrap();
        let ordinary = unsafe { Jitter::new_at_rate(descriptor, 8000, 160, 160) }.unwrap();
        assert_eq!(phone.observe().unwrap().delay, 320);
        assert_eq!(ordinary.observe().unwrap().delay, 1440);
        assert_eq!(phone.release_span(), 160);
        assert!(phone.put(1000, 160, 20_000_000, &[0; 161]).is_err());
        let packet = [0x51; 160];
        assert_eq!(
            phone.put(1000, 160, 20_000_000, &packet),
            Ok(Admission::Accepted)
        );
        let mut payload = [0; 160];
        let waiting = phone.get(20_000_000, 160, &mut payload).unwrap();
        assert_eq!(waiting.kind, Kind::NotDue);
        let due = phone.get(60_000_000, 160, &mut payload).unwrap();
        assert_eq!(due.kind, Kind::Packet);
        assert_eq!(
            (due.timestamp, due.span, due.start_offset, due.length),
            (1000, 160, 0, 160)
        );
        assert_eq!(payload, packet);
        phone.advance(60_000_000, 0).unwrap();
    });
}

#[test]
fn live_policy_edit_preserves_queued_payloads_pending_release_and_other_calls() {
    with_provider(|descriptor| {
        let before = [30, 85, 140];
        let mut active =
            unsafe { Jitter::with_delays(descriptor, 8000, 160, 160, before) }.unwrap();
        let other = unsafe { Jitter::with_delays(descriptor, 8000, 160, 160, before) }.unwrap();
        assert!(active.supports_delay_update());
        for id in 0..6_u8 {
            assert_eq!(
                active.put(u64::from(id) * 160, 160, u64::from(id) * 10_000_000, &[id]),
                Ok(Admission::Accepted)
            );
        }
        let mut now = 85_000_000;
        let mut payload = [0; 160];
        for id in 0..6_u8 {
            let mut decision = active.get(now, 160, &mut payload).unwrap();
            if decision.kind == Kind::NotDue {
                now = decision.deadline_ns;
                decision = active.get(now, 160, &mut payload).unwrap();
            }
            assert_eq!(decision.kind, Kind::Packet);
            assert_eq!(
                (
                    decision.timestamp,
                    decision.span,
                    decision.length,
                    payload[0]
                ),
                (u64::from(id) * 160, 160, 1, id)
            );
            if id == 0 {
                let pending = active.observe().unwrap();
                assert!(active.set_delays([30, 10, 40]).is_err());
                assert!(active.set_delays([10, 20, u32::MAX]).is_err());
                assert!(active.set_delays([10, 10, 10]).is_err());
                active.set_delays([10, 20, 40]).unwrap();
                active.set_delays([10, 20, 40]).unwrap();
                let retained = active.observe().unwrap();
                assert_eq!(
                    (retained.delay, retained.queued_packets),
                    (pending.delay, pending.queued_packets)
                );
            }
            active.advance(now, 0).unwrap();
            assert!(active.advance(now, 0).is_err());
        }
        let drained = active.observe().unwrap();
        assert_eq!(
            (
                drained.queued_packets,
                drained.limit_resets,
                drained.overload_drops
            ),
            (0, 0, 0)
        );
        assert!(drained.delay <= 320);
        assert_eq!(other.observe().unwrap().delay, 680);
        active.reset().unwrap();
        assert_eq!(active.observe().unwrap().delay, 160);
        active.set_delays(before).unwrap();
        assert_eq!(active.observe().unwrap().delay, 680);
        assert_eq!(active.put(0, 160, 0, &[7]), Ok(Admission::Accepted));
        let first = active.get(85_000_000, 160, &mut payload).unwrap();
        assert_eq!(first.kind, Kind::Packet);
        active.set_delays([100, 110, 140]).unwrap();
        assert_eq!(active.observe().unwrap().delay, 680);
        active.advance(85_000_000, 0).unwrap();
        assert!(active.observe().unwrap().delay >= 800);
        active.reset().unwrap();
        assert_eq!(active.observe().unwrap().delay, 880);
        assert_eq!(other.observe().unwrap().delay, 680);
    });
}

fn assert_packet(jitter: &mut Jitter, at_ms: u64, span: u32, timestamp: u64, byte: u8) {
    let mut payload = [0; 160];
    let result = jitter.get(at_ms * 1_000_000, span, &mut payload).unwrap();
    assert_eq!(result.kind, Kind::Packet);
    assert_eq!(
        (result.timestamp, result.span, result.start_offset),
        (timestamp, span, 0)
    );
    assert_eq!((result.length, payload[0]), (1, byte));
}

#[test]
fn policy_revision_continuous_lookahead_drains_at_effective_deadlines() {
    with_provider(|descriptor| {
        let open =
            || unsafe { Jitter::with_delays(descriptor, 8000, 160, 160, [30, 85, 140]) }.unwrap();
        let mut active = open();
        let mut other = open();
        assert_eq!(other.put(960, 160, 0, &[42]), Ok(Admission::Accepted));
        assert_packet(&mut other, 85, 160, 960, 42);
        for id in 0..7_u8 {
            assert_eq!(
                active.put(u64::from(id) * 160, 160, u64::from(id) * 10_000_000, &[id]),
                Ok(Admission::Accepted)
            );
        }
        for (id, (at, next, delay)) in [
            (85, 105, 680),
            (105, 115, 600),
            (115, 125, 520),
            (125, 135, 440),
            (135, 145, 360),
            (145, 160, 320),
            (160, 180, 320),
        ]
        .into_iter()
        .enumerate()
        {
            assert_packet(&mut active, at, 160, id as u64 * 160, id as u8);
            if id == 0 {
                active.set_delays([10, 20, 40]).unwrap();
            }
            assert!(active.advance(at * 1_000_000, 321).is_err());
            active.advance(at * 1_000_000, 160).unwrap();
            assert!(active.advance(at * 1_000_000, 160).is_err());
            let waiting = active.get(at * 1_000_000, 160, &mut [0; 160]).unwrap();
            assert_eq!(waiting.kind, Kind::NotDue);
            assert_eq!(
                waiting.deadline_ns,
                next * 1_000_000,
                "lookahead deadline after packet {id}"
            );
            let status = active.observe().unwrap();
            assert_eq!(status.delay, delay);
            assert_eq!(
                (status.queued_packets, status.buffered_span),
                (6 - id as u32, (6 - id as u64) * 160)
            );
            assert_eq!(
                (
                    status.skipped_span,
                    status.missing_span,
                    status.insertion_span,
                    status.late_drops,
                    status.limit_resets,
                    status.overload_drops
                ),
                (0, 0, 0, 0, 0, 0)
            );
        }
        other.advance(85_000_000, 0).unwrap();
        assert!(other.advance(85_000_000, 0).is_err());
        assert_eq!(other.observe().unwrap().delay, 680);
    });
}

#[test]
fn policy_revision_reordered_priming_distinguishes_existing_from_new_excess() {
    with_provider(|descriptor| {
        for reordering_before_edit in [true, false] {
            let mut call =
                unsafe { Jitter::with_delays(descriptor, 8000, 160, 160, [30, 85, 140]) }.unwrap();
            assert_eq!(call.put(320, 160, 0, &[2]), Ok(Admission::Accepted));
            call.set_delays([10, 20, 40]).unwrap();
            if reordering_before_edit {
                assert_eq!(call.put(160, 160, 1_000_000, &[1]), Ok(Admission::Accepted));
            }
            call.set_delays([10, 20, 100]).unwrap();
            if !reordering_before_edit {
                assert_eq!(call.put(160, 160, 1_000_000, &[1]), Ok(Admission::Accepted));
            }
            let mut bytes = [0; 160];
            let result = call.get(85_000_000, 160, &mut bytes).unwrap();
            let status = call.observe().unwrap();
            if reordering_before_edit {
                assert_eq!(result.kind, Kind::Packet);
                assert_eq!(
                    (
                        result.timestamp,
                        result.span,
                        result.start_offset,
                        result.length,
                        bytes[0]
                    ),
                    (160, 160, 0, 1, 1)
                );
                assert_eq!((status.limit_resets, status.overload_drops), (0, 0));
                call.advance(85_000_000, 0).unwrap();
            } else {
                assert_eq!(
                    result.kind,
                    Kind::NotDue,
                    "fresh reordered packet must cross the new overload limit"
                );
                assert_eq!(
                    (
                        status.limit_resets,
                        status.overload_drops,
                        status.overload_span,
                        status.queued_packets
                    ),
                    (1, 2, 320, 0)
                );
            }
        }
    });
}

#[test]
fn policy_revision_short_coding_unit_rolls_back_on_the_original_call() {
    with_provider(|descriptor| {
        let mut call =
            unsafe { Jitter::with_delays(descriptor, 8000, 80, 160, [10, 10, 10]) }.unwrap();
        assert_eq!(call.release_span(), 160);
        for id in 0..3_u8 {
            assert_eq!(
                call.put(u64::from(id) * 80, 80, u64::from(id) * 1_000_000, &[id]),
                Ok(Admission::Accepted)
            );
        }
        assert_packet(&mut call, 10, 80, 0, 0);
        call.set_delays([10, 20, 40]).unwrap();
        call.advance(10_000_000, 0).unwrap();
        assert_packet(&mut call, 20, 80, 80, 1);
        assert_eq!(
            call.set_delays([10, 10, 10]),
            Ok(()),
            "restore the short-unit policy accepted at construction"
        );
        call.advance(20_000_000, 0).unwrap();
        assert_packet(&mut call, 30, 80, 160, 2);
        call.advance(30_000_000, 0).unwrap();
        assert_eq!(call.observe().unwrap().delay, 80);
    });
}
