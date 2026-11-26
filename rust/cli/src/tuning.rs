//! One authenticated HTTP connection owns each short-lived radio tuning lease.
use crate::{
    Result,
    http::{Client, Reply},
    operation,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read},
    sync::mpsc::{RecvTimeoutError, sync_channel},
    time::{Duration, Instant},
};
use url::Url;
use uuid::Uuid;

const RENEW: Duration = Duration::from_secs(5);
const LINE_LIMIT: usize = 8192;

struct Lease<'a> {
    client: &'a Client,
    href: Url,
    port: Uuid,
    revision: String,
    body: Vec<u8>,
    etag: String,
    renew_at: Instant,
}

impl<'a> Lease<'a> {
    fn acquire(client: &'a Client, port_href: &str) -> Result<Self> {
        let port = client.get(&client.target(Some(port_href))?)?.body;
        let id = Uuid::parse_str(port["id"].as_str().ok_or("missing port identity")?)?;
        let status = client.get(&client.target(Some("rpt:status"))?)?.body;
        let revision = status["status"]["active_revision"]
            .as_str()
            .ok_or("no active revision")?
            .to_owned();
        let collection = client.get(&client.follow(&port, "rpt:maintenance")?)?.body;
        let template = &collection["_links"]["rpt:lease"];
        let pattern = template["href"]
            .as_str()
            .ok_or("missing maintenance lease link")?;
        if template["templated"] != true || pattern.matches("{lease}").count() != 1 {
            return Err("invalid maintenance lease template".into());
        }
        let href =
            client.href(&pattern.replace("{lease}", &operation::request_id(None)?.to_string()))?;
        let body = serde_json::to_vec(
            &json!({"configuration":{"active":true,"expected_active_revision":revision,"ttl_ms":30000}}),
        )?;
        let created = client.request("PUT", &href, Some(&body), Some(("If-None-Match", "*")))?;
        let etag = match client
            .get(&href)
            .and_then(|r| r.etag.ok_or("missing lease validator".into()))
        {
            Ok(tag) => tag,
            Err(e) => {
                if let Some(tag) = created.etag {
                    client.request("DELETE", &href, None, Some(("If-Match", &tag)))?;
                }
                return Err(e);
            }
        };
        Ok(Self {
            client,
            href,
            port: id,
            revision,
            body,
            etag,
            renew_at: Instant::now() + RENEW,
        })
    }

    fn renew_due(&mut self) -> Result<()> {
        if Instant::now() >= self.renew_at {
            self.client.request(
                "PUT",
                &self.href,
                Some(&self.body),
                Some(("If-Match", &self.etag)),
            )?;
            self.etag = self
                .client
                .get(&self.href)?
                .etag
                .ok_or("missing renewed lease validator")?;
            self.renew_at = Instant::now() + RENEW;
        }
        Ok(())
    }

    fn submit(&mut self, line: &str) -> Result<()> {
        let mut action: Value = rptadv_json::decode_value(line.as_bytes())?;
        let fields = action
            .as_object_mut()
            .ok_or("expected a radio maintenance object")?;
        if fields
            .get("action")
            .is_some_and(|v| v != "radio_maintenance")
        {
            return Err("expected radio_maintenance".into());
        }
        fields.insert("action".into(), "radio_maintenance".into());
        let body = serde_json::to_vec(
            &json!({"configuration":{"submitted":true,"targets":[self.port.to_string()],
            "action":action,"expected_active_revision":self.revision,"maintenance_lease":self.href.path()}}),
        )?;
        let client = self.client;
        let (url, receipt) = operation::create(client, &body, None)?;
        let result = operation::wait_with(client, &url, receipt, || self.renew_due())?;
        println!("{}", result.body);
        Ok(())
    }

    fn input_loop(&mut self) -> Result<()> {
        // Wait for operator input independently of the lease renewal deadline. One pending
        // line bounds memory; process exit also closes the connection on interruption.
        let (tx, rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("tuning-input".into())
            .spawn(move || {
                let mut input = io::stdin().lock();
                loop {
                    let mut bytes = Vec::new();
                    match input
                        .by_ref()
                        .take((LINE_LIMIT + 1) as u64)
                        .read_until(b'\n', &mut bytes)
                    {
                        Ok(0) => break,
                        Ok(_) if bytes.len() <= LINE_LIMIT => {
                            if tx.send(Ok(bytes)).is_err() {
                                break;
                            }
                        }
                        _ => {
                            let _ = tx.send(Err("invalid or oversized tuning input"));
                            break;
                        }
                    }
                }
            })?;
        loop {
            self.renew_due()?;
            match rx.recv_timeout(self.renew_at.saturating_duration_since(Instant::now())) {
                Ok(result) => {
                    let bytes = result?;
                    let line = std::str::from_utf8(&bytes)?.trim();
                    match line {
                        "" => continue,
                        "quit" => return Ok(()),
                        _ => self.submit(line)?,
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }
}

/// Release the specific original lease on every normal/error exit of the input loop.
pub fn run(client: &Client, port: &str) -> Result<Reply> {
    let mut lease = Lease::acquire(client, port)?;
    let session = lease.input_loop();
    let released = client.request("DELETE", &lease.href, None, Some(("If-Match", &lease.etag)));
    match (session, released) {
        (Ok(()), result) => result,
        (Err(e), Ok(_)) => Err(e),
        (Err(e), Err(cleanup)) => Err(format!("{e}; lease release: {cleanup}").into()),
    }
}
