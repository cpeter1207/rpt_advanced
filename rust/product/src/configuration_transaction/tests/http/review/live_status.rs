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
