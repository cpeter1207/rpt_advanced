//! Request/profile discovery reuses logical setting definitions without exposing an aggregate resource.
use super::*;
use rpt_advanced_core::management::routes::View;
use std::sync::OnceLock;

fn definitions() -> &'static Value {
    static PARAMETER_INVENTORY: OnceLock<Value> = OnceLock::new();
    PARAMETER_INVENTORY.get_or_init(|| {
        serde_json::from_str::<Value>(include_str!("../../../../repeater-management.schema.json"))
            .expect("validated parameter inventory")["$defs"]
            .clone()
    })
}

fn object(properties: Value) -> Value {
    json!({"type":"object","properties":properties,"additionalProperties":false})
}
fn live_row_schema(target: Target) -> Value {
    let text = json!({"type":"string"});
    let flag = json!({"type":"boolean"});
    if target == Target::Peers {
        let mut peer = object(
            json!({"type":{"enum":["allstarlink","echolink","phone","webrtc"]},
                "node":text,"call_id":text,"muted":flag,"active":flag,
                "media_binding":{"type":"string","format":"uuid"},
                "connection_generation":status_decimal(false),
                "direction":{"enum":["incoming","outgoing"]},
                "mode":{"enum":["transceive","listen","monitor","local-monitor"]},
                "permanent":flag,"group_selected":flag,
                "state":{"enum":["connected","paused","retrying","disconnected"]},
                "quality":{"enum":["valid","stale","unavailable","fault","warming"]},
                "transmit_keyed":{"type":["boolean","null"]},
                "media":{"type":"object"}
            }),
        );
        peer["required"] = json!(["type", "muted", "active"]);
        peer["oneOf"] = json!([
            {"required":["node"]},{"required":["call_id"]},{"required":["media_binding"]}
        ]);
        peer
    } else {
        let optional = json!({"type":["boolean","null"]});
        let mut call = object(
            json!({"call_id":text,"account":{"type":"string","format":"uuid"},
            "generation":{"type":"integer","minimum":1},"direction":{"enum":["incoming","outgoing"]},
            "phase":{"enum":["private","authenticated","attached"]},"attached":flag,"authenticated":flag,
            "muted":optional,"active":optional}),
        );
        call["required"] = json!([
            "call_id",
            "account",
            "generation",
            "direction",
            "phase",
            "attached",
            "muted",
            "active"
        ]);
        call
    }
}

fn status_decimal(nullable: bool) -> Value {
    let types = if nullable {
        json!(["string", "null"])
    } else {
        json!("string")
    };
    json!({"type":types,"pattern":"^[0-9]+$"})
}

fn site_resource_status() -> Value {
    let quality = json!({"enum":["valid","warming","unavailable","stale","fault"]});
    let timestamp = json!({"type":["string","null"],"format":"date-time"});
    let count = json!({"type":["integer","null"],"minimum":0});
    let decimal = status_decimal(true);
    let cpu = object(json!({"percent":{"type":["number","null"],"minimum":0},
        "window_ms":decimal,"quality":quality}));
    let interface = object(json!({"ifindex":count,"rx_bps":decimal,"tx_bps":decimal,
        "window_ms":decimal,"observed_at":timestamp,"quality":quality}));
    object(json!({
        "observed_at":timestamp,"monotonic_ms":status_decimal(false),
        "cpu":object(json!({"online_count":count,"host":cpu,"process":cpu})),
        "memory":object(json!({
            "host":object(json!({"available_bytes":decimal,"total_bytes":decimal,"quality":quality})),
            "process":object(json!({"resident_bytes":decimal,"quality":quality})),
            "service":object(json!({"current_bytes":decimal,"limit_bytes":decimal,"quality":quality,"limit_quality":quality}))
        })),
        "network":object(json!({"quality":quality,"interfaces":{"type":"object","additionalProperties":interface}}))
    }))
}

