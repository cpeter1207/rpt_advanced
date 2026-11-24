//! Public station bindings survive real authenticated authoring, inheritance and readback.
use super::*;

fn source(doc: &Value, path: &str, href: &str) {
    assert_eq!(doc["meta"]["sources"][path]["href"], href, "{path}");
}

pub(super) fn keyboard(client: &Client<'_>) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let interface = format!("{port}interface/");
    let site_rx = format!("{SITE_PATH}rx/signaling/");
    let rx = format!("{port}rx/signaling/");
    put(
        client,
        &site_interface,
        json!({"keyboard":{"device":"/dev/input/by-id/site-event-kbd"}}),
    );
    put(
        client,
        &site_rx,
        json!({"squelch":{"source":"key_down","scancode":42}}),
    );
    assert_eq!(
        client.get(&site_interface).body["configuration"]["keyboard"]["device"],
        "/dev/input/by-id/site-event-kbd"
    );
    let (inherited, _) = checked_schema(
        client,
        "/effective/ports/{port}/interface/",
        &effective(&interface),
    );
    assert_eq!(
        inherited["effective"]["keyboard"]["device"],
        "/dev/input/by-id/site-event-kbd"
    );
    source(&inherited, "/keyboard/device", &site_interface);
    let (inherited, _) = checked_schema(
        client,
        "/effective/ports/{port}/rx/signaling/",
        &effective(&rx),
    );
    assert_eq!(inherited["effective"]["squelch"]["scancode"], 42);
    source(&inherited, "/squelch/scancode", &site_rx);
    assert_eq!(
        client.get("/api/v1/schemas/interface/").body["properties"]["configuration"]["properties"]
            ["keyboard"]["properties"]["device"]["minLength"],
        1
    );

    put(
        client,
        &interface,
        json!({"keyboard":{"device":"/dev/input/by-id/local-event-kbd"}}),
    );
    edit(
        client,
        "PATCH",
        &rx,
        json!([{"op":"add","path":"/configuration/squelch","value":{"scancode":57}}]),
    );
    assert_eq!(
        client.get(&rx).body["configuration"],
        json!({"squelch":{"scancode":57}})
    );
    let doc = client.get(&effective(&rx)).body;
    assert_eq!(doc["effective"]["squelch"]["source"], "key_down");
    assert_eq!(doc["effective"]["squelch"]["scancode"], 57);
    source(&doc, "/squelch/source", &site_rx);
    source(&doc, "/squelch/scancode", &rx);
    let doc = client.get(&effective(&interface)).body;
    assert_eq!(
        doc["effective"]["keyboard"]["device"],
        "/dev/input/by-id/local-event-kbd"
    );
    source(&doc, "/keyboard/device", &interface);
    edit(
        client,
        "PATCH",
        &interface,
        json!([{"op":"remove","path":"/configuration/keyboard/device"}]),
    );
    let doc = client.get(&effective(&interface)).body;
    assert_eq!(
        doc["effective"]["keyboard"]["device"],
        "/dev/input/by-id/site-event-kbd"
    );
    source(&doc, "/keyboard/device", &site_interface);
    edit(
        client,
        "PATCH",
        &rx,
        json!([{"op":"add","path":"/configuration/squelch/source","value":"dsp"}]),
    );
    let (doc, _) = checked_schema(
        client,
        "/effective/ports/{port}/rx/signaling/",
        &effective(&rx),
    );
    assert_eq!(doc["effective"]["squelch"]["source"], "dsp");
    assert!(doc["effective"]["squelch"].get("scancode").is_none());
    assert!(doc["meta"]["sources"].get("/squelch/scancode").is_none());
    assert_eq!(
        client.get(&rx).body["configuration"],
        json!({"squelch":{"source":"dsp"}})
    );
}

