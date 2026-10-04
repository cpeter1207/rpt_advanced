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
fn statpost_options_are_recognized_in_global_and_node_sections() {
    let document = ConfigDocument::parse(
        "[general]\nstatpost_url=https://stats.example/report?site=main\nstatpost_time=60\n[1000]\nstatpost_url=http://127.0.0.1:8080/status\nstatpost_time=300\n",
    )
    .unwrap();

    let result = Schema::validate(&document).unwrap();
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
}

#[test]
fn statpost_url_without_host_and_blank_or_oversized_group_names_are_invalid() {
    let source = format!(
        "[1000]\nstatpost_url=http://\n[permanent 1000 net]\nremote_node=2000\ngroup_name=   \n[permanent 1000 long]\nremote_node=3000\ngroup_name={}\n",
        "x".repeat(64)
    );
    let document = ConfigDocument::parse(&source).unwrap();
    let warnings = Schema::validate(&document).unwrap().warnings;
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning.key == "statpost_url")
            .count(),
        1
    );
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning.key == "group_name")
            .count(),
        2
    );
}

#[test]
fn status_snapshot_interval_is_recognized_in_global_and_node_sections() {
    let document = ConfigDocument::parse(
        "[general]\nstatus_snapshot_interval_ms=80\n[1000]\nstatus_snapshot_interval_ms=25\n",
    )
    .unwrap();

    let result = Schema::validate(&document).unwrap();
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
}

#[test]
fn invalid_statpost_values_warn_and_use_inherited_fallback() {
    let document = ConfigDocument::parse(
        "[general]\nstatpost_url=https://stats.example/report\nstatpost_time=90\n[1000]\nstatpost_url=ftp://stats.example/report\nstatpost_time=601\n",
    )
    .unwrap();
    let warnings = Schema::validate(&document).unwrap().warnings;

    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings[0].key, "statpost_url");
    assert_eq!(warnings[0].message, "invalid option value");
    assert_eq!(warnings[0].fallback, "https://stats.example/report");
    assert_eq!(warnings[1].key, "statpost_time");
    assert_eq!(warnings[1].fallback, "90");

    let credentials =
        ConfigDocument::parse("[1000]\nstatpost_url=https://user:password@stats.example/report\n")
            .unwrap();
    let warning = Schema::validate(&credentials)
        .unwrap()
        .warnings
        .into_iter()
        .find(|warning| warning.key == "statpost_url")
        .unwrap();
    assert_eq!(warning.value, "<redacted URL>");
    assert!(!format!("{warning:?}").contains("password"));
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
    Schema::validate(
        &ConfigDocument::parse(&valid.replace("end_time=12:00", "end_time=24:00")).unwrap(),
    )
    .unwrap();

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
        (
            "[node]\n[permanent node primary]\nremote_node=1\n[schedule node net]\nremote_node=2\nreplace_permanent=primary\nstart_time=24:00\nend_time=24:01\n",
            "schedule remote node, replacement, start time, and end time are required",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(structure_error(source).1, expected, "{source}");
    }
}

#[test]
fn scheduled_link_warnings_are_validated_as_positive_ordered_millisecond_lists() {
    let source = "[524950]\n[permanent 524950 primary]\nremote_node=506315\n[schedule 524950 weekday-net]\nremote_node=2627\nreplace_permanent=primary\ndays=Monday-Friday\nstart_time=11:00\nend_time=12:00\nwarning_before_start_ms=3600000,1800000,900000,600000,300000,60000\nwarning_before_end_ms=600000,300000,60000\nwarning_message_id=scheduled-link-change\n";
    assert!(
        Schema::validate(&ConfigDocument::parse(source).unwrap())
            .unwrap()
            .warnings
            .is_empty()
    );
}

#[test]
fn invalid_or_incomplete_scheduled_link_warnings_are_reported_without_rejecting_config() {
    let base = "[524950]\n[permanent 524950 primary]\nremote_node=506315\n[schedule 524950 weekday-net]\nremote_node=2627\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n";
    for (extra, expected_key) in [
        (
            "warning_before_start_ms=60000,0\nwarning_message_id=scheduled-link-change\n",
            "warning_before_start_ms",
        ),
        (
            "warning_before_end_ms=60000,,30000\nwarning_message_id=scheduled-link-change\n",
            "warning_before_end_ms",
        ),
        ("warning_before_start_ms=60000\n", "warning_message_id"),
        (
            "warning_before_start_ms=60000\nwarning_message_id=unknown\n",
            "warning_message_id",
        ),
    ] {
        let document = ConfigDocument::parse(&format!("{base}{extra}")).unwrap();
        assert!(
            Schema::validate(&document)
                .unwrap()
                .warnings
                .iter()
                .any(|warning| warning.key == expected_key)
        );
    }
}

