use super::*;

unsafe extern "C" fn failed_create(
    _: *const ffi::rpcr2_config,
    _: *mut *mut ffi::rpcr2_ring,
) -> i32 {
    -1
}
unsafe extern "C" fn empty_create(
    _: *const ffi::rpcr2_config,
    _: *mut *mut ffi::rpcr2_ring,
) -> i32 {
    0
}
unsafe extern "C" fn failed_push(
    _: *mut ffi::rpcr2_ring,
    _: *const f32,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_render(
    _: *mut ffi::rpcr2_ring,
    _: *mut f32,
    _: u64,
    _: u64,
    _: u64,
    _: *mut u64,
) -> i32 {
    -1
}
unsafe extern "C" fn failed_observe(
    _: *const ffi::rpcr2_ring,
    _: *mut ffi::rpcr2_observation,
) -> i32 {
    -1
}

#[test]
fn malformed_ring_tables_and_failed_operations_never_publish_or_retain_invalid_audio() {
    // SAFETY: real immutable released descriptor; modified copies are retained for process lifetime.
    unsafe {
        assert!(InboundRing::from_descriptor(8000, ptr::null()).is_err());
        for rate in [0, 48001] {
            assert!(InboundRing::open(rate).is_err());
        }
        let original = *ffi::rpcr2_descriptor();
        for case in 0..12 {
            let mut api = original;
            match case {
                0 => api.struct_size = 8,
                1 => api.abi_version = 99,
                2 => api.capability_name = ptr::null(),
                3 => api.capability_name = c"other".as_ptr(),
                4 => api.ring_create = None,
                5 => api.ring_destroy = None,
                6 => api.ring_producer_push = None,
                7 => api.ring_consumer_render = None,
                8 => api.ring_observe = None,
                9 => api.ring_create = Some(failed_create),
                10 => api.ring_create = Some(empty_create),
                _ => api.ring_consumer_reset = None,
            }
            assert!(InboundRing::from_descriptor(8000, Box::leak(Box::new(api))).is_err());
        }
        let mut api = original;
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        let (mut producer, mut consumer) =
            InboundRing::from_descriptor(8000, Box::leak(Box::new(api))).unwrap();
        assert!(producer.write(&[0.5]).is_err());
        let mut samples = [0.5; 8];
        assert!(consumer.render(&mut samples).is_err());
        assert_eq!(samples, [0.0; 8]);
        assert!(consumer.observe().is_err());
        assert_eq!(PeerInput::available(&consumer), 0);
        assert!(!PeerInput::render(&mut consumer, &mut samples));
        assert_eq!(PeerInput::source_rate(&consumer), 8000);
        assert!(std::ptr::eq(
            PeerInput::signals(&consumer),
            producer.signals()
        ));
        assert!(producer.observer().observe().is_err());
        let (mut producer, consumer) = InboundRing::open(8000).unwrap();
        assert_eq!(producer.write(&[]), Ok(0));
        assert_eq!(PeerInput::available(&consumer), 0);
    }
}

pub(crate) fn failed_endpoints() -> (InboundProducer, InboundConsumer) {
    // SAFETY: retain a copied released descriptor and its real create/destroy pair.
    unsafe {
        let mut api = *ffi::rpcr2_descriptor();
        api.ring_producer_push = Some(failed_push);
        api.ring_consumer_render = Some(failed_render);
        api.ring_observe = Some(failed_observe);
        InboundRing::from_descriptor(48000, Box::leak(Box::new(api))).unwrap()
    }
}

#[test]
fn released_ring_endpoints_publish_render_and_report_their_generation() {
    let (mut producer, mut consumer) = InboundRing::open(8000).unwrap();
    let observer = producer.observer();
    let same = producer.observer();
    let (other, _) = InboundRing::open(8000).unwrap();
    assert!(observer.same_generation(&same));
    assert!(!observer.same_generation(&other.observer()));
    assert_eq!(observer.rate(), 8000);
    assert_eq!(producer.rate(), 8000);
    assert!(std::ptr::eq(observer.signals(), producer.signals()));

    assert_eq!(producer.write(&[0.25; 160]), Ok(160));
    let mut output = [0.0; 960];
    assert!(consumer.render(&mut output).is_ok());
}