pub(super) fn serial(client: &Client<'_>) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let interface = format!("{port}interface/");
    let site_rx = format!("{SITE_PATH}rx/signaling/");
    let rx = format!("{port}rx/signaling/");
    let site_tx = format!("{SITE_PATH}tx/signaling/");
    let tx = format!("{port}tx/signaling/");
    put(
        client,
        &site_interface,
        json!({"serial":{"device":"/dev/ttyUSB-station","data_bits":5,"stop_bits":1.5}}),
    );
    put(
        client,
        &site_rx,
        json!({"squelch":{"source":"serial","pin":["CTS","DSR"]},"ctcss":{"enabled":true,"source":"serial","pin":["DCD","RI"]}}),
    );
    put(
        client,
        &site_tx,
        json!({"ptt":{"source":"serial","pin":["RTS","DTR"]}}),
    );
    let (doc, _) = checked_schema(
        client,
        "/effective/ports/{port}/rx/signaling/",
        &effective(&rx),
    );
    for (name, pins) in [
        ("squelch", json!(["CTS", "DSR"])),
        ("ctcss", json!(["DCD", "RI"])),
    ] {
        assert_eq!(doc["effective"][name]["pin"], pins);
        source(&doc, &format!("/{name}/pin"), &site_rx);
    }
    let (doc, _) = checked_schema(
        client,
        "/effective/ports/{port}/tx/signaling/",
        &effective(&tx),
    );
    assert_eq!(doc["effective"]["ptt"]["pin"], json!(["RTS", "DTR"]));
    source(&doc, "/ptt/pin", &site_tx);
    for encoder in ["ctcss", "dcs"] {
        let mut fields = json!({"ptt":{"source":"parallel","pin":[2]}});
        fields[encoder] = json!({"enabled":true,"source":"external","output":{"source":"serial","pin":["RTS","DTR"]}});
        put(client, &tx, fields);
        let (doc, _) = checked_schema(
            client,
            "/effective/ports/{port}/tx/signaling/",
            &effective(&tx),
        );
        assert_eq!(
            doc["effective"][encoder]["output"]["pin"],
            json!(["RTS", "DTR"])
        );
        source(&doc, &format!("/{encoder}/output/pin"), &tx);
    }
    edit(
        client,
        "PATCH",
        &rx,
        json!([{"op":"add","path":"/configuration/squelch","value":{"pin":["CTS"]}}]),
    );
    let doc = client.get(&effective(&rx)).body;
    assert_eq!(doc["effective"]["squelch"]["pin"], json!(["CTS"]));
    source(&doc, "/squelch/pin", &rx);
    put(client, &interface, json!({"serial":{"data_bits":8}}));
    let (doc, _) = checked_schema(
        client,
        "/effective/ports/{port}/interface/",
        &effective(&interface),
    );
    // The admitted pair retains both compatible authored site values.
    assert_eq!(doc["effective"]["serial"]["data_bits"], 5);
    assert_eq!(doc["effective"]["serial"]["stop_bits"], 1.5);
    source(&doc, "/serial/data_bits", &site_interface);
    source(&doc, "/serial/stop_bits", &site_interface);
    edit(
        client,
        "PATCH",
        &interface,
        json!([{"op":"add","path":"/configuration/serial/stop_bits","value":2}]),
    );
    let doc = client.get(&effective(&interface)).body;
    assert_eq!(doc["effective"]["serial"]["data_bits"], 8);
    assert_eq!(doc["effective"]["serial"]["stop_bits"], 2.0);
    source(&doc, "/serial/data_bits", &interface);
    source(&doc, "/serial/stop_bits", &interface);
}