fn status_media_correlation() -> Value {
    let binding = json!({"type":"string","format":"uuid"});
    let decimal = status_decimal(false);
    let mut anchor = object(json!({
        "binding":binding,"ssrc":{"type":"integer","minimum":0,"maximum":4294967295_u64},
        "epoch":decimal,"source_generation":decimal,
        "source_sample":decimal,"rtp_timestamp":decimal,
        "time_basis":{"enum":["session_monotonic","tai",null]},
        "time_ns":status_decimal(true),"uncertainty_ns":status_decimal(true)
    }));
    anchor["required"] = json!([
        "binding",
        "ssrc",
        "epoch",
        "source_generation",
        "source_sample",
        "rtp_timestamp",
        "time_basis",
        "time_ns",
        "uncertainty_ns"
    ]);
    let mut window = object(json!({
        "first_sample":decimal,"end_sample":decimal,
        "first_rtp_timestamp":decimal,"end_rtp_timestamp":decimal
    }));
    window["required"] = json!([
        "first_sample",
        "end_sample",
        "first_rtp_timestamp",
        "end_rtp_timestamp"
    ]);
    let mut correlation = object(json!({
        "binding":binding,"track":{"const":"audio"},"direction":{"const":"egress"},
        "quality":{"enum":["valid","unavailable"]},
        "anchor":{"oneOf":[anchor,{"type":"null"}]},"window":window
    }));
    correlation["required"] = json!(["binding", "track", "direction", "quality", "anchor"]);
    correlation
}

fn status_meter_schema() -> Value {
    let decimal = status_decimal(false);
    let level = json!({"type":["number","null"]});
    let mut sample = object(json!({"source_generation":decimal,"end_sample":decimal,
        "rate_hz":{"type":"integer","minimum":1}}));
    sample["required"] = json!(["source_generation", "end_sample", "rate_hz"]);
    object(json!({
        "port_id":{"type":"string","format":"uuid"},
        "chain_id":{"type":"string"},"stage_id":{"type":"string"},"signal":{"type":"string"},
        "entity":object(json!({"type":{"enum":["link","call"]},"id":{"type":"string"},"generation":decimal})),
        "graph_generation":decimal,"measurement_epoch":decimal,
        "observed_at":{"type":["string","null"],"format":"date-time"},"monotonic_ms":decimal,
        "audio_position":{"oneOf":[sample,{"type":"null"}]},
        "media_correlation":status_media_correlation(),
        "quality":{"enum":["valid","warming","unavailable","stale","fault"]},
        "signal_state":{"enum":["present","silence","unknown"]},
        "freshness_ms":{"type":"integer","minimum":0},"valid_samples":decimal,
        "clipped_samples":decimal,"dropped_snapshots":decimal,
        "peak_dbfs":level,"rms_dbfs":level,"lufs":level,"peak_hold_dbfs":level,
        "reason":{"type":"string"},"integrated_lufs":level,"true_peak_dbtp":level,
        "processed_blocks":decimal,"bypassed_blocks":decimal,"failed_blocks":decimal
    }))
}

