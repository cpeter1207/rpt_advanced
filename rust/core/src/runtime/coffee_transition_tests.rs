use super::*;
use crate::runtime::links::{ConnectAttempt, LinkEffect};

fn at(hour: u8, minute: u8, second: u8, sub_ms: u16) -> RuntimeClock {
    let seconds = u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second);
    RuntimeClock {
        now_ms: seconds * 1000 + u64::from(sub_ms),
        wall_seconds: 1_795_586_400 + seconds as i64, // 2026-11-25 00:00 CST
        wall_subsecond_ms: sub_ms,
        civil: Some((
            CivilTime::new(2026, 11, 25, Weekday::Wednesday, hour, minute).unwrap(),
            second,
        )),
    }
}

fn replacement_configuration() -> Arc<crate::config::resources::generation::RuntimeConfiguration> {
    station_fixture::configuration(&[(PORT, "station")], &replacement_resources())
}

fn replacement_resources() -> [(&'static str, ResourceKind, &'static str, serde_json::Value); 8] {
    let action =
        |verb: &str, target: &str| json!({"actions":[{"action":verb, "parameter":target}]});
    let window = |sequence, start: &str, end: &str, target: &str| {
        json!({
            "sequence_number":sequence, "restart_recovery":true, "replace_permanent":"blind-hams",
            "start":{"at":format!("every weekday at {start} in America/Chicago"),
                     "macro":format!("connect_{target}"), "politeness_minutes":0},
            "end":{"at":format!("every weekday at {end} in America/Chicago"),
                   "macro":format!("disconnect_{target}"), "politeness_minutes":0}
        })
    };
    [
        (
            PORT,
            ResourceKind::Allstarlink,
            "",
            json!({"enabled":true,"node_number":524950}),
        ),
        (
            PORT,
            ResourceKind::PermanentGroup,
            "blind-hams",
            json!({"remote_node":["506315"],"group_name":"The Blind Hams Network"}),
        ),
        (
            PORT,
            ResourceKind::Macro,
            "connect_51018",
            action("connect", "51018"),
        ),
        (
            PORT,
            ResourceKind::Macro,
            "disconnect_51018",
            action("disconnect", "51018"),
        ),
        (
            PORT,
            ResourceKind::Macro,
            "connect_2627",
            action("connect", "2627"),
        ),
        (
            PORT,
            ResourceKind::Macro,
            "disconnect_2627",
            action("disconnect", "2627"),
        ),
        (
            PORT,
            ResourceKind::Event,
            "coffeebreak_net",
            window(10, "09:30", "11:00", "51018"),
        ),
        (
            PORT,
            ResourceKind::Event,
            "handiham_radio_club_net",
            window(20, "11:00", "12:00", "2627"),
        ),
    ]
}

fn accept_effect(
    rt: &mut Runtime,
    effect: LinkEffect,
    now: RuntimeClock,
    dials: &mut Vec<(u64, ConnectAttempt)>,
    delay_ms: u64,
    origin: &str,
) -> bool {
    match effect {
        LinkEffect::Connect(attempt) => {
            eprintln!(
                "schedule offer {origin} {} target={} permanent={} current={}",
                now.now_ms,
                attempt.remote(),
                attempt.permanent(),
                attempt.current_at(now.now_ms)
            );
            dials.push((now.now_ms + delay_ms, attempt));
            false
        }
        LinkEffect::Detach(peers) => {
            for peer in peers {
                eprintln!("schedule detach {origin} {} target={peer}", now.now_ms);
                rt.node(PORT).unwrap().links().ended(&peer);
                rt.peer_detached(PORT, &peer, now.now_ms);
            }
            true
        }
        LinkEffect::None => false,
        _ => panic!("unexpected link effect"),
    }
}

