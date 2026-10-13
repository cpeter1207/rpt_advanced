use super::*;
use std::{ffi::c_void, mem::size_of, ptr};

fn config() -> abi::rptadv_jitter_config_v1 {
    abi::rptadv_jitter_config_v1 {
        struct_size: size_of::<abi::rptadv_jitter_config_v1>() as u32,
        abi_version: 1,
        clock_rate_hz: 8000,
        minimum_delay: 320,
        initial_delay: 480,
        maximum_delay: 4000,
        adjustment_step: 80,
        concealment_span: 80,
        maximum_packets: 32,
        maximum_payload_bytes: 1500,
    }
}

fn descriptor() -> &'static abi::rptadv_jitter_descriptor_v1 {
    let pointer = rptadv_jitter_adapter_descriptor_v1();
    assert!(
        !pointer.is_null(),
        "jitter capability must provide its descriptor"
    );
    unsafe { &*pointer }
}

struct Jitter(*mut c_void);

impl Jitter {
    fn new(options: abi::rptadv_jitter_config_v1) -> Self {
        let mut handle = ptr::null_mut();
        assert_eq!(
            unsafe { descriptor().create.unwrap()(&options, &mut handle) },
            0
        );
        assert!(!handle.is_null());
        Self(handle)
    }

    fn put(&mut self, timestamp: u64, span: u32, arrival_ms: u64, bytes: &[u8]) -> i32 {
        let packet = abi::rptadv_jitter_packet_v1 {
            struct_size: size_of::<abi::rptadv_jitter_packet_v1>() as u32,
            timestamp,
            span,
            arrival_ns: arrival_ms * 1_000_000,
            payload: bytes.as_ptr(),
            length: bytes.len(),
        };
        unsafe { descriptor().put.unwrap()(self.0, &packet) }
    }

    fn get(&mut self, now_ms: u64, span: u32) -> (abi::rptadv_jitter_result_v1, Vec<u8>) {
        let mut result: abi::rptadv_jitter_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_jitter_result_v1>() as u32;
        let mut bytes = vec![0; 1500];
        assert_eq!(
            unsafe {
                descriptor().get.unwrap()(
                    self.0,
                    now_ms * 1_000_000,
                    span,
                    bytes.as_mut_ptr(),
                    bytes.len(),
                    &mut result,
                )
            },
            0
        );
        bytes.truncate(result.length);
        (result, bytes)
    }

    fn advance(&mut self, now_ms: u64) {
        assert_eq!(
            unsafe { descriptor().advance.unwrap()(self.0, now_ms * 1_000_000, 0) },
            0
        );
    }

    fn observe(&self) -> abi::rptadv_jitter_observation_v1 {
        let mut result: abi::rptadv_jitter_observation_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_jitter_observation_v1>() as u32;
        assert_eq!(
            unsafe { descriptor().observe.unwrap()(self.0, &mut result) },
            0
        );
        result
    }
}

impl Drop for Jitter {
    fn drop(&mut self) {
        unsafe { descriptor().destroy.unwrap()(self.0) };
    }
}

#[test]
fn descriptor_can_create_and_destroy_an_independent_jitter_context() {
    let api = descriptor();
    assert_eq!(api.abi_version, 1);
    assert_eq!(api.capability, *b"rptadv.jitter\0\0\0");
    let config = config();
    let mut context = std::ptr::null_mut();
    assert_eq!(unsafe { api.create.unwrap()(&config, &mut context) }, 0);
    assert!(!context.is_null());
    unsafe { api.destroy.unwrap()(context) };
}

#[test]
fn startup_waits_sixty_ms_and_returns_copied_packets_in_timestamp_order() {
    let mut jitter = Jitter::new(config());
    let mut bytes = [1, 2, 3];
    assert_eq!(jitter.put(1000, 160, 100, &bytes), 0);
    bytes.fill(9);
    assert_eq!(jitter.put(1320, 160, 120, &[5]), 0);
    assert_eq!(jitter.put(1160, 160, 130, &[4]), 0);
    let (waiting, _) = jitter.get(159, 160);
    assert_eq!(waiting.kind, abi::RPTADV_JITTER_NOT_DUE);
    assert_eq!(waiting.deadline_ns, 160_000_000);
    for (now, timestamp, expected) in [
        (160, 1000, vec![1, 2, 3]),
        (180, 1160, vec![4]),
        (200, 1320, vec![5]),
    ] {
        let (packet, bytes) = jitter.get(now, 160);
        assert_eq!(
            (
                packet.kind,
                packet.timestamp,
                packet.span,
                packet.start_offset
            ),
            (0, timestamp, 160, 0)
        );
        assert_eq!(bytes, expected);
        jitter.advance(now);
    }
}

