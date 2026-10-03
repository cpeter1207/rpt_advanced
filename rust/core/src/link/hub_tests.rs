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
