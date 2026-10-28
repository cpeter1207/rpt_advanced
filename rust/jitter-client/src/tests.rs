use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn protocol_payload_budget_is_preserved_and_invalid_bounds_fail_before_creation() {
    let descriptor = descriptor();
    for maximum in [0, 65_508, usize::MAX] {
        assert!(matches!(
            unsafe { Jitter::new_at_rate(ptr::from_ref(&descriptor).cast(), 8000, 160, maximum) },
            Err(Error::InvalidDescriptor)
        ));
    }
    let jitter =
        unsafe { Jitter::new_at_rate(ptr::from_ref(&descriptor).cast(), 8000, 160, 252) }.unwrap();
    assert_eq!(
        unsafe { &*jitter.context.as_ptr().cast::<State>() }
            .config
            .maximum_payload_bytes,
        252
    );
}

#[test]
fn invalid_source_clock_and_loss_units_fail_before_context_creation() {
    let descriptor = descriptor();
    for (rate, loss_unit) in [(44100, 160), (8000, 0), (8000, 4001)] {
        assert!(matches!(
            unsafe {
                Jitter::new_at_rate(
                    ptr::from_ref(&descriptor).cast(),
                    rate,
                    loss_unit,
                    MAX_PAYLOAD,
                )
            },
            Err(Error::InvalidDescriptor)
        ));
    }
}

struct State {
    config: Config,
    destroyed: Arc<AtomicUsize>,
    puts: usize,
    admission: i32,
    kind: u32,
    decision: Option<RawDecision>,
}

