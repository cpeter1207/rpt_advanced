//! Shared control-side presentation; never borrow protocol owners or payloads.
use super::abi;

/// Format a copied account snapshot, including when it has no connected peers.
/// Foreign category/callsign bytes are escaped, never interpreted as log lines.
pub fn describe_account(local: &str, account: &abi::AccountObservation) -> String {
    let age = |value| {
        if value == u64::MAX {
            "unknown".into()
        } else {
            format!("{value}ms")
        }
    };
    let label = |items: &[&str], value: u32| {
        items
            .get(value as usize)
            .copied()
            .unwrap_or("unknown")
            .to_owned()
    };
    let text = |bytes: &[u8]| {
        String::from_utf8_lossy(&bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())])
            .into_owned()
    };
    format!(
        "echo-account local={local} callsign={:?} node={} generation={} state={} transport={} available={} fresh={} registered={} cause={} detail={:?} directory-failures={} directory-operations={} registration-age={} snapshot-age={} retry-in={} admitted={} pending={} selected-node={} selected-generation={} receiving={} suppressed={} control-overflows={} routed={} dropped={} malformed={} ingress-full={} proxy-admitted={} proxy-sent={} proxy-expired={} proxy-stalls={} proxy-stall-ms={} proxy-queued-bytes={} proxy-head-age={}ms",
        text(&account.callsign),
        account.node_number,
        account.generation,
        label(
            &[
                "prepared",
                "registering",
                "online",
                "retrying",
                "stopping",
                "stopped",
                "disabled",
                "failed"
            ],
            account.state
        ),
        label(&["direct", "proxy"], account.transport),
        account.available,
        account.fresh,
        account.registered,
        account.first_cause,
        text(&account.cause_detail),
        account.directory_failures,
        account.directory_operations,
        age(account.registration_age_ms),
        age(account.snapshot_age_ms),
        age(account.retry_in_ms),
        account.admitted,
        account.pending,
        account.selected_node,
        account.selected_generation,
        account.receiving,
        account.suppressed_packets,
        account.control_overflows,
        account.routed_packets,
        account.dropped_packets,
        account.malformed_packets,
        account.ingress_full_packets,
        account.proxy.admitted,
        account.proxy.sent,
        account.proxy.expired,
        account.proxy.stalls,
        account.proxy.stall_ms,
        account.proxy.queued_bytes,
        account.proxy.head_age_ms,
    )
}
