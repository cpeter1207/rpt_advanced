//! Copied metadata only. Counts never grant admission or prove remote receipt.

/// Actual source-clock packet-jitter counters, acquired on the serial owner.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct JitterObservation {
    /// Packets retained for playout.
    pub queued_packets: u32,
    /// Current adaptive delay in 8 kHz samples.
    pub delay_samples: u32,
    /// Retained packet duration in source samples.
    pub buffered_samples: u64,
    /// Duplicate packet rejections.
    pub duplicate_drops: u64,
    /// Packets too late for playout.
    pub late_drops: u64,
    /// Packets rejected at the queue bound.
    pub full_drops: u64,
    /// Released missing source samples, not wire-loss proof.
    pub missing_samples: u64,
    /// Inserted delay in source samples.
    pub insertion_samples: u64,
    /// Source samples skipped during delay reduction.
    pub skipped_samples: u64,
    /// Adaptive resets at the delay limit.
    pub limit_resets: u64,
    /// Packets shed during overload recovery.
    pub overload_drops: u64,
    /// Source samples shed during overload recovery.
    pub overload_samples: u64,
}

/// Account-wide standard Proxy TCP output; fields are individually atomic.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct ProxyObservation {
    /// Records admitted to the queue, not sent packets.
    pub admitted: u64,
    /// Complete framed writes accepted by TCP, not remote receipt.
    pub sent: u64,
    /// Expired records discarded before any byte was submitted.
    pub expired: u64,
    /// Distinct socket-not-ready episodes.
    pub stalls: u64,
    /// Cumulative stall milliseconds, including a currently active stall.
    pub stall_ms: u64,
    /// Retained payload bytes, including partially written records.
    pub queued_bytes: u64,
    /// Current head record age; zero when no head has been selected.
    pub head_age_ms: u64,
}

/// Account control snapshot. Unknown ages/deadlines use u64::MAX, never zero.
/// Query off provider/audio executors; no QSO is required.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct AccountObservation {
    /// Complete writable record size.
    pub struct_size: u32,
    /// Exact ABI one.
    pub abi_version: u32,
    /// Prepared=0, registering=1, online=2, retrying=3, stopping=4,
    /// stopped=5, disabled=6, failed=7. Online does not prove inbound reachability.
    pub state: u32,
    /// Selected direct UDP=0 or standard TCP Proxy=1.
    pub transport: u32,
    /// Complete real account station ID, zero when disabled/unconfigured.
    pub node_number: u32,
    /// Nonzero only while the committed directory remains fresh.
    pub fresh: u32,
    /// A successful directory login has not been logged out.
    pub registered: u32,
    /// Nonzero when the account-owner counters below were acquired successfully.
    pub available: u32,
    /// Exact account generation, including failed activation attempts.
    pub generation: u64,
    /// First terminal category: none=0, I/O=1, Proxy=2, owner=3.
    pub first_cause: u64,
    /// NUL-padded ASCII category detail, at most31 bytes; never an OS message,
    /// credential, endpoint payload or human text. Empty before terminal failure.
    pub cause_detail: [u8; 32],
    /// Failed matching directory transactions.
    pub directory_failures: u64,
    /// Started directory operations, including finite logout.
    pub directory_operations: u64,
    /// Age of the last successful directory login.
    pub registration_age_ms: u64,
    /// Age of the last complete directory publication.
    pub snapshot_age_ms: u64,
    /// Remaining directory retry delay, or inactive.
    pub retry_in_ms: u64,
    /// Admitted QSOs, not unverified incoming traffic.
    pub admitted: u64,
    /// Retained verified incoming candidates awaiting admission.
    pub pending: u64,
    /// Real selected station ID; zero when no station is selected.
    pub selected_node: u64,
    /// Exact selected QSO generation; zero when none is selected.
    pub selected_generation: u64,
    /// Nonzero while the selected talker has raw receive activity.
    pub receiving: u64,
    /// Valid competing talker packets suppressed by policy.
    pub suppressed_packets: u64,
    /// First-talker control-credit failures, not network loss.
    pub control_overflows: u64,
    /// Packets routed into an exact connection mailbox.
    pub routed_packets: u64,
    /// Unroutable or mailbox-rejected packets.
    pub dropped_packets: u64,
    /// Wire-parser failures, a subset of dropped packets.
    pub malformed_packets: u64,
    /// Packets shed by a full peer mailbox, never a hangup.
    pub ingress_full_packets: u64,
    /// Selected Proxy output; all zero for direct mode.
    pub proxy: ProxyObservation,
    /// Complete NUL-padded configured station callsign, at most15 ASCII bytes.
    /// Available without a connected QSO; empty for an unconfigured account.
    pub callsign: [u8; 16],
}

/// Sole-serial-owner peer snapshot; never query from control/audio directly.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct PeerObservation {
    /// Complete writable record size.
    pub struct_size: u32,
    /// Exact ABI one.
    pub abi_version: u32,
    /// Connecting=0, connected=1, closing=2, closed=3.
    pub state: u32,
    /// None=0, local request=1, remote BYE=2, connect expired=3,
    /// liveness expired=4, transport lost=5, owner failed=6.
    pub first_cause: u32,
    /// NUL-padded content-free first transport/media error category, at most31
    /// ASCII bytes. It supplements, never replaces, the first QSO cause.
    pub cause_detail: [u8; 32],
    /// GSM-FR=0, narrowband Speex=1; negotiated outbound codec.
    pub codec: u32,
    /// Session-bound packet activity, not selected RF activity.
    pub raw_receiving: u32,
    /// Selected/draining RF activity for this exact connection.
    pub selected: u32,
    /// Direct UDP=0 or selected standard TCP Proxy=1.
    pub transport: u32,
    /// Time since the last valid session-bound voice/control packet.
    pub last_valid_age_ms: u64,
    /// Remaining sent-heartbeat deadline, or u64::MAX when inactive.
    pub heartbeat_due_ms: u64,
    /// Remaining setup/liveness deadline, or u64::MAX when inactive.
    pub liveness_due_ms: u64,
    /// Retained original datagram age, zero when none is retained.
    pub retained_age_ms: u64,
    /// Retired generation packets ignored before parsing.
    pub stale_packets: u64,
    /// Invalid wire or media framing, not terminal errors.
    pub malformed_packets: u64,
    /// Lower RTP sequences within a selected burst, excluding equal duplicates.
    pub out_of_order_packets: u64,
    /// Actual jitter counters; every sample count uses 8 kHz.
    pub jitter: JitterObservation,
    /// Native codec PLC samples accepted by the product sink.
    pub codec_plc_samples: u64,
    /// Ordered unresolved samples accepted for shared-ring fallback PLC.
    pub fallback_gap_samples: u64,
    /// Direct UDP datagram sends accepted by the socket.
    pub direct_datagrams: u64,
    /// Send attempts blocked by socket readiness or Proxy queue credit.
    pub blocked_sends: u64,
    /// Records admitted to the Proxy writer, not actual TCP sends.
    pub proxy_admitted: u64,
    /// Human-text messages admitted by the selected transport.
    pub text_admitted: u64,
    /// Human-text messages rejected before transport admission.
    pub text_rejected: u64,
}