#[test]
fn packets_cross_native_timestamp_wrap_without_reordering() {
    let mut jitter = Jitter::new(config());
    assert_eq!(jitter.put(0xffff_fff0, 160, 0, &[1]), 0);
    assert_eq!(jitter.put(0x1_0000_0090, 160, 20, &[2]), 0);
    assert_eq!(jitter.get(60, 160).0.timestamp, 0xffff_fff0);
    jitter.advance(60);
    assert_eq!(jitter.get(80, 160).0.timestamp, 0x1_0000_0090);
}

#[test]
fn missing_time_precedes_later_payload_instead_of_compressing_the_timeline() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.put(1320, 160, 20, &[3]);
    assert_eq!(jitter.get(60, 160).0.kind, abi::RPTADV_JITTER_PACKET);
    jitter.advance(60);
    let (gap, bytes) = jitter.get(80, 160);
    assert_eq!(
        (gap.kind, gap.timestamp, gap.span),
        (abi::RPTADV_JITTER_MISSING, 1160, 80)
    );
    assert!(bytes.is_empty());
    jitter.advance(80);
    assert_eq!(jitter.get(90, 160).0.span, 80);
    jitter.advance(90);
    assert_eq!(jitter.get(100, 160).1, [3]);
    assert_eq!(jitter.observe().missing_span, 160);
}

#[test]
fn signed_offsets_preserve_partial_and_overlapping_packet_spans() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 80, 0, &[1]);
    jitter.put(1040, 160, 10, &[2]);
    jitter.put(1240, 120, 20, &[3]);
    assert_eq!(jitter.get(60, 160).0.span, 80);
    jitter.advance(60);
    // Arrival projection after a short return must not make Speex warn about negative buffering.
    assert_eq!(jitter.put(1400, 160, 65, &[4]), 0);
    let (overlap, _) = jitter.get(70, 160);
    assert_eq!(
        (overlap.timestamp, overlap.span, overlap.start_offset),
        (1040, 160, -40)
    );
    jitter.advance(70);
    let (prefix, _) = jitter.get(85, 160);
    assert_eq!(
        (prefix.timestamp, prefix.span, prefix.start_offset),
        (1240, 120, 40)
    );
}

#[test]
fn an_overlap_shorter_than_native_quantum_is_returned_without_losing_its_tail() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.put(1120, 41, 10, &[2]);
    jitter.get(60, 160);
    jitter.advance(60);
    let (tail, bytes) = jitter.get(80, 160);
    assert_eq!(
        (tail.timestamp, tail.span, tail.start_offset),
        (1120, 41, -40)
    );
    assert_eq!(bytes, [2]);
    jitter.advance(80);
    assert_eq!(jitter.put(1320, 160, 80, &[3]), 0);
}

#[test]
fn duplicate_and_full_admission_do_not_replace_retained_payloads() {
    let mut options = config();
    options.maximum_packets = 2;
    let mut jitter = Jitter::new(options);
    assert_eq!(jitter.put(1000, 160, 0, &[1]), 0);
    assert_eq!(
        jitter.put(1000, 160, 10, &[9]),
        abi::RPTADV_JITTER_DUPLICATE as i32
    );
    assert_eq!(jitter.put(1160, 160, 20, &[2]), 0);
    assert_eq!(
        jitter.put(1320, 160, 40, &[3]),
        abi::RPTADV_JITTER_FULL as i32
    );
    assert_eq!(jitter.get(60, 160).1, [1]);
    let observation = jitter.observe();
    assert_eq!(
        (observation.duplicate_drops, observation.full_drops),
        (1, 1)
    );
}

#[test]
fn reset_starts_an_independent_timestamp_epoch_and_clears_queued_media() {
    let mut first = Jitter::new(config());
    let mut second = Jitter::new(config());
    first.put(1000, 160, 0, &[1]);
    second.put(7000, 160, 20, &[2]);
    assert_eq!(unsafe { descriptor().reset.unwrap()(first.0) }, 0);
    assert_eq!(first.observe().queued_packets, 0);
    first.put(10, 160, 100, &[3]);
    assert_eq!(second.get(80, 160).1, [2]);
    assert_eq!(first.get(160, 160).1, [3]);
}

