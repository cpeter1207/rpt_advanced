//! Serial-owner publication and control-side atomic metadata copies.
use super::PeerObservation;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Control/log presentation of copied Echo metadata; no live provider borrow.
pub(crate) fn describe(peer: &PeerObservation) -> Option<String> {
    if peer.echo_valid != 1 {
        return None;
    }
    let label = |labels: &[&str], value: u32| {
        labels
            .get(value as usize)
            .copied()
            .unwrap_or("unknown")
            .to_owned()
    };
    let age = |value: u64| {
        if value == u64::MAX {
            "inactive".to_owned()
        } else {
            format!("{value}ms")
        }
    };
    let end = peer
        .echo_cause_detail
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(32);
    // Debug formatting escapes control characters even if a trusted native
    // provider returns a malformed category; it can never inject a log line.
    let detail = String::from_utf8_lossy(&peer.echo_cause_detail[..end]);
    Some(format!(
        "echo state={} cause={} detail={detail:?} codec={} transport={} raw-receiving={} selected={} last-valid-age={}ms heartbeat-in={} liveness-in={} retained-age={}ms stale={} malformed={} reordered={} codec-plc-samples={} ring-gap-admitted={} proxy-admitted={} text-admitted={} text-rejected={} ingress-retained-bytes={}",
        label(
            &["connecting", "connected", "closing", "closed"],
            peer.echo_state
        ),
        label(
            &[
                "none",
                "local-request",
                "remote-bye",
                "connect-expired",
                "liveness-expired",
                "transport-lost",
                "owner-failed"
            ],
            peer.echo_first_cause
        ),
        label(&["gsm", "speex"], peer.echo_codec),
        label(&["direct", "proxy"], peer.echo_transport),
        peer.echo_raw_receiving,
        peer.echo_selected,
        peer.echo_last_valid_age_ms,
        age(peer.echo_heartbeat_due_ms),
        age(peer.echo_liveness_due_ms),
        peer.echo_retained_age_ms,
        peer.echo_stale_packets,
        peer.echo_malformed_packets,
        peer.echo_out_of_order_packets,
        peer.echo_codec_plc_samples,
        peer.echo_fallback_gap_samples,
        peer.echo_proxy_admitted,
        peer.echo_text_admitted,
        peer.echo_text_rejected,
        peer.echo_ingress_bytes,
    ))
}

pub(super) struct PeerObservationCache {
    valid: AtomicBool,
    fields: [AtomicU64; 40],
    detail: [AtomicU64; 4],
}
impl PeerObservationCache {
    pub(super) fn new() -> Self {
        Self {
            valid: AtomicBool::new(false),
            fields: std::array::from_fn(|_| AtomicU64::new(0)),
            detail: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
    pub(super) fn store(&self, peer: PeerObservation) {
        let fields = [
            peer.jitter_queued_packets as u64,
            peer.jitter_delay_samples as u64,
            peer.jitter_buffered_span_samples,
            peer.jitter_duplicate_drops,
            peer.jitter_late_drops,
            peer.jitter_full_drops,
            peer.jitter_missing_samples,
            peer.jitter_insertion_samples,
            peer.jitter_skipped_samples,
            peer.jitter_limit_resets,
            peer.jitter_overload_drops,
            peer.jitter_overload_span_samples,
            peer.ingress_overflow_drops,
            peer.ingress_queued_packets as u64,
            peer.ingress_capacity_packets as u64,
            peer.egress_datagrams,
            peer.egress_would_block,
            peer.egress_reliable_retries,
            peer.egress_reliable_pending as u64,
            peer.egress_reply_pending as u64,
            peer.echo_valid as u64,
            peer.echo_state as u64,
            peer.echo_first_cause as u64,
            peer.echo_codec as u64,
            peer.echo_raw_receiving as u64,
            peer.echo_selected as u64,
            peer.echo_transport as u64,
            peer.echo_last_valid_age_ms,
            peer.echo_heartbeat_due_ms,
            peer.echo_liveness_due_ms,
            peer.echo_retained_age_ms,
            peer.echo_stale_packets,
            peer.echo_malformed_packets,
            peer.echo_out_of_order_packets,
            peer.echo_codec_plc_samples,
            peer.echo_fallback_gap_samples,
            peer.echo_proxy_admitted,
            peer.echo_text_admitted,
            peer.echo_text_rejected,
            peer.echo_ingress_bytes,
        ];
        for (slot, value) in self.fields.iter().zip(fields) {
            slot.store(value, Ordering::Relaxed);
        }
        for (slot, bytes) in self
            .detail
            .iter()
            .zip(peer.echo_cause_detail.chunks_exact(8))
        {
            slot.store(
                u64::from_ne_bytes(bytes.try_into().expect("eight-byte category chunk")),
                Ordering::Relaxed,
            );
        }
        self.valid.store(true, Ordering::Release);
    }
    pub(super) fn load(&self) -> Option<PeerObservation> {
        if !self.valid.load(Ordering::Acquire) {
            return None;
        }
        let values = self
            .fields
            .each_ref()
            .map(|field| field.load(Ordering::Relaxed));
        let mut detail = [0; 32];
        for (bytes, slot) in detail.chunks_exact_mut(8).zip(&self.detail) {
            bytes.copy_from_slice(&slot.load(Ordering::Relaxed).to_ne_bytes());
        }
        Some(PeerObservation {
            struct_size: std::mem::size_of::<PeerObservation>() as u32,
            abi_version: crate::abi::RPTADV_PEER_OBSERVATION_ABI_VERSION,
            jitter_queued_packets: values[0] as u32,
            jitter_delay_samples: values[1] as u32,
            jitter_buffered_span_samples: values[2],
            jitter_duplicate_drops: values[3],
            jitter_late_drops: values[4],
            jitter_full_drops: values[5],
            jitter_missing_samples: values[6],
            jitter_insertion_samples: values[7],
            jitter_skipped_samples: values[8],
            jitter_limit_resets: values[9],
            jitter_overload_drops: values[10],
            jitter_overload_span_samples: values[11],
            ingress_overflow_drops: values[12],
            ingress_queued_packets: values[13] as u32,
            ingress_capacity_packets: values[14] as u32,
            egress_datagrams: values[15],
            egress_would_block: values[16],
            egress_reliable_retries: values[17],
            egress_reliable_pending: values[18] as u32,
            egress_reply_pending: values[19] as u32,
            echo_valid: values[20] as u32,
            echo_state: values[21] as u32,
            echo_first_cause: values[22] as u32,
            echo_codec: values[23] as u32,
            echo_raw_receiving: values[24] as u32,
            echo_selected: values[25] as u32,
            echo_transport: values[26] as u32,
            echo_last_valid_age_ms: values[27],
            echo_heartbeat_due_ms: values[28],
            echo_liveness_due_ms: values[29],
            echo_retained_age_ms: values[30],
            echo_stale_packets: values[31],
            echo_malformed_packets: values[32],
            echo_out_of_order_packets: values[33],
            echo_codec_plc_samples: values[34],
            echo_fallback_gap_samples: values[35],
            echo_proxy_admitted: values[36],
            echo_text_admitted: values[37],
            echo_text_rejected: values[38],
            echo_ingress_bytes: values[39],
            echo_cause_detail: detail,
        })
    }
}
