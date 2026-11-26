//! Authenticated local operator client for controller HAL resources.
mod http;
mod operation;
mod tuning;
use http::{Client, Reply};
use serde_json::Value;
use std::{env, fs::File, io, process::ExitCode};
use zeroize::Zeroizing;

type Error = Box<dyn std::error::Error>;
type Result<T> = std::result::Result<T, Error>;

const HELP: &str = "Usage: rpt-advanced-cli [--url URL] [--ca FILE] [--cert FILE] [--key FILE] COMMAND
  discover [REL|HREF] | get [REL|HREF] | status [REL]
  put|patch|post REL|HREF --if-match ETAG FILE|-
  delete REL|HREF --if-match ETAG
  operation draft [--request UUID] | operation submit-draft HREF --if-match ETAG
  operation submit FILE|- [--request UUID] | operation show|wait HREF
  apply|reload [--request UUID] | restore desired|last_known_good [--request UUID]
  tune PORT-HREF  (one radio_maintenance JSON object per input line; quit or EOF releases)
Edits use a single HAL resource. Supply a quoted strong ETag as returned by get.
Operation submit takes a server-shaped JSON document with configuration.submitted=true.
Connection defaults: https://localhost:8443/api/v1/ and /etc/rpt_advanced/cli/{server-ca.pem,client.pem,client.key}.";

fn body(input: &str) -> Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(if input == "-" {
        http::bounded(io::stdin().lock())?
    } else {
        http::bounded(File::open(input)?)?
    }))
}

fn report(reply: Reply) -> Value {
    if let Some(etag) = reply.etag {
        eprintln!("ETag: {etag}");
    }
    reply.body
}

fn strong_tag(etag: &str) -> Result<&str> {
    if etag.len() < 2
        || !etag.starts_with('"')
        || !etag.ends_with('"')
        || etag[1..etag.len() - 1]
            .bytes()
            .any(|b| !(0x21..=0x7e).contains(&b) || b == b'"')
    {
        return Err("expected a strong target ETag".into());
    }
    Ok(etag)
}

fn execute(client: &Client, args: &[&str]) -> Result<Value> {
    let reply = match args {
        ["discover", tail @ ..] | ["get", tail @ ..] if tail.len() <= 1 => {
            let r = client.get(&client.target(tail.first().copied())?)?;
            let doc = report(r);
            return Ok(if args[0] == "discover" {
                doc["_links"].clone()
            } else {
                doc
            });
        }
        ["status", tail @ ..] if tail.len() <= 1 => {
            let root = client.get(&client.target(Some("rpt:status"))?)?;
            if let Some(part) = tail.first() {
                let rel = format!("rpt:{}", part.strip_prefix("rpt:").unwrap_or(part));
                client.get(&client.follow(&root.body, &rel)?)?
            } else {
                root
            }
        }
        [
            method @ ("put" | "patch" | "post" | "delete"),
            path,
            "--if-match",
            etag,
            tail @ ..,
        ] => {
            let etag = strong_tag(etag)?;
            let payload = match (*method, tail) {
                ("delete", []) => None,
                ("put" | "post" | "patch", [file]) => Some(body(file)?),
                _ => return Err(HELP.into()),
            };
            client.request(
                &method.to_uppercase(),
                &client.target(Some(path))?,
                payload.as_ref().map(|b| b.as_slice()),
                Some(("If-Match", etag)),
            )?
        }
        ["operation", "submit-draft", path, "--if-match", tag] => {
            let url = client.href(path)?;
            let submit = br#"[{"op":"replace","path":"/configuration/submitted","value":true}]"#;
            let r = operation::write_receipt(
                client,
                &url,
                "PATCH",
                submit,
                ("If-Match", strong_tag(tag)?),
            )?;
            operation::wait(client, &url, r)?
        }
        ["tune", port] => tuning::run(client, port)?,
        ["operation", op @ ("show" | "wait"), path] => {
            let url = client.href(path)?;
            let r = client.get(&url)?;
            if *op == "wait" {
                operation::wait(client, &url, r)?
            } else {
                r
            }
        }
        _ => {
            let (args, request) = match args {
                [prefix @ .., "--request", id] => (prefix, Some(*id)),
                _ => (args, None),
            };
            match args {
                ["apply" | "reload"] => operation::recovery(client, args[0], None, request)?,
                ["restore", which] => operation::recovery(client, "restore", Some(which), request)?,
                ["operation", "draft"] => {
                    operation::create(client, br#"{"configuration":{"submitted":false}}"#, request)?
                        .1
                }
                ["operation", "submit", file] => operation::submit(client, &body(file)?, request)?,
                _ => return Err(HELP.into()),
            }
        }
    };
    Ok(report(reply))
}

fn run() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] || args == ["help"] {
        println!("{HELP}");
        return Ok(());
    }
    let words = args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut remaining = words.as_slice();
    let (mut url, mut ca, mut cert, mut key) = (
        "https://localhost:8443/api/v1/",
        "/etc/rpt_advanced/cli/server-ca.pem",
        "/etc/rpt_advanced/cli/client.pem",
        "/etc/rpt_advanced/cli/client.key",
    );
    while let [flag, value, rest @ ..] = remaining {
        match *flag {
            "--url" => url = value,
            "--ca" => ca = value,
            "--cert" => cert = value,
            "--key" => key = value,
            _ => break,
        }
        remaining = rest;
    }
    if remaining.is_empty() {
        return Err(HELP.into());
    }
    let client = Client::new(url, ca, cert, key)?;
    println!("{}", execute(&client, remaining)?);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rpt-advanced-cli: {error}");
            ExitCode::FAILURE
        }
    }
}