#[test]
fn too_small_output_does_not_consume_a_packet_and_double_advance_is_rejected() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    let mut result: abi::rptadv_jitter_result_v1 = unsafe { std::mem::zeroed() };
    result.struct_size = size_of::<abi::rptadv_jitter_result_v1>() as u32;
    let mut output = [0];
    assert_eq!(
        unsafe {
            descriptor().get.unwrap()(
                jitter.0,
                60_000_000,
                160,
                output.as_mut_ptr(),
                1,
                &mut result,
            )
        },
        -1
    );
    assert_eq!(jitter.get(60, 160).1, [1]);
    jitter.advance(60);
    assert_eq!(
        unsafe { descriptor().advance.unwrap()(jitter.0, 60_000_000, 0) },
        -1
    );
}

#[test]
fn adaptive_delay_reaches_but_never_exceeds_five_hundred_ms() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.get(60, 160);
    jitter.advance(60);
    jitter.put(1160, 160, 480, &[2]);
    assert_eq!(jitter.get(480, 160).1, [2]);
    jitter.advance(480);
    let insertion = jitter.get(480, 160).0;
    assert_eq!(
        (insertion.kind, insertion.span),
        (abi::RPTADV_JITTER_INSERTION, 3520)
    );
    jitter.advance(480);
    assert_eq!(jitter.observe().delay, 4000);
    // A separate initially late trace asks for >500ms in one native adjustment.
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.get(60, 160);
    jitter.advance(60);
    jitter.put(1160, 160, 600, &[3]);
    assert_eq!(jitter.get(600, 160).1, [3]);
    jitter.advance(600);
    jitter.put(1320, 160, 600, &[4]);
    let reset = jitter.get(600, 160).0;
    assert_eq!(reset.kind, abi::RPTADV_JITTER_NOT_DUE);
    let observation = jitter.observe();
    assert_eq!(
        (
            observation.delay,
            observation.limit_resets,
            observation.overload_drops,
            observation.overload_span,
            observation.queued_packets
        ),
        (480, 1, 1, 160, 0)
    );
    // Recreating native state clears its otherwise-surviving deferred insertion.
    jitter.put(9000, 160, 610, &[9]);
    assert_eq!(jitter.get(669, 160).0.kind, abi::RPTADV_JITTER_NOT_DUE);
    assert_eq!(jitter.get(670, 160).1, [9]);
}

#[test]
fn batching_uses_receive_metadata_not_task_execution_time_and_delay_stays_above_forty_ms() {
    fn trace(task_lag: u64) -> Vec<(u32, u64, u32, u32)> {
        let mut jitter = Jitter::new(config());
        let mut result = Vec::new();
        for index in 0..100 {
            assert_eq!(
                jitter.put(1000 + index * 160, 160, index * 20, &[index as u8]),
                0
            );
            let (decision, _) = jitter.get(60 + index * 20 + task_lag, 160);
            jitter.advance(60 + index * 20 + task_lag);
            let observation = jitter.observe();
            assert!((320..=4000).contains(&observation.delay));
            result.push((
                decision.kind,
                decision.timestamp,
                decision.span,
                observation.delay,
            ));
        }
        assert_eq!(jitter.observe().delay, 320);
        assert!(jitter.observe().skipped_span > 0);
        result
    }
    assert_eq!(trace(0), trace(40));
}

#[test]
fn explicit_reset_clears_deferred_insertion() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.get(60, 160);
    jitter.advance(60);
    jitter.put(1160, 160, 100, &[2]);
    jitter.get(100, 160);
    jitter.advance(100);
    assert_eq!(unsafe { descriptor().reset.unwrap()(jitter.0) }, 0);
    jitter.put(9000, 160, 110, &[9]);
    assert_eq!(jitter.get(170, 160).1, [9]);
}

