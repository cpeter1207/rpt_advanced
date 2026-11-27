//! Retrieve and assemble one bounded HAL status capture.
use crate::{
    Error, Result,
    http::{Client, HttpStatus},
};
use serde_json::{Value, map::Entry};
use std::collections::HashSet;
use url::Url;

const MAX_PAGES: usize = 64;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const CAPTURE: [&str; 7] = [
    "boot_id",
    "snapshot_id",
    "state_cursor",
    "selection",
    "active_revision",
    "observed_at",
    "monotonic_ms",
];

fn selectors<'a>(mut args: &[&'a str]) -> Result<Vec<(&'static str, &'a str)>> {
    let mut pairs = Vec::new();
    let (mut site, mut topics) = (false, false);
    loop {
        args = match args {
            [] => return Ok(pairs),
            ["--site", tail @ ..] if !site => {
                site = true;
                pairs.push(("site", "true"));
                tail
            }
            ["--port", id, tail @ ..] => {
                pairs.push(("port", id));
                tail
            }
            ["--topics", list, tail @ ..] if !topics => {
                topics = true;
                pairs.push(("topics", list));
                tail
            }
            _ => {
                return Err(
                    "usage: status snapshot [--site] [--port ID]... [--topics LIST]".into(),
                );
            }
        };
    }
}

fn is_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn is_text_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(is_text))
}

fn is_decimal(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.bytes().all(|byte| byte.is_ascii_digit()) && text.parse::<u64>().is_ok()
    })
}

fn next_url(client: &Client, page: &Value, index: usize) -> Result<Option<Url>> {
    if page["page_index"].as_u64() != Some(index as u64) {
        return Err("invalid snapshot page index".into());
    }
    if !["boot_id", "snapshot_id"]
        .iter()
        .all(|key| is_text(&page[*key]))
        || !["state_cursor", "monotonic_ms"]
            .iter()
            .all(|key| is_decimal(&page[*key]))
        || ["active_revision", "observed_at"].iter().any(|key| {
            page.get(*key)
                .is_none_or(|value| !value.is_null() && !is_text(value))
        })
        || !page["selection"]["site"].is_boolean()
        || !is_text_array(&page["selection"]["ports"])
        || !is_text_array(&page["selection"]["topics"])
        || !page["status"].is_object()
        || ["site", "ports"].iter().any(|key| {
            page["status"]
                .get(*key)
                .is_some_and(|value| !value.is_object())
        })
    {
        return Err("invalid snapshot metadata or status".into());
    }
    let links = page["_links"].as_object().ok_or("missing snapshot links")?;
    if index == 0 {
        client.follow(page, "self")?;
    }
    match (page["complete"].as_bool(), links.contains_key("next")) {
        (Some(true), false) => Ok(None),
        (Some(false), true) => client.follow(page, "next").map(Some),
        (Some(false), false) => Err("missing snapshot continuation".into()),
        _ => Err("invalid snapshot completion flag".into()),
    }
}

fn merge(target: &mut Value, incoming: Value) -> Result<()> {
    match (target, incoming) {
        (Value::Object(target), Value::Object(incoming)) => {
            for (key, value) in incoming {
                match target.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    Entry::Occupied(mut entry) => merge(entry.get_mut(), value)?,
                }
            }
            Ok(())
        }
        (target, incoming) if *target == incoming => Ok(()),
        _ => Err("conflicting snapshot record".into()),
    }
}

/// Follow the server-selected capture and return one complete JSON document.
pub(crate) fn read(client: &Client, args: &[&str]) -> Result<Value> {
    let selectors = selectors(args)?;
    let status = client.get(&client.target(Some("rpt:status"))?)?;
    let mut url = client.follow(&status.body, "rpt:status-snapshot")?;
    if !selectors.is_empty() {
        url.query_pairs_mut().extend_pairs(selectors);
    }

    let mut seen = HashSet::new();
    let mut bytes = 0;
    let mut capture = Value::Null;
    for index in 0..MAX_PAGES {
        if !seen.insert(url.clone()) {
            return Err("cyclic snapshot continuation".into());
        }
        let reply = client.get(&url).map_err(|error| {
            if error
                .downcast_ref::<HttpStatus>()
                .is_some_and(|code| code.0 == 410)
            {
                Error::from("snapshot expired (HTTP 410)")
            } else {
                error
            }
        })?;
        bytes += reply.body_bytes;
        if bytes > MAX_BYTES {
            return Err("snapshot exceeds the total response size limit".into());
        }
        let mut page = reply.body;
        let next = next_url(client, &page, index)?;
        if index == 0 {
            capture = page;
        } else {
            if CAPTURE.iter().any(|key| page[*key] != capture[*key]) {
                return Err("snapshot page belongs to a different capture".into());
            }
            merge(&mut capture["status"], page["status"].take())?;
        }
        let Some(next) = next else {
            capture["complete"] = Value::Bool(true);
            capture["_links"]
                .as_object_mut()
                .expect("checked links")
                .remove("next");
            return Ok(capture);
        };
        url = next;
    }
    Err("snapshot exceeds the page limit".into())
}
