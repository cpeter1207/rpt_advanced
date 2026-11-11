use super::*;

#[test]
fn retirement_domains_deduplicate_preparation_and_do_not_cross_nodes() {
    let a = RetirementDomain::default();
    let b = RetirementDomain::default();
    let c = RetirementDomain::default();
    let reply = PeerSignals::new().reply_target();
    for _ in 0..4096 {
        reply.bind_retirement_domain(&a);
        reply.bind_retirement_domain(&b);
    }
    let mut count = 0;
    let mut link = reply.0.domains.get();
    while let Some(current) = link {
        count += 1;
        link = current.next.get();
    }
    assert_eq!(count, 2);
    reply.retire();
    reply.retire();
    assert!(reply.ended());
    assert_eq!(a.revision(), Some(1));
    assert_eq!(b.revision(), Some(1));
    assert_eq!(c.revision(), Some(0));
    reply.bind_retirement_domain(&c);
    assert_eq!(c.revision(), Some(1));
    reply.bind_retirement_domain(&c);
    assert_eq!(c.revision(), Some(1));
}

#[test]
fn retirement_domain_registration_cannot_miss_concurrent_completion() {
    for _ in 0..128 {
        let reply = PeerSignals::new().reply_target();
        let domain = RetirementDomain::default();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                reply.bind_retirement_domain(&domain);
            });
            start.wait();
            reply.retire();
        });
        assert!(reply.ended());
        assert!(domain.revision().unwrap() > 0);
    }
}

#[test]
fn retirement_domain_saturates_without_revalidating_old_blocks() {
    let domain = RetirementDomain(Arc::new(AtomicU64::new(u64::MAX - 1)));
    assert_eq!(domain.revision(), Some(u64::MAX - 1));
    for _ in 0..2 {
        let reply = PeerSignals::new().reply_target();
        reply.bind_retirement_domain(&domain);
        reply.retire();
        assert_eq!(domain.revision(), None);
    }
}

#[test]
fn retirement_completion_helps_an_already_started_owner_without_waiting() {
    let reply = PeerSignals::new().reply_target();
    let domain = RetirementDomain::default();
    reply.bind_retirement_domain(&domain);
    // The other retiring owner has published revocation but has not yet
    // traversed domains. A second owner must complete, never wait on it.
    reply.0.phase.store(RETIRING, Ordering::Release);
    assert!(reply.ended());
    reply.retire();
    assert_eq!(domain.revision(), Some(1));
    assert_eq!(reply.0.phase.load(Ordering::Acquire), RETIRED);
}
