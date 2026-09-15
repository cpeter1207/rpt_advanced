use super::{ConfigDocument, ConfigError, Schema};

fn structure_error(source: &str) -> (String, String) {
    match Schema::validate(&ConfigDocument::parse(source).unwrap()) {
        Err(ConfigError::Structure { section, message }) => (section, message),
        other => panic!("expected structural error, got {other:?}"),
    }
}

#[test]
fn unknown_input_warns_once_with_source_context() {
    let source = "; first\n[future extension]\nunknown=yes\n[future extension]\nother=no\n[general]\nfull_duplex=maybe\n";
    let result = Schema::validate(&ConfigDocument::parse(source).unwrap()).unwrap();
    assert_eq!(result.warnings.len(), 2);
    assert_eq!(result.warnings[0].line, 2);
    assert_eq!(result.warnings[0].section, "future extension");
    assert!(result.warnings[0].key.is_empty());
    assert_eq!(result.warnings[0].message, "unknown section ignored");
    assert_eq!(result.warnings[0].fallback, "section ignored");
    assert_eq!(result.warnings[1].line, 7);
    assert_eq!(result.warnings[1].key, "full_duplex");
    assert_eq!(result.warnings[1].value, "maybe");
    assert_eq!(result.warnings[1].fallback, "yes");
}

#[test]
fn repeated_default_sections_are_merged_without_duplicate_warnings() {
    let source = "[general]\nfull_duplex=maybe\n[general]\ntransmit_hang_ms=invalid\n";
    let result = Schema::validate(&ConfigDocument::parse(source).unwrap()).unwrap();

    assert_eq!(result.warnings.len(), 2);
    assert_eq!(result.warnings[0].key, "full_duplex");
    assert_eq!(result.warnings[1].key, "transmit_hang_ms");
}

#[test]
fn scoped_sections_require_declared_bounded_nodes_and_labels() {
    assert_eq!(
        structure_error("[identifier missing set]\ninterval_ms=1\n").1,
        "scoped section references an unknown node"
    );
    let long = "n".repeat(64);
    assert_eq!(
        structure_error(&format!("[{long}]\n")).1,
        "node or label exceeds transport limit"
    );
    assert_eq!(
        structure_error("[node]\n[template node same]\ntext=x\n[template node same]\ntext=y\n").1,
        "duplicate named definition"
    );
    assert_eq!(
        structure_error("[node]\n[identifier node same]\n[identifier node same]\n").1,
        "duplicate named definition"
    );
}

#[test]
fn courtesy_assignments_are_complete_and_unique_per_node() {
    let cases = [
        (
            "[node]\n[courtesy node missing]\nmorse_text=M\n",
            "courtesy input is required",
        ),
        (
            "[node]\n[courtesy node receiver]\ninput=receiver\nremote_node=100\n",
            "courtesy remote node requires link input",
        ),
        (
            "[node]\n[courtesy node a]\ninput=receiver\n[courtesy node b]\ninput=receiver\n",
            "duplicate courtesy input assignment",
        ),
        (
            "[node]\n[courtesy node a]\ninput=link\n[courtesy node b]\ninput=link\n",
            "duplicate courtesy input assignment",
        ),
        (
            "[node]\n[courtesy node a]\ninput=link\nremote_node=100\n[courtesy node b]\ninput=link\nremote_node=100\n",
            "duplicate courtesy input assignment",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(structure_error(source).1, expected, "{source}");
    }

    Schema::validate(
        &ConfigDocument::parse(
            "[node]\n[courtesy node receiver]\ninput=receiver\n[courtesy node generic]\ninput=link\n[courtesy node peer]\ninput=link\nremote_node=100\n",
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn templates_macros_and_events_are_validated_after_inheritance() {
    let cases = [
        ("[template missing]\n", "template text is required"),
        (
            "[template bad]\ntext=${unknown}\n",
            "invalid message template",
        ),
        (
            "[macro bad]\naction=connect\n",
            "macro target node is required",
        ),
        (
            "[macro bad]\naction=disconnect_all\ntarget_node=123\n",
            "macro target node is not allowed",
        ),
        (
            "[node]\n[event node bad]\nmessage=hello\n",
            "event at is required",
        ),
        (
            "[node]\n[event node bad]\nat=daily 01:00\n",
            "event message, template, or macro is required",
        ),
        (
            "[node]\n[event node bad]\nat=daily 01:00\nmessage=x\ntemplate=y\n",
            "event message and template are mutually exclusive",
        ),
        (
            "[node]\n[event node bad]\nat=daily 25:00\nmessage=x\n",
            "invalid event time",
        ),
        (
            "[node]\n[event node bad]\nat=daily 01:00\ntemplate=missing\n",
            "event references an unknown template",
        ),
        (
            "[node]\n[event node bad]\nat=daily 01:00\nmacro=missing\n",
            "event references an unknown macro",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(structure_error(source).1, expected, "{source}");
    }

    Schema::validate(
        &ConfigDocument::parse(
            "[node]\n[template greeting]\ntext=hello ${node}\n[macro reconnect]\naction=reconnect_all\n[event node morning]\nat=weekly Monday 08:00\ntemplate=greeting\nmacro=reconnect\n",
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn configured_links_are_complete_unique_and_reference_a_distinct_primary() {
    let valid = "[524950]\n[permanent 524950 primary]\nremote_node=506315\n[schedule 524950 net]\nremote_node=2627\nreplace_permanent=primary\ndays=Monday-Friday\nstart_time=11:00\nend_time=12:00\nend_inactivity_ms=300000\n";
    Schema::validate(&ConfigDocument::parse(valid).unwrap()).unwrap();

    let cases = [
        (
            "[node]\n[permanent node primary]\n",
            "permanent remote node is required",
        ),
        (
            "[123]\n[permanent 123 self]\nremote_node=123\n",
            "configured link cannot target its local node",
        ),
        (
            "[node]\n[permanent node a]\nremote_node=1\n[permanent node b]\nremote_node=1\n",
            "duplicate configured link remote node",
        ),
        (
            "[node]\n[permanent node primary]\nremote_node=1\n[schedule node net]\nremote_node=2\nreplace_permanent=missing\nstart_time=11:00\nend_time=12:00\n",
            "schedule references an unknown permanent link",
        ),
        (
            "[node]\n[permanent node primary]\nremote_node=1\n[schedule node net]\nremote_node=1\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n",
            "schedule replacement must select another node",
        ),
        (
            "[node]\n[permanent node primary]\nremote_node=1\n[schedule node net]\nremote_node=2\nreplace_permanent=primary\nstart_time=12:00\nend_time=11:00\n",
            "invalid schedule window",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(structure_error(source).1, expected, "{source}");
    }
}