fn status_schema(kind: Kind, port: bool) -> Value {
    let flag = json!({"type":"boolean"});
    let boolean = json!({"type":["boolean","null"]});
    let number = json!({"type":["number","null"]});
    let uint = json!({"type":"integer","minimum":0});
    let receiver = json!({"type":["object","null"],"properties":{"muted":flag,"active":flag},
                          "required":["muted","active"],"additionalProperties":false});
    let mut p = json!({"active_revision":{"type":["string","null"]},"owner_generation":{"type":["integer","null"],"minimum":0}});
    match kind {
        Kind::Metadata | Kind::Radio if port => {
            p["receiver"] = receiver;
            p["transmit_key_requested"] = boolean;
            let seconds = json!({"type":"integer"});
            let mut gap = object(json!({
                "previous_wall_seconds":seconds,
                "current_wall_seconds":seconds
            }));
            gap["required"] = json!(["previous_wall_seconds", "current_wall_seconds"]);
            p["calendar"] = json!({"type":["object","null"],"properties":{
                "previous_wall_seconds":seconds,"current_wall_seconds":seconds,"last_abnormal_gap":gap
            },"required":["current_wall_seconds"],"additionalProperties":false});
            p["last_disconnect"] = json!({"type":["object","null"],"properties":{
                "selected_count":uint,"removed_count":uint,
                "peer_nodes":{"type":"array","items":{"type":"string"},"maxItems":live::DISCONNECT_PEERS_LIMIT}
            },"required":["selected_count","removed_count","peer_nodes"],"additionalProperties":false});
            p["last_dial"] = json!({"type":["object","null"],"properties":{
                "node":{"type":"string"},"attached":flag
            },"required":["node","attached"],"additionalProperties":false});
        }
        Kind::Metadata if !port => p["resources"] = site_resource_status(),
        Kind::RxSignaling if port => p["receiver"] = receiver,
        Kind::TxSignaling if port => p["transmit_key_requested"] = boolean,
        Kind::RxAudio | Kind::TxAudio if port => {
            p["meter"] = json!({"type":["object","null"],"properties":{
                "timestamp_ms":uint,"samples":uint,"peak":number,"rms":number,
                "clipped_samples":uint,"dropped_snapshots":uint},"additionalProperties":false});
        }
        Kind::Allstarlink | Kind::Echolink | Kind::PhonePatch if port => {
            p["registration"] = object(
                json!({"state":{"enum":["unavailable","disabled","dormant","pending",
                "registered","retry","withdrawn","registering","online","retrying","inactive","starting","retiring","failed"]},
                "generation":uint,"registered":flag,"fresh":flag,"available":flag}),
            );
        }
        Kind::Sensor => {
            for name in ["value", "smoothed", "minimum", "maximum"] {
                p[name] = number.clone();
            }
            p["quality"] = json!({"type":"string"});
            for name in ["observed_at_ms", "minimum_at_ms", "maximum_at_ms"] {
                p[name] = json!({"type":["integer","null"]});
            }
        }
        Kind::Input => {
            p["value"] = boolean;
            p["quality"] = json!({"type":"string"});
            p["observed_at_ms"] = json!({"type":["integer","null"]});
        }
        Kind::Alarm => {
            for name in ["active", "acknowledged", "unsafe", "reset_ready"] {
                p[name] = boolean.clone();
            }
        }
        Kind::Output => {
            for name in ["requested_on", "fault", "readback"] {
                p[name] = boolean.clone();
            }
            p["submitted"] = json!({"type":["object","null"],"properties":{"level":number,"observed_at_ms":uint},"additionalProperties":false});
            p["submission_quality"] = json!({"type":"string"});
            p["pulse_deadline_ms"] = json!({"type":["integer","null"]});
        }
        Kind::Fan => {
            p["requested_duty"] = number;
            p["fault"] = flag;
            p["run_on_until_ms"] = json!({"type":["integer","null"]});
        }
        Kind::Event => {
            p["issue"] = json!({"type":["string","null"]});
            let mut boundary = object(json!({
                "nominal_wall_seconds":{"type":"integer"},
                "observed_wall_seconds":{"type":"integer"},
                "outcome":{"enum":["completed","rejected","cancelled","limit"]}
            }));
            boundary["required"] =
                json!(["nominal_wall_seconds", "observed_wall_seconds", "outcome"]);
            p["last_start"] = boundary.clone();
            p["last_end"] = boundary.clone();
            p["last_warning"] =
                object(json!({"boundary":{"enum":["start","end"]},"result":boundary}));
            p["last_warning"]["required"] = json!(["boundary", "result"]);
        }
        _ => {}
    }
    object(p)
}

