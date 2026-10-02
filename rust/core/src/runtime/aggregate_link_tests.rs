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
