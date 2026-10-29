//! Execute the private production cache without compiling the full controller
//! unit-test binary. No copied implementation or test-only public API is needed.
#[allow(warnings, missing_docs, clippy::all)]
mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}
type PeerObservation = abi::rptadv_peer_observation_v1;
#[path = "../src/link/session/observation.rs"]
mod observation;

#[test]
fn copied_observations_preserve_every_counter_without_borrowing_a_peer() {
    let cache = observation::PeerObservationCache::new();
    assert!(cache.load().is_none());
    let input = PeerObservation {
        struct_size: std::mem::size_of::<PeerObservation>() as u32,
        abi_version: abi::RPTADV_PEER_OBSERVATION_ABI_VERSION,
        jitter_queued_packets: 1,
        jitter_delay_samples: 2,
        jitter_buffered_span_samples: 3,
        jitter_duplicate_drops: 4,
        jitter_late_drops: 5,
        jitter_full_drops: 6,
        jitter_missing_samples: 7,
        jitter_insertion_samples: 8,
        jitter_skipped_samples: 9,
        jitter_limit_resets: 10,
        jitter_overload_drops: 11,
        jitter_overload_span_samples: 12,
        ingress_overflow_drops: 13,
        ingress_queued_packets: 14,
        ingress_capacity_packets: 15,
        egress_datagrams: 16,
        egress_would_block: 17,
        egress_reliable_retries: 18,
        egress_reliable_pending: 19,
        egress_reply_pending: 20,
    };
    cache.store(input);
    let got = cache.load().unwrap();
    assert_eq!(
        got.struct_size as usize,
        std::mem::size_of::<PeerObservation>()
    );
    assert_eq!(got.abi_version, 1);
    assert_eq!(
        [
            u64::from(got.jitter_queued_packets),
            u64::from(got.jitter_delay_samples),
            got.jitter_buffered_span_samples,
            got.jitter_duplicate_drops,
            got.jitter_late_drops,
            got.jitter_full_drops,
            got.jitter_missing_samples,
            got.jitter_insertion_samples,
            got.jitter_skipped_samples,
            got.jitter_limit_resets,
            got.jitter_overload_drops,
            got.jitter_overload_span_samples,
            got.ingress_overflow_drops,
            u64::from(got.ingress_queued_packets),
            u64::from(got.ingress_capacity_packets),
            got.egress_datagrams,
            got.egress_would_block,
            got.egress_reliable_retries,
            u64::from(got.egress_reliable_pending),
            u64::from(got.egress_reply_pending),
        ],
        [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20
        ]
    );
}