#[test]
fn native_silence_resynchronization_gets_a_fresh_monotonic_startup() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[1]);
    jitter.get(60, 160);
    jitter.advance(60);
    for now in (80..=280).step_by(10) {
        assert_eq!(jitter.get(now, 160).0.kind, abi::RPTADV_JITTER_MISSING);
        jitter.advance(now);
    }
    jitter.put(9000, 160, 300, &[9]);
    assert_eq!(jitter.get(359, 160).0.kind, abi::RPTADV_JITTER_NOT_DUE);
    assert_eq!(jitter.get(360, 160).1, [9]);
    assert_eq!(jitter.observe().missing_span, 1680);
}

#[test]
fn retained_decoded_tail_counts_once_and_matches_whole_publication() {
    fn trace(retain: u32) -> Vec<(u32, u64, u32, u64, u32)> {
        let mut jitter = Jitter::new(config());
        let mut trace = Vec::new();
        for (index, arrival) in [0, 75, 90, 100, 110, 120].into_iter().enumerate() {
            // Receive metadata predates the delayed owner task's current clock.
            jitter.put(1000 + index as u64 * 160, 160, arrival, &[index as u8]);
            let now = 200 + index as u64 * 100;
            let (decision, _) = jitter.get(now, 160);
            let tail = if decision.kind == abi::RPTADV_JITTER_PACKET {
                retain.min((i64::from(decision.span) + i64::from(decision.start_offset)) as u32)
            } else {
                0
            };
            assert_eq!(
                unsafe { descriptor().advance.unwrap()(jitter.0, now * 1_000_000, tail) },
                0
            );
            trace.push((
                decision.kind,
                decision.timestamp,
                decision.span,
                decision.deadline_ns,
                jitter.observe().delay,
            ));
        }
        trace
    }
    assert_eq!(trace(0), trace(80));
}

#[test]
fn malformed_configuration_rejects_without_returning_a_handle() {
    let mutations: &[fn(&mut abi::rptadv_jitter_config_v1)] = &[
        |c| c.struct_size = 0,
        |c| c.abi_version = 2,
        |c| c.clock_rate_hz = 0,
        |c| c.clock_rate_hz = 48001,
        |c| c.minimum_delay = 0,
        |c| c.minimum_delay = c.initial_delay + 1,
        |c| c.initial_delay = c.maximum_delay + 1,
        |c| c.maximum_delay = 32768,
        |c| c.adjustment_step = 0,
        |c| c.adjustment_step = c.maximum_delay + 1,
        |c| c.concealment_span = 0,
        |c| c.concealment_span = c.maximum_delay + 1,
        |c| c.adjustment_step = 81,
        |c| c.maximum_packets = 0,
        |c| c.maximum_packets = 201,
        |c| c.maximum_payload_bytes = 0,
        |c| c.maximum_payload_bytes = 1501,
    ];
    for mutate in mutations {
        let mut options = config();
        mutate(&mut options);
        let mut handle = ptr::dangling_mut();
        assert_eq!(
            unsafe { descriptor().create.unwrap()(&options, &mut handle) },
            -1
        );
        assert!(handle.is_null());
    }
    unsafe { descriptor().destroy.unwrap()(ptr::null_mut()) };
}

#[test]
fn invalid_packet_arguments_do_not_change_the_queue_and_expired_packets_are_counted() {
    let mut jitter = Jitter::new(config());
    for (timestamp, span, arrival, payload) in [
        (1000, 0, 0, &[1][..]),
        (1000, 4001, 0, &[1][..]),
        (1000, 160, 0, &[][..]),
        (u64::MAX, 160, 0, &[1][..]),
    ] {
        assert_eq!(jitter.put(timestamp, span, arrival, payload), -1);
        assert_eq!(jitter.observe().queued_packets, 0);
    }
    jitter.put(1000, 160, 0, &[1]);
    jitter.get(60, 160);
    assert_eq!(jitter.put(1160, 160, 20, &[2]), -1);
    jitter.advance(60);
    assert_eq!(
        jitter.put(700, 160, 100, &[0]),
        abi::RPTADV_JITTER_LATE as i32
    );
    assert_eq!(jitter.observe().late_drops, 1);
    assert_eq!(jitter.observe().queued_packets, 0);
}

