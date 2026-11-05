//! Bounded machine-result presentation through the node's Fluent message bundle.

use rpt_advanced_core::{
    messages::{MessageArgument, MessageForms},
    remote_base_command::{RemoteCommand, RemoteReplyPart},
    runtime::RuntimeError,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) struct Completion {
    pub(crate) successful: bool,
    pub(crate) parts: Vec<RemoteReplyPart>,
}

fn invalid() -> RuntimeError {
    RuntimeError::Preparation
}
fn text(value: impl Into<String>) -> MessageArgument {
    let text = value.into();
    MessageArgument::Forms(MessageForms {
        tts: text.clone(),
        morse: text.clone(),
        text,
    })
}
fn part(id: &'static str, values: Value) -> Result<RemoteReplyPart, RuntimeError> {
    let mut arguments = BTreeMap::new();
    for (key, value) in values.as_object().ok_or_else(invalid)? {
        let value = match value {
            Value::String(value) => text(value),
            Value::Bool(value) => MessageArgument::Number(f64::from(u32::from(*value))),
            Value::Number(value) => MessageArgument::Number(value.as_f64().ok_or_else(invalid)?),
            _ => return Err(invalid()),
        };
        arguments.insert(key.clone(), value);
    }
    Ok(RemoteReplyPart { id, arguments })
}
fn decimal(value: i64, places: u32) -> Result<String, RuntimeError> {
    if places > 6 {
        return Err(invalid());
    }
    if places == 0 {
        return Ok(value.to_string());
    }
    let magnitude = i128::from(value).abs();
    let scale = 10_i128.pow(places);
    Ok(format!(
        "{}{}.{:0width$}",
        if value < 0 { "-" } else { "" },
        magnitude / scale,
        magnitude % scale,
        width = places as usize
    ))
}
fn setting_value(value: i64, setting: &Value) -> Result<String, RuntimeError> {
    let places: u32 = setting["decimal_places"].as_u64().ok_or_else(invalid)?.try_into().map_err(|_| invalid())?;
    if setting["units"].as_u64().ok_or_else(invalid)? == 3 {
        if !(0..=511).contains(&value) || places != 0 { return Err(invalid()); }
        Ok(format!("{value:03o}"))
    } else {
        decimal(value, places)
    }
}
fn field(parts: &mut Vec<RemoteReplyPart>, name: &str, value: &Value) -> Result<(), RuntimeError> {
    let display = match value {
        Value::Null => String::new(),
        Value::Bool(value) => u32::from(*value).to_string(),
        Value::Number(value) if name.ends_with("dcs_bits") => {
            format!("{:03o}", value.as_u64().ok_or_else(invalid)?)
        }
        Value::Number(value) if name.ends_with("ctcss_tenths_hz") => {
            decimal(value.as_i64().ok_or_else(invalid)?, 1)?
        }
        Value::Number(value) => value.to_string(),
        _ => return Err(invalid()),
    };
    parts.push(part("remote-field", json!({"field":name,"value":display,"enum":value.as_u64().unwrap_or(u64::from(value.as_bool().unwrap_or(false))),"available":!value.is_null()}))?);
    Ok(())
}
fn state(
    parts: &mut Vec<RemoteReplyPart>,
    label: &str,
    value: &Value,
    long: bool,
) -> Result<(), RuntimeError> {
    parts.push(part(
        "remote-state",
        json!({"state":label,"available":!value.is_null()}),
    )?);
    if value.is_null() {
        return Ok(());
    }
    let value = value.as_object().ok_or_else(invalid)?;
    for name in [
        "rx_hz",
        "tx_hz",
        "mode",
        "bandwidth_hz",
        "rx_vfo",
        "tx_vfo",
        "split",
        "offset_hz",
        "rit_hz",
        "rit_enabled",
        "xit_hz",
        "xit_enabled",
        "power_mw",
        "channel",
    ] {
        if long || matches!(name, "rx_hz" | "tx_hz" | "mode" | "power_mw") {
            if let Some(value) = value.get(name) {
                field(parts, name, value)?;
            }
        }
    }
    if long {
        for direction in ["encode", "decode"] {
            if let Some(signal) = value.get(direction) {
                let signal = signal.as_object().ok_or_else(invalid)?;
                for name in [
                    "source",
                    "control_source",
                    "enabled",
                    "ctcss_tenths_hz",
                    "dcs_bits",
                    "inverted",
                ] {
                    if let Some(value) = signal.get(name) {
                        field(parts, &format!("{direction}_{name}"), value)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn scan_status(parts: &mut Vec<RemoteReplyPart>, value: &Value) -> Result<(), RuntimeError> {
    for name in ["list", "running", "paused", "activity_hold", "direction", "priority", "memory",
        "failed_memory", "last_error", "deadline_ms", "dwell_ms", "resume_delay_ms", "skipped"] {
        if let Some(value) = value.get(name) { field(parts, name, value)?; }
    }
    Ok(())
}

/// Decode the SDK's versioned copied result. No packet selects message IDs;
/// each supported kind maps to shipped localizable fragments on control.
pub(crate) fn parse(source: &str, request: RemoteCommand) -> Result<Completion, RuntimeError> {
    let result: Value = serde_json::from_str(source).map_err(|_| invalid())?;
    if result["version"] != 1 {
        return Err(invalid());
    }
    let kind = result["kind"].as_str().ok_or_else(invalid)?;
    let mut parts = Vec::new();
    match kind {
        "applied" => parts.push(part("remote-applied", json!({}))?),
        "error" => parts.push(part("remote-error", json!({"reason":result["reason"].as_str().ok_or_else(invalid)?,"code":result["code"].as_i64().unwrap_or(0)}))?),
        "value" => {
            let setting = &result["setting"];
            let selector = setting["selector"].as_u64().ok_or_else(invalid)?;
            let value = result["value"].as_i64().ok_or_else(invalid)?;
            parts.push(part("remote-value", json!({"selector":selector,"value":setting_value(value, setting)?,"units":setting["units"].as_u64().ok_or_else(invalid)?}))?);
        }
        "status" => {
            let long = !matches!(request, RemoteCommand::Status { long: false });
            if long { state(&mut parts, "requested", &result["requested"], true)?; }
            state(&mut parts, "applied", &result["applied"], long)?;
            if long { state(&mut parts, "observed", &result["observed_values"], true)?; }
            if long { scan_status(&mut parts, &result["scan"])?; }
            for name in ["tx_inhibited", "receive_permitted", "serviced_ptt"] {
                if let Some(value) = result.get(name) { field(&mut parts, name, value)?; }
            }
            if result["failure_code"].as_i64().is_some_and(|code| code != 0) {
                parts.push(part("remote-error", json!({"reason":"rejected","code":result["failure_code"]}))?);
            }
        }
        "capabilities" | "memories" | "scans" => {
            let entries = result["entries"].as_array().ok_or_else(invalid)?;
            if entries.is_empty() { parts.push(part("remote-empty", json!({"catalog":kind}))?); }
            for entry in entries {
                let selector = entry["selector"].as_u64().ok_or_else(invalid)?;
                if kind == "capabilities" {
                    parts.push(part("remote-capability", json!({"selector":selector,"read":entry["readable"].as_bool().ok_or_else(invalid)?,"write":entry["writable"].as_bool().ok_or_else(invalid)?,"minimum":setting_value(entry["minimum"].as_i64().ok_or_else(invalid)?, entry)?,"maximum":setting_value(entry["maximum"].as_i64().ok_or_else(invalid)?, entry)?,"units":entry["units"].as_u64().ok_or_else(invalid)?,"places":entry["decimal_places"].as_u64().ok_or_else(invalid)?}))?);
                    for choice in entry["choices"].as_array().ok_or_else(invalid)? { parts.push(part("remote-choice", json!({"selector":selector,"value":setting_value(choice.as_i64().ok_or_else(invalid)?, entry)?}))?); }
                } else {
                    parts.push(part("remote-entry", json!({"catalog":kind,"selector":selector,"name":entry["name"].as_str().ok_or_else(invalid)?}))?);
                    if kind == "memories" { state(&mut parts, "memory", &entry["state"], false)?; }
                }
            }
        }
        "scan" => {
            parts.push(part("remote-entry", json!({"catalog":"scans","selector":result["selector"].as_u64().ok_or_else(invalid)?,"name":result["name"].as_str().ok_or_else(invalid)?}))?);
            let members = result["members"].as_array().ok_or_else(invalid)?;
            for value in members { parts.push(part("remote-choice", json!({"selector":result["selector"],"value":value.as_u64().ok_or_else(invalid)?}))?); }
            scan_status(&mut parts, &result["state"])?;
        }
        _ => return Err(invalid()),
    }
    Ok(Completion {
        successful: kind != "error",
        parts,
    })
}

/// Preserve synchronous provider failure codes in the same localized path.
pub(crate) fn failure(code: i32) -> Completion {
    Completion {
        successful: false,
        parts: vec![
            part(
                "remote-error",
                json!({"reason":if code == 1 { "busy" } else { "rejected" },"code":code}),
            )
            .expect("fixed error arguments"),
        ],
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_scan_queries_and_complete_status_include_retained_failure_and_hold() {
        use rpt_advanced_core::messages::MessageCatalog;
        let catalog = MessageCatalog::load("en-US").unwrap();
        let scan = serde_json::json!({"list":7,"running":true,"paused":false,"activity_hold":true,
            "direction":1,"priority":3,"memory":1,"failed_memory":2,"last_error":-42,
            "deadline_ms":9000,"dwell_ms":500,"resume_delay_ms":2000,"skipped":1});
        for source in [
            serde_json::json!({"version":1,"kind":"status","applied":null,"requested":null,"observed_values":null,"scan":scan}),
            serde_json::json!({"version":1,"kind":"scan","selector":7,"name":"Test list","members":[],"state":scan}),
        ] {
            let reply = super::parse(&source.to_string(), super::RemoteCommand::CurrentStatus).unwrap();
            let speech = reply.parts.iter().map(|part| catalog.format_named(part.id, &part.arguments).unwrap().tts).collect::<Vec<_>>().join(" ");
            for required in ["Failed memory 2", "Last scan error -42", "Scan activity hold 1", "Priority memory 3"] {
                assert!(speech.contains(required), "missing {required}: {speech}");
            }
        }
    }

    #[test]
    fn remote_reply_uses_localized_forms_and_retains_truthful_units() {
        use rpt_advanced_core::{messages::MessageCatalog, remote_base_command::RemoteCommand};
        let catalog = MessageCatalog::load("en-US").unwrap();
        for (source, success, words) in [
            (r#"{"version":1,"kind":"value","setting":{"selector":14,"units":3,"decimal_places":0},"value":165}"#, true, "245"),
            (r#"{"version":1,"kind":"applied"}"#, true, "completed"),
            (
                r#"{"version":1,"kind":"error","reason":"busy","code":null}"#,
                false,
                "busy",
            ),
            (
                r#"{"version":1,"kind":"value","setting":{"selector":11,"units":2,"decimal_places":0},"value":2500}"#,
                true,
                "2500",
            ),
            (
                r#"{"version":1,"kind":"memories","entries":[{"selector":2,"name":"Local repeater","state":{"rx_hz":146520000,"tx_hz":null,"mode":1}}]}"#,
                true,
                "Local repeater",
            ),
            (
                r#"{"version":1,"kind":"capabilities","entries":[{"selector":307,"readable":true,"writable":true,"units":0,"decimal_places":0,"minimum":0,"maximum":1,"choices":[0,1]}]}"#,
                true,
                "Receive incremental",
            ),
            (
                r#"{"version":1,"kind":"scans","entries":[{"selector":2,"name":"Local scan"}]}"#,
                true,
                "Local scan",
            ),
            (
                r#"{"version":1,"kind":"scan","selector":2,"name":"Local scan","members":[1,2],"state":{"running":true,"paused":false,"memory":1}}"#,
                true,
                "Scan running",
            ),
            (
                r#"{"version":1,"kind":"capabilities","entries":[]}"#,
                true,
                "No more settings",
            ),
            (
                r#"{"version":1,"kind":"status","requested":{"rx_hz":146520000},"applied":null,"observed_values":{"rx_hz":146520000},"receive_permitted":false,"failure_code":-123}"#,
                true,
                "Observed settings",
            ),
        ] {
            let response = super::parse(source, RemoteCommand::CurrentStatus).unwrap();
            assert_eq!(response.successful, success);
            let speech = response
                .parts
                .iter()
                .map(|part| catalog.format_named(part.id, &part.arguments).unwrap().tts)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(speech.contains(words), "{speech}");
        }
        for invalid in [
            "{}",
            r#"{"version":2,"kind":"applied"}"#,
            r#"{"version":1,"kind":"unknown"}"#,
            r#"{"version":1,"kind":"value"}"#,
        ] {
            assert!(super::parse(invalid, RemoteCommand::CurrentStatus).is_err());
        }
    }
}