#[test]
fn permanent_remote_node_list_is_accepted_without_config_warnings() {
    let source = "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315, 506312,506310,506311,506313,506314\ngroup_name=The Blind Hams Network\n";
    let validation = Schema::validate(&ConfigDocument::parse(source).unwrap()).unwrap();
    assert!(
        validation.warnings.is_empty(),
        "remote_node lists are valid"
    );
}

#[test]
fn scheduled_remote_node_list_and_group_name_are_accepted_without_warnings() {
    let source = "[524950]\n[permanent 524950 blind-hams]\nremote_node=506315,506312\n[schedule 524950 weekday-net]\nremote_node=2627,2628\ngroup_name=The Blind Hams Network\nreplace_permanent=blind-hams\nstart_time=11:00\nend_time=12:00\n";
    let result = Schema::validate(&ConfigDocument::parse(source).unwrap());
    assert!(
        result.is_ok(),
        "scheduled link groups should validate: {result:?}"
    );
    assert!(result.unwrap().warnings.is_empty());
}

#[test]
fn unnamed_single_member_permanent_group_remains_valid() {
    let source = "[524950]\n[permanent 524950 home]\nremote_node=506315\n";
    assert!(
        Schema::validate(&ConfigDocument::parse(source).unwrap())
            .unwrap()
            .warnings
            .is_empty()
    );
}

#[test]
fn malformed_or_conflicting_permanent_node_lists_report_specific_errors() {
    for remote in ["506315,", "506315,,506312", "506315,not-a-node"] {
        let source = format!("[524950]\n[permanent 524950 group]\nremote_node={remote}\n");
        assert_eq!(
            structure_error(&source).1,
            "permanent remote node list is invalid",
            "{remote}"
        );
    }
    for (source, expected) in [
        (
            "[524950]\n[permanent 524950 group]\nremote_node=506315,524950\n",
            "configured link cannot target its local node",
        ),
        (
            "[524950]\n[permanent 524950 one]\nremote_node=506315,506312\n[permanent 524950 two]\nremote_node=506314,506312\n",
            "duplicate configured link remote node",
        ),
    ] {
        assert_eq!(structure_error(source).1, expected, "{source}");
    }
    for remote in ["2627,", "2627,,2628", "2627,not-a-node"] {
        let source = format!(
            "[524950]\n[permanent 524950 group]\nremote_node=506315\n[schedule 524950 net]\nremote_node={remote}\nreplace_permanent=group\nstart_time=11:00\nend_time=12:00\n"
        );
        assert_eq!(
            structure_error(&source).1,
            "schedule remote node list is invalid",
            "{remote}"
        );
    }
}

#[test]
fn invalid_typed_options_report_exact_builtin_fallbacks() {
    for (section, key, expected) in [
        ("general", "transmit_timeout_ms", "180000"),
        ("general", "timeout_lockout_ms", "30000"),
        ("general", "kerchunk_max_ms", "500"),
        ("general", "telemetry_duck_db", "-20"),
        ("general", "courtesy_delay_ms", "250"),
        ("general", "link_lookup_method", "both"),
        ("general", "link_allow_nodes", "not configured"),
        ("identifier", "interval_ms", "600000"),
        ("identifier", "priority", "0"),
        ("identifier", "first_key_only", "no"),
        ("identifier", "polite_maximum_wait_ms", "60000"),
        ("identifier", "speech_speed_percent", "100"),
        ("identifier", "speech_level_db", "0"),
        ("identifier", "morse_speed_wpm", "20"),
        ("identifier", "morse_frequency_hz", "800"),
        ("identifier", "morse_level_db", "-6"),
        ("announcement", "interval_ms", "0"),
        ("courtesy", "level_db", "-20"),
        ("morse", "level_db", "not configured"),
        ("time", "format", "12"),
        ("general", "link_command_disconnect", "1"),
        ("general", "link_command_monitor", "2"),
        ("general", "link_command_transceive", "3"),
        ("general", "link_command_remote", "4"),
        ("general", "link_command_status", "70"),
        ("general", "link_command_disconnect_all", "806"),
        ("general", "link_command_last_keyed", "72"),
        ("general", "link_command_local_monitor", "75"),
        ("general", "link_command_disconnect_permanent", "811"),
        ("general", "link_command_permanent_monitor", "812"),
        ("general", "link_command_permanent_transceive", "813"),
        ("general", "link_command_full_status", "73"),
        ("general", "link_command_reconnect_all", "816"),
        ("general", "link_command_permanent_local_monitor", "818"),
    ] {
        let document = ConfigDocument::parse(&format!("[{section}]\n{key}=?\n")).unwrap();
        let warnings = Schema::validate(&document).unwrap().warnings;
        assert_eq!(warnings.len(), 1, "{section}/{key}");
        assert_eq!(warnings[0].fallback, expected, "{section}/{key}");
    }
}

