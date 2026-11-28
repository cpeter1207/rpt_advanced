//! Status is joined to the running port, independently of the authored account and its ETag.
use super::*;

pub(super) fn read(client: &Client<'_>, runtime: &RuntimeFixture) {
    let port = port(client);
    let station: ResourceId = port
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let account = edit(
        client,
        "POST",
        &format!("{port}protocols/allstarlink/"),
        json!({"configuration":{"node_number":716701,"iax":{"registration_url":""}}}),
    )
    .headers["location"]
        .clone();
    let public = client.get(&account);
    assert_eq!(public.code, 200);
    let account_status = public.body["_links"]["rpt:status"]["href"]
        .as_str()
        .unwrap();
    assert_ne!(public.body["id"], station.to_string());
    runtime.observed.lock().unwrap().owners.insert(station, 37);
    let port_status = port.replacen("/configuration/", "/status/", 1);
    let station_doc = client.get(&port_status);
    assert_eq!(station_doc.code, 200);
    assert_eq!(station_doc.body["status"]["owner_generation"], 37);
    let selected = client.get(account_status);
    assert_eq!(selected.code, 200);
    assert_eq!(
        selected.body["status"]["active_revision"],
        station_doc.body["status"]["active_revision"]
    );
    assert_eq!(
        selected.body["status"]["owner_generation"], 37,
        "account status must use the generation of its actual port owner"
    );
    assert_eq!(
        selected.body["status"]["registration"]["state"],
        "unavailable"
    );
    for relation in ["rpt:peers", "rpt:calls"] {
        let href = station_doc.body["_links"][relation]["href"]
            .as_str()
            .unwrap();
        let page = client.get(&format!("{href}?offset=0&limit=1"));
        assert_eq!(page.code, 200);
        assert_eq!(page.body["_embedded"]["item"], json!([]));
    }
    runtime.observed.lock().unwrap().owners.remove(&station);
    let withdrawn = client.get(account_status);
    assert_eq!(withdrawn.code, 200);
    assert!(withdrawn.body["status"]["owner_generation"].is_null());
    assert_ne!(withdrawn.headers["etag"], selected.headers["etag"]);
    assert_eq!(client.get(&account).headers["etag"], public.headers["etag"]);
}

pub(super) fn paging(client: &Client<'_>, runtime: &RuntimeFixture) {
    use crate::management::live::{Domain, Peer, Port, Receive};

    let port = port(client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    let station_id = station.parse::<ResourceId>().unwrap();
    let publish = |mut graph: Port, generation| {
        graph.audio_generation = Some(generation);
        let mut observed = runtime.observed.lock().unwrap();
        observed.owners.insert(station_id, generation);
        observed.status_ready = true;
        observed.domain = Arc::new(Domain {
            ports: BTreeMap::from([(station_id, graph)]),
            ..Domain::default()
        });
    };
    let mut populated = Port::default();
    for node in 700_001u64..=700_025 {
        populated.peers.push(Peer {
            kind: "allstarlink",
            node: Some(node.to_string()),
            call_id: None,
            media_binding: None,
            connection: Some(node),
            connection_generation: Some(node.to_string()),
            direction: Some("outbound"),
            mode: None,
            permanent: Some(false),
            group_selected: None,
            state: "connected",
            quality: "valid",
            transmit_keyed: Some(false),
            // Force a single port over the HTTP response budget in the held fixture.
            media: Some(json!({"fixture":"x".repeat(256 * 1024)})),
            receive: Receive {
                muted: false,
                active: false,
            },
        });
    }
    publish(populated, 17);
    let read_as = |path, principal| client.send_as("GET", path, None, None, Some(principal));
    let target = format!("/api/v1/status/snapshot/?port={station}&topics=links,meters");
    assert!(wait_for(client, &target, |body| body["complete"] == false));
    let first = read_as(&target, "first");
    assert_eq!(first.code, 200);
    assert_eq!(first.body["complete"], false);
    let first_port = &first.body["status"]["ports"][station];
    let first_peers = first_port["links"].as_object().unwrap().len();
    let next = first.body["_links"]["next"]["href"].as_str().unwrap();
    assert_eq!(read_as(next, "another").code, 404);
    assert_eq!(client.get(next).code, 404);

    publish(Port::default(), 18);
    put(
        client,
        &port,
        json!({"name":"Renamed Radio","inherit_from":"site","enabled":false}),
    );
    assert!(wait_for(client, &target, |body| {
        body["status"]["ports"][station]["graphs"]["local_rx"]["graph_generation"] == "18"
            && body["status"]["ports"][station].get("links").is_none()
            && body["active_revision"] != first.body["active_revision"]
    }));
    let second = read_as(next, "first");
    assert_eq!(second.code, 200);
    for field in [
        "snapshot_id",
        "observed_at",
        "boot_id",
        "active_revision",
        "state_cursor",
        "selection",
    ] {
        assert_eq!(second.body[field], first.body[field], "{field}");
    }
    assert_eq!(second.body["page_index"], 1);
    assert_eq!(second.body["complete"], true);
    let second_port = &second.body["status"]["ports"][station];
    assert_eq!(second_port["graphs"]["local_rx"]["graph_generation"], "17");
    let remaining_peers = second_port["links"].as_object().unwrap().len();
    assert_eq!(first_peers + remaining_peers, 25);
    let missing = format!("{target}&cursor=361c602c-7664-4600-990d-6bd6cae4b32c&page=1");
    let expired = read_as(&missing, "first");
    assert_eq!(expired.code, 410);
    assert_eq!(expired.body["error"], "expired_snapshot");
    let fresh = expired.body["_links"]["rpt:status-snapshot"]["href"]
        .as_str()
        .unwrap();
    assert_eq!(read_as(fresh, "first").code, 200);
}

/// Gather an observation without unwinding the foreground before its teardown checks.
pub(super) fn wait_for(client: &Client<'_>, path: &str, matches: impl Fn(&Value) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let reply = client.get(path);
        if reply.code == 200 && matches(&reply.body) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}