#[test]
fn abi_rejects_null_short_and_oversized_arguments_without_mutating_valid_context() {
    let api = descriptor();
    let options = config();
    let mut handle = ptr::dangling_mut();
    unsafe {
        assert_eq!(api.create.unwrap()(&options, ptr::null_mut()), -1);
        assert_eq!(api.create.unwrap()(ptr::null(), &mut handle), -1);
        assert!(handle.is_null());
        assert_eq!(api.reset.unwrap()(ptr::null_mut()), -1);
        assert_eq!(api.advance.unwrap()(ptr::null_mut(), 0, 0), -1);
    }
    let mut jitter = Jitter::new(options);
    let mut packet = abi::rptadv_jitter_packet_v1 {
        struct_size: size_of::<abi::rptadv_jitter_packet_v1>() as u32,
        timestamp: 1000,
        span: 160,
        arrival_ns: 0,
        payload: [1].as_ptr(),
        length: 1,
    };
    unsafe {
        assert_eq!(api.put.unwrap()(ptr::null_mut(), &packet), -1);
        assert_eq!(api.put.unwrap()(jitter.0, ptr::null()), -1);
    }
    for mutation in [
        |p: &mut abi::rptadv_jitter_packet_v1| p.struct_size = 0,
        |p: &mut abi::rptadv_jitter_packet_v1| p.payload = ptr::null(),
        |p: &mut abi::rptadv_jitter_packet_v1| p.length = isize::MAX as usize + 1,
        |p: &mut abi::rptadv_jitter_packet_v1| p.arrival_ns = u64::MAX,
    ] {
        let original = packet;
        mutation(&mut packet);
        assert_eq!(unsafe { api.put.unwrap()(jitter.0, &packet) }, -1);
        packet = original;
    }
    assert_eq!(jitter.put(1000, 160, 0, &[1; 1501]), -1);
    assert_eq!(jitter.get(0, 160).0.kind, abi::RPTADV_JITTER_NOT_DUE);
    assert_eq!(jitter.put(1000, 160, 0, &[1]), 0);
    assert_eq!(jitter.put(1000 + i32::MAX as u64 + 1, 160, 0, &[1]), -1);
    let mut output: abi::rptadv_jitter_result_v1 = unsafe { std::mem::zeroed() };
    let mut bytes = [0; 1500];
    output.struct_size = size_of::<abi::rptadv_jitter_result_v1>() as u32;
    for (context, capacity, payload, result) in [
        (
            ptr::null_mut(),
            bytes.len(),
            bytes.as_mut_ptr(),
            ptr::from_mut(&mut output),
        ),
        (jitter.0, bytes.len(), bytes.as_mut_ptr(), ptr::null_mut()),
        (
            jitter.0,
            bytes.len(),
            ptr::null_mut(),
            ptr::from_mut(&mut output),
        ),
        (
            jitter.0,
            isize::MAX as usize + 1,
            bytes.as_mut_ptr(),
            ptr::from_mut(&mut output),
        ),
    ] {
        assert_eq!(
            unsafe { api.get.unwrap()(context, 0, 160, payload, capacity, result) },
            -1
        );
    }
    output.struct_size = 0;
    assert_eq!(
        unsafe {
            api.get.unwrap()(
                jitter.0,
                0,
                160,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut output,
            )
        },
        -1
    );
    output.struct_size = size_of::<abi::rptadv_jitter_result_v1>() as u32;
    for desired in [0, 4001, 1] {
        assert_eq!(
            unsafe {
                api.get.unwrap()(
                    jitter.0,
                    0,
                    desired,
                    bytes.as_mut_ptr(),
                    bytes.len(),
                    &mut output,
                )
            },
            -1
        );
    }
    jitter.get(59, 160);
    assert_eq!(
        unsafe {
            api.get.unwrap()(
                jitter.0,
                0,
                160,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut output,
            )
        },
        -1
    );
    jitter.get(60, 160);
    assert_eq!(
        unsafe {
            api.get.unwrap()(
                jitter.0,
                60_000_000,
                160,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut output,
            )
        },
        -1
    );
    for (now, tail) in [(59_000_000, 0), (60_000_000, 4001), (60_000_000, 161)] {
        assert_eq!(unsafe { api.advance.unwrap()(jitter.0, now, tail) }, -1);
    }
    jitter.advance(60);
    let mut observation = jitter.observe();
    unsafe {
        assert_eq!(api.observe.unwrap()(ptr::null(), &mut observation), -1);
        assert_eq!(api.observe.unwrap()(jitter.0, ptr::null_mut()), -1);
        observation.struct_size = 0;
        assert_eq!(api.observe.unwrap()(jitter.0, &mut observation), -1);
    }
    assert_eq!(jitter.observe().queued_packets, 0);
    assert_eq!(boundary(|| panic!("exercise the ABI panic barrier")), -1);
}

