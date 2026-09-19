use rpt_advanced_core::template::{MessageTemplate, TemplateError, TemplateValues};

#[test]
fn accepts_only_documented_template_grammar_with_bounded_worst_case_output() {
    let largest = "x".repeat(127);
    let too_large = "x".repeat(128);
    let cases = [
        ("${day_of_week} ${date} ${time}", true),
        ("plain text", true),
        ("$x", true),
        ("", true),
        ("${link_status} ${date} ${time}x", true),
        ("${link_status} ${date} ${time}xx", false),
        ("${link_status}${greeting}${date}", false),
        ("${node}${callsign}x", true),
        ("${node}${callsign}xx", false),
        ("${unknown}", false),
        ("${xxxxxxxxxxx}", false),
        ("${xxxx}", false),
        ("${xxxxxxxx}", false),
        ("${node", false),
        ("${}", false),
        (&largest, true),
        (&too_large, false),
    ];

    for (text, valid) in cases {
        assert_eq!(
            MessageTemplate::parse(text).is_ok(),
            valid,
            "unexpected validation result for {text:?}"
        );
    }
}

#[test]
fn renders_every_documented_substitution_literally() {
    let values = TemplateValues {
        day_of_week: "Wednesday",
        date: "2026-09-10",
        time: "8:15 PM",
        greeting: "Good Evening",
        link_status: "no links",
        node: "524950",
        callsign: "KG0BP",
    };
    let cases = [
        (
            "${day_of_week} ${date} ${time}",
            "Wednesday 2026-09-10 8:15 PM",
        ),
        ("${greeting}", "Good Evening"),
        ("${link_status}", "no links"),
        ("${node}", "524950"),
        ("${callsign}", "KG0BP"),
        ("plain text", "plain text"),
        ("$x", "$x"),
        ("", ""),
    ];

    for (text, expected) in cases {
        assert_eq!(
            MessageTemplate::parse(text)
                .unwrap()
                .render(&values)
                .unwrap(),
            expected
        );
    }
}

#[test]
fn rendering_preserves_empty_values() {
    let values = TemplateValues {
        day_of_week: "Wednesday",
        date: "2026-09-10",
        time: "8:15 PM",
        greeting: "Good Evening",
        link_status: "no links",
        node: "524950",
        callsign: "",
    };

    assert_eq!(
        MessageTemplate::parse("${callsign}")
            .unwrap()
            .render(&values)
            .unwrap(),
        ""
    );
}

#[test]
fn rendering_rejects_actual_output_overflow_without_truncation() {
    let oversized = "x".repeat(128);
    let values = TemplateValues {
        day_of_week: "Wednesday",
        date: "2026-09-10",
        time: "8:15 PM",
        greeting: "Good Evening",
        link_status: &oversized,
        node: "524950",
        callsign: "KG0BP",
    };

    assert_eq!(
        MessageTemplate::parse("${link_status}")
            .unwrap()
            .render(&values),
        Err(TemplateError::OutputTooLong)
    );
}

#[test]
fn reports_stable_template_errors() {
    assert_eq!(
        MessageTemplate::parse("${unknown}"),
        Err(TemplateError::InvalidSyntax)
    );
    assert_eq!(
        TemplateError::InvalidSyntax.to_string(),
        "invalid message template syntax"
    );
    assert_eq!(
        TemplateError::OutputTooLong.to_string(),
        "message template output is too long"
    );
}
