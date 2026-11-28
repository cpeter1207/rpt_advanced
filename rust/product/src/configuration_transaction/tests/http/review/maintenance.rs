//! The actual authenticated HAL advertises connected, boot-scoped tuning ownership.
use super::*;

pub(super) fn read(client: &Client<'_>) {
    let port = port(client);
    let member = client.get(&port);
    assert_eq!(member.code, 200);
    let href = member.body["_links"]["rpt:maintenance"]["href"]
        .as_str()
        .expect("a port must advertise its actual maintenance lease collection");
    assert!(href.starts_with("/api/v1/maintenance/ports/"));
    let collection = client.get(href);
    assert_eq!(collection.code, 200);
    assert_eq!(collection.body["_links"]["item"], json!([]));
    assert_eq!(collection.body["_links"]["rpt:lease"]["templated"], true);
    let template = collection.body["_links"]["rpt:lease"]["href"]
        .as_str()
        .unwrap();
    assert!(template.starts_with(href) && template.ends_with("/{lease}/"));
}