#[test]
fn inherited_warning_fallbacks_follow_each_family_scope() {
    for (family, key, value) in [
        ("identifier", "priority", "9"),
        ("announcement", "interval_ms", "91"),
        ("courtesy", "level_db", "-9"),
        ("morse", "frequency_hz", "901"),
        ("speech", "speed_percent", "151"),
        ("time", "format", "24"),
    ] {
        let source = format!("[1000]\n[{family}]\n{key}={value}\n[{family} 1000]\n{key}=?\n");
        let warnings = Schema::validate(&ConfigDocument::parse(&source).unwrap())
            .unwrap()
            .warnings;
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].fallback, value);
    }
    for (key, family, family_key, value) in [
        ("speech_speed_percent", "speech", "speed_percent", "151"),
        ("speech_level_db", "speech", "level_db", "-9"),
        ("morse_speed_wpm", "morse", "speed_wpm", "31"),
        ("morse_frequency_hz", "morse", "frequency_hz", "901"),
        ("morse_level_db", "morse", "level_db", "-11"),
    ] {
        let source =
            format!("[1000]\n[{family}]\n{family_key}={value}\n[identifier 1000 test]\n{key}=?\n");
        let warnings = Schema::validate(&ConfigDocument::parse(&source).unwrap())
            .unwrap()
            .warnings;
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].fallback, value);
    }
}

#[test]
fn structural_errors_cover_bounded_labels_missing_macros_and_colliding_schedules() {
    assert_eq!(
        structure_error(&format!(
            "[node]\n[template node {}]\ntext=x\n",
            "x".repeat(64)
        ))
        .1,
        "node or label exceeds transport limit"
    );
    assert_eq!(
        structure_error("[macro empty]\n").1,
        "macro action is required"
    );
    assert_eq!(
        structure_error("[node]\n[template node absent]\n").1,
        "template text is required"
    );
    assert_eq!(
        structure_error(&format!("[template long]\ntext={}\n", "x".repeat(1025))).1,
        "scheduled message exceeds maximum output"
    );
    let primary = "[1000]\n[permanent 1000 primary]\nremote_node=2000\n";
    let schedule = |label, remote| {
        format!(
            "[schedule 1000 {label}]\nremote_node={remote}\nreplace_permanent=primary\nstart_time=11:00\nend_time=12:00\n"
        )
    };
    assert_eq!(
        structure_error(&format!("{primary}{}", schedule("self", "1000"))).1,
        "configured link cannot target its local node"
    );
    assert_eq!(
        structure_error(&format!(
            "{primary}{}{}",
            schedule("one", "3000"),
            schedule("two", "3000")
        ))
        .1,
        "duplicate configured link remote node"
    );
    let repeated_peer = format!(
        "{primary}[schedule 1000 morning]\nremote_node=3000\nreplace_permanent=primary\ndays=Monday-Friday\nstart_time=09:30\nend_time=11:00\n\
         [schedule 1000 evening]\nremote_node=3000\nreplace_permanent=primary\ndays=Monday\nstart_time=20:00\nend_time=21:30\n"
    );
    assert!(ConfigDocument::parse(&repeated_peer).is_ok());
    for missing in ["remote_node", "replace_permanent", "start_time", "end_time"] {
        let complete = schedule("incomplete", "3000");
        let incomplete = complete
            .lines()
            .filter(|line| !line.starts_with(missing))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            structure_error(&format!("{primary}{incomplete}\n")).1,
            "schedule remote node, replacement, start time, and end time are required"
        );
    }
}
