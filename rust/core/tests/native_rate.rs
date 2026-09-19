use rpt_advanced_core::NATIVE_SAMPLE_RATE_HZ;

#[test]
fn native_rate_is_fixed() {
    assert_eq!(NATIVE_SAMPLE_RATE_HZ, 48_000);
}
