use super::*;

#[test]
fn incoming_priority_member_uses_the_current_access_policy() {
    let mut hub = LinkManager::new("1000").unwrap();
    let denied = AccessPolicy::new("", "2000").unwrap();
    assert_eq!(
        hub.admit_incoming_member("2000", true, &denied, None),
        Err(AdmissionError::Denied)
    );

    let allowed = AccessPolicy::new("2000", "").unwrap();
    assert_eq!(
        hub.admit_incoming_member("2000", true, &allowed, None),
        Ok(())
    );
}

#[test]
fn grouped_topology_evidence_tracks_only_the_direct_group_member() {
    let mut hub = LinkManager::new("1000").unwrap();
    hub.attach_group("2000", Mode::TRANSCEIVE, true, Some("north"))
        .unwrap();
    hub.attach("3000", Mode::TRANSCEIVE, true).unwrap();

    assert_eq!(
        hub.topology_evidence("2000", Some("north")),
        TopologyEvidence(vec![("2000".into(), 'D')])
    );
}

#[test]
fn topology_marks_fallback_standby_local_only_without_changing_its_runtime_mode() {
    let mut hub = LinkManager::new("1000").unwrap();
    let selection = crate::link::GroupSelection::new();
    let member = |slot| {
        GroupMemberSelection::new(
            "network",
            std::num::NonZeroUsize::new(slot).unwrap(),
            selection.clone(),
        )
    };
    hub.attach_member("2000", Mode::TRANSCEIVE, true, Some(member(1)))
        .unwrap();
    hub.attach_member("3000", Mode::TRANSCEIVE, true, Some(member(2)))
        .unwrap();
    hub.attach("4000", Mode::MONITOR, true).unwrap();
    selection.callback_slot(true);

    assert_eq!(hub.full_topology(), "T2000,L3000,R4000");
    let status = hub.snapshot();
    assert!(
        status
            .iter()
            .any(|link| link.name == "3000" && link.mode == Mode::MONITOR && link.local_only)
    );
}

#[test]
fn disconnect_permanent_all_pauses_and_invalidates_inflight_retries() {
    let mut hub = LinkManager::new("1000").unwrap();
    hub.retain_retry("2000", Mode::TRANSCEIVE, 0).unwrap();
    let attempt = hub.take_retry(0).unwrap();

    assert!(hub.disconnect_permanent_all().is_empty());
    assert_eq!(hub.publish_retry(&attempt), Err(AdmissionError::Stale));
    assert!(
        hub.snapshot()
            .iter()
            .any(|peer| peer.name == "2000" && peer.paused)
    );
}

#[test]
fn explicit_reconnect_resumes_only_a_matching_paused_permanent_retry() {
    let mut hub = LinkManager::new("1000").unwrap();
    hub.attach("2000", Mode::MONITOR, false).unwrap();
    hub.disconnect_all();

    assert!(!hub.resume_permanent_retry("3000", Mode::TRANSCEIVE, 10));
    assert!(!hub.resume_permanent_retry("2000", Mode::TRANSCEIVE, 10));
    assert!(hub.snapshot().iter().any(|peer| peer.name == "2000"));

    hub.retain_retry("4000", Mode::TRANSCEIVE, 0).unwrap();
    assert!(!hub.resume_permanent_retry("4000", Mode::TRANSCEIVE, 10));
    hub.pause_retries();
    assert!(hub.resume_permanent_retry("4000", Mode::MONITOR, 20));
    let attempt = hub.take_retry(20).unwrap();
    assert_eq!(
        (attempt.name.as_str(), attempt.mode),
        ("4000", Mode::MONITOR)
    );
}
