use super::scope::{ScopeKind, is_named, is_node_scoped, parse_scope};

#[test]
fn documented_section_shapes_are_classified_without_prefix_matches() {
    let cases = [
        ("general", ScopeKind::General, None, None),
        ("usb", ScopeKind::Node, Some("usb"), None),
        (
            "identifier usb",
            ScopeKind::IdentifierNode,
            Some("usb"),
            None,
        ),
        (
            "identifier usb welcome",
            ScopeKind::IdentifierSet,
            Some("usb"),
            Some("welcome"),
        ),
        (
            "template greeting",
            ScopeKind::TemplateGlobal,
            None,
            Some("greeting"),
        ),
        (
            "template usb greeting",
            ScopeKind::TemplateNode,
            Some("usb"),
            Some("greeting"),
        ),
        (
            "event usb morning",
            ScopeKind::EventNode,
            Some("usb"),
            Some("morning"),
        ),
    ];
    for (name, kind, node, label) in cases {
        let scope = parse_scope(name).expect(name);
        assert_eq!(scope.kind, kind);
        assert_eq!(scope.node, node);
        assert_eq!(scope.label, label);
    }
    assert_eq!(
        parse_scope("future extension").unwrap().kind,
        ScopeKind::Unknown
    );
}

#[test]
fn malformed_documented_shapes_are_not_reinterpreted_as_nodes() {
    for name in [
        "",
        "identifier  usb",
        "identifier usb too many",
        "morse usb label",
        "template",
        "event label",
        "schedule usb",
        "usb\tlabel",
        "usb[label",
    ] {
        assert!(parse_scope(name).is_err(), "{name}");
    }
}

#[test]
fn scope_role_helpers_cover_named_and_node_owned_definitions() {
    assert!(is_named(ScopeKind::CourtesySet));
    assert!(is_named(ScopeKind::TemplateGlobal));
    assert!(!is_named(ScopeKind::CourtesyNode));
    assert!(is_node_scoped(ScopeKind::ScheduleNode));
    assert!(!is_node_scoped(ScopeKind::TemplateGlobal));
}