fn lease_fields() -> Value {
    let mut cfg = object(
        json!({"active":{"type":"boolean"},"ttl_ms":{"type":"integer","minimum":1,"maximum":60000,"default":30000},"expected_active_revision":{"type":"string"}}),
    );
    cfg["required"] = json!(["active", "expected_active_revision"]);
    cfg
}
fn ids() -> Value {
    json!({"type":"array","uniqueItems":true,"items":{"type":"string","format":"uuid"}})
}
fn field_schema(kind: Kind, port: bool) -> Value {
    let defs = definitions();
    let extracted = |root: &str, field: &str| defs[root]["properties"][field].clone();
    let only = |name: &str, names: &[&str]| {
        let map = names
            .iter()
            .filter_map(|k| Some(((*k).to_owned(), defs[name]["properties"].get(*k)?.clone())))
            .collect::<serde_json::Map<_, _>>();
        object(Value::Object(map))
    };
    let mut config = match kind {
        Kind::Metadata => {
            let name = if port {
                "nodeDefinition"
            } else {
                "siteDefinition"
            };
            let mut schema = only(
                name,
                &["name", "inherit_from", "enabled", "callsign", "language"],
            );
            schema["required"] = json!(["name", "inherit_from"]);
            schema
        }
        Kind::Radio => only(
            "radio",
            &[
                "parrot",
                "timing",
                "startup_memory",
                "offset_magnitude_mhz",
                "duplex_mode",
            ],
        ),
        Kind::RxAudio => defs["receiveAudio"].clone(),
        Kind::TxAudio => defs["transmitAudio"].clone(),
        Kind::RxSignaling => {
            let mut s = extracted("receive", "signaling");
            s["properties"]["timing"] = extracted("receive", "timing");
            s
        }
        Kind::TxSignaling => extracted("transmit", "signaling"),
        Kind::Interface => only(
            "interface",
            &["audio", "gpio", "parallel", "serial", "keyboard"],
        ),
        Kind::PeerAudio => defs["linkProcessing"].clone(),
        Kind::Control | Kind::Power | Kind::Limits => extracted("radio", kind.as_str()),
        Kind::Memories => object(json!({"entries":extracted("radio","memories")})),
        Kind::ScanLists => extracted("radio", "scan_lists"),
        Kind::Allstarlink => only(
            "allstarlink",
            &[
                "allow_nodes",
                "deny_nodes",
                "static_nodes",
                "directory_file",
                "lookup_method",
                "iax",
                "status_post",
                "callsign",
                "node_number",
                "jitter",
                "credentials",
            ],
        ),
        Kind::Echolink => {
            let mut v = defs["echolink"].clone();
            v["properties"]
                .as_object_mut()
                .expect("EchoLink profile")
                .remove("proxy");
            v
        }
        Kind::EcholinkProxy => extracted("echolink", "proxy"),
        Kind::PhonePatch => defs["phonePatch"].clone(),
        Kind::Command => only(
            "commandSettings",
            &[
                "admin",
                "muting",
                "regeneration",
                "macro_limits",
                "mappings",
                "terminator",
                "maximum_digits",
                "inactivity_timeout_ms",
            ],
        ),
        Kind::Event => defs["event"].clone(),
        Kind::Macro => defs["commandMacro"].clone(),
        Kind::Message => defs["message"].clone(),
        Kind::TelemetrySettings => only("telemetry", &["duck_db", "timing", "speech", "morse"]),
        Kind::Identifier | Kind::Announcement | Kind::Tail | Kind::Courtesy => {
            defs[kind.as_str()].clone()
        }
        Kind::PermanentGroup => defs["permanentLink"].clone(),
        _ => defs[kind.as_str()].clone(),
    };
    config["description"] = json!(
        "Recognized public authoring fields. Recoverable invalid values or unknown settings produce warnings and fall back through inheritance."
    );
    config
}

/// Nonzero decimal ICE generation. Text retains the full unsigned 64-bit value.
fn ice_generation() -> Value {
    json!({"type":"string","pattern":"^[1-9][0-9]{0,19}$",
        "description":"Canonical nonzero unsigned 64-bit ICE generation."})
}

