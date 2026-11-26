//! Verify connection ownership and renewal of a streamed temporary tuning session.
#[test]
fn tuning_renews_and_releases_the_original_connection_owned_lease() {
    assert!(
        std::process::Command::new("python3")
            .args([
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tuning.py"),
                env!("CARGO_BIN_EXE_rpt-advanced-cli")
            ])
            .status()
            .expect("start tuning lease fixture")
            .success()
    );
}
