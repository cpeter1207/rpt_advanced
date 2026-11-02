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
fn copied_echo_status_exposes_the_first_cause_without_logging_untrusted_controls() {
    let mut input: PeerObservation = unsafe { std::mem::zeroed() };
    assert!(observation::describe(&input).is_none());
    input.echo_valid = 1;
    input.echo_state = 3;
    input.echo_first_cause = 5;
    input.echo_codec = 1;
    input.echo_heartbeat_due_ms = u64::MAX;
    input.echo_liveness_due_ms = u64::MAX;
    input.echo_codec_plc_samples = 640;
    input.echo_cause_detail[..8].copy_from_slice(b"TimedOut");
    input.echo_cause_detail[8] = b'\n';
    let line = observation::describe(&input).unwrap();
    for text in [
        "state=closed",
        "cause=transport-lost",
        "codec=speex",
        "codec-plc-samples=640",
        "heartbeat-in=inactive",
        "TimedOut\\n",
    ] {
        assert!(line.contains(text), "missing {text}: {line}");
    }
    assert!(
        !line.contains('\n'),
        "a copied category cannot inject a log line"
    );
}

#[test]
fn copied_observations_preserve_every_counter_without_borrowing_a_peer() {
    let cache = observation::PeerObservationCache::new();
    assert!(cache.load().is_none());
    let mut input = PeerObservation {
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
        echo_valid: 1,
        echo_state: 2,
        echo_first_cause: 5,
        echo_codec: 1,
        echo_raw_receiving: 1,
        echo_selected: 0,
        echo_transport: 1,
        echo_last_valid_age_ms: 101,
        echo_heartbeat_due_ms: 102,
        echo_liveness_due_ms: 103,
        echo_retained_age_ms: 104,
        echo_stale_packets: 105,
        echo_malformed_packets: 106,
        echo_out_of_order_packets: 107,
        echo_codec_plc_samples: 108,
        echo_fallback_gap_samples: 109,
        echo_proxy_admitted: 110,
        echo_text_admitted: 111,
        echo_text_rejected: 112,
        echo_ingress_bytes: 113,
        echo_cause_detail: [0; 32],
    };
    input.echo_cause_detail[..8].copy_from_slice(b"TimedOut");
    cache.store(input);
    let got = cache.load().unwrap();
    assert_eq!(got.echo_ingress_bytes, 113);
    assert_eq!(
        got.struct_size as usize,
        std::mem::size_of::<PeerObservation>()
    );
    assert_eq!(got.abi_version, 1);
    assert_eq!(
        [
            u64::from(got.echo_valid),
            u64::from(got.echo_state),
            u64::from(got.echo_first_cause),
            u64::from(got.echo_codec),
            u64::from(got.echo_raw_receiving),
            u64::from(got.echo_selected),
            u64::from(got.echo_transport),
            got.echo_last_valid_age_ms,
            got.echo_heartbeat_due_ms,
            got.echo_liveness_due_ms,
            got.echo_retained_age_ms,
            got.echo_stale_packets,
            got.echo_malformed_packets,
            got.echo_out_of_order_packets,
            got.echo_codec_plc_samples,
            got.echo_fallback_gap_samples,
            got.echo_proxy_admitted,
            got.echo_text_admitted,
            got.echo_text_rejected,
        ],
        [
            1, 2, 5, 1, 1, 0, 1, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112
        ]
    );
    assert_eq!(&got.echo_cause_detail[..8], b"TimedOut");
    assert!(got.echo_cause_detail[8..].iter().all(|byte| *byte == 0));
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
