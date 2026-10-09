use super::prepare_links;
use crate::config::{ConfigDocument, NodeId};

#[test]
fn permanent_group_config_is_expanded_into_ordered_routes() {
    let document = ConfigDocument::parse(
        "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315,506312,506310,506311,506313,506314\ngroup_name=The Blind Hams Network\n",
    )
    .unwrap();
    let routes = prepare_links(&document, &NodeId::new("524950").unwrap(), 1, None)
        .unwrap()
        .route_specs();

    assert_eq!(
        routes
            .iter()
            .map(|route| route.remote.as_str())
            .collect::<Vec<_>>(),
        ["506315", "506312", "506310", "506311", "506313", "506314"]
    );
    assert!(routes.iter().all(|route| {
        route.group_label.as_deref() == Some("blind-hams")
            && route.group_name.as_deref() == Some("The Blind Hams Network")
    }));
    assert_eq!(
        routes
            .iter()
            .map(|route| route.group_priority)
            .collect::<Vec<_>>(),
        (0..6).map(Some).collect::<Vec<_>>()
    );
}

#[test]
fn scheduled_replacement_names_every_member_of_its_permanent_group() {
    let document = ConfigDocument::parse(
        "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315,506312,506310\n\
         [schedule 524950 daytime]\nremote_node=2627\nreplace_permanent=blind-hams\n\
         start_time=11:00\nend_time=12:00\n",
    )
    .unwrap();
    let schedule = prepare_links(&document, &NodeId::new("524950").unwrap(), 1, None).unwrap();
    assert_eq!(schedule.window_specs()[0].replaced, [0, 1, 2]);
}

#[test]
fn scheduled_link_group_expands_ordered_members_and_replaces_the_primary_group() {
    let document = ConfigDocument::parse(
        "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315,506312\n\
         [schedule 524950 weekday-net]\nremote_node=2627,2628\ngroup_name=The Blind Hams Network\n\
         replace_permanent=blind-hams\nstart_time=11:00\nend_time=12:00\n",
    )
    .unwrap();
    let result = prepare_links(&document, &NodeId::new("524950").unwrap(), 1, None);
    assert!(result.is_ok(), "scheduled link group should resolve");
    let mut schedule = result.unwrap();
    let routes = schedule.route_specs();
    let scheduled = &routes[2..];
    assert_eq!(
        scheduled
            .iter()
            .map(|route| route.remote.as_str())
            .collect::<Vec<_>>(),
        ["2627", "2628"]
    );
    assert!(scheduled.iter().all(|route| {
        !route.permanent
            && route.group_label.as_deref() == Some("schedule:weekday-net")
            && route.group_name.as_deref() == Some("The Blind Hams Network")
    }));
    assert_eq!(
        scheduled
            .iter()
            .map(|route| route.group_priority)
            .collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    assert!(schedule.is_group_member("2627"));
    assert!(schedule.is_group_member("2628"));
    assert_eq!(schedule.window_specs()[0].replaced, [0, 1]);

    let active =
        crate::schedule::CivilTime::new(2026, 9, 15, crate::schedule::Weekday::Tuesday, 11, 30)
            .unwrap();
    schedule.tick(active, 0, 0, |_| None, |_| false);
    for remote in ["2627", "2628"] {
        let attach = schedule.next_operation().unwrap();
        assert_eq!(attach.remote(), remote);
        assert_eq!(
            attach.action(),
            crate::runtime::link_schedule::LinkTransition::Attach
        );
        assert!(schedule.complete(&attach, true));
    }
    assert!(schedule.next_operation().is_none());
}

#[test]
fn named_nets_can_reuse_peers_in_separate_local_time_windows() {
    let document = ConfigDocument::parse(
        "[524950]\n[permanent 524950 blind-hams]\n\
         remote_node=506315,506312,506310,506311,506313,506314\n\
         group_name=The Blind Hams Network\n\
         [schedule 524950 coffeebreak_net]\nremote_node=51018\n\
         group_name=Coffeebreak Net\nreplace_permanent=blind-hams\n\
         days=Monday-Friday\nstart_time=09:30\nend_time=11:00\n\
         [schedule 524950 handiham_radio_club_net]\nremote_node=2627\n\
         group_name=Handiham Radio Club Net\nreplace_permanent=blind-hams\n\
         days=Monday-Friday\nstart_time=11:00\nend_time=12:00\n\
         [schedule 524950 all_nodes_net]\nremote_node=51018\n\
         group_name=All Nodes Net\nreplace_permanent=blind-hams\n\
         days=Monday\nstart_time=20:00\nend_time=21:30\n\
         [schedule 524950 science_net]\nremote_node=517300\n\
         group_name=Science Net\nreplace_permanent=blind-hams\n\
         days=Saturday\nstart_time=20:00\nend_time=22:00\n\
         [schedule 524950 handiham_trivia_net]\nremote_node=2627\n\
         group_name=Handiham Trivia Net\nreplace_permanent=blind-hams\n\
         days=Wednesday\nstart_time=19:00\nend_time=20:30\n",
    )
    .unwrap();

    let scheduler = prepare_links(&document, &NodeId::new("524950").unwrap(), 1, None).unwrap();
    assert_eq!(scheduler.window_specs().len(), 5);
    assert_eq!(
        scheduler
            .route_specs()
            .iter()
            .map(|route| route.remote.as_str())
            .collect::<Vec<_>>(),
        [
            "506315", "506312", "506310", "506311", "506313", "506314", "51018", "2627", "51018",
            "517300", "2627"
        ]
    );
}