fn webrtc_request(target: Target) -> Value {
    let offer = json!({"type":"string","minLength":1,"writeOnly":true,
        "description":"Session description in SDP. The decoded size may not exceed 64 KiB."});
    let generation = ice_generation();
    match target {
        Target::WebRtcSessions => json!({
            "type":"object",
            "properties":{"direction":{"enum":["receive_only","transceive"]},"offer":offer},
            "required":["direction","offer"],"additionalProperties":false
        }),
        Target::WebRtcSession => json!({"oneOf":[
            {"type":"object","properties":{"action":{"const":"renew"},"ice_generation":generation},
                "required":["action","ice_generation"],"additionalProperties":false},
            {"type":"object","properties":{"action":{"const":"restart"},"ice_generation":generation,"offer":offer},
                "required":["action","ice_generation","offer"],"additionalProperties":false}
        ]}),
        Target::WebRtcCandidates => json!({
            "type":"object","properties":{
                "ice_generation":generation,"sequence":{"type":"integer","minimum":0},
                "candidate":{"type":["string","null"],"minLength":1,"writeOnly":true,
                    "description":"ICE candidate up to 4 KiB decoded, or null for end-of-candidates."}
            },"required":["ice_generation","sequence","candidate"],"additionalProperties":false
        }),
        _ => unreachable!("signaling request target"),
    }
}

pub(super) fn name(route: &Route, create: bool) -> String {
    if create {
        format!("{}-create", route.profile)
    } else {
        route.profile.clone()
    }
}

pub(super) fn response_name(route: &Route) -> String {
    let prefix = match route.view {
        Some(View::Configuration) => "configuration-",
        Some(View::Effective) => "effective-",
        Some(View::Status) => "status-",
        None => "",
    };
    format!("{prefix}{}-response", route.profile)
}

