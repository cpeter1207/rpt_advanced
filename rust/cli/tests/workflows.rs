//! Run the installed client through actual mTLS requests with stable validator and receipt checks.
#[test]
fn writes_resources_and_waits_for_the_same_operation() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/workflows.py");
    assert!(
        std::process::Command::new("python3")
            .args([script, env!("CARGO_BIN_EXE_rpt-advanced-cli")])
            .status()
            .expect("start operator workflow fixture")
            .success(),
        "resource and operation workflows failed"
    );
}
