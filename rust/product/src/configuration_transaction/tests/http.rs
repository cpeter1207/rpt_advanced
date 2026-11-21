//! Exercise the actual mTLS admission, HAL coordinator, durable edit guard and owner handoff.
use super::*;
#[path = "../../../../peer-support/tests/support/https_frontend.rs"]
mod frontend;
use crate::management::{
    Adoption, AdoptionOutcome, Coordinator, Effect, Fault, Observed, OperationOwner, Runtime,
};
use rpt_advanced_core::{
    command_language::Invocation,
    config::resources::candidate::{ModularCandidate, ResourceRegistry},
};
use rptadv_peer_support::https::{
    CertificateDer, Listener, OriginPolicy, PrivateKeyDer, ServerIdentity, ServiceId, ServiceTrust,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct RuntimeFixture {
    hold: AtomicBool,
    contend: Mutex<Option<std::path::PathBuf>>,
    release_competitor: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    fail_admission: AtomicBool,
    rollback: AtomicBool,
    remove_key: AtomicBool,
    damage_working: Mutex<Option<std::path::PathBuf>>,
    fail_selector: AtomicBool,
    adoptions: std::sync::atomic::AtomicUsize,
    inhibit_history: Mutex<Vec<(Option<String>, Fault)>>,
    inhibits: std::sync::atomic::AtomicUsize,
    warnings: std::sync::atomic::AtomicUsize,

    terminal: Mutex<Option<(String, std::sync::mpsc::SyncSender<AdoptionOutcome>)>>,
    hold_dispatch: AtomicBool,
    reject_credentials: AtomicBool,
    observed: Mutex<Observed>,
    domain: Mutex<
        Option<(
            rpt_advanced_core::runtime::NodeHost<(), ()>,
            rpt_advanced_core::site::SiteController,
        )>,
    >,
    dispatched: std::sync::atomic::AtomicUsize,
}
impl Runtime for RuntimeFixture {
    type Prepared = String;
    fn prepare(
        &self,
        candidate: &ModularCandidate<'_>,
        r: &ResourceRegistry,
        rev: &str,
    ) -> Result<String, Fault> {
        for (_, id) in r.port_keys() {
            candidate.port_metadata(id).ok_or(Fault::Invalid)?;
        }
        Ok(rev.to_owned())
    }
    fn prepare_credentials(
        &self,
        _: &mut String,
        _: &crate::configuration_private::Credentials,
    ) -> Result<(), Fault> {
        if self.reject_credentials.swap(false, Ordering::AcqRel) {
            Err(Fault::Conflict)
        } else {
            Ok(())
        }
    }
    fn admission_rename_observed(&self) -> bool {
        !self.fail_admission.swap(false, Ordering::AcqRel)
    }
    fn after_publication(&self) {
        if let Some(path) = self.contend.lock().unwrap().take() {
            let (acquired, wait) = std::sync::mpsc::channel();
            let (release, hold) = std::sync::mpsc::channel();
            *self.release_competitor.lock().unwrap() = Some(release);
            thread::spawn(move || {
                let store = WorkingFiles::open(&path).unwrap();
                let _guard = store.try_edit().unwrap();
                acquired.send(()).unwrap();
                let _ = hold.recv_timeout(Duration::from_secs(8));
            });
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
        }
    }
    fn recovery_desired_rename_observed(&self) -> bool {
        !self.fail_selector.swap(false, Ordering::AcqRel)
    }
    fn before_final_inspection(&self) {
        if self.remove_key.swap(false, Ordering::AcqRel) {
            fixture_key_available(false);
        }
        if let Some(path) = self.damage_working.lock().unwrap().take() {
            fs::rename(&path, path.with_file_name("test-held-working")).unwrap();
        }
    }
    fn adopt(&self, revision: String) -> Adoption {
        self.adoptions.fetch_add(1, Ordering::AcqRel);
        if let Some(unlock) = self.release_competitor.lock().unwrap().take() {
            unlock.send(()).unwrap();
        }
        if self.rollback.swap(false, Ordering::AcqRel) {
            return Adoption::observed(AdoptionOutcome::RolledBack);
        }
        if self.hold.load(Ordering::Acquire) {
            let (pending, terminal) = Adoption::pending();
            *self.terminal.lock().unwrap() = Some((revision, terminal));
            return pending;
        }
        self.observed.lock().unwrap().active_revision = Some(revision);
        Adoption::observed(AdoptionOutcome::Confirmed)
    }
    fn inhibit_uncertain(&self, revision: Option<&str>, cause: Fault) {
        self.inhibit_history
            .lock()
            .unwrap()
            .push((revision.map(str::to_owned), cause));
        self.inhibits.fetch_add(1, Ordering::AcqRel);
        if let Some((_, site)) = self.domain.lock().unwrap().as_mut() {
            let _ = site.command(rpt_advanced_core::site::SiteCommand::Inhibit(true), 0);
        }
    }
    fn diagnostic(&self, _: crate::management::Diagnostic) {
        self.warnings.fetch_add(1, Ordering::AcqRel);
    }
    fn observed(&self) -> Observed {
        self.observed.lock().unwrap().clone()
    }
    fn operation_owner(
        &self,
        target: ResourceId,
        expected: &str,
        _: &rpt_advanced_core::command_language::PreparedAction,
    ) -> Result<OperationOwner, Fault> {
        if self.observed().active_revision.as_deref() != Some(expected) {
            return Err(Fault::Conflict);
        }
        let mut domain = self.domain.lock().unwrap();
        let (host, _) = domain.as_mut().ok_or(Fault::Unavailable)?;
        if target != SITE.parse().unwrap() {
            return Err(Fault::Unavailable);
        }
        Ok(OperationOwner {
            scope: ResourceScope::Site(target),
            generation: Arc::new(host.control().work().ok_or(Fault::Unavailable)?),
            requires_grant: false,
        })
    }
    fn dispatch(&self, expected: &str, intent: Invocation) -> Effect {
        let until = Instant::now() + Duration::from_secs(8);
        while self.hold_dispatch.load(Ordering::Acquire) && Instant::now() < until {
            thread::sleep(Duration::from_millis(2));
        }
        if self.observed().active_revision.as_deref() != Some(expected) {
            return Effect::Rejected;
        }
        let rpt_advanced_core::command_language::Action::Site(command) = intent.action else {
            return Effect::Rejected;
        };
        if intent.context.recheck(false).is_err() {
            return Effect::Rejected;
        }
        let mut domain = self.domain.lock().unwrap();
        let Some((_, site)) = domain.as_mut() else {
            return Effect::Rejected;
        };
        if site.command(command, 0).is_err() {
            return Effect::Rejected;
        }
        self.dispatched.fetch_add(1, Ordering::SeqCst);
        Effect::Admitted
    }
}

fn fixture_key_available(available: bool) {
    let key = std::path::PathBuf::from(std::env::var_os("RPTADV_HAL_FIXTURE_KEY").unwrap());
    let parent = key.parent().unwrap();
    let held = key.with_extension("test-held");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    if available {
        fs::rename(&held, &key).unwrap()
    } else {
        fs::rename(&key, &held).unwrap()
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
}
struct Client<'a> {
    pki: &'a Path,
    port: u16,
    proxy: Option<&'a frontend::Proxy>,
}
struct Reply {
    code: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}
impl Client<'_> {
    fn send(
        &self,
        method: &str,
        path: &str,
        cond: Option<(&str, &str)>,
        body: Option<&Value>,
    ) -> Reply {
        let mut c = Command::new("curl");
        c.args([
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-time",
            "10",
            "--http1.1",
            "--dump-header",
            "-",
            "--request",
            method,
            "--header",
            "Host: localhost",
            "--cacert",
        ])
        .arg(self.pki.join("backend.pem"))
        .arg("--cert")
        .arg(self.pki.join("cli.pem"))
        .arg("--key")
        .arg(self.pki.join("cli.key"));
        if let Some(proxy) = self.proxy {
            c.args([
                "--user",
                "user:fixture-password",
                "--header",
                "Origin: https://console.local",
                "--header",
            ])
            .arg(format!("X-CSRF-Token: {}", proxy.csrf));
        }
        if let Some((k, v)) = cond {
            c.arg("--header").arg(format!("{k}: {v}"));
        }
        if let Some(v) = body {
            c.arg("--header")
                .arg(if method == "PATCH" {
                    "Content-Type: application/json-patch+json"
                } else {
                    "Content-Type: application/hal+json"
                })
                .arg("--data-binary")
                .arg(v.to_string());
        }
        let output = c
            .arg(format!("https://localhost:{}{path}", self.port))
            .output()
            .unwrap();
        assert!(output.status.success(), "curl transport failed");
        let raw = String::from_utf8(output.stdout).unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        let mut lines = headers.lines();
        let code = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .map(|l| {
                let (k, v) = l.split_once(':').unwrap();
                (k.to_lowercase(), v.trim().into())
            })
            .collect();
        Reply {
            code,
            headers,
            body: serde_json::from_str(body)
                .unwrap_or_else(|_| panic!("expected JSON for {method} {path}, got status {code}")),
        }
    }
    fn get(&self, path: &str) -> Reply {
        self.send("GET", path, None, None)
    }
    fn etag(&self, path: &str) -> String {
        let r = self.get(path);
        assert_eq!(r.code, 200, "{path}");
        r.headers["etag"].clone()
    }
    fn edit(&self, m: &str, p: &str, b: Option<&Value>) -> Reply {
        let tag = self.etag(p);
        self.send(m, p, Some(("If-Match", &tag)), b)
    }
    fn ready(&self) {
        let until = Instant::now() + Duration::from_secs(8);
        loop {
            let r = self.get("/api/v1/health/");
            if r.body["status"]["historical_activation"] == "confirmed"
                && r.body["status"]["desired_revision"] == r.body["status"]["active_revision"]
            {
                return;
            }
            assert!(Instant::now() < until, "activation did not finish");
            thread::sleep(Duration::from_millis(4));
        }
    }
}

