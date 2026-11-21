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
        Kind::Interface => defs["interface"].clone(),
        Kind::PeerAudio => defs["linkProcessing"].clone(),
        Kind::Control | Kind::Power | Kind::Limits => extracted("radio", kind.as_str()),
        Kind::Memories => object(json!({"entries":extracted("radio","memories")})),
        Kind::ScanLists => extracted("radio", "scan_lists"),
        Kind::Allstarlink => only(
            "allstarlink",
            &[
                "allow_nodes",
                "deny_nodes",
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
        Kind::Telemetry => defs["telemetry"].clone(),
        Kind::PermanentGroup => defs["permanentLink"].clone(),
        _ => defs[kind.as_str()].clone(),
    };
    config["description"] = json!(
        "Recognized public authoring fields. Recoverable invalid values or unknown settings produce warnings and fall back through inheritance."
    );
    config
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
    let nullable_text = json!({"type":["string","null"]});
    let flags = object(
        json!({"password_configured":{"type":"boolean"},"pin_configured":{"type":"boolean"},"unlock_configured":{"type":"boolean"},"lock_configured":{"type":"boolean"}}),
    );
    let mut properties = json!({"_links":links,"meta":object(json!({"configuration_revision":text,"resource_version":text,"request_version":id,"secrets_state":{"enum":["ready","locked","error"]},"credentials":flags}))});
    match route.target {
        Target::Section(_) | Target::Member(_) | Target::Metadata => {
            let kind = match route.target {
                Target::Section(k) | Target::Member(k) => k,
                _ => Kind::Metadata,
            };
            properties["id"] = id;
            properties["key"] = text;
            if route.view == Some(View::Status) {
                properties["status"] = json!({"type":"object"});
            } else {
                let mut cfg = field_schema(kind, route.profile == "port");
                if let Some(props) = cfg.get_mut("properties").and_then(Value::as_object_mut) {
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
        Target::Operations => properties["boot_id"] = id,
        Target::Active => properties["active_revision"] = nullable_text,
        Target::LastKnownGood => properties["last_known_good"] = nullable_text,
        Target::Desired => properties["desired_revision"] = text,
        Target::Storage => {
            properties["storage"] = text;
            properties["secrets_state"] = nullable_text.clone();
            properties["working_fingerprint"] = nullable_text;
        }
        Target::Credentials(_) => properties["id"] = id,
        Target::Owners => {
            properties["owners"] =
                json!({"type":"object","additionalProperties":{"type":"integer"}})
        }
        Target::Health | Target::Changes => properties["status"] = json!({"type":"object"}),
        _ => {}
    }
    s["properties"] = properties;
    s["required"] = json!(["_links"]);
    let mut defs = serde_json::Map::new();
    dependencies(&s, &mut defs);
    if !defs.is_empty() {
        s["$defs"] = Value::Object(defs);
    }
    s
}

/// Rooted schema with only transitively used reusable definitions.
pub(super) fn document(route: &Route, create: bool) -> Value {
    let mut schema = match route.target {
        Target::Operation => {
            let mut cfg = object(
                json!({"submitted":{"type":"boolean"},"targets":ids(),"expected_active_revision":{"type":"string"},"desired_etag":{"type":"string"},
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