#[test]
fn adjacent_events_replace_one_permanent_route_across_peer_audio_generations() {
    let mut runs = Vec::new();
    for answer_delay_ms in [0, 40] {
        let mut rt = station_fixture::start(
            replacement_configuration(),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
            at(9, 29, 59, 0),
        )
        .unwrap();
        let (mut rx, mut tx) = rt.node(PORT).unwrap().register_audio().unwrap();
        let mut generation = rx.acquire_pair(&mut tx).unwrap().id();
        let mut outstanding = Vec::new();
        let mut receipts = Vec::new();
        let mut snapshots = Vec::new();
        let mut middle_primary_offers = 0;
        let mut refresh = false;
        for (hour, minute, second) in [
            (9, 29, 59),
            (9, 30, 0),
            (10, 59, 59),
            (11, 0, 0),
            (11, 59, 59),
            (12, 0, 0),
        ] {
            for turn in 0..16 {
                let now = at(hour, minute, second, turn * 20);
                rt.tick_links(now);
                if let Some((effect, receipt)) = rt.advance_commands(PORT, now).unwrap() {
                    refresh |= accept_effect(
                        &mut rt,
                        effect,
                        now,
                        &mut outstanding,
                        answer_delay_ms,
                        "macro",
                    );
                    receipt.settle(true); // The host acknowledges submission before the dial answers.
                }
                if let Some(completion) = rt.node(PORT).unwrap().take_command_completion() {
                    eprintln!(
                        "calendar completion {} {:?}",
                        now.now_ms, completion.outcome
                    );
                    receipts.push(completion.outcome);
                }
                while let Some((owner, effect)) = rt.next_link() {
                    assert_eq!(owner, PORT);
                    if let LinkEffect::Connect(attempt) = &effect
                        && hour == 11
                        && minute == 0
                        && attempt.remote() == "506315"
                    {
                        middle_primary_offers += 1;
                    }
                    refresh |= accept_effect(
                        &mut rt,
                        effect,
                        now,
                        &mut outstanding,
                        answer_delay_ms,
                        "route",
                    );
                }
                while outstanding
                    .first()
                    .is_some_and(|(due, _)| *due <= now.now_ms)
                {
                    let (_, attempt) = outstanding.remove(0);
                    let remote = attempt.remote().to_owned();
                    let current = attempt.current_at(now.now_ms);
                    let answer = rt.finish_connect(PORT, attempt, true, now);
                    eprintln!(
                        "schedule answer {} target={remote} current={current} result={answer:?}",
                        now.now_ms
                    );
                    refresh |= matches!(answer, Ok(true));
                }
                if refresh {
                    let prepared = adapter(PORT, rt.settings(PORT).unwrap()).unwrap();
                    match rt.replace_adapter(PORT, prepared, now.now_ms) {
                        Ok(new) => {
                            assert_eq!(rx.acquire_pair(&mut tx).unwrap().id(), new);
                            rt.detached(PORT, generation);
                            rt.reclaim();
                            generation = new;
                            refresh = false;
                        }
                        Err(RuntimeError::Busy) => (),
                        Err(other) => panic!("peer generation refresh: {other:?}"),
                    }
                }
            }
            let peers = rt
                .node(PORT)
                .unwrap()
                .links()
                .manager()
                .snapshot()
                .into_iter()
                .filter(|p| !p.ended && !p.retrying)
                .map(|p| p.name)
                .collect::<Vec<_>>();
            eprintln!(
                "schedule state {hour:02}:{minute:02}:{second:02} peers={peers:?} generation={generation}"
            );
            snapshots.push(peers);
        }
        let acquired = snapshots[0].as_slice() == ["506315"];
        let coffee = snapshots[1].as_slice() == ["51018"] && snapshots[2] == snapshots[1];
        let handi = snapshots[3].as_slice() == ["2627"] && snapshots[4] == snapshots[3];
        let restored = snapshots[5].as_slice() == ["506315"];
        let completed = receipts == [crate::command_language::MacroOutcome::Completed; 4];
        let quiescent = outstanding.is_empty() && !refresh;
        drop((rx, tx, outstanding));
        let end = at(12, 0, 1, 0).now_ms;
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
            "CALENDAR_HANDOFF delay_ms={answer_delay_ms} initial={acquired} coffee={coffee} handi={handi} restored={restored} middle_primary_offers={middle_primary_offers} receipts={receipts:?} quiescent={quiescent} generation={generation}"
        );
        assert!(
            acquired && generation > 1,
            "positive permanent acquisition and audio adoption"
        );
        runs.push(
            coffee && handi && restored && completed && middle_primary_offers == 0 && quiescent,
        );
    }
    assert_eq!(
        runs,
        [true, true],
        "adjacent replacement windows must keep the permanent route suspended through both acknowledged net transitions"
    );
}

#[path = "inbound_group_tests.rs"]
mod inbound_group;

#[path = "coffee_warning_tests.rs"]
mod warning_handoff;