pub(super) fn invalid(client: &Client<'_>, rt: &RuntimeFixture) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let interface = format!("{port}interface/");
    let site_rx = format!("{SITE_PATH}rx/signaling/");
    let rx = format!("{port}rx/signaling/");
    put(
        client,
        &site_interface,
        json!({"keyboard":{"device":"/dev/input/by-id/site-event-kbd"}}),
    );
    put(
        client,
        &site_rx,
        json!({"squelch":{"source":"key_down","scancode":767}}),
    );
    let count = rt.warnings.load(Ordering::Acquire);
    put(
        client,
        &interface,
        json!({"keyboard":{"device":"","future":true}}),
    );
    assert!(rt.warnings.load(Ordering::Acquire) > count);
    assert_eq!(
        client.get(&interface).body["configuration"],
        json!({"keyboard":{}})
    );
    for value in [json!(0), json!(768), json!(4294967295u32), json!("42")] {
        let count = rt.warnings.load(Ordering::Acquire);
        put(
            client,
            &rx,
            json!({"squelch":{"scancode":value,"level":600}}),
        );
        assert!(rt.warnings.load(Ordering::Acquire) > count);
        let (local, _) = checked_schema(client, "/configuration/ports/{port}/rx/signaling/", &rx);
        assert_eq!(local["configuration"], json!({"squelch":{"level":600}}));
        let (doc, _) = checked_schema(
            client,
            "/effective/ports/{port}/rx/signaling/",
            &effective(&rx),
        );
        assert_eq!(doc["effective"]["squelch"]["scancode"], 767);
        assert_eq!(doc["effective"]["squelch"]["level"], 600);
        source(&doc, "/squelch/scancode", &site_rx);
        source(&doc, "/squelch/level", &rx);
    }
    let site_tx = format!("{SITE_PATH}tx/signaling/");
    let tx = format!("{port}tx/signaling/");
    put(
        client,
        &site_interface,
        json!({
            "keyboard":{"device":"/dev/input/by-id/site-event-kbd"},
            "serial":{"device":"/dev/ttyUSB-station"}
        }),
    );
    put(
        client,
        &site_tx,
        json!({"ptt":{"source":"serial","pin":["RTS"]}}),
    );
    let before = rt.warnings.load(Ordering::Acquire);
    put(
        client,
        &tx,
        json!({"ptt":{
            "source":"key_down","scancode":57,"timing":{"settle_ms":77}
        }}),
    );
    assert!(rt.warnings.load(Ordering::Acquire) > before);
    assert_eq!(
        client.get(&tx).body["configuration"]["ptt"],
        json!({"timing":{"settle_ms":77}}),
        "obsolete keyboard PTT fields must be ignored in authored readback"
    );
    let (doc, schema) = checked_schema(
        client,
        "/effective/ports/{port}/tx/signaling/",
        &effective(&tx),
    );
    assert_eq!(doc["effective"]["ptt"]["source"], "serial");
    assert_eq!(doc["effective"]["ptt"]["pin"], json!(["RTS"]));
    assert_eq!(doc["effective"]["ptt"]["timing"]["settle_ms"], 77);
    source(&doc, "/ptt/source", &site_tx);
    source(&doc, "/ptt/pin", &site_tx);
    source(&doc, "/ptt/timing/settle_ms", &tx);
    assert!(doc["effective"]["ptt"].get("scancode").is_none());
    let properties = &schema["$defs"]["ptt"]["properties"];
    assert!(
        properties["source"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v != "key_down")
    );
    assert!(properties.get("scancode").is_none());
    let rx = client.get(&effective(&rx)).body;
    assert_eq!(rx["effective"]["squelch"]["source"], "key_down");
    assert_eq!(rx["effective"]["squelch"]["scancode"], 767);
    put(client, &tx, json!({"ptt":{"source":"none"}}));
    let doc = client.get(&effective(&tx)).body;
    assert_eq!(doc["effective"]["ptt"]["source"], "none");
    source(&doc, "/ptt/source", &tx);
    assert!(doc["effective"]["ptt"].get("pin").is_none());
    assert!(
        properties["source"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "none")
    );
}

pub(super) fn sparse_keyboard(client: &Client<'_>, rt: &RuntimeFixture) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let site_rx = format!("{SITE_PATH}rx/signaling/");
    let rx = format!("{port}rx/signaling/");
    put(
        client,
        &site_interface,
        json!({"keyboard":{"device":"/dev/input/by-id/site-event-kbd"}}),
    );
    put(
        client,
        &site_rx,
        json!({"squelch":{"source":"key_down","scancode":42}}),
    );
    let request = json!({"configuration":{"squelch":{"source":"key_down"}}});
    edit(client, "PUT", &rx, request.clone());
    let request_schema = client.get("/api/v1/schemas/rx-signaling/").body;
    validate_document(&request_schema, &request);
    let (local, _) = checked_schema(client, "/configuration/ports/{port}/rx/signaling/", &rx);
    assert_eq!(local["configuration"], request["configuration"]);
    put(
        client,
        &rx,
        json!({"squelch":{"source":"key_down","scancode":57}}),
    );
    assert_eq!(
        client.get(&rx).body["configuration"]["squelch"]["scancode"],
        57
    );
    for (method, body) in [
        (
            "PATCH",
            json!([{"op":"remove","path":"/configuration/squelch/scancode"}]),
        ),
        (
            "PUT",
            json!({"configuration":{"squelch":{"source":"key_down","scancode":0}}}),
        ),
    ] {
        let count = rt.warnings.load(Ordering::Acquire);
        edit(client, method, &rx, body);
        if method == "PUT" {
            assert!(rt.warnings.load(Ordering::Acquire) > count);
        }
        let (local, _) = checked_schema(client, "/configuration/ports/{port}/rx/signaling/", &rx);
        assert_eq!(local["configuration"], request["configuration"]);
        validate_document(
            &request_schema,
            &json!({"configuration":local["configuration"]}),
        );
        let (doc, _) = checked_schema(
            client,
            "/effective/ports/{port}/rx/signaling/",
            &effective(&rx),
        );
        assert_eq!(doc["effective"]["squelch"]["scancode"], 42);
        source(&doc, "/squelch/scancode", &site_rx);
        source(&doc, "/squelch/source", &rx);
    }
}