/// The read envelope is independent of the write instruction schema. Credential slots
/// are excluded before resolving any reusable schema definitions.
pub(super) fn response_document(route: &Route) -> Value {
    let name = response_name(route);
    let mut s = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":name,"type":"object"});
    if matches!(route.target, Target::Schema | Target::OpenApi) {
        return s;
    }
    let link = json!({"type":"object","properties":{"href":{"type":"string"},"templated":{"type":"boolean"}},"required":["href"]});
    let links = json!({"type":"object","properties":{"self":link,"profile":link,"item":{"type":"array","items":link},"curies":{"type":"array","items":link}},"required":["self","profile","curies"]});
    let text = json!({"type":"string"});
    let id = json!({"type":"string","format":"uuid"});
    if route.target.is_webrtc() {
        let mut properties = json!({"_links":links});
        properties["_links"]["properties"]["up"] = link.clone();
        match route.target {
            Target::WebRtcSessions => s["required"] = json!(["_links"]),
            Target::WebRtcSession => {
                properties["_links"]["properties"]["rpt:webrtc-candidates"] = link.clone();
                properties["_links"]["properties"]["rpt:close"] = link;
                properties["session_id"] = id.clone();
                properties["port_id"] = id;
                properties["phase"] = json!({"enum":["pending","active","closed"]});
                properties["ice_generation"] = ice_generation();
                properties["answer"] = json!({"type":"string","description":"SDP answer when ready. Available only to the owning browser."});
                s["required"] =
                    json!(["_links", "session_id", "port_id", "phase", "ice_generation"]);
            }
            Target::WebRtcCandidates => {
                properties["ice_generation"] = ice_generation();
                properties["candidates"] = json!({"type":"array","items":{
                    "type":"object","properties":{
                        "sequence":{"type":"integer","minimum":0},
                        "candidate":{"type":["string","null"],"description":"Local trickled ICE candidate, or null for end-of-candidates."}
                    },"required":["sequence","candidate"],"additionalProperties":false
                }});
                s["required"] = json!(["_links", "ice_generation", "candidates"]);
            }
            _ => unreachable!("signaling response target"),
        }
        s["properties"] = properties;
        s["additionalProperties"] = json!(false);
        return s;
    }
    let nullable_text = json!({"type":["string","null"]});
    let flags = object(
        json!({"password_configured":{"type":"boolean"},"pin_configured":{"type":"boolean"},"unlock_configured":{"type":"boolean"},"lock_configured":{"type":"boolean"}}),
    );
    let mut properties = json!({"_links":links,"meta":object(json!({"configuration_revision":text,"resource_version":text,"request_version":id,"secrets_state":{"enum":["ready","locked","error"]},"credentials":flags}))});
    if route.view == Some(View::Effective) {
        let origin = json!({"oneOf":[{"type":"object","properties":{"kind":{"const":"built-in"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"resource"},"href":{"type":"string","format":"uri-reference"}},"required":["kind","href"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"derived"},"sources":{"type":"array","items":link,"minItems":1}},"required":["kind","sources"],"additionalProperties":false}]});
        properties["meta"]["properties"]["sources"] = json!({"type":"object","additionalProperties":origin,"description":"Accepted effective leaves by RFC 6901 pointer. Each array has one source."});
        properties["meta"]["properties"]["deferred_relationships"] =
            json!({"type":"array","items":{"type":"string"}});
    }
    match route.target {
        Target::Section(_) | Target::Member(_) | Target::Metadata => {
            let kind = match route.target {
                Target::Section(k) | Target::Member(k) => k,
                _ => Kind::Metadata,
            };
            properties["id"] = id.clone();
            properties["key"] = text;
            if route.view == Some(View::Status) {
                properties["status"] = status_schema(kind, route.path.contains("{port}"));
            } else {
                let mut cfg = field_schema(kind, route.profile == "port");
                if let Some(props) = cfg.get_mut("properties").and_then(Value::as_object_mut) {
                    if route.view == Some(View::Effective) && kind == Kind::Limits {
                        for key in ["modes", "rx_ranges_mhz"] {
                            if let Some(constraints) =
                                props.get_mut(key).and_then(Value::as_object_mut)
                            {
                                constraints.remove("minItems");
                            }
                        }
                    }
                    props.remove("credentials");
                    props.remove("pin_hash");
                    if let Some(admin) = props
                        .get_mut("admin")
                        .and_then(|a| a.get_mut("properties"))
                        .and_then(Value::as_object_mut)
                    {
                        admin.remove("lock_hash");
                        admin.remove("unlock_hash");
                    }
                }
                if kind == Kind::Event {
                    if route.view == Some(View::Effective) {
                        properties["meta"]["properties"]["timezones"] = json!({
                            "type":"object",
                            "readOnly":true,
                            "additionalProperties":{"type":"string"},
                            "description":"Effective IANA timezones keyed by JSON pointer to each calendar expression."
                        });
                    }
                    if let Some(fields) = cfg.get("properties").and_then(Value::as_object) {
                        properties
                            .as_object_mut()
                            .expect("fields")
                            .extend(fields.clone());
                    }
                } else {
                    properties[if route.view == Some(View::Effective) {
                        "effective"
                    } else {
                        "configuration"
                    }] = cfg;
                }
            }
        }
        Target::Collection(_) => {
            properties[if route.view == Some(View::Effective) {
                "effective"
            } else {
                "configuration"
            }] = object(json!({"members":ids()}));
            properties["_embedded"] = object(
                json!({"item":{"type":"array","items":{"type":"object","properties":{"id":id,"_links":{"type":"object"}},"required":["id","_links"]}}}),
            );
        }
        Target::Operation => {
            properties["configuration"] = json!({"type":"object"});
            properties["receipt"] = object(
                json!({"state":{"enum":["draft","submitted","completed","rejected","indeterminate"]},"outcomes":{"type":"array","items":{"type":"object"}}}),
            );
        }
        Target::Peers | Target::Calls => {
            properties["_embedded"] = object(json!({"item":{
                "type":"array","maxItems":100,"items":live_row_schema(route.target)
            }}));
        }
        Target::MaintenanceLease => {
            properties["configuration"] = lease_fields();
            properties["meta"] =
                object(json!({"version":{"type":"integer"},"generation":{"type":"integer"}}));
        }
        Target::Operations => properties["boot_id"] = id.clone(),
        Target::Active => properties["active_revision"] = nullable_text.clone(),
        Target::LastKnownGood => properties["last_known_good"] = nullable_text.clone(),
        Target::Desired => properties["desired_revision"] = text,
        Target::Storage => {
            properties["storage"] = text;
            properties["secrets_state"] = nullable_text.clone();
            properties["working_fingerprint"] = nullable_text.clone();
        }
        Target::Credentials(_) => properties["id"] = id.clone(),
        Target::Owners => {
            properties["owners"] =
                json!({"type":"object","additionalProperties":{"type":"integer"}})
        }
        Target::Health | Target::Changes => properties["status"] = json!({"type":"object"}),
        _ => {}
    }
    if route.target == Target::StatusSnapshot {
        properties["snapshot_id"] = id.clone();
        properties["boot_id"] = id.clone();
        properties["state_cursor"] = status_decimal(false);
        properties["monotonic_ms"] = status_decimal(false);
        properties["observed_at"] = json!({"type":["string","null"],"format":"date-time"});
        properties["active_revision"] = nullable_text;
        properties["page_index"] = json!({"type":"integer","minimum":0});
        properties["complete"] = json!({"type":"boolean"});
        properties["selection"] = object(json!({
            "site":{"type":"boolean"},
            "all_ports":{"type":"boolean","const":true},
            "ports":{"type":"array","items":id,"uniqueItems":true},
            "topics":{"type":"array","items":{"enum":["radio","links","phone_patch","telemetry","equipment","resources","diagnostics","meters"]},"uniqueItems":true}
        }));
        properties["status"] = object(json!({
            "site":object(json!({"identity":{"type":"object"},"resources":site_resource_status(),"equipment":{"type":"object"}})),
            "ports":{"type":"object","additionalProperties":object(json!({
                "identity":{"type":"object"},"radio":{"type":"object"},"links":{"type":"object"},
                "radio_signals":{"type":"object","additionalProperties":{"type":"object"}},
                "link_registrations":{"type":"object"},"phone_patch":{"type":"object"},
                "equipment":{"type":"object"},"telemetry":{"type":"object"},"diagnostics":{"type":"object"},
                "graphs":{"type":"object","additionalProperties":{
                    "type":"object","properties":{"media_correlation":status_media_correlation()}
                }},
                "meters":{"type":"object","additionalProperties":status_meter_schema()}
            }))}
        }));
        s["required"] = json!([
            "_links",
            "snapshot_id",
            "boot_id",
            "state_cursor",
            "monotonic_ms",
            "observed_at",
            "active_revision",
            "page_index",
            "complete",
            "selection",
            "status"
        ]);
    } else {
        s["required"] = json!(["_links"]);
    }
    s["properties"] = properties;
    let mut defs = serde_json::Map::new();
    dependencies(&s, &mut defs);
    if route.view != Some(View::Effective) {
        sparse_bindings(&mut defs);
    }
    if !defs.is_empty() {
        s["$defs"] = Value::Object(defs);
    }
    s
}

