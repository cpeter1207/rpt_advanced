//! Serial-owner publication and control-side atomic metadata copies.
use super::PeerObservation;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(super) struct PeerObservationCache {
    valid: AtomicBool,
    fields: [AtomicU64; 20],
}
impl PeerObservationCache {
    pub(super) fn new() -> Self {
        Self {
            valid: AtomicBool::new(false),
            fields: std::array::from_fn(|_| AtomicU64::new(0)),
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
        ];
        for (slot, value) in self.fields.iter().zip(fields) {
            slot.store(value, Ordering::Relaxed);
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
        })
    }
}