#[test]
fn reordered_startup_beyond_maximum_delay_is_counted_and_restarts_cleanly() {
    let mut jitter = Jitter::new(config());
    assert_eq!(jitter.put(5000, 160, 0, &[1]), 0);
    assert_eq!(jitter.put(1000, 160, 20, &[2]), 0);
    let result = jitter.get(60, 160).0;
    assert_eq!(
        (result.kind, result.deadline_ns),
        (abi::RPTADV_JITTER_NOT_DUE, 0)
    );
    let observation = jitter.observe();
    assert_eq!(
        (
            observation.limit_resets,
            observation.overload_drops,
            observation.overload_span
        ),
        (1, 2, 320)
    );
    assert_eq!(jitter.put(6000, 160, 100, &[3]), 0);
    assert_eq!(jitter.get(159, 160).0.kind, abi::RPTADV_JITTER_NOT_DUE);
    assert_eq!(jitter.get(160, 160).1, [3]);
    jitter.advance(160);
}

#[test]
fn native_allocation_failure_does_not_publish_or_destroy_a_valid_context() {
    native_calls::fail_init();
    let mut output = ptr::dangling_mut();
    assert_eq!(unsafe { create(&config(), &mut output) }, -1);
    assert!(output.is_null());
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[7]);
    native_calls::fail_init();
    assert_eq!(unsafe { reset(jitter.0) }, -1);
    assert_eq!(jitter.observe().queued_packets, 1);
    assert_eq!(jitter.get(60, 160).1, [7]);
    jitter.advance(60);
}

#[test]
fn native_bad_get_results_fail_without_committing_or_losing_the_pending_packet() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[7]);
    let context = unsafe { &mut *jitter.0.cast::<Context>() };
    let mut result = unsafe { std::mem::zeroed() };
    let mut bytes = [0; 1500];
    for (kind, span) in [(-1, 160), (0, 0)] {
        native_calls::get_response(kind, span);
        assert_eq!(context.get(60_000_000, 160, &mut bytes, &mut result), -1);
        assert!(context.pending.is_none());
    }
    assert_eq!(jitter.get(60, 160).1, [7]);
    jitter.advance(60);
}

#[test]
fn native_reset_failures_propagate_at_resync_and_both_maximum_delay_guards() {
    for state in 0..3 {
        let mut jitter = Jitter::new(config());
        jitter.put(5000, 160, 0, &[7]);
        let context = unsafe { &mut *jitter.0.cast::<Context>() };
        let mut result = unsafe { std::mem::zeroed() };
        let mut bytes = [0; 1500];
        match state {
            0 => {
                context.unsuccessful_gets = 21;
                native_calls::fail_init();
                assert_eq!(jitter.put(5160, 160, 20, &[8]), -1);
            }
            1 => {
                context.timeline.as_mut().unwrap().deadline_ns = 501_000_000;
                native_calls::fail_init();
                assert_eq!(context.get(501_000_000, 160, &mut bytes, &mut result), -1);
            }
            2 => {
                context.timeline.as_mut().unwrap().deadline_ns = 500_000_000;
                native_calls::get_response(abi::JITTER_BUFFER_INSERTION as i32, 80);
                native_calls::fail_init();
                assert_eq!(context.get(500_000_000, 160, &mut bytes, &mut result), -1);
            }
            _ => unreachable!(),
        }
        assert_eq!(jitter.observe().queued_packets, 1);
    }
}

#[test]
fn forced_native_insertion_counts_output_without_advancing_media() {
    let mut jitter = Jitter::new(config());
    jitter.put(1000, 160, 0, &[7]);
    native_calls::get_response(abi::JITTER_BUFFER_INSERTION as i32, 80);
    let decision = jitter.get(60, 160).0;
    assert_eq!(
        (decision.kind, decision.timestamp, decision.span),
        (abi::RPTADV_JITTER_INSERTION, 1000, 80)
    );
    assert_eq!(jitter.observe().insertion_span, 80);
    jitter.advance(60);
    assert_eq!(jitter.get(70, 160).1, [7]);
    jitter.advance(70);
}
