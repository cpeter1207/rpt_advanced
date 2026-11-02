//! Metadata presentation is shared by both non-audio host adapters.
use rptadv_peer_support::echolink::{abi, host};

#[test]
fn account_status_without_peers_preserves_identity_failure_and_proxy_provenance() {
    let mut record = abi::AccountObservation {
        state: 3,
        transport: 1,
        node_number: 1234567,
        generation: 9,
        directory_failures: 4,
        retry_in_ms: 30000,
        snapshot_age_ms: u64::MAX,
        ..Default::default()
    };
    record.callsign[..8].copy_from_slice(b"KG0BP-R\n");
    record.proxy.admitted = 12;
    record.proxy.sent = 10;
    record.proxy.queued_bytes = 84;
    let text = host::describe_account("524950", &record);
    for expected in [
        "local=524950",
        "callsign=\"KG0BP-R\\n\"",
        "node=1234567",
        "state=retrying",
        "generation=9",
        "directory-failures=4",
        "retry-in=30000ms",
        "snapshot-age=unknown",
        "proxy-admitted=12",
        "proxy-sent=10",
        "proxy-queued-bytes=84",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(
        !text.contains('\n'),
        "foreign bytes cannot inject log lines"
    );
}