fn cert(dir: &Path, name: &str) -> CertificateDer<'static> {
    let result = Command::new("openssl")
        .args(["x509", "-outform", "DER", "-in"])
        .arg(dir.join(format!("{name}.pem")))
        .output()
        .unwrap();
    assert!(result.status.success());
    CertificateDer::from(result.stdout)
}

const TEST: &str = "configuration_transaction::tests::http::authenticated_edits_and_receipts_use_the_selected_store";
#[test]
fn authenticated_edits_and_receipts_use_the_selected_store() {
    if std::env::var_os("RPTADV_HAL_TEST_CHILD").is_none() {
        let (fixture, _) = Fixture::new();
        fixture.put("keys/rpt-advanced.aead.example", &[19; 32]);
        let keys = fixture.0.join("keys");
        fs::set_permissions(
            keys.join("rpt-advanced.aead.example"),
            fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o500)).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("CREDENTIALS_DIRECTORY", &keys)
            .env(
                "RPTADV_HAL_FIXTURE_KEY",
                keys.join("rpt-advanced.aead.example"),
            )
            .env("RPTADV_HAL_TEST_CHILD", "1")
            .status()
            .unwrap();
        fs::set_permissions(keys, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(out.success());
        return;
    }
    let (fixture, store) = Fixture::new();
    let key = KeyId::new("example").unwrap();
    {
        let guard = store.try_edit().unwrap();
        let tx = Transaction::new(&guard, LIMITS).with_write_key(&key);
        first(&tx);
    }
    for (activation, journal) in [
        (Activation::Pending, true),
        (Activation::Pending, false),
        (Activation::Failed, false),
        (Activation::RecoveryRequired, false),
    ] {
        let (_startup, handle) = Fixture::new();
        {
            let guard = handle.try_edit().unwrap();
            let tx = Transaction::new(&guard, LIMITS).with_write_key(&key);
            let (manifest, bytes) = prepared("startup");
            tx.commit(
                Admission::Provision {
                    store: STORE.parse().unwrap(),
                },
                &tx.inventory().unwrap(),
                &manifest,
                &[(SITE.parse().unwrap(), &bytes)],
                JOURNAL.parse().unwrap(),
            )
            .unwrap();
            if !journal {
                tx.reconcile().unwrap();
            }
            if activation != Activation::Pending {
                tx.record_activation(&tx.inspect().unwrap(), activation)
                    .unwrap();
            }
        }
        let probe = Arc::new(RuntimeFixture::default());
        let _controller = Coordinator::new(
            handle,
            KeyId::new("example").unwrap(),
            LIMITS,
            "/api/v1",
            probe.clone(),
        )
        .unwrap();
        assert!(
            probe.inhibits.load(Ordering::Acquire) > 0,
            "startup did not inhibit {activation:?}"
        );
    }
    let dir = fixture.0.join("test-pki");
    fs::create_dir(&dir).unwrap();
    assert!(
        Command::new("sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../peer-support/tests/https-pki.sh"
            ))
            .arg(&dir)
            .output()
            .unwrap()
            .status
            .success()
    );
    let privkey = Command::new("openssl")
        .args(["pkcs8", "-topk8", "-nocrypt", "-outform", "DER", "-in"])
        .arg(dir.join("server.key"))
        .output()
        .unwrap();
    assert!(privkey.status.success());
    let trust = ServiceTrust::new(
        vec![cert(&dir, "client")],
        ["cli", "nginx", "apache"]
            .into_iter()
            .map(|service| {
                (
                    ServiceTrust::leaf_pin(&cert(&dir, service)),
                    ServiceId::new(service).unwrap(),
                )
            })
            .collect(),
    )
    .unwrap();
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let (listener, mut inbox) = Listener::start(
        address,
        ServerIdentity::new(
            vec![cert(&dir, "server")],
            PrivateKeyDer::try_from(privkey.stdout).unwrap(),
        ),
        trust,
        OriginPolicy::new(
            vec!["localhost".into(), format!("localhost:{}", address.port())],
            vec!["https://console.local".into()],
        )
        .unwrap(),
    )
    .unwrap();
    let client = Client {
        pki: &dir,
        port: listener.local_addr().port(),
        proxy: None,
    };
    let runtime = Arc::new(RuntimeFixture::default());
    let app = Coordinator::new(store, key, LIMITS, "/api/v1", runtime.clone()).unwrap();
    let done = AtomicBool::new(false);
    let (frozen_bytes, prior_path) = thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if !app.poll_listener(&listener, &mut inbox) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release)
            }
        }
        let _stop = Stop(&done);
        let ports = "/api/v1/configuration/ports/";
        let site = "/api/v1/configuration/site/";
        let description = client.get("/api/v1/openapi.json");
        assert_eq!(description.code, 200);
        assert_eq!(description.body["openapi"], "3.1.0");
        assert_eq!(
            description.body["components"]["securitySchemes"]["service"]["type"],
            "mutualTLS"
        );
        let ops = &description.body["paths"];
        for route in rpt_advanced_core::management::routes::inventory().values() {
            let path = &ops[&route.path];
            for method in ["get", "patch", "put", "post", "delete"] {
                assert_eq!(
                    path.get(method).is_some(),
                    route.methods.contains(&method.to_uppercase().as_str()),
                    "{} {method}",
                    route.path
                );
            }
        }
        let create = &ops["/configuration/ports/"]["post"]["requestBody"]["content"]["application/hal+json"]
            ["schema"]["$ref"];
        let create_schema = client.get(create.as_str().unwrap());
        assert_eq!(
            create_schema.headers["content-type"],
            "application/schema+json"
        );
        assert!(create_schema.body["properties"]["key"].is_object());
        assert!(
            create_schema.body["properties"]["configuration"]["properties"]["inherit_from"]
                .is_object()
        );
        let event = client.get("/api/v1/schemas/event/");
        assert!(event.body["properties"]["start"].is_object());
        assert!(event.body["properties"].get("configuration").is_none());
        assert!(client.get("/api/v1/schemas/json-patch/").body["items"]["oneOf"].is_array());
        let inventory = client.get(ports);
        assert_eq!(inventory.code, 200);
        assert_eq!(inventory.body["configuration"]["members"], json!([]));
        assert!(inventory.body["_links"]["item"].is_array());
        let add = json!({"key":"../Radio Owner","configuration":{"name":"Port One","inherit_from":"site","enabled":false}});
        assert_eq!(client.send("POST", ports, None, Some(&add)).code, 428);
        assert_eq!(
            client
                .send(
                    "POST",
                    ports,
                    Some(("If-Match", &client.etag(site))),
                    Some(&add)
                )
                .code,
            412
        );
        let h = client.get("/api/v1/health/");
        assert!(h.body["status"]["active_revision"].is_null());
        *runtime.contend.lock().unwrap() = Some(fixture.0.clone());
        runtime.hold.store(true, Ordering::Release);
        let created = client.edit("POST", ports, Some(&add));
        assert_eq!(created.code, 201, "{}", created.body);
        let port = &created.headers["location"];
        assert_eq!(created.body["_links"]["self"]["href"], *port);
        assert_eq!(created.body["_links"]["up"]["href"], ports);
        assert_eq!(
            client
                .get(
                    created.body["_links"]["rpt:changes"]["href"]
                        .as_str()
                        .unwrap()
                )
                .code,
            200
        );
        assert!(port.starts_with(ports) && !port.contains("Owner"));
        let pending = client.get("/api/v1/status/");
        assert_eq!(pending.code, 200);
        assert!(pending.body["status"]["active_revision"].is_null());
        assert_eq!(client.edit("POST", ports, Some(&add)).code, 409);
        runtime.hold.store(false, Ordering::Release);
        let until = Instant::now() + Duration::from_secs(5);
        let (confirmed, terminal) = loop {
            if let Some(token) = runtime.terminal.lock().unwrap().take() {
                break token;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(2));
        };
        runtime.observed.lock().unwrap().active_revision = Some(confirmed);
        terminal.send(AdoptionOutcome::Confirmed).unwrap();
        client.ready();
        let commands = format!("{site}command/settings/");
        let warning_count = runtime.warnings.load(Ordering::Acquire);
        assert_eq!(
            client
                .edit(
                    "PUT",
                    &commands,
                    Some(&json!({"configuration":{"admin":[],"muting":false}}))
                )
                .code,
            202
        );
        client.ready();
        assert_eq!(
            client.get(&commands).body["configuration"],
            json!({"muting":false})
        );
        assert!(runtime.warnings.load(Ordering::Acquire) > warning_count);
        let details = json!({"configuration":{"muting":false,"admin":{"timing":{"timeout_ms":12345}},"regeneration":{"level_dbfs":-19}}});
        assert_eq!(client.edit("PUT", &commands, Some(&details)).code, 202);
        client.ready();
        assert_eq!(
            client.get(&commands).body["configuration"],
            details["configuration"]
        );
        let effective = client.get(&format!(
            "{}command/settings/",
            port.replace("/configuration/", "/effective/")
        ));
        assert_eq!(
            effective.body["effective"]["admin"]["timing"]["timeout_ms"],
            12345
        );
        assert_eq!(
            effective.body["effective"]["regeneration"]["level_dbfs"],
            -19
        );
        assert_eq!(effective.body["effective"]["muting"], false);
        assert!(effective.body["effective"]["mappings"].is_object());
        let changed = client.edit(
            "PATCH",
            &commands,
            Some(&json!([{"op":"replace","path":"/configuration/admin","value":null}])),
        );
        assert_eq!(changed.code, 202, "{}", changed.body);
        client.ready();
        assert!(
            client.get(&commands).body["configuration"]
                .get("admin")
                .is_none()
        );
        let site_mail = format!("{site}mailboxes/");
        let phc = "$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";
        assert_eq!(
            client
                .edit(
                    "PUT",
                    &commands,
                    Some(&json!({"configuration":{"admin":{"lock_hash":phc}}}))
                )
                .code,
            202
        );
        client.ready();
        assert_eq!(
            client.get(&commands).body["meta"]["credentials"]["lock_configured"],
            true
        );
        assert_eq!(client.edit("PATCH",&commands,Some(&json!([{"op":"replace","path":"/configuration/admin","value":[{"nested":{"lock_hash":"fixture-reserved"}}]}]))).code,422);
        let count = runtime.warnings.load(Ordering::Acquire);
        assert_eq!(
            client
                .edit(
                    "PUT",
                    &commands,
                    Some(&json!({"configuration":{"admin":[],"muting":false}}))
                )
                .code,
            202
        );
        client.ready();
        let normalized = client.get(&commands);
        assert_eq!(normalized.body["configuration"], json!({"muting":false}));
        assert_eq!(
            normalized.body["meta"]["credentials"]["lock_configured"],
            true
        );
        assert!(runtime.warnings.load(Ordering::Acquire) > count);
        let mut failures = Vec::<&str>::new();
        let sequential = json!([
          {"op":"add","path":"/configuration/admin","value":[4000]},
          {"op":"copy","from":"/configuration/admin/0","path":"/configuration/inactivity_timeout_ms"},
          {"op":"remove","path":"/configuration/admin"}]);
        let result = client.edit("PATCH", &commands, Some(&sequential));
        if result.code != 202 {
            failures.push("safe RFC 6902 intermediate array");
        } else {
            client.ready();
            let after = client.get(&commands);
            assert_eq!(
                after.body["configuration"],
                json!({"muting":false,"inactivity_timeout_ms":4000})
            );
            assert_eq!(after.body["meta"]["credentials"]["lock_configured"], true);
        }
        for insertion in [
            json!({"op":"add","path":"/configuration/admin/0/lock_hash","value":"fixture-reserved"}),
            json!({"op":"add","path":"/configuration/admin/-","value":[{"unlock_hash":"fixture-reserved"}]}),
            json!({"op":"copy","from":"/configuration/staged","path":"/configuration/admin/-"}),
        ] {
            let unsafe_sequence = json!([
                {"op":"add","path":"/configuration/staged","value":{"nested":{"lock_hash":"fixture-reserved"}}},
                {"op":"add","path":"/configuration/admin","value":[{}]},insertion,
                {"op":"remove","path":"/configuration/admin"},{"op":"remove","path":"/configuration/staged"}
            ]);
            assert_eq!(
                client.edit("PATCH", &commands, Some(&unsafe_sequence)).code,
                422
            );
        }
        let mailbox = json!({"key":"741","configuration":{"first_name":"Test","callsign":"N0TEST","pin_hash":phc}});
        let created_mail = client.edit("POST", &site_mail, Some(&mailbox));
        assert_eq!(created_mail.code, 201, "{}", created_mail.body);
        client.ready();
        let origin = &created_mail.headers["location"];
        let m = client.get(origin);
        assert_eq!(
            m.body["configuration"],
            json!({"first_name":"Test","callsign":"N0TEST"})
        );
        assert_eq!(m.body["meta"]["credentials"]["pin_configured"], true);
        assert!(!m.body.to_string().contains(phc));
        let effective_mail = format!(
            "{}mailboxes/",
            port.replace("/configuration/", "/effective/")
        );
        let item = client.get(&effective_mail).body["_links"]["item"][0]["href"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            client.get(&item).body["_links"]["rpt:source"]["href"],
            *origin
        );
        assert!(
            client.get(&format!("{port}mailboxes/")).body["_links"]["item"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let deleted = client.edit("DELETE", origin, None);
        assert_eq!(deleted.code, 202);
        for rel in ["self", "up", "rpt:changes"] {
            assert_eq!(
                client
                    .get(deleted.body["_links"][rel]["href"].as_str().unwrap())
                    .code,
                200
            );
        }
        client.ready();
        let site_echo = format!("{site}protocol-defaults/echolink/");
        let site_proxy = format!("{site_echo}proxy/");
        assert_eq!(client.get(&site_proxy).code, 404);
        let echo_defaults =
            json!({"configuration":{"enabled":false,"callsign":"N0AUX-L","node_number":100002}});
        assert_eq!(
            client.edit("PUT", &site_echo, Some(&echo_defaults)).code,
            202
        );
        client.ready();
        let selector = fs::read(fixture.0.join("state.json")).unwrap();
        let logical = client.get(&site_proxy);
        assert_eq!(logical.body["configuration"], json!({}));
        assert!(logical.body.get("id").is_none());
        assert!(logical.body["meta"].get("credentials").is_none());
        assert_eq!(client.get(&format!("{site_proxy}credentials/")).code, 404);
        assert_eq!(fs::read(fixture.0.join("state.json")).unwrap(), selector);
        let proxy_body = json!({"configuration":{"host":"test.invalid","port":8100}});
        assert_eq!(
            client
                .send(
                    "PUT",
                    &site_proxy,
                    Some(("If-Match", &client.etag(&site_echo))),
                    Some(&proxy_body)
                )
                .code,
            412
        );
        assert_eq!(
            client
                .send(
                    "PUT",
                    &site_proxy,
                    Some(("If-Match", &logical.headers["etag"])),
                    Some(&proxy_body)
                )
                .code,
            202
        );
        client.ready();
        assert!(client.get(&site_proxy).body["id"].is_string());
        let echo = format!("{port}protocols/echolink/");
        let empty = client.get(&echo);
        assert_eq!(empty.body["configuration"]["members"], json!([]));
        runtime.reject_credentials.store(true, Ordering::Release);
        let pre = client.etag(&echo);
        let rejected=client.edit("POST",&echo,Some(&json!({"configuration":{"enabled":false,"callsign":"N0TEST-L","node_number":100001,"credentials":{"password":"rejected-example"}}})));
        assert_eq!(rejected.code, 409);
        assert_eq!(client.etag(&echo), pre);
        let account=client.edit("POST",&echo,Some(&json!({"configuration":{"enabled":false,"callsign":"N0TEST-L","node_number":100001,"credentials":{"password":"fixture-writeonly"}}})));
        assert_eq!(account.code, 201, "{}", account.body);
        client.ready();
        let account = &account.headers["location"];
        let boot_probe = client.get("/api/v1/operations/").body["boot_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let new_probe = || {
            format!(
                "/api/v1/operations/{boot_probe}/{}/",
                rptadv_peer_support::working_files::transaction::new_identity().unwrap()
            )
        };
        let probe = new_probe();
        let draft_probe = json!({"configuration":{}});
        assert_eq!(
            client
                .send(
                    "PUT",
                    &probe,
                    Some(("If-None-Match", "*")),
                    Some(&draft_probe)
                )
                .code,
            200
        );
        let probe_etag = client.etag(&probe);
        let original_ledger = fs::read(fixture.0.join("operations.json")).unwrap();
        let fixture_key =
            std::path::PathBuf::from(std::env::var_os("RPTADV_HAL_FIXTURE_KEY").unwrap());
        fs::set_permissions(&fixture_key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&fixture_key, [27; 32]).unwrap();
        fs::set_permissions(&fixture_key, fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            client
                .send(
                    "PUT",
                    &new_probe(),
                    Some(("If-None-Match", "*")),
                    Some(&draft_probe)
                )
                .code,
            503
        );
        assert_eq!(
            client
                .send(
                    "PATCH",
                    &probe,
                    Some(("If-Match", &probe_etag)),
                    Some(&json!([]))
                )
                .code,
            503
        );
        assert_eq!(
            fs::read(fixture.0.join("operations.json")).unwrap(),
            original_ledger
        );
        fs::set_permissions(&fixture_key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&fixture_key, [19; 32]).unwrap();
        fs::set_permissions(&fixture_key, fs::Permissions::from_mode(0o400)).unwrap();

        let member = client.get(account);
        assert!(!member.body.to_string().contains("fixture-writeonly"));
        let proxy = format!("{account}proxy/");
        assert_eq!(client.get(&proxy).code, 200);
        let private = format!("{account}credentials/");
        assert_eq!(client.edit("PATCH",&private,Some(&json!([{"op":"test","path":"/configuration/credentials/password","value":""}]))).code,422);
        assert_eq!(
            client
                .edit("POST", &echo, Some(&json!({"configuration":{}})))
                .code,
            409
        );
        let old = client.etag(&echo);
        assert_eq!(client.edit("DELETE", &echo, None).code, 202);
        client.ready();
        assert_eq!(client.get(account).code, 404);
        assert_eq!(
            client
                .send("DELETE", &echo, Some(("If-Match", &old)), None)
                .code,
            412
        );
        assert_eq!(
            client.get(&echo).body["configuration"]["members"],
            json!([])
        );
        let index = client.get("/api/v1/operations/");
        let boot = index.body["boot_id"].as_str().unwrap();
        let op = format!("/api/v1/operations/{boot}/{JOURNAL}/");
        let draft = json!({"configuration":{"submitted":false}});
        assert_eq!(client.send("PATCH", &op, None, Some(&json!([]))).code, 404);
        assert_eq!(
            client
                .send("PUT", &op, Some(("If-None-Match", "*")), Some(&draft))
                .code,
            200
        );
        let desired = client.etag("/api/v1/status/desired/");
        let base = client.etag(&op);
        *runtime.contend.lock().unwrap() = Some(fixture.0.clone());
        let patch = json!([{"op":"test","path":"/configuration/submitted","value":false},{"op":"add","path":"/configuration/action","value":{"action":"apply_configuration"}},{"op":"add","path":"/configuration/desired_etag","value":desired},{"op":"replace","path":"/configuration/submitted","value":true}]);
        assert_eq!(
            client
                .send("PATCH", &op, Some(("If-Match", &base)), Some(&patch))
                .code,
            200
        );
        assert_eq!(
            client
                .send("PATCH", &op, Some(("If-Match", &base)), Some(&patch))
                .code,
            200
        );
        let receipt = client.get(&op);
        assert_eq!(receipt.body["receipt"]["state"], "completed");
        let safe = client.edit(
            "PATCH",
            &op,
            Some(&json!([{"op":"test","path":"/configuration/submitted","value":true}])),
        );
        assert_eq!(safe.code, 200);
        assert_eq!(client.edit("PUT", &op, Some(&draft)).code, 409);
        // Use the existing generation fence and SiteController command implementation.
        use rpt_advanced_core::runtime::{GenerationSettings, NodeHost, RuntimeGeneration};
        let generation = RuntimeGeneration::prepare(
            11,
            GenerationSettings {
                node: "fixture".into(),
                device: "fixture".into(),
                receive_maximum: 480,
                transmit_maximum: 480,
                squelch_delay_ms: 0,
                status_snapshot_interval_ms: 50,
            },
            (),
            (),
        )
        .unwrap();
        *runtime.domain.lock().unwrap() = Some((
            NodeHost::new(generation),
            rpt_advanced_core::site::SiteController::new(Default::default()),
        ));
        let effect = format!("/api/v1/operations/{boot}/{SITE}/");
        let active = runtime.observed().active_revision.unwrap();
        let request = json!({"configuration":{"submitted":true,"targets":[SITE],"expected_active_revision":active,"action":{"action":"set_transmit_inhibit","enabled":true,"parameter":"synthetic-private-operand"}}});
        runtime.hold_dispatch.store(true, Ordering::Release);
        let (submitted, frozen) = thread::scope(|s| {
            let run = s.spawn(|| {
                client.send("PUT", &effect, Some(("If-None-Match", "*")), Some(&request))
            });
            let timeout = Instant::now() + Duration::from_secs(6);
            while client.get(&effect).body["receipt"]["state"] != "submitted" {
                assert!(Instant::now() < timeout, "durable admission missing");
            }
            let frozen = fs::read(fixture.0.join("operations.json")).unwrap();
            runtime.hold_dispatch.store(false, Ordering::Release);
            (run.join().unwrap(), frozen)
        });
        assert_eq!(submitted.code, 200, "{}", submitted.body);
        assert_eq!(submitted.body["receipt"]["state"], "completed");
        assert!(
            runtime
                .domain
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .1
                .transmit_inhibited()
        );
        assert_eq!(
            client
                .send("PUT", &effect, Some(("If-None-Match", "*")), Some(&request))
                .code,
            200
        );
        assert_eq!(
            client
                .send(
                    "PUT",
                    &effect,
                    Some(("If-Match", &client.etag(site))),
                    Some(&request)
                )
                .code,
            412
        );
        assert_eq!(runtime.dispatched.load(Ordering::Acquire), 1);
        assert!(
            !submitted
                .body
                .to_string()
                .contains("synthetic-private-operand")
        );
        assert!(!String::from_utf8_lossy(&frozen).contains("synthetic-private-operand"));
        let mut different = request.clone();
        different["configuration"]["action"]["parameter"] = json!("other-private-operand");
        assert_eq!(client.edit("PUT", &effect, Some(&different)).code, 409);
        let uncertain = new_probe();
        runtime.fail_admission.store(true, Ordering::Release);
        assert_eq!(
            client
                .send(
                    "PUT",
                    &uncertain,
                    Some(("If-None-Match", "*")),
                    Some(&request)
                )
                .code,
            503
        );
        assert_eq!(
            client.get(&uncertain).body["receipt"]["state"],
            "indeterminate"
        );
        assert_eq!(
            client
                .send(
                    "PUT",
                    &uncertain,
                    Some(("If-None-Match", "*")),
                    Some(&request)
                )
                .body["receipt"]["state"],
            "indeterminate"
        );
        assert_eq!(runtime.dispatched.load(Ordering::Acquire), 1);
        let unrelated = new_probe();
        assert_eq!(
            client
                .send(
                    "PUT",
                    &unrelated,
                    Some(("If-None-Match", "*")),
                    Some(&draft_probe)
                )
                .code,
            200
        );
        let append = json!([{"op":"test","path":"/configuration/submitted","value":false},
           {"op":"add","path":"/configuration/targets","value":[]},
           {"op":"add","path":"/configuration/targets/-","value":SITE},
           {"op":"add","path":"/configuration/action","value":request["configuration"]["action"]},
           {"op":"add","path":"/configuration/expected_active_revision","value":active},
           {"op":"replace","path":"/configuration/submitted","value":true}]);
        let draft_etag = client.etag(&unrelated);
        for _ in 0..2 {
            assert_eq!(
                client
                    .send(
                        "PATCH",
                        &unrelated,
                        Some(("If-Match", &draft_etag)),
                        Some(&append)
                    )
                    .body["receipt"]["state"],
                "completed"
            );
        }
        assert_eq!(runtime.dispatched.load(Ordering::Acquire), 2);
        let protected = format!(
            "/api/v1/operations/{boot}/{}/",
            rptadv_peer_support::working_files::transaction::new_identity().unwrap()
        );
        let injection =
            json!({"configuration":{"action":{"action":"execute_dtmf","digits":"*87351967#"}}});
        let original = client.send(
            "PUT",
            &protected,
            Some(("If-None-Match", "*")),
            Some(&injection),
        );
        assert_eq!(original.code, 200, "{}", original.body);
        assert!(!original.body.to_string().contains("87351967"));
        let old = client.etag(&protected);
        assert_eq!(client.edit("PATCH",&protected,Some(&json!([{"op":"test","path":"/configuration/action","value":injection["configuration"]["action"]}]))).code,422);
        let replaced=client.edit("PATCH",&protected,Some(&json!([{"op":"replace","path":"/configuration/action/digits","value":"*84762310#"}])));
        assert_eq!(replaced.code, 200, "{}", replaced.body);
        assert_ne!(client.etag(&protected), old);
        assert!(!client.get(&protected).body.to_string().contains("84762310"));
        let disk = fs::read_to_string(fixture.0.join("operations.json")).unwrap();
        assert!(
            ![
                "87351967",
                "84762310",
                "synthetic-private-operand",
                "other-private-operand"
            ]
            .iter()
            .any(|v| disk.contains(v))
        );
        let cleared=client.edit("PATCH",&protected,Some(&json!([{"op":"replace","path":"/configuration/action","value":{"action":"set_transmit_inhibit","enabled":true}}])));
        assert_eq!(cleared.code, 200, "{}", cleared.body);
        let forbidden = format!("/api/v1/operations/{boot}/{STORE}/");
        let credential_action = json!({"configuration":{"submitted":false,"action":{"action":"unlock_administration","parameter":"87351967"}}});
        assert_eq!(
            client
                .send(
                    "PUT",
                    &forbidden,
                    Some(("If-None-Match", "*")),
                    Some(&credential_action)
                )
                .code,
            403
        );
        assert_eq!(client.get(&forbidden).code, 404);
        assert!(
            !fs::read_to_string(fixture.0.join("operations.json"))
                .unwrap()
                .contains("87351967")
        );
        if std::env::var_os("RPTADV_HAL_STAGED_PROXY").is_some() {
            for kind in ["nginx", "apache"] {
                let proxy = frontend::Proxy::start(
                    kind,
                    &dir,
                    listener.local_addr(),
                    "backend",
                    "localhost",
                );
                let frontend = Client {
                    pki: &dir,
                    port: proxy.port,
                    proxy: Some(&proxy),
                };
                let fresh = frontend.etag(site);
                let op = json!([{"op":"replace","path":"/configuration/name","value":kind}]);
                let result = frontend.send("PATCH", site, Some(("If-Match", &fresh)), Some(&op));
                assert_eq!(result.code, 202, "{kind} {}", result.body);
                client.ready();
                assert_eq!(client.get(site).body["configuration"]["name"], kind);
                assert_eq!(
                    frontend
                        .send("PATCH", site, Some(("If-Match", &fresh)), Some(&op))
                        .code,
                    412
                );
            }
        }
        let other = format!("/api/v1/operations/{STORE}/{JOURNAL}/");
        assert_eq!(
            client
                .send("PUT", &other, Some(("If-None-Match", "*")), Some(&draft))
                .code,
            410
        );
        let single = client.edit("DELETE", port, None);
        assert_eq!(single.code, 202);
        assert_eq!(single.body["_links"]["self"]["href"], ports);
        client.ready();
        let empty = client.edit("DELETE", ports, None);
        assert_eq!(empty.code, 202);
        assert_eq!(empty.body["_links"]["self"]["href"], ports);
        client.ready();
        assert_eq!(
            client.get(ports).body["configuration"]["members"],
            json!([])
        );
        let changed = json!({"id":SITE,"kind":"metadata","configuration":{"name":"hand-edited","inherit_from":null,"unsupported":"ignored"}});
        fixture.put("working/site/metadata.json", changed.to_string().as_bytes());
        let dirty = client.get("/api/v1/status/");
        assert_eq!(dirty.body["status"]["storage"], "external_edit");
        let observed = dirty.body["status"]["working_fingerprint"]
            .as_str()
            .unwrap();
        assert_eq!(client.edit("PATCH",site,Some(&json!([{"op":"replace","path":"/configuration/name","value":"must-not-overwrite"}]))).code,409);
        let load_id = rptadv_peer_support::working_files::transaction::new_identity().unwrap();
        let load = format!("/api/v1/operations/{boot}/{load_id}/");
        let do_reload = json!({"configuration":{"submitted":true,"action":{"action":"reload_configuration"},
            "desired_etag":client.etag("/api/v1/status/desired/"),"working_fingerprint":observed}});
        let reload = client.send("PUT", &load, Some(("If-None-Match", "*")), Some(&do_reload));
        assert_eq!(reload.code, 200, "{}", reload.body);
        assert_eq!(
            reload.body["receipt"]["state"], "completed",
            "{}",
            reload.body
        );
        assert_eq!(
            client.get(site).body["configuration"]["name"],
            "hand-edited"
        );
        assert!(
            client.get(site).body["configuration"]
                .get("unsupported")
                .is_none()
        );
        let different = json!({"id":SITE,"kind":"metadata","configuration":{"name":"external","inherit_from":null}});
        fixture.put(
            "working/site/metadata.json",
            different.to_string().as_bytes(),
        );
        let restore_id = rptadv_peer_support::working_files::transaction::new_identity().unwrap();
        let restore = format!("/api/v1/operations/{boot}/{restore_id}/");
        let mut intent = json!({"configuration":{"submitted":true,"action":{"action":"restore_configuration"},"restore":"last_known_good",
            "desired_etag":client.etag("/api/v1/status/desired/"),"working_fingerprint":observed}});
        assert_eq!(
            client
                .send("PUT", &restore, Some(("If-None-Match", "*")), Some(&intent))
                .code,
            412
        );
        intent["configuration"]["working_fingerprint"] =
            client.get("/api/v1/status/").body["status"]["working_fingerprint"].clone();
        let restored = client.send("PUT", &restore, Some(("If-None-Match", "*")), Some(&intent));
        assert_eq!(
            restored.body["receipt"]["state"], "completed",
            "{}",
            restored.body
        );
        let disk: Value = serde_json::from_slice(
            &fs::read(fixture.0.join("working/site/metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(disk["configuration"]["name"], "hand-edited");
        let prior_active = runtime.observed().active_revision.unwrap();
        let attempts = runtime.adoptions.load(Ordering::Acquire);
        runtime.remove_key.store(true, Ordering::Release);
        let save = client.edit(
            "PATCH",
            site,
            Some(&json!([{"op":"replace","path":"/configuration/name","value":"final-key-probe"}])),
        );
        let selected = client.get("/api/v1/status/desired/").body["desired_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let readable = client.get(&commands);
        if save.code != 503
            || save.body["error"] != "durability_unknown"
            || prior_active == selected
            || runtime.adoptions.load(Ordering::Acquire) != attempts
            || !runtime
                .inhibit_history
                .lock()
                .unwrap()
                .contains(&(Some(selected.clone()), Fault::Unavailable))
            || readable.code != 200
            || readable.body["meta"]["secrets_state"] != "locked"
        {
            failures.push("post-selector save adopted or hid a locked selection");
        }
        fixture_key_available(true);
        let apply = || json!({"configuration":{"submitted":true,"action":{"action":"apply_configuration"},"desired_etag":client.etag("/api/v1/status/desired/")}});
        let attempts = runtime.adoptions.load(Ordering::Acquire);
        let attempt = new_probe();
        runtime.remove_key.store(true, Ordering::Release);
        let rejected = client.send(
            "PUT",
            &attempt,
            Some(("If-None-Match", "*")),
            Some(&apply()),
        );
        if rejected.body["receipt"]["state"] != "rejected"
            || runtime.adoptions.load(Ordering::Acquire) != attempts
            || client.get(&commands).body["meta"]["secrets_state"] != "locked"
            || client.get("/api/v1/status/desired/").body["desired_revision"] != selected
        {
            failures.push("explicit apply adopted or hid a locked selection");
        }
        fixture_key_available(true);
        let working = fixture.0.join("working");
        let attempts = runtime.adoptions.load(Ordering::Acquire);
        *runtime.damage_working.lock().unwrap() = Some(working.clone());
        let failed = client.send(
            "PUT",
            &new_probe(),
            Some(("If-None-Match", "*")),
            Some(&apply()),
        );
        if failed.body["receipt"]["state"] != "rejected"
            || runtime.adoptions.load(Ordering::Acquire) != attempts
            || !runtime
                .inhibit_history
                .lock()
                .unwrap()
                .contains(&(Some(selected.clone()), Fault::Storage))
            || client.get("/api/v1/status/storage/").body["storage"] != "storage_fault"
            || client.get(&commands).code != 200
        {
            failures.push("explicit apply adopted or hid an unsafe working tree");
        }
        fs::rename(working.with_file_name("test-held-working"), &working).unwrap();
        let recovered = client.send(
            "PUT",
            &new_probe(),
            Some(("If-None-Match", "*")),
            Some(&apply()),
        );
        assert_eq!(
            recovered.body["receipt"]["state"], "completed",
            "{failures:?} {:?}",
            recovered.body
        );
        client.ready();
        let previous = runtime.observed().active_revision.unwrap();
        let before = runtime.inhibits.load(Ordering::Acquire);
        runtime.rollback.store(true, Ordering::Release);
        let failed = client.edit(
            "PATCH",
            site,
            Some(&json!([{"op":"replace","path":"/configuration/name","value":"failed-at-owner"}])),
        );
        assert_eq!(failed.code, 202);
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let status = client.get("/api/v1/status/").body["status"].clone();
            if status["historical_activation"] == "failed" {
                assert_eq!(status["active_revision"], previous);
                assert_eq!(status["last_known_good"], previous);
                assert_eq!(status["desired_revision"], failed.body["desired_revision"]);
                break;
            }
            assert!(Instant::now() < until, "rollback not observed");
        }
        assert!(runtime.inhibits.load(Ordering::Acquire) > before);
        let active_before = runtime.observed().active_revision;
        let attempts = runtime.adoptions.load(Ordering::Acquire);
        let desired_before = client.get("/api/v1/status/desired/").body["desired_revision"].clone();
        let uncertain = new_probe();
        let body = json!({"configuration":{"submitted":true,"action":{"action":"restore_configuration"},"restore":"last_known_good",
            "desired_etag":client.etag("/api/v1/status/desired/"),"working_fingerprint":client.get("/api/v1/status/storage/").body["working_fingerprint"]}});
        runtime.fail_selector.store(true, Ordering::Release);
        let receipt = client.send("PUT", &uncertain, Some(("If-None-Match", "*")), Some(&body));
        let notifications = runtime.inhibit_history.lock().unwrap().clone();
        let desired_now = client.get("/api/v1/status/desired/").body["desired_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let retried = client.send("PUT", &uncertain, Some(("If-None-Match", "*")), Some(&body));
        if receipt.body["receipt"]["state"] != "indeterminate"
            || receipt.body != retried.body
            || desired_before == desired_now
            || !notifications.contains(&(Some(desired_now.clone()), Fault::DurabilityUnknown))
            || runtime.adoptions.load(Ordering::Acquire) != attempts
            || runtime.observed().active_revision != active_before
            || client.get("/api/v1/status/desired/").body["desired_revision"] != desired_now
        {
            failures
                .push("missing revision-specific inhibit after uncertain recovery selector rename");
        }
        assert!(failures.is_empty(), "{failures:?}");
        (frozen, effect)
    });
    drop(app);
    // Model a power loss after effect dispatch but before outcome publication by restoring
    // the actual durable bytes captured while that dispatch was held.
    fixture.put("operations.json", &frozen_bytes);
    let restarted = Coordinator::new(
        WorkingFiles::open(&fixture.0).unwrap(),
        KeyId::new("missing-test-key").unwrap(),
        LIMITS,
        "/api/v1",
        runtime.clone(),
    )
    .unwrap();
    done.store(false, Ordering::Release);
    thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if !restarted.poll_listener(&listener, &mut inbox) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release)
            }
        }
        let _stop = Stop(&done);
        let locked = client.get("/api/v1/status/storage/");
        assert_eq!(locked.body["secrets_state"], "locked");
        let command = "/api/v1/configuration/site/command/settings/";
        let readable = client.get(command);
        assert_eq!(readable.code, 200);
        assert_eq!(readable.body["meta"]["secrets_state"], "locked");
        assert!(
            readable.body["meta"]["credentials"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .edit(
                    "PUT",
                    command,
                    Some(&json!({"configuration":{"muting":true}}))
                )
                .code,
            503
        );
        let ledger_epoch = client.get("/api/v1/operations/");
        let key_draft = format!(
            "/api/v1/operations/{}/{SITE}/",
            ledger_epoch.body["boot_id"].as_str().unwrap()
        );
        assert_eq!(
            client
                .send(
                    "PUT",
                    &key_draft,
                    Some(("If-None-Match", "*")),
                    Some(&json!({"configuration":{}}))
                )
                .code,
            503
        );
        let historical = client.get(&prior_path);
        assert_eq!(historical.body["receipt"]["state"], "indeterminate");
        let retry = json!({"configuration":historical.body["configuration"]});
        assert_eq!(
            client
                .send(
                    "PUT",
                    &prior_path,
                    Some(("If-None-Match", "*")),
                    Some(&retry)
                )
                .code,
            410
        );
        assert_eq!(runtime.dispatched.load(Ordering::Acquire), 2);
        let before = runtime.inhibits.load(Ordering::Acquire);
        fixture.put("state.json", b"corrupted");
        assert_eq!(
            client.get("/api/v1/status/storage/").body["storage"],
            "storage_fault"
        );
        assert!(runtime.inhibits.load(Ordering::Acquire) > before);
    });
}
