//! Independent EchoLink directory and optional Proxy account settings.

use super::{
    CandidateWarning, jitter,
    normalize::{Fields, Layers, Rule, timings, warn},
};
use crate::config::{
    EchoLinkSettings, EchoLinkTransport,
    echolink::{aprs_supported, default_value, host, value_valid},
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

macro_rules! field {
    ($wire:literal, $parser:ident) => {
        field!($wire, $wire, $parser)
    };
    ($wire:literal, $model:literal, $parser:ident) => {
        ($wire, Rule::Value(|value| $parser($model, value)))
    };
}

pub(super) const FIELDS: Fields = &[
    ("enabled", Rule::Bool),
    field!("callsign", text),
    field!("node_number", number),
    field!("name", text),
    field!("location", text),
    field!("email", text),
    field!("status_message", text),
    field!("latitude", scalar),
    field!("longitude", scalar),
    field!("frequency_mhz", scalar),
    field!("tone_hz", scalar),
    field!("power_w", scalar),
    field!("antenna_height_m", scalar),
    field!("antenna_gain_db", scalar),
    field!("antenna_direction_deg", scalar),
    field!("directory_servers", list),
    field!("aprs_server", text),
    field!("max_stations", number),
    field!("max_pending_handshakes", number),
    (
        "timing",
        Rule::Object(&[
            field!("heartbeat_interval_ms", number),
            field!("heartbeat_timeout_ms", number),
            field!("connect_timeout_ms", number),
            field!("directory_timeout_ms", number),
            field!("registration_refresh_ms", number),
            field!("directory_refresh_ms", number),
            field!("directory_retry_ms", number),
            field!("directory_freshness_ms", number),
        ]),
    ),
    (
        "rx",
        Rule::Object(&[field!("gain_db", "receive_gain_db", scalar)]),
    ),
    (
        "tx",
        Rule::Object(&[field!("gain_db", "transmit_gain_db", scalar)]),
    ),
    field!("announcement", text),
    field!("telemetry", text),
    ("telemetry_dynamic", Rule::Bool),
    field!("transport", text),
    field!("bind_address", text),
    field!("audio_port", number),
    field!("allow", list),
    field!("deny", list),
    (
        "proxy",
        Rule::Object(&[
            field!("host", "proxy_host", text),
            field!("port", "proxy_port", number),
        ]),
    ),
    ("jitter", Rule::Object(jitter::FIELDS)),
];

fn text(field: &str, value: &Value) -> Option<Value> {
    let raw = value.as_str()?;
    if !value_valid(field, raw) {
        return None;
    }
    Some(match field {
        "callsign" => Value::from(raw.to_ascii_uppercase()),
        "proxy_host" if !raw.is_empty() => Value::from(host(raw)?),
        "aprs_server" if !raw.is_empty() => {
            let (name, port) = raw.rsplit_once(':')?;
            Value::from(format!("{}:{}", host(name)?, port.parse::<u16>().ok()?))
        }
        _ => Value::from(raw),
    })
}

fn number(field: &str, value: &Value) -> Option<Value> {
    let number = value.as_u64()?;
    value_valid(field, &number.to_string()).then(|| Value::from(number))
}

fn scalar(field: &str, value: &Value) -> Option<Value> {
    let number = value.as_f64()?;
    value_valid(field, &number.to_string()).then(|| value.clone())
}

fn list(field: &str, value: &Value) -> Option<Value> {
    let entries = value.as_array()?;
    let normalized: Option<Vec<_>> = entries
        .iter()
        .map(|entry| {
            let raw = entry.as_str()?.trim();
            if raw.is_empty() || raw.contains(',') || !value_valid(field, raw) {
                return None;
            }
            if field == "directory_servers" {
                host(raw).map(Value::from)
            } else {
                Some(Value::from(raw.to_ascii_uppercase()))
            }
        })
        .collect();
    normalized
        .filter(|entries| field != "directory_servers" || !entries.is_empty())
        .map(Value::Array)
}

/// Identity of the directory credential, independent of Proxy authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoLinkDirectoryIdentity<'a> {
    /// Uppercase EchoLink account callsign.
    pub callsign: &'a str,
    /// Real EchoLink node number.
    pub node_number: u32,
    /// Configured directory pool in order. EchoLink directory TCP uses fixed port 5200.
    pub directory_servers: &'a [String],
}

