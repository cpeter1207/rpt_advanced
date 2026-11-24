//! Canonical top-level action fields used when overlaying sparse settings.

pub(super) fn fields(
    action: &str,
    operation: Option<&str>,
    target: Option<&str>,
) -> Option<&'static [&'static str]> {
    Some(match action {
        "disconnect" => &[],
        "select_command_target" => &[],
        "link_status" => &[],
        "disconnect_all" => &[],
        "pause_permanent_links" => &[],
        "last_keyed" => &[],
        "disconnect_permanent" => &[],
        "network_status" => &[],
        "reconnect_all" => &[],
        "resume_permanent_links" => &[],
        "disconnect_nonpermanent_all" => &[],
        "announce_time" => &[],
        "set_parrot" => &["enabled"],
        "site_status" => &["selector"],
        "cancel_message" => &[],
        "set_echo_telemetry" => &["mode"],
        "connect" => &["mode"],
        "connect_permanent" => &["mode"],
        "exit_command_mode" => &[],
        "send_command_digit" => &["digit"],
        "radio" => match operation {
            Some(
                "recall_memory" | "set_mode" | "get_setting" | "start_scan" | "get_memory"
                | "scan_status" | "set_scan_priority" | "invoke",
            ) => &["operation", "selector"],
            Some("set_frequency") => &["operation", "frequency_mhz", "offset_direction"],
            Some("set_tone") => &["operation", "direction", "frequency_hz"],
            Some("set_tone_enabled") => &["operation", "direction", "enabled"],
            Some("set_power_preset") => &["operation", "preset"],
            Some("status") => &["operation", "detail"],
            Some("step") => &["operation", "step_mhz"],
            Some("scan_preset") => &["operation", "direction", "preset"],
            Some(
                "tune" | "pause_scan" | "resume_scan" | "stop_scan" | "skip" | "current_status",
            ) => &["operation"],
            Some("set_setting") => &["operation", "selector", "value", "units"],
            Some("set_scan_direction") => &["operation", "direction"],
            Some("catalog") => &["operation", "kind", "page"],
            Some("set_inhibit") => &["operation", "enabled"],
            Some("set_scan_dwell" | "set_scan_resume_delay") => &["operation", "timing"],
            Some("set") => &["operation", "state"],
            None => &[],
            _ => return None,
        },
        "set_output" => &["name", "state"],
        "acknowledge_alarm" => &["name"],
        "reset_alarm" => &["name"],
        "reset_extrema" => &["name"],
        "set_fan" => &["name", "duty"],
        "set_transmit_inhibit" => &["enabled"],
        "call_macro" => &["macro"],
        "execute_dtmf" => &["digits"],
        "message" => &["destination", "message"],
        "pause" => &["timing"],
        "unlock_administration" => &["code"],
        "lock_administration" => &["code"],
        "start_pad_test" => &[],
        "controller_query" => match operation {
            Some("nodes" | "configuration_path" | "status" | "help" | "link_status") => {
                &["operation"]
            }
            _ => return None,
        },
        "radio_maintenance" => match operation {
            Some(
                "status"
                | "processing_status"
                | "active_configuration"
                | "read_eeprom"
                | "save_tuning_eeprom"
                | "clear_manual",
            ) => &["operation"],
            Some("get_mixer") => &["operation", "target"],
            Some("set_mixer") => &["operation", "target", "level"],
            Some("set_test_tone" | "set_ctcss_inhibit" | "set_subaudible_override") => {
                &["operation", "enabled"]
            }
            Some("write_eeprom") => &["operation", "words"],
            Some("transmit") => &["operation", "keyed", "ctcss_frequency_hz"],
            _ => return None,
        },
        "controller_management" => match operation {
            Some(
                "reload" | "validate_configuration" | "validate_secrets" | "validate_providers",
            ) => &["operation"],
            _ => return None,
        },
        "controller_lifecycle" => match operation {
            Some("start" | "stop" | "reload_poll") => &["operation"],
            _ => return None,
        },
        "radio_host_query" => match operation {
            Some("receive_policy") => &["operation"],
            _ => return None,
        },
        "feed_dtmf_symbol" => &["digit"],
        "actual_receiver_unkey" => &[],
        "phone_attach" => &[],
        "phone_dial" | "phone_hangup" => &[],
        "phone_select_command_target" => &[],
        "phone_send_node_digit" => &["digit"],
        "phone_send_telephone_digit" => &["digit"],
        "phone_exit_command_mode" => &[],
        "phone_manual_key" => &[],
        "phone_manual_unkey" => &[],
        "phone_session" => match operation {
            Some("authenticate") => &["operation", "mailbox", "pin"],
            Some("retire") => &["operation"],
            _ => return None,
        },
        "controller_admission" => match operation {
            Some("authorize_incoming" | "admit_incoming") => &["operation"],
            _ => return None,
        },
        "interface_control" => match operation {
            Some("set_channel") => &["operation", "channel"],
            Some("set_receive_ctcss" | "set_transmit_ctcss" | "set_echo") => {
                &["operation", "enabled"]
            }
            Some("write_pin") => &["operation", "pin", "state", "timing"],
            Some("set_frequencies") => &[
                "operation",
                "rx_frequency_mhz",
                "tx_frequency_mhz",
                "rx_ctcss_hz",
                "tx_ctcss_hz",
                "power",
            ],
            Some("get_echo") => &["operation"],
            _ => return None,
        },
        "controller_input" => match operation {
            Some("dtmf") => &["operation", "digits"],
            _ => return None,
        },
        "rx_mute_toggle" => match target {
            Some("port" | "peer" | "phone_patch") => &["target"],
            Some("peers") => &["target", "link_type"],
            _ => return None,
        },
        _ => return None,
    })
}

pub(super) fn known_field(key: &str) -> bool {
    matches!(
        key,
        "action"
            | "channel"
            | "code"
            | "ctcss_frequency_hz"
            | "destination"
            | "detail"
            | "digit"
            | "digits"
            | "direction"
            | "duty"
            | "enabled"
            | "frequency_hz"
            | "frequency_mhz"
            | "keyed"
            | "kind"
            | "level"
            | "link_type"
            | "macro"
            | "mailbox"
            | "message"
            | "mode"
            | "name"
            | "offset_direction"
            | "operation"
            | "page"
            | "parameter"
            | "pin"
            | "power"
            | "preset"
            | "rx_ctcss_hz"
            | "rx_frequency_mhz"
            | "selector"
            | "state"
            | "step_mhz"
            | "target"
            | "timing"
            | "tx_ctcss_hz"
            | "tx_frequency_mhz"
            | "units"
            | "value"
            | "words"
    )
}
