use super::DuplexPolicy;

#[test]
fn duplex_applies_half_duplex_override_and_exact_hang_boundary() {
    let mut policy = DuplexPolicy::default();
    assert!(!policy.update(true, false, false, 0, 100));
    assert!(policy.update(true, true, false, 1, 100));
    assert!(policy.update(true, false, false, 100, 1));
    assert!(!policy.update(true, false, false, 101, 1));
    assert!(policy.update(false, false, true, 102, 50));
    assert!(!policy.update(false, true, true, 103, 50));
}