/// Rooted schema with only transitively used reusable definitions.
pub(super) fn document(route: &Route, create: bool) -> Value {
    let mut schema = match route.target {
        target if target.is_webrtc() => webrtc_request(target),
        Target::MaintenanceLease => object(json!({"configuration":lease_fields()})),
        Target::Operation => {
            let mut cfg = object(
                json!({"submitted":{"type":"boolean"},"targets":ids(),"maintenance_lease":{"type":"string","format":"uri-reference"},"expected_active_revision":{"type":"string"},"desired_etag":{"type":"string"},
                "working_fingerprint":{"type":"string","pattern":"^[0-9a-f]{64}$"},"restore":{"enum":["desired","last_known_good"]},
                "action":{"oneOf":[{"$ref":"#/$defs/controllerAction"},{"type":"object","properties":{"action":{"enum":["apply_configuration","reload_configuration","restore_configuration"]}},"required":["action"],"additionalProperties":false}]}}),
            );
            cfg["properties"]["action"]["properties"] = json!({"parameter":{"type":"string","writeOnly":true},"digits":{"type":"string","writeOnly":true},"digit":{"type":"string","writeOnly":true}});
            cfg["description"] = json!(
                "Drafts may omit fields until submission. The live management source and owner restrict submitted actions."
            );
            object(json!({"configuration":cfg}))
        }
        Target::Collection(_) if !create => object(
            json!({"configuration":{"type":"object","properties":{"members":ids()},"required":["members"],"additionalProperties":false}}),
        ),
        Target::Member(k) | Target::Section(k) | Target::Collection(k) => {
            resource(k, route, create)
        }
        Target::Metadata => resource(Kind::Metadata, route, false),
        Target::Credentials(k) => {
            let mut properties = json!({});
            for (path, _) in super::authoring::secrets(k) {
                let tokens = path
                    .trim_start_matches("/configuration/")
                    .split('/')
                    .collect::<Vec<_>>();
                let private = json!({"type":"string","writeOnly":true});
                match tokens.as_slice() {
                    [key] => properties[*key] = private,
                    [parent, key] => {
                        if properties.get(*parent).is_none() {
                            properties[*parent] = object(json!({}));
                        }
                        properties[*parent]["properties"][*key] = private;
                    }
                    _ => unreachable!("documented credential paths"),
                }
            }
            object(json!({"configuration":object(properties)}))
        }
        _ => json!({"type":"object","readOnly":true}),
    };
    schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    schema["title"] = json!(name(route, create));
    if route.methods.contains(&"PUT")
        && route.view != Some(View::Effective)
        && !matches!(route.target,Target::Member(Kind::Event)|Target::Collection(Kind::Event) if create)
        && schema["properties"].get("configuration").is_some()
    {
        schema["required"] = json!(["configuration"]);
    }
    if create && schema["properties"].get("key").is_some() {
        schema["required"] = if schema["properties"].get("configuration").is_some() {
            json!(["key", "configuration"])
        } else {
            json!(["key"])
        };
    }
    let mut defs = serde_json::Map::new();
    dependencies(&schema, &mut defs);
    sparse_bindings(&mut defs);
    if !defs.is_empty() {
        schema["$defs"] = Value::Object(defs);
    }
    schema
}

