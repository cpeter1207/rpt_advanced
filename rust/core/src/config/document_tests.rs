use super::{ConfigDocument, ConfigError};

#[test]
fn document_retains_order_duplicates_empty_sections_and_long_values() {
    let long = "x".repeat(16_384);
    let source = format!(
        "; comment\n[general]\n[empty]\n[general]\nvalue={long}\n[identifier node first]\n[identifier node second]"
    );
    let document = ConfigDocument::parse(&source).unwrap();
    assert_eq!(
        document.sections(),
        [
            "general",
            "empty",
            "general",
            "identifier node first",
            "identifier node second"
        ]
    );
    assert_eq!(document.entries()[0].value, long);
    assert_eq!(document.entries()[0].line, 5);
}

#[test]
fn document_reports_exact_physical_syntax_line_and_accepts_unterminated_input() {
    let valid = ConfigDocument::parse("[node]\nvalue=last").unwrap();
    assert_eq!(valid.entries()[0].value, "last");

    for (source, expected_line) in [
        ("value=before\n[node]\n", 1),
        ("\n[node]\nvalid=yes\nbroken\n", 4),
        ("[node]\nvalid=yes\nembedded=bad\0value\n", 3),
    ] {
        assert!(matches!(
            ConfigDocument::parse(source),
            Err(ConfigError::Syntax { line, .. }) if line == expected_line
        ));
    }
    assert!(ConfigDocument::parse("").unwrap().entries().is_empty());
}

#[test]
fn document_lookup_is_exact_specific_and_last_definition_wins() {
    let document = ConfigDocument::parse(
        "[identifier]\ntext=global\n[identifier node]\ntext=first\ntext=second\n[identifier node set]\ntext=\n[identifier other]\ntext=other\n",
    )
    .unwrap();
    assert_eq!(
        document.lookup(
            "text",
            &["identifier node set", "identifier node", "identifier"]
        ),
        Some("")
    );
    assert_eq!(
        document.lookup("text", &["identifier node", "identifier"]),
        Some("second")
    );
    assert_eq!(document.lookup("missing", &["identifier"]), None);
}

#[test]
fn enumeration_keeps_unique_known_definitions_in_source_order() {
    let document = ConfigDocument::parse(
        "[general]\n[identifier]\n[node]\n[node]\n[other]\n[template global]\ntext=x\n[template node local]\ntext=y\n[event node first]\nat=daily 00:00\nmessage=x\n[event node first]\n",
    )
    .unwrap();
    assert_eq!(document.nodes(), ["node", "other"]);
    assert_eq!(
        document.named_sections("template", None),
        ["template global"]
    );
    assert_eq!(
        document.named_sections("template", Some("node")),
        ["template node local"]
    );
    assert_eq!(
        document.named_sections("event", Some("node")),
        ["event node first"]
    );
}
