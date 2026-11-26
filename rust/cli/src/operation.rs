//! Submit a single typed operation and keep the receipt identity across transport failures.
use crate::{
    Error, Result,
    http::{Client, Reply},
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    time::{Duration, Instant},
};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) fn request_id(id: Option<&str>) -> Result<Uuid> {
    if let Some(id) = id {
        return Ok(Uuid::parse_str(id)?);
    }
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(uuid::Builder::from_random_bytes(bytes).into_uuid())
}

fn receipt_url(client: &Client, request: Uuid) -> Result<Url> {
    let ops = client.get(&client.target(Some("rpt:operations"))?)?.body;
    let template = &ops["_links"]["rpt:request"];
    let href = template["href"].as_str().ok_or("missing request link")?;
    if template["templated"] != true || href.matches("{request}").count() != 1 {
        return Err("invalid request template".into());
    }
    client.href(&href.replace("{request}", &request.to_string()))
}

/// Poll just this receipt. A rejected or indeterminate operation is an unsuccessful command.
pub fn wait(client: &Client, url: &Url, reply: Reply) -> Result<Reply> {
    wait_with(client, url, reply, || Ok(()))
}

pub(crate) fn wait_with(
    client: &Client,
    url: &Url,
    mut reply: Reply,
    mut on_pending: impl FnMut() -> Result<()>,
) -> Result<Reply> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match reply.body["receipt"]["state"].as_str() {
            Some("completed") => return Ok(reply),
            Some("rejected" | "indeterminate") => {
                println!("{}", reply.body);
                return Err("operation did not complete successfully".into());
            }
            Some("submitted") if Instant::now() < deadline => {
                on_pending()?;
                std::thread::sleep(Duration::from_millis(150));
                reply = client.get(url)?;
            }
            Some("submitted") => return Err("operation still pending; inspect its receipt".into()),
            Some("draft") => return Err("operation is still a draft".into()),
            _ => return Err("invalid operation receipt".into()),
        }
    }
}

/// Create or submit one request from the actor's advertised template; do not repeat writes.
pub fn submit(client: &Client, intent: &[u8], request: Option<&str>) -> Result<Reply> {
    let (url, reply) = create(client, intent, request)?;
    wait(client, &url, reply)
}

pub(crate) fn create(
    client: &Client,
    intent: &[u8],
    request: Option<&str>,
) -> Result<(Url, Reply)> {
    let url = receipt_url(client, request_id(request)?)?;
    eprintln!("Receipt: {}", url.path());
    let reply = write_receipt(client, &url, "PUT", intent, ("If-None-Match", "*"))?;
    Ok((url, reply))
}

pub(crate) fn write_receipt(
    client: &Client,
    url: &Url,
    method: &str,
    intent: &[u8],
    condition: (&str, &str),
) -> Result<Reply> {
    match client.request(method, url, Some(intent), Some(condition)) {
        Err(lost)
            if lost.is::<crate::http::TransportFailure>()
                || lost
                    .downcast_ref::<crate::http::HttpStatus>()
                    .is_some_and(|status| status.0 >= 500) =>
        {
            client
                .get(url)
                .map_err(|lookup| Error::from(format!("{lost}; receipt lookup: {lookup}")))
        }
        other => other,
    }
}

/// Capture the same desired and working preconditions required by the coordinator.
pub fn recovery(
    client: &Client,
    action: &str,
    restore: Option<&str>,
    id: Option<&str>,
) -> Result<Reply> {
    let state = client.get(&client.target(Some("rpt:status"))?)?.body;
    let desired = client.get(&client.follow(&state, "rpt:desired")?)?;
    let tag = desired.etag.ok_or("missing desired validator")?;
    let mut configuration = json!({"submitted":true,"action":{"action":format!("{action}_configuration")},"desired_etag":tag});
    if action != "apply" {
        let storage = client.get(&client.follow(&state, "rpt:storage")?)?.body;
        configuration["working_fingerprint"] = Value::from(
            storage["working_fingerprint"]
                .as_str()
                .ok_or("missing working fingerprint")?,
        );
    }
    if let Some(which) = restore {
        if !matches!(which, "desired" | "last_known_good") {
            return Err(Error::from("invalid restore selection"));
        }
        configuration["restore"] = which.into();
    }
    let body = Zeroizing::new(serde_json::to_vec(&json!({"configuration":configuration}))?);
    submit(client, &body, id)
}