fn resource(kind: Kind, route: &Route, create: bool) -> Value {
    let cfg = field_schema(
        kind,
        kind == Kind::Metadata && (create || route.path.contains("{port}")),
    );
    let mut s = if kind == Kind::Event {
        cfg
    } else {
        object(json!({"configuration":cfg}))
    };
    if create && (kind == Kind::Metadata || kind.is_named()) {
        s["properties"]["key"] = json!({"type":"string"});
    }
    s
}

/// Sparse declarations inherit binding companions; effective RX retains its required code.
fn sparse_bindings(defs: &mut serde_json::Map<String, Value>) {
    if let Some(squelch) = defs.get_mut("squelch") {
        for clause in squelch["allOf"]
            .as_array_mut()
            .expect("squelch source rules")
        {
            clause["then"]
                .as_object_mut()
                .expect("source constraint")
                .remove("required");
        }
    }
}

fn dependencies(schema: &Value, out: &mut serde_json::Map<String, Value>) {
    match schema {
        Value::Object(fields) => {
            if let Some(name) = fields
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("#/$defs/"))
                && !out.contains_key(name)
            {
                let child = &definitions()[name];
                if !child.is_null() {
                    out.insert(name.to_owned(), child.clone());
                    dependencies(child, out);
                }
            }
            for v in fields.values() {
                dependencies(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                dependencies(v, out);
            }
        }
        _ => {}
    }
}

pub(super) fn patch() -> Value {
    let pointer = json!({"type":"string","pattern":"^(|/.*)$"});
    let variants = [
        ("add", true, false),
        ("replace", true, false),
        ("test", true, false),
        ("remove", false, false),
        ("move", false, true),
        ("copy", false, true),
    ]
    .into_iter()
    .map(|(op, value, from)| {
        let mut fields = json!({"op":{"const":op},"path":pointer});
        let mut required = vec!["op", "path"];
        if value {
            fields["value"] = json!({});
            required.push("value");
        }
        if from {
            fields["from"] = pointer.clone();
            required.push("from");
        }
        json!({"type":"object","properties":fields,"required":required})
    })
    .collect::<Vec<_>>();
    json!({"type":"array","maxItems":512,"items":{"oneOf":variants}})
}