pub(super) fn invalid_serial(client: &Client<'_>, rt: &RuntimeFixture) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let site_rx = format!("{SITE_PATH}rx/signaling/");
    let rx = format!("{port}rx/signaling/");
    let site_tx = format!("{SITE_PATH}tx/signaling/");
    let tx = format!("{port}tx/signaling/");
    put(
        client,
        &site_interface,
        json!({"serial":{"device":"/dev/ttyUSB-station"}}),
    );
    put(
        client,
        &site_rx,
        json!({"squelch":{"source":"serial","pin":["CTS"]}}),
    );
    for explicit in [true, false] {
        let count = rt.warnings.load(Ordering::Acquire);
        let fields = if explicit {
            json!({"squelch":{"source":"serial","pin":[1],"level":680}})
        } else {
            json!({"squelch":{"pin":[1],"level":680}})
        };
        put(client, &rx, fields);
        assert!(rt.warnings.load(Ordering::Acquire) > count);
        let (local, _) = checked_schema(client, "/configuration/ports/{port}/rx/signaling/", &rx);
        let expected = if explicit {
            json!({"squelch":{"source":"serial","level":680}})
        } else {
            json!({"squelch":{"level":680}})
        };
        assert_eq!(local["configuration"], expected);
        let doc = client.get(&effective(&rx)).body;
        assert_eq!(doc["effective"]["squelch"]["pin"], json!(["CTS"]));
        assert_eq!(doc["effective"]["squelch"]["level"], 680);
        source(&doc, "/squelch/pin", &site_rx);
    }
    for encoder in ["ctcss", "dcs"] {
        let mut fields = json!({});
        fields[encoder] =
            json!({"enabled":true,"source":"external","output":{"source":"serial","pin":["RTS"]}});
        put(client, &site_tx, fields);
        for explicit in [true, false] {
            let count = rt.warnings.load(Ordering::Acquire);
            let output = if explicit {
                json!({"source":"serial","pin":["CTS"]})
            } else {
                json!({"pin":["CTS"]})
            };
            let mut fields = json!({});
            fields[encoder] = json!({"source":"external","output":output,"level_dbfs":-21});
            put(client, &tx, fields);
            assert!(rt.warnings.load(Ordering::Acquire) > count);
            let (local, _) =
                checked_schema(client, "/configuration/ports/{port}/tx/signaling/", &tx);
            let output = if explicit {
                json!({"source":"serial"})
            } else {
                json!({})
            };
            assert_eq!(
                local["configuration"][encoder],
                json!({"source":"external","output":output,"level_dbfs":-21})
            );
            let (doc, _) = checked_schema(
                client,
                "/effective/ports/{port}/tx/signaling/",
                &effective(&tx),
            );
            assert_eq!(doc["effective"][encoder]["output"]["pin"], json!(["RTS"]));
            assert_eq!(doc["effective"][encoder]["level_dbfs"], -21.0);
            source(&doc, &format!("/{encoder}/output/pin"), &site_tx);
        }
    }
}

pub(super) fn source_fallback(client: &Client<'_>, rt: &RuntimeFixture) {
    let port = port(client);
    let site_interface = format!("{SITE_PATH}interface/");
    let site_tx = format!("{SITE_PATH}tx/signaling/");
    let tx = format!("{port}tx/signaling/");
    put(
        client,
        &site_interface,
        json!({"gpio":{"wiring_profile":"dudeusb"}}),
    );
    put(
        client,
        &site_tx,
        json!({"ptt":{"source":"serial","pin":["RTS"]}}),
    );
    put(client, &tx, json!({"ptt":{"pin":[3]}}));
    let (local, _) = checked_schema(client, "/configuration/ports/{port}/tx/signaling/", &tx);
    assert_eq!(local["configuration"], json!({"ptt":{"pin":[3]}}));
    let resource = local["id"].as_str().unwrap().parse().unwrap();
    let (doc, _) = checked_schema(
        client,
        "/effective/ports/{port}/tx/signaling/",
        &effective(&tx),
    );
    assert_eq!(doc["effective"]["ptt"]["source"], "sound_chip_gpio");
    assert_eq!(doc["effective"]["ptt"]["pin"], json!([3]));
    source(&doc, "/ptt/pin", &tx);
    assert!(!rt.resource_warnings.lock().unwrap().iter().any(|warning| {
        warning.resource.id == resource
            && warning.warning
                == rpt_advanced_core::config::resources::ResourceWarning::InvalidValue("pin")
    }));
}
