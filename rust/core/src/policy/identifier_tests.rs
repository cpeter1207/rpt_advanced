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
    policy.first_key(&rules, 99, 99);
    assert_eq!(policy.select(&rules, 1_000, false, true), None);
    policy.first_key(&rules, 100, 100);
    assert_eq!(policy.select(&rules, 1_000, true, false), None);
    assert_eq!(policy.select(&rules, 1_000, false, false), Some(0));
    policy.complete(&rules, 0, 1_001);
    assert_eq!(policy.select(&rules, 10_000, false, true), None);
}

#[test]
fn first_key_ids_rotate_one_per_qualifying_key_without_a_second_pending_id() {
    let rules = [
        IdentifierRule::new(100, 1, true, false),
        IdentifierRule::new(10_000, 1, false, false),
        IdentifierRule::new(200, 1, true, false),
        IdentifierRule::new(100, 8, true, false),
    ];
    let mut policy = IdentifierPolicy::new(&rules);
    let mut now = 1000;
    for (idle_ms, expected) in [(100, 0), (100, 3), (200, 0), (200, 2), (200, 3)] {
        policy.activity();
        now += idle_ms;
        policy.first_key(&rules, idle_ms, now);
        assert_eq!(policy.select(&rules, now, false, true), Some(expected));
        policy.complete(&rules, expected, now);
        assert_eq!(policy.select(&rules, now, false, true), None);
    }
}

#[test]
fn first_key_rotation_keeps_pending_work_when_idle_is_too_short() {
    let rules = [
        IdentifierRule::new(100, 1, true, false),
        IdentifierRule::new(100, 1, true, false),
    ];
    let mut policy = IdentifierPolicy::new(&rules);
    policy.first_key(&rules, 100, 1000);
    policy.first_key(&rules, 99, 1000);
    assert_eq!(policy.select(&rules, 1000, false, true), Some(0));
    // Rekey during a polite hold or retry must retain the selected initial.
    policy.defer(&rules, 0, 1000);
    policy.first_key(&rules, 100, 1100);
    assert_eq!(policy.select(&rules, 1099, false, true), None);
    assert_eq!(policy.select(&rules, 1100, false, true), Some(0));
    policy.complete(&rules, 0, 1100);
    assert_eq!(policy.select(&rules, 1100, false, true), None);
    policy.activity();
    policy.first_key(&rules, 100, 1200);
    assert_eq!(policy.select(&rules, 1200, false, true), Some(1));
    policy.complete(&rules, 1, 1200);
    assert_eq!(policy.select(&rules, 1200, false, true), None);
    let mut empty = IdentifierPolicy::new(&[]);
    empty.first_key(&[], 1000, 1000);
    assert_eq!(empty.select(&[], 1000, false, true), None);
}

#[test]
fn completed_initial_id_consumes_one_receive_idle_opportunity() {
    let rules = [
        IdentifierRule::new(940_000, 10, true, false),
        IdentifierRule::new(940_000, 10, true, false),
        IdentifierRule::new(940_000, 20, false, true),
    ];
    for completed in [0, 2] {
        let mut policy = IdentifierPolicy::new(&rules);
        assert_eq!(policy.first_key(&rules, 940_000, 1_000_000), Some(0));
        // Reception during the polite wait must not reopen the completed opportunity.
        policy.activity();
        policy.complete(&rules, completed, 1_000_000);
        for now in [1_000_000, 1_060_000, 10_000_000] {
            assert_eq!(policy.first_key(&rules, now, now), None);
        }
        policy.activity();
        assert_eq!(policy.first_key(&rules, 939_999, 10_940_000), None);
        assert_eq!(policy.first_key(&rules, 940_000, 10_940_000), Some(1));
        policy.complete(&rules, 1, 10_940_000);
        policy.activity();
        assert_eq!(policy.first_key(&rules, 940_000, 11_000_000), None);
    }
}

#[test]
fn failed_identifier_waits_one_interval_without_satisfying_lower_priorities() {
    let rules = [
        IdentifierRule::new(100, 1, false, true),
        IdentifierRule::new(100, 3, false, true),
    ];
    let mut policy = IdentifierPolicy::new(&rules);
    policy.defer(&rules, rules.len(), 100);
    assert_eq!(policy.select(&rules, 100, false, true), Some(1));
    policy.defer(&rules, 1, 100);
    assert_eq!(policy.select(&rules, 100, false, true), Some(0));
    policy.complete(&rules, 0, 100);
    assert_eq!(policy.select(&rules, 199, false, true), None);
    assert_eq!(policy.select(&rules, 200, false, true), Some(1));
    policy.complete(&rules, 1, 200);
    assert_eq!(policy.select(&rules, 299, false, true), None);
    assert_eq!(policy.select(&rules, 300, false, true), Some(1));
}
