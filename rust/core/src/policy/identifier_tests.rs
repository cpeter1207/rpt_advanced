use super::{IdentifierPolicy, IdentifierRule};

#[test]
fn identifier_selects_by_priority_without_consuming_equal_priority_siblings() {
    let rules = [
        IdentifierRule::new(100, 1, false, false),
        IdentifierRule::new(100, 3, false, true),
        IdentifierRule::new(100, 3, false, true),
        IdentifierRule::new(100, 2, false, true),
    ];
    let mut policy = IdentifierPolicy::new(&rules);
    assert_eq!(policy.select(&rules, 100, false, true), Some(1));
    policy.complete(&rules, rules.len(), 100);
    assert_eq!(policy.select(&rules, 100, false, true), Some(1));
    policy.complete(&rules, 1, 100);
    assert_eq!(policy.select(&rules, 100, false, true), Some(2));
}

#[test]
fn first_key_ids_are_nonperiodic_and_are_suppressed_by_half_duplex_reception() {
    let rules = [IdentifierRule::new(100, 10, true, true)];
    let mut policy = IdentifierPolicy::new(&rules);
    policy.first_key(&rules, 99);
    assert_eq!(policy.select(&rules, 1_000, false, true), None);
    policy.first_key(&rules, 100);
    assert_eq!(policy.select(&rules, 1_000, true, false), None);
    assert_eq!(policy.select(&rules, 1_000, false, false), Some(0));
    policy.complete(&rules, 0, 1_001);
    assert_eq!(policy.select(&rules, 10_000, false, true), None);
}