/// Identity of the distinct EchoLink Proxy credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoLinkProxyIdentity<'a> {
    /// Parent EchoLink account callsign.
    pub callsign: &'a str,
    /// Parent EchoLink network node number.
    pub node_number: u32,
    /// Configured TCP Proxy hostname.
    pub host: &'a str,
    /// Configured TCP Proxy port.
    pub port: u16,
}

pub(super) fn resolve(
    layers: &Layers<'_>,
    warnings: &mut Vec<CandidateWarning>,
) -> EchoLinkSettings {
    let defaults = |field| default_value(field).expect("known public account field");
    let transport = layers
        .text(&["transport"])
        .unwrap_or_else(|| defaults("transport"));
    let is_proxy = transport == "proxy";
    let (budgets, selected) = timings(
        layers,
        [&["max_stations"], &["max_pending_handshakes"]],
        [
            defaults("max_stations").parse().unwrap(),
            defaults("max_pending_handshakes").parse().unwrap(),
        ],
        |[stations, pending]| stations.checked_add(pending).is_some(),
        warnings,
    );
    let mut normalized_text = BTreeMap::new();
    for path in [
        &["enabled"][..],
        &["callsign"],
        &["node_number"],
        &["name"],
        &["location"],
        &["email"],
        &["status_message"],
        &["latitude"],
        &["longitude"],
        &["frequency_mhz"],
        &["tone_hz"],
        &["power_w"],
        &["antenna_height_m"],
        &["antenna_gain_db"],
        &["antenna_direction_deg"],
        &["timing", "heartbeat_interval_ms"],
        &["timing", "heartbeat_timeout_ms"],
        &["timing", "connect_timeout_ms"],
        &["timing", "directory_timeout_ms"],
        &["timing", "registration_refresh_ms"],
        &["timing", "directory_refresh_ms"],
        &["timing", "directory_retry_ms"],
        &["timing", "directory_freshness_ms"],
        &["rx", "gain_db"],
        &["tx", "gain_db"],
        &["announcement"],
        &["telemetry"],
        &["telemetry_dynamic"],
        &["transport"],
        &["bind_address"],
        &["audio_port"],
        &["allow"],
        &["deny"],
        &["proxy", "host"],
        &["proxy", "port"],
    ] {
        let field = match path {
            ["rx", _] => "receive_gain_db",
            ["tx", _] => "transmit_gain_db",
            ["proxy", "host"] => "proxy_host",
            ["proxy", "port"] => "proxy_port",
            [.., field] => *field,
            [] => unreachable!("known field paths"),
        };
        let relevant = match field {
            "proxy_host" | "proxy_port" => is_proxy,
            "bind_address" | "audio_port" => !is_proxy,
            _ => true,
        };
        if relevant && let Some(value) = layers.get(path) {
            normalized_text.insert(field, public_text(value));
        }
    }
    for (i, field) in ["max_stations", "max_pending_handshakes"]
        .into_iter()
        .enumerate()
    {
        normalized_text.insert(field, budgets[i][selected[i]].milliseconds.to_string());
    }
    for (value, binding) in layers.values(&["aprs_server"]) {
        let endpoint = value.as_str().expect("normalized APRS endpoint");
        if aprs_supported(endpoint, transport) {
            normalized_text.insert("aprs_server", endpoint.to_owned());
            break;
        }
        warn(warnings, binding, "aprs_server");
    }
    let directory_servers = layers.get(&["directory_servers"]).map(|value| {
        value
            .as_array()
            .expect("normalized directory array")
            .iter()
            .map(|host| host.as_str().expect("normalized directory host").to_owned())
            .collect()
    });
    let mut settings = EchoLinkSettings::from_public_fields(
        |field| {
            normalized_text
                .get(field)
                .map_or_else(|| defaults(field), String::as_str)
        },
        directory_servers,
    );
    if settings.enabled && !settings.complete_binding() {
        for (_, binding) in layers
            .values(&["enabled"])
            .take_while(|(value, _)| value.as_bool() == Some(true))
        {
            warn(warnings, binding, "enabled");
        }
        settings.enabled = false;
    }
    settings
}

fn public_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => (if *value { "yes" } else { "no" }).to_owned(),
        Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().expect("normalized public string"))
            .collect::<Vec<_>>()
            .join(","),
        _ => value.to_string(),
    }
}

pub(super) fn trim_inactive(transport: EchoLinkTransport, fields: &mut Map<String, Value>) {
    let inactive = match transport {
        EchoLinkTransport::Direct => &["proxy"][..],
        EchoLinkTransport::Proxy => &["bind_address", "audio_port"][..],
    };
    for key in inactive {
        fields.remove(*key);
    }
}
