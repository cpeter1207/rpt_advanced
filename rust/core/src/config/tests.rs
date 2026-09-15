use super::{ConfigDocument, ConfigError, NodeId, ResolvedNodeSettings, Schema};

#[test]
fn parses_owned_sections_and_options_without_borrowing_input() {
    let source = String::from("[general]\nnode_enabled = yes\n\n[node]\nradio_channel = usb\n");
    let document = ConfigDocument::parse(&source).expect("valid document");
    drop(source);

    assert_eq!(document.entries().len(), 2);
    assert_eq!(document.entries()[0].section, "general");
    assert_eq!(document.entries()[0].key, "node_enabled");
    assert_eq!(document.entries()[0].value, "yes");
}

#[test]
fn parser_rejects_malformed_lines_and_options_before_sections() {
    let cases = ["node_enabled = yes\n", "[general\n", "[general] trailing\n", "[general]\n= yes\n"];
    for source in cases {
        assert!(matches!(ConfigDocument::parse(source), Err(ConfigError::Syntax { .. })));
    }
}

#[test]
fn schema_warns_unknown_options_and_retired_media_options_are_not_special() {
    let document = ConfigDocument::parse(
        "[node]\nnode_enabled = maybe\nsample_rate_hz = 8000\ncodec = ulaw\nunknown = yes\n",
    )
    .expect("syntax is valid");
    let result = Schema::validate(&document).expect("recoverable options use defaults");
    assert!(result.warnings.iter().any(|warning| warning.key == "unknown"));
    assert!(result.warnings.iter().any(|warning| warning.key == "sample_rate_hz"));
    assert!(result.warnings.iter().any(|warning| warning.key == "codec"));
}

#[test]
fn node_resolution_uses_scoped_values_over_defaults_regardless_of_file_order() {
    let document = ConfigDocument::parse(
        "[node]\ntransmit_hang_ms = 77\nfull_duplex = no\n[general]\ntransmit_hang_ms = 11\nfull_duplex = yes\n",
    )
    .expect("valid document");
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap())
        .expect("valid settings");
    assert_eq!(resolved.value.transmit_hang_ms, 77);
    assert!(!resolved.value.full_duplex);
}

#[test]
fn unknown_or_invalid_values_warn_and_retain_defaults() {
    let document = ConfigDocument::parse(
        "[general]\nfull_duplex = maybe\ntransmit_hang_ms = invalid\nnode_enabled = invalid\n",
    )
    .expect("valid document");
    let resolved = ResolvedNodeSettings::resolve(&document, &NodeId::new("node").unwrap())
        .expect("invalid values are recoverable");
    assert!(resolved.value.enabled);
    assert!(resolved.value.full_duplex);
    assert_eq!(resolved.value.transmit_hang_ms, 0);
    assert_eq!(resolved.warnings.len(), 3);
}

#[test]
fn structural_schema_errors_are_not_recovered() {
    let document = ConfigDocument::parse("[template greeting]\ntext=one\n[template greeting]\ntext=two\n")
        .expect("valid syntax");
    assert!(matches!(Schema::validate(&document), Err(ConfigError::Structure { .. })));
}