unsafe extern "C" fn create(config: *const Config, context: *mut *mut c_void) -> i32 {
    let config = unsafe { &*config };
    unsafe {
        *context = Box::into_raw(Box::new(State {
            config: *config,
            destroyed: Arc::new(AtomicUsize::new(0)),
            puts: 0,
            admission: 0,
            kind: 0,
            decision: None,
        }))
        .cast()
    };

    0
}
unsafe extern "C" fn destroy(context: *mut c_void) {
    let state = unsafe { Box::from_raw(context.cast::<State>()) };
    state.destroyed.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn reset(_: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn put(context: *mut c_void, packet: *const Packet) -> i32 {
    let packet = unsafe { &*packet };
    assert_eq!(
        (
            packet.timestamp,
            packet.span,
            packet.arrival_ns,
            packet.length
        ),
        (1000, 160, 20_000_000, 2)
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(packet.payload, packet.length) },
        [1, 2]
    );
    unsafe { (*context.cast::<State>()).puts += 1 };
    unsafe { (*context.cast::<State>()).admission }
}
unsafe extern "C" fn get(
    context: *mut c_void,
    now: u64,
    desired: u32,
    payload: *mut u8,
    capacity: usize,
    result: *mut RawDecision,
) -> i32 {
    assert_eq!((now, desired, capacity), (80_000_000, 160, MAX_PAYLOAD));
    unsafe {
        *payload = 9;
        let state = &mut *context.cast::<State>();
        let kind = state.kind;
        *result = state.decision.take().unwrap_or(RawDecision {
            struct_size: size_of::<RawDecision>() as u32,
            kind,
            timestamp: 1000,
            span: 160,
            start_offset: -40,
            length: usize::from(kind == 0),
            deadline_ns: 80_000_000,
        });
    }
    0
}
unsafe extern "C" fn advance(_: *mut c_void, now: u64, tail: u32) -> i32 {
    assert_eq!((now, tail), (80_000_000, 0));
    0
}
unsafe extern "C" fn observe(_: *const c_void, result: *mut Observation) -> i32 {
    unsafe { (*result).delay = 480 };
    0
}

fn descriptor() -> Descriptor {
    Descriptor {
        struct_size: size_of::<Descriptor>() as u32,
        abi_version: 1,
        capability: CAPABILITY,
        create: Some(create),
        destroy: Some(destroy),
        reset: Some(reset),
        put: Some(put),
        get: Some(get),
        advance: Some(advance),
        observe: Some(observe),
    }
}

#[test]
fn composed_context_uses_approved_bounds_and_destroys_once() {
    let descriptor = descriptor();
    let mut jitter = unsafe { Jitter::new(ptr::from_ref(&descriptor).cast()) }.unwrap();
    let config = unsafe { &*jitter.context.as_ptr().cast::<State>() }.config;
    assert_eq!(
        (
            config.clock_rate_hz,
            config.minimum_delay,
            config.initial_delay,
            config.maximum_delay,
            config.adjustment_step,
            config.concealment_span,
            config.maximum_packets,
            config.maximum_payload_bytes
        ),
        (8000, 1280, 1440, 4000, 80, 80, 64, MAX_PAYLOAD as u32)
    );
    let destroyed = unsafe { &*jitter.context.as_ptr().cast::<State>() }
        .destroyed
        .clone();
    assert_eq!(
        jitter.put(1000, 160, 20_000_000, &[1, 2]),
        Ok(Admission::Accepted)
    );
    assert_eq!(unsafe { &*jitter.context.as_ptr().cast::<State>() }.puts, 1);
    let mut output = [0; MAX_PAYLOAD];
    let result = jitter.get(80_000_000, 160, &mut output).unwrap();
    assert_eq!(
        (
            result.kind,
            result.timestamp,
            result.span,
            result.start_offset,
            result.length,
            result.deadline_ns,
            output[0]
        ),
        (Kind::Packet, 1000, 160, -40, 1, 80_000_000, 9)
    );
    jitter.advance(80_000_000, 0).unwrap();
    assert_eq!(jitter.observe().unwrap().delay, 480);
    jitter.reset().unwrap();
    drop(jitter);
    assert_eq!(destroyed.load(Ordering::SeqCst), 1);
}

#[test]
fn incompatible_or_incomplete_tables_fail_before_creating_context() {
    assert!(matches!(
        unsafe { Jitter::new(ptr::null()) },
        Err(Error::InvalidDescriptor)
    ));
    let prefix = [8u32, 1];
    assert!(matches!(
        unsafe { Jitter::new(prefix.as_ptr().cast()) },
        Err(Error::InvalidDescriptor)
    ));
    let mutations: &[fn(&mut Descriptor)] = &[
        |d| d.abi_version = 2,
        |d| d.capability[0] = b'x',
        |d| d.create = None,
        |d| d.destroy = None,
        |d| d.reset = None,
        |d| d.put = None,
        |d| d.get = None,
        |d| d.advance = None,
        |d| d.observe = None,
    ];
    for mutate in mutations {
        let mut api = descriptor();
        mutate(&mut api);
        assert!(matches!(
            unsafe { Jitter::new(ptr::from_ref(&api).cast()) },
            Err(Error::InvalidDescriptor)
        ));
    }
}

#[test]
fn layouts_match_the_neutral_header_on_supported_64_bit_targets() {
    assert_eq!(
        (
            size_of::<Config>(),
            size_of::<Packet>(),
            size_of::<RawDecision>(),
            size_of::<Observation>(),
            size_of::<Descriptor>()
        ),
        (40, 48, 40, 96, 80)
    );
    assert_eq!(std::mem::offset_of!(Packet, arrival_ns), 24);
    assert_eq!(std::mem::offset_of!(RawDecision, deadline_ns), 32);
    assert_eq!(std::mem::offset_of!(Descriptor, create), 24);
    assert_eq!(std::mem::offset_of!(Observation, overload_drops), 80);
}

#[test]
fn admission_and_decision_variants_are_translated_without_codec_work() {
    let api = descriptor();
    let mut jitter = unsafe { Jitter::new(ptr::from_ref(&api).cast()) }.unwrap();
    for (value, expected) in [
        (1, Ok(Admission::Duplicate)),
        (2, Ok(Admission::Late)),
        (3, Ok(Admission::Full)),
        (-1, Err(Error::OperationFailed)),
        (4, Err(Error::OperationFailed)),
    ] {
        unsafe { (*jitter.context.as_ptr().cast::<State>()).admission = value };
        assert_eq!(jitter.put(1000, 160, 20_000_000, &[1, 2]), expected);
    }
    let mut output = [0; MAX_PAYLOAD];
    for (value, expected) in [(1, Kind::Missing), (2, Kind::Insertion), (3, Kind::NotDue)] {
        unsafe { (*jitter.context.as_ptr().cast::<State>()).kind = value };
        assert_eq!(
            jitter.get(80_000_000, 160, &mut output).unwrap().kind,
            expected
        );
    }
    unsafe { (*jitter.context.as_ptr().cast::<State>()).kind = 4 };
    assert!(matches!(
        jitter.get(80_000_000, 160, &mut output),
        Err(Error::OperationFailed)
    ));
}

#[test]
fn unsuccessful_or_null_creation_is_not_an_owned_context() {
    unsafe extern "C" fn failed(_: *const Config, _: *mut *mut c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn null(_: *const Config, _: *mut *mut c_void) -> i32 {
        0
    }
    for callback in [failed as unsafe extern "C" fn(_, _) -> _, null] {
        let mut api = descriptor();
        api.create = Some(callback);
        assert!(matches!(
            unsafe { Jitter::new(ptr::from_ref(&api).cast()) },
            Err(Error::OperationFailed)
        ));
    }
}

#[test]
fn malformed_decisions_are_rejected_before_exposing_payload_or_timing() {
    let api = descriptor();
    let mut jitter = unsafe { Jitter::new(ptr::from_ref(&api).cast()) }.unwrap();
    let corrupt: &[fn(&mut RawDecision)] = &[
        |decision| decision.struct_size -= 1,
        |decision| decision.length = MAX_PAYLOAD + 1,
        |decision| decision.kind = 1,
        |decision| decision.span = 0,
        |decision| decision.start_offset = -160,
        |decision| decision.start_offset = -161,
    ];
    for corrupt in corrupt {
        let mut decision = RawDecision {
            struct_size: size_of::<RawDecision>() as u32,
            kind: 0,
            span: 160,
            length: 1,
            ..RawDecision::default()
        };
        corrupt(&mut decision);
        unsafe { (*jitter.context.as_ptr().cast::<State>()).decision = Some(decision) };
        assert!(matches!(
            jitter.get(80_000_000, 160, &mut [0; MAX_PAYLOAD]),
            Err(Error::OperationFailed)
        ));
    }
}

#[test]
fn failed_operations_and_truncated_observations_do_not_look_successful() {
    unsafe extern "C" fn failed(_: *mut c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn truncated(_: *const c_void, output: *mut Observation) -> i32 {
        unsafe { (*output).struct_size -= 1 };
        0
    }
    let mut api = descriptor();
    api.reset = Some(failed);
    api.observe = Some(truncated);
    let mut jitter = unsafe { Jitter::new(ptr::from_ref(&api).cast()) }.unwrap();
    assert_eq!(jitter.reset(), Err(Error::OperationFailed));
    assert!(matches!(jitter.observe(), Err(Error::OperationFailed)));
}
