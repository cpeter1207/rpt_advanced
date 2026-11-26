//! Exercise the operator's installed entry point against a mutually authenticated listener.
use std::process::Command;

#[test]
fn discovers_status_over_authenticated_hal() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/discovery.py");
    let status = Command::new("python3")
        .args([script, env!("CARGO_BIN_EXE_rpt-advanced-cli")])
        .status()
        .expect("start loopback discovery fixture");
    assert!(status.success(), "operator discovery contract failed");
}
