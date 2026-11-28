use super::*;

const MEMBER: &str = "506315";
const OTHER: &str = "842001";
const COFFEE: &str = "51018";

#[test]
fn coffee_start_withdraws_an_existing_inbound_group_member_after_either_dial_outcome() {
    let mut cases = Vec::new();
    for duplicate_answered in [false, true] {
        let before = at(9, 29, 59, 0);
        let mut rt = station_fixture::start(
            replacement_configuration(),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
            before,
        )
        .unwrap();
        assert!(rt.advance_commands(PORT, before).unwrap().is_none());
        for remote in [MEMBER, OTHER] {
            rt.authorize_incoming(PORT, remote, true).unwrap();
            rt.incoming(PORT, remote, true).unwrap();
        }
        let initial = rt.node(PORT).unwrap().links().manager().snapshot();
        assert_eq!(initial.len(), 2);
        assert!(
            initial
                .iter()
                .all(|peer| !peer.permanent && !peer.retrying && !peer.ended)
        );
        assert!(
            initial.iter().any(|p| p.name == MEMBER) && initial.iter().any(|p| p.name == OTHER)
        );

        // A successful transport answer cannot publish a duplicate identity. Preserve the
        // existing inbound session; collect the separate automatic route's actual disposition.
        rt.tick_links(before);
        let duplicate = rt.next_link().map(|(owner, effect)| {
            assert_eq!(owner, PORT);
            let LinkEffect::Connect(attempt) = effect else {
                panic!("unexpected initial withdrawal")
            };
            assert!(attempt.permanent() && attempt.remote() == MEMBER);
            rt.finish_connect(PORT, attempt, duplicate_answered, before)
        });
        assert!(!matches!(duplicate, Some(Ok(true))));
        let automatic_intent = rt
            .node(PORT)
            .unwrap()
            .links()
            .manager()
            .owns_permanent(MEMBER);
        let mut detach_acks = Vec::new();
        let mut completions = Vec::new();
        for turn in 0..16 {
            let now = at(9, 30, 0, turn * 20);
            rt.tick_links(now);
            let mut effects = Vec::new();
            if let Some((effect, receipt)) = rt.advance_commands(PORT, now).unwrap() {
                effects.push(effect);
                receipt.settle(true);
            }
            while let Some((owner, effect)) = rt.next_link() {
                assert_eq!(owner, PORT);
                effects.push(effect);
            }
            for effect in effects {
                match effect {
                    LinkEffect::Connect(attempt) => {
                        let name = attempt.remote().to_owned();
                        let result = rt.finish_connect(
                            PORT,
                            attempt,
                            name == COFFEE || duplicate_answered,
                            now,
                        );
                        match name.as_str() {
                            COFFEE => assert_eq!(result, Ok(true)),
                            MEMBER => assert_ne!(
                                result,
                                Ok(true),
                                "duplicate published over existing peer"
                            ),
                            _ => panic!("unexpected dial target {name}"),
                        }
                    }
                    LinkEffect::Detach(peers) => {
                        for peer in peers {
                            rt.node(PORT).unwrap().links().ended(&peer);
                            rt.peer_detached(PORT, &peer, now.now_ms);
                            detach_acks.push(peer);
                        }
                    }
                    LinkEffect::None => {}
                    _ => panic!("unexpected coffee action"),
                }
            }
            if let Some(c) = rt.node(PORT).unwrap().take_command_completion() {
                completions.push(c.outcome);
            }
        }
        let status = rt.node(PORT).unwrap().links().manager().snapshot();
        let mut active = status
            .iter()
            .filter(|p| !p.ended && !p.retrying)
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>();
        active.sort_unstable();
        let coffee_live = active.contains(&COFFEE);
        let other_retained =
            active.contains(&OTHER) && !detach_acks.iter().any(|name| name == OTHER);
        let member_removed = !active.contains(&MEMBER) && detach_acks.as_slice() == [MEMBER];
        let completed = completions == [crate::command_language::MacroOutcome::Completed];
        let end = at(9, 30, 1, 0).now_ms;
        if member_removed {
            // A new, authorized connection to the same number is independent of the old member.
            rt.authorize_incoming(PORT, MEMBER, true).unwrap();
            rt.incoming(PORT, MEMBER, true).unwrap();
            for turn in 0..3 {
                let now = at(9, 30, 1, turn * 20);
                rt.tick_links(now);
                assert!(rt.advance_commands(PORT, now).unwrap().is_none());
                assert!(
                    rt.next_link().is_none(),
                    "a held group must not evict a later inbound call"
                );
            }
            assert!(
                rt.node(PORT)
                    .unwrap()
                    .links()
                    .manager()
                    .snapshot()
                    .iter()
                    .any(|peer| peer.name == MEMBER
                        && !peer.ended
                        && !peer.retrying
                        && !peer.permanent)
            );
        }
        let generation = rt.node(PORT).unwrap().status(end).active.unwrap();
        rt.stop(end);
        for (owner, effect) in rt.take_effects() {
            if let LinkEffect::Detach(peers) = effect {
                for peer in peers {
                    rt.peer_detached(&owner, &peer, end);
                }
            }
        }
        rt.detached(PORT, generation);
        assert!(rt.stop(end));
        eprintln!(
            "INBOUND_REPLACEMENT duplicate_answered={duplicate_answered} duplicate={duplicate:?} automatic_intent={automatic_intent} detach_acks={detach_acks:?} active={active:?} completed={completed} stop=true"
        );
        assert!(
            coffee_live && other_retained && completed,
            "Coffee macro and unrelated inbound control must remain live"
        );
        cases.push(member_removed);
    }
    assert_eq!(
        cases,
        [true, true],
        "Coffee replacement must detach the existing inbound group member regardless of the automatic dial outcome"
    );
}
