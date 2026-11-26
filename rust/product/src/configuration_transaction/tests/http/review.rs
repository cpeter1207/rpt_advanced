//! Actual store and mTLS regressions for mutable dependencies and typed response contracts.
use super::*;
mod live_status;
mod maintenance;
mod native;
mod station;
use crate::management::{ManagedStore, ProfileInputs};
use rpt_advanced_core::config::resources::profiles::equipment::InterfaceCapability;
use rptadv_peer_support::https::CoordinatorInbox;

const SITE_PATH: &str = "/api/v1/configuration/site/";
const PORTS: &str = "/api/v1/configuration/ports/";
const DESIRED: &str = "/api/v1/status/desired/";
const STORAGE: &str = "/api/v1/status/storage/";
const CASE: &str = "RPTADV_HAL_REVIEW_CASE";

#[test]
fn authenticated_review_regressions() {
    let mut failed = Vec::new();
    for case in [
        "restore",
        "reload",
        "echo",
        "audio",
        "schemas",
        "event-timezones",
        "live-status",
        "maintenance-lease",
        "poll",
        "native-runtime",
        "native-site-zero",
        "native-site-views",
        "native-site-fence",
        "native-site-fault-reload",
        "native-site-claim-release",
        "native-site-output-override",
        "native-site-alarm-zero",
        "native-site-alarm-live",
        "native-site-action-fault",
        "native-site-ptt-silence",
        "native-cold",
        "native-reopen",
        "native-two-live",
        "native-confirm",
        "native-confirm-failure",
        "native-remove",
        "native-disable",
        "native-stop",
        "native-inspection-registered",
        "inspection-r1",
        "native-bookkeeping",
        "native-repair",
        "native-new-fault",
        "native-local",
        "managed-bootstrap",
        "managed-maintenance",
        "managed-offline-first",
        "managed-backup-first",
        "public-pending-backup",
        "station-keyboard",
        "station-serial",
        "station-invalid",
        "station-sparse-keyboard",
        "station-invalid-serial",
        "station-source-fallback",
    ] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CASE, case)
            .status()
            .unwrap();
        if !status.success() {
            failed.push(case);
        }
    }
    assert!(failed.is_empty(), "review cases failed {failed:?}");
}

#[test]
fn corrected_reload_requires_a_settled_publication_and_known_rollback() {
    let (_fixture, store) = Fixture::new();
    let guard = store.try_edit().unwrap();
    let tx = Transaction::new(&guard, LIMITS);
    first(&tx);
    let selected = tx.inspect().unwrap();
    let (manifest, bytes) = prepared("second");
    let site = SITE.parse().unwrap();
    tx.commit(
        Admission::Save {
            state: selected.digest,
        },
        &tx.inventory().unwrap(),
        &manifest,
        &[(site, &bytes)],
        "88010a36-0761-47ce-9205-e817111530fd".parse().unwrap(),
    )
    .unwrap();
    let (next, bytes) = prepared("third");
    let reject = |reload: bool| {
        let selected = tx.inspect().unwrap();
        let working = tx.inventory().unwrap();
        let admission = if reload {
            Admission::Reload {
                state: selected.digest,
                working: digest(&working),
            }
        } else {
            Admission::Save {
                state: selected.digest,
            }
        };
        let result = tx.commit(
            admission,
            &working,
            &next,
            &[(site, &bytes)],
            "49ec3915-0dda-45fe-af75-d7e6185674cb".parse().unwrap(),
        );
        assert_eq!(result, Err(TransactionError::Conflict));
        assert_eq!(tx.inspect().unwrap().digest, selected.digest);
    };
    reject(true); // Journal is selected.
    tx.reconcile().unwrap();
    reject(true); // Owner decision is still pending.
    tx.record_activation(&tx.inspect().unwrap(), Activation::RecoveryRequired)
        .unwrap();
    reject(true); // Rollback has not been confirmed.
    tx.record_activation(&tx.inspect().unwrap(), Activation::Failed)
        .unwrap();
    reject(false); // Ordinary authoring still requires confirmed adoption.
}

struct Stop<'a>(&'a AtomicBool);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Scenario<'a> {
    fixture: &'a Fixture,
    listener: &'a Listener,
    inbox: &'a mut CoordinatorInbox,
    client: &'a Client<'a>,
}

impl Scenario<'_> {
    fn serve<T>(
        &mut self,
        inputs: ProfileInputs,
        run: impl FnOnce(&Client<'_>, &RuntimeFixture) -> T,
    ) -> T {
        let store = self.fixture.managed_store().unwrap();
        self.serve_with_store(store, inputs, run)
    }

    fn serve_with_store<T>(
        &mut self,
        store: Arc<ManagedStore>,
        inputs: ProfileInputs,
        run: impl FnOnce(&Client<'_>, &RuntimeFixture) -> T,
    ) -> T {
        let rt = Arc::new(RuntimeFixture::default());
        let app = Coordinator::with_profile_inputs(
            store,
            KeyId::new("example").unwrap(),
            "/api/v1",
            rt.clone(),
            Arc::new(inputs),
        )
        .unwrap();
        let stopping = AtomicBool::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                while !stopping.load(Ordering::Acquire) {
                    if !app.poll_listener(self.listener, self.inbox) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            let _stop = Stop(&stopping);
            run(self.client, &rt)
        })
    }
}

fn inputs() -> ProfileInputs {
    ProfileInputs {
        messages: rpt_advanced_core::messages::InstalledMessageSources::capture(),
        interfaces: BTreeMap::new(),
    }
}
fn edit(client: &Client<'_>, method: &str, path: &str, body: Value) -> Reply {
    let result = client.edit(method, path, Some(&body));
    assert_eq!(
        result.code,
        if method == "POST" { 201 } else { 202 },
        "{method} {path} {}",
        result.body
    );
    client.ready();
    result
}
fn put(client: &Client<'_>, path: &str, fields: Value) {
    edit(client, "PUT", path, json!({"configuration":fields}));
}
fn port(client: &Client<'_>) -> String {
    edit(client,"POST",PORTS,json!({"key":"radio","configuration":{"name":"Radio","inherit_from":"site","enabled":false}})).headers["location"].clone()
}
fn effective(path: &str) -> String {
    path.replacen("/configuration/", "/effective/", 1)
}
fn operation(client: &Client<'_>, body: Value) -> Reply {
    let operations = client.get("/api/v1/operations/");
    assert_eq!(operations.code, 200, "{}", operations.body);
    let boot = operations.body["boot_id"]
        .as_str()
        .expect("current boot identity");
    let uuid = rptadv_peer_support::working_files::transaction::new_identity().unwrap();
    client.send(
        "PUT",
        &format!("/api/v1/operations/{boot}/{uuid}/"),
        Some(("If-None-Match", "*")),
        Some(&json!({"configuration":body})),
    )
}
fn recover(client: &Client<'_>, action: &str, target: &str) -> Reply {
    let mut body = json!({"submitted":true,"action":{"action":action},"desired_etag":client.etag(DESIRED),"working_fingerprint":client.get(STORAGE).body["working_fingerprint"]});
    if action == "restore_configuration" {
        body["restore"] = json!(target);
    }
    operation(client, body)
}

fn recovery(s: &mut Scenario<'_>, restore: bool) {
    let (port_id, port_path) = s.serve(inputs(), |client, _| {
        let loc = port(client);
        (
            client.get(&loc).body["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
            loc,
        )
    });
    let mut available = inputs();
    available.interfaces.insert(
        (
            rpt_advanced_core::config::resources::candidate::ResourceKind::Sensor,
            port_id,
            "/gpio/pins/1".into(),
        ),
        InterfaceCapability::Number,
    );
    let (sensor_id,sensor_path)=s.serve(available,|client,rt|{
        let lkg=client.get(DESIRED).body["desired_revision"].clone();
        rt.rollback.store(true,Ordering::Release);
        let sensor=client.edit("POST",&format!("{SITE_PATH}sensors/"),Some(&json!({"key":"meter","configuration":{"enabled":true,"source":{"type":"interface","node":"radio","property":"/gpio/pins/1"}}})));
        assert_eq!(sensor.code,201,"{}",sensor.body);
        let until=Instant::now()+Duration::from_secs(8);
        while client.get("/api/v1/health/").body["status"]["historical_activation"]!="failed"{assert!(Instant::now()<until);thread::sleep(Duration::from_millis(4));}
        assert_eq!(client.get("/api/v1/status/last-known-good/").body["last_known_good"],lkg);
        let location=sensor.headers["location"].clone();
        (location.trim_end_matches('/').rsplit('/').next().unwrap().to_owned(),location)
    });
    let root = s.fixture.0.clone();
    let before = crate::management::FULL_PREPARATIONS.load(Ordering::Relaxed);
    s.serve(inputs(), |client, rt| {
        assert!(
            crate::management::FULL_PREPARATIONS.load(Ordering::Relaxed) > before,
            "changed inputs require fresh preparation"
        );
        assert!(rt.inhibits.load(Ordering::Acquire) > 0);
        assert_eq!(client.get(&effective(&sensor_path)).code, 503);
        let tag = client.etag(DESIRED);
        assert!(
            client.get(STORAGE).body["working_fingerprint"].is_string(),
            "verified working state"
        );
        let selector = fs::read(root.join("state.json")).unwrap();
        let invalid = recover(
            client,
            if restore {
                "restore_configuration"
            } else {
                "reload_configuration"
            },
            "desired",
        );
        assert_eq!(invalid.code, 409, "{}", invalid.body);
        assert_eq!(fs::read(root.join("state.json")).unwrap(), selector);
        assert_eq!(client.etag(DESIRED), tag);
        assert_eq!(rt.adoptions.load(Ordering::Acquire), 0);
        if !restore {
            fs::remove_file(root.join(format!("working/site/sensors/{sensor_id}.json"))).unwrap();
        }
        let accepted = recover(
            client,
            if restore {
                "restore_configuration"
            } else {
                "reload_configuration"
            },
            "last_known_good",
        );
        assert_eq!(
            accepted.body["receipt"]["state"], "completed",
            "{}",
            accepted.body
        );
        client.ready();
        assert_eq!(rt.adoptions.load(Ordering::Acquire), 1);
        assert!(rt.prepared_credentials.load(Ordering::Acquire) > 0);
        assert!(rt.final_inspections.load(Ordering::Acquire) > 0);
        assert_eq!(client.get(&port_path).code, 200);
        assert_eq!(client.get(&sensor_path).code, 404);
        assert!(
            rt.prepared_domain.lock().unwrap().upgrade().is_some(),
            "recovery views must retain the exact prepared runtime domain"
        );
    });
}

fn echo(client: &Client<'_>, rt: &RuntimeFixture) {
    let port = port(client);
    let site_messages = format!("{SITE_PATH}telemetry/messages/");
    let msg = edit(
        client,
        "POST",
        &site_messages,
        json!({"key":"hal-status","configuration":{"content":"READY"}}),
    )
    .headers["location"]
        .clone();
    let defaults = format!("{SITE_PATH}protocol-defaults/echolink/");
    put(
        client,
        &defaults,
        json!({"status_message":"{ hal-status }","enabled":false}),
    );
    let accounts = format!("{port}protocols/echolink/");
    assert!(
        client.get(&accounts).body["_links"]["item"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let account =
        edit(client, "POST", &accounts, json!({"configuration":{}})).headers["location"].clone();
    let view = effective(&account);
    assert_eq!(
        client.get(&view).body["_links"]["rpt:messages"][0]["href"],
        msg
    );
    let local = edit(
        client,
        "POST",
        &format!("{port}telemetry/messages/"),
        json!({"key":"hal-status","configuration":{"content":"LOCAL"}}),
    )
    .headers["location"]
        .clone();
    assert_eq!(
        client.get(&view).body["_links"]["rpt:messages"][0]["href"],
        local
    );
    assert_eq!(
        client.get(&effective(&defaults)).body["_links"]["rpt:messages"][0]["href"],
        msg
    );
    put(
        client,
        &account,
        json!({"status_message":"{ hal-status }","enabled":false}),
    );
    assert_eq!(
        client.get(&view).body["_links"]["rpt:messages"][0]["href"],
        local
    );
    let tag = client.etag(DESIRED);
    let count = rt.adoptions.load(Ordering::Acquire);
    for (method, path, body) in [
        ("DELETE", &msg, None),
        (
            "PUT",
            &local,
            Some(json!({"configuration":{"tone":{"sequence":"440/20"}}})),
        ),
        (
            "PATCH",
            &account,
            Some(
                json!([{"op":"add","path":"/configuration/status_message","value":"{ missing-hal-message }"}]),
            ),
        ),
    ] {
        let rejected = client.edit(method, path, body.as_ref());
        assert_eq!(rejected.code, 409, "{method} {path} {}", rejected.body);
        assert_eq!(client.etag(DESIRED), tag);
        assert_eq!(rt.adoptions.load(Ordering::Acquire), count);
    }
}

fn audio(client: &Client<'_>) {
    let port = port(client);
    let site = format!("{SITE_PATH}rx/audio/");
    let local = format!("{port}rx/audio/");
    put(
        client,
        &site,
        json!({"processing":{"compressor":{"defaults":{"ratio":3,"timing":{"attack_ms":25}}},"multiband_limiter":{"defaults":{"ratio":4}}}}),
    );
    put(
        client,
        &local,
        json!({"processing":{"compressor":{"mid":{"ratio":3},"high":{"ratio":5,"timing":{"attack_ms":27}}}}}),
    );
    let doc = client.get(&effective(&local)).body;
    for (path, value, href) in [
        ("/processing/compressor/low/ratio", 3.0, &site),
        ("/processing/compressor/mid/ratio", 3.0, &local),
        ("/processing/compressor/high/ratio", 5.0, &local),
        ("/processing/compressor/low/timing/attack_ms", 25.0, &site),
        ("/processing/compressor/high/timing/attack_ms", 27.0, &local),
        ("/processing/multiband_limiter/mid/ratio", 4.0, &site),
    ] {
        assert_eq!(
            doc["effective"].pointer(path).and_then(Value::as_f64),
            Some(value)
        );
        assert_eq!(doc["meta"]["sources"][path]["href"], *href, "{path}");
        assert_eq!(client.get(href).code, 200);
    }
}

fn validate_document(schema: &Value, document: &Value) {
    // The staged quality fixture supplies Debian's 2020-12 validator.
    if let Some(python) = std::env::var_os("RPTADV_HAL_SCHEMA_PYTHON") {
        let mut checker=Command::new(python).args(["-c",r#"
import json,sys
from decimal import Decimal
from jsonschema import Draft202012Validator
pair=json.load(sys.stdin,parse_float=Decimal)
schema=pair["schema"]
Draft202012Validator.check_schema(schema)
Draft202012Validator(schema,format_checker=Draft202012Validator.FORMAT_CHECKER).validate(pair["document"])
"#]).stdin(std::process::Stdio::piped()).spawn().unwrap();
        serde_json::to_writer(
            checker.stdin.take().unwrap(),
            &json!({"schema":schema,"document":document}),
        )
        .unwrap();
        assert!(
            checker.wait().unwrap().success(),
            "served schema rejected document"
        );
    }
}
fn checked_schema(client: &Client<'_>, template: &str, path: &str) -> (Value, Value) {
    let links = client.get("/api/v1/openapi.json").body;
    let href=links["paths"][template]["get"]["responses"]["200"]["content"]["application/hal+json"]["schema"]["$ref"].as_str().unwrap();
    let schema = client.get(href);
    let doc = client.get(path);
    assert_eq!(schema.code, 200);
    assert_eq!(doc.code, 200);
    validate_document(&schema.body, &doc.body);
    eprintln!("Validated complete authenticated response at {path} against {href}");
    (doc.body, schema.body)
}
fn event_timezones(client: &Client<'_>) {
    let port = port(client);
    edit(
        client,
        "POST",
        &format!("{port}command/macros/"),
        json!({"key":"notice","configuration":{"actions":[{"action":"pause"}]}}),
    );
    let create = |fields: Value| {
        edit(client, "POST", &format!("{port}schedule/events/"), fields).headers["location"].clone()
    };
    let calendar = create(json!({
        "key":"calendar",
        "start":{"at":"every day at 19:00 in America/Chicago","macro":"notice",
            "warnings":[{"at":"every day at 18:55 in Europe/London","macro":"notice"}]},
        "end":{"at":"every day at 20:00 in America/New_York","macro":"notice"}
    }));
    let (doc, schema) = checked_schema(
        client,
        "/effective/ports/{port}/schedule/events/{member}/",
        &effective(&calendar),
    );
    assert_eq!(
        doc["meta"]["timezones"],
        json!({
            "/start/at":"America/Chicago",
            "/start/warnings/0/at":"Europe/London",
            "/end/at":"America/New_York"
        })
    );
    assert_eq!(doc["meta"]["sources"]["/start/warnings"]["href"], calendar);
    assert_eq!(
        schema["properties"]["meta"]["properties"]["timezones"]["readOnly"],
        true
    );
    let authored = client.get(&calendar).body;
    assert!(authored["meta"].get("timezones").is_none());
    assert_eq!(
        authored["start"]["at"],
        "every day at 19:00 in America/Chicago"
    );

    let startup = create(json!({
        "key":"startup",
        "start":{"at":"controller_start","macro":"notice"}
    }));
    let doc = client.get(&effective(&startup));
    assert_eq!(doc.code, 200);
    assert!(doc.body["meta"].get("timezones").is_none());
}

fn audience_schemas(client: &Client<'_>, port: &str) {
    let create = |at: &str, family: &str, key: &str, configuration: Value| {
        edit(
            client,
            "POST",
            &format!("{at}telemetry/{family}/"),
            json!({"key":key,"configuration":configuration}),
        )
        .headers["location"]
            .clone()
    };
    let message = create(
        SITE_PATH,
        "messages",
        "notice",
        json!({"morse":{"text":"CQ"},"playback":{"destination":"peers"}}),
    );
    let inherited = create(
        SITE_PATH,
        "identifiers",
        "id",
        json!({"morse":{"text":"CQ"},"playback":{"destination":"local_and_links"}}),
    );
    let id = create(
        port,
        "identifiers",
        "id",
        json!({"playback":{"destination":"peers","repeat":false}}),
    );
    let tail = create(
        port,
        "tails",
        "tail",
        json!({"content":"{ notice }","playback":{"destination":"peers"}}),
    );
    for (resource, route, key, expected) in [
        (
            &message,
            "site/telemetry/messages/{member}/",
            Some("peers"),
            "peers",
        ),
        (
            &inherited,
            "site/telemetry/identifiers/{member}/",
            Some("local_and_links"),
            "local_and_links",
        ),
        (
            &id,
            "ports/{port}/telemetry/identifiers/{member}/",
            None,
            "local_and_links",
        ),
        (
            &tail,
            "ports/{port}/telemetry/tails/{member}/",
            None,
            "local",
        ),
    ] {
        let (author, _) = checked_schema(client, &format!("/configuration/{route}"), resource);
        assert_eq!(
            author
                .pointer("/configuration/playback/destination")
                .and_then(Value::as_str),
            key
        );
        let (current, _) =
            checked_schema(client, &format!("/effective/{route}"), &effective(resource));
        assert_eq!(current["effective"]["playback"]["destination"], expected);
    }
}
fn schemas(client: &Client<'_>) {
    let limits = format!("{SITE_PATH}limits/");
    let template = "/effective/site/limits/";
    let (empty, schema) = checked_schema(client, template, &effective(&limits));
    assert_eq!(empty["effective"]["modes"], json!([]));
    for key in ["modes", "rx_ranges_mhz"] {
        assert_eq!(
            schema["properties"]["effective"]["properties"][key]["minItems"]
                .as_u64()
                .unwrap_or(0),
            0,
            "{key} effective empty allowed"
        );
        assert_eq!(
            client.get("/api/v1/schemas/limits/").body["properties"]["configuration"]["properties"]
                [key]["minItems"],
            1
        );
    }
    put(
        client,
        &limits,
        json!({"modes":["FM"],"rx_ranges_mhz":[[144,148]]}),
    );
    let (configured, _) = checked_schema(client, template, &effective(&limits));
    assert_eq!(configured["effective"]["modes"], json!(["FM"]));
    assert_eq!(
        configured["effective"]["rx_ranges_mhz"][0][0].as_f64(),
        Some(144.0)
    );
    let port = port(client);
    audience_schemas(client, &port);
    let accounts = format!("{port}protocols/allstarlink/");
    let loc=edit(client,"POST",&accounts,json!({"configuration":{"node_number":1234,"iax":{"registration_url":""},"status_post":{"url":""}}})).headers["location"].clone();
    let template = "/effective/ports/{port}/protocols/allstarlink/{member}/";
    let (disabled, _) = checked_schema(client, template, &effective(&loc));
    for (parent, key) in [("iax", "registration_url"), ("status_post", "url")] {
        assert!(disabled["effective"][parent].get(key).is_none());
    }
    put(
        client,
        &loc,
        json!({"iax":{"registration_url":"https://node.example/iax"},"status_post":{"url":"https://node.example/status"}}),
    );
    let (enabled, _) = checked_schema(client, template, &effective(&loc));
    assert_eq!(
        enabled["effective"]["iax"]["registration_url"],
        "https://node.example/iax"
    );
    assert_eq!(
        enabled["effective"]["status_post"]["url"],
        "https://node.example/status"
    );
}
fn poll(client: &Client<'_>, fixture: &Fixture, rt: &RuntimeFixture) {
    let count = || crate::management::FULL_PREPARATIONS.load(Ordering::Relaxed);
    let before = count();
    let fingerprint = client.get(STORAGE).body["working_fingerprint"].clone();
    for path in [
        "/api/v1/health/",
        "/api/v1/status/",
        DESIRED,
        STORAGE,
        "/api/v1/changes/",
    ] {
        assert_eq!(client.get(path).code, 200);
    }
    assert_eq!(count(), before, "status reads rebuilt domain profiles");
    let file = "working/site/metadata.json";
    let original = fs::read(fixture.0.join(file)).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    changed["configuration"]["name"] = json!("external");
    fixture.put(file, &serde_json::to_vec(&changed).unwrap());
    assert_ne!(client.get(STORAGE).body["working_fingerprint"], fingerprint);
    assert_eq!(count(), before);
    fixture.put(file, &original);
    put(
        client,
        SITE_PATH,
        json!({"name":"new revision","inherit_from":null}),
    );
    assert!(count() > before);
    assert!(
        rt.prepared_domain.lock().unwrap().upgrade().is_some(),
        "published views must retain the exact prepared runtime domain"
    );
    let after = count();
    client.get("/api/v1/health/");
    assert_eq!(count(), after);
}

/// A read-only backup has already released its edit guard when the sole cold-start runs.
fn first_cold_start_after_backup(fixture: &Fixture) {
    let reader = WorkingFiles::open(&fixture.0).unwrap();
    let guard = reader.try_edit().unwrap();
    let store = fixture.managed_store().unwrap();
    drop(guard);
    let runtime = Arc::new(RuntimeFixture::default());
    let coordinator = Coordinator::for_service(
        store,
        KeyId::new("example").unwrap(),
        Arc::clone(&runtime),
        Arc::new(inputs()),
    )
    .unwrap();
    // Join the sole scheduled worker. This runtime fixture confirms adoption synchronously.
    drop(coordinator);
    assert_eq!(
        runtime.adoptions.load(Ordering::Acquire),
        1,
        "released backup edit guard prevented the first cold-start adoption"
    );
    assert_eq!(runtime.observed().active_revision.as_deref(), Some("first"));
    assert!(!fixture.0.join("operations.json").exists());
}

/// The public constructor has no cold-start worker to release a provisional lease.
fn public_pending_after_backup(scenario: &mut Scenario<'_>) {
    use crate::configuration_transaction::maintenance::execute_test_request as maintenance;

    let fixture = scenario.fixture;
    let root = &fixture.0;
    let owner = WorkingFiles::open(root).unwrap();
    let mut request = json!({"store_root":root,"store_uuid":STORE});
    request["expected_state_sha256"] =
        maintenance(1, &request, None).unwrap()["state_sha256"].clone();
    let pending = |point| {
        if point == Boundary::IntentBarrier {
            Err(TransactionError::StorageFault)
        } else {
            Ok(())
        }
    };
    assert!(maintenance(2, &request, Some(&pending)).is_err());
    let selector = fs::read(root.join("state.json")).unwrap();
    let backup = owner.try_edit().unwrap();
    assert_eq!(
        Transaction::new(&backup, LIMITS).ordinary_writer_admission(STORE.parse().unwrap()),
        Err(TransactionError::Conflict)
    );
    let store = fixture.managed_store().unwrap();
    drop(backup);
    scenario.serve_with_store(store, inputs(), |client, runtime| {
        let health = client.get("/api/v1/health/");
        assert_eq!(health.code, 200);
        assert_eq!(health.body["status"]["storage"], "storage_fault");
        assert!(health.body["status"]["active_revision"].is_null());
        assert_eq!(client.get("/api/v1/").code, 200);
        assert_eq!(client.get(SITE_PATH).code, 200);
        assert!(runtime.inhibits.load(Ordering::Acquire) > 0);
        let edit = client.edit(
            "PUT",
            SITE_PATH,
            Some(&json!({"configuration":{"name":"Blocked"}})),
        );
        assert!(matches!(edit.code, 409 | 503));
        assert_eq!(runtime.adoptions.load(Ordering::Acquire), 0);
        assert_eq!(runtime.dispatched.load(Ordering::Acquire), 0);
        assert!(!root.join("operations.json").exists());
        assert_eq!(fs::read(root.join("state.json")).unwrap(), selector);

        let lease = owner
            .try_writer_lease()
            .expect("pending public coordinator retained its provisional writer lease");
        drop(lease);
        maintenance(2, &request, None).unwrap();
        assert_eq!(client.get(SITE_PATH).code, 200);
        assert!(runtime.observed().active_revision.is_none());
        assert_eq!(runtime.adoptions.load(Ordering::Acquire), 0);
        assert!(!root.join("operations.json").exists());
    });

    // A busy initial public refresh can discard only a lease still awaiting admission.
    for admitted in [false, true] {
        let checked = admitted.then(|| fixture.managed_store().unwrap());
        let backup = owner.try_edit().unwrap();
        let store = checked.unwrap_or_else(|| fixture.managed_store().unwrap());
        let app = Coordinator::new(
            store,
            KeyId::new("example").unwrap(),
            "/api/v1",
            Arc::new(RuntimeFixture::default()),
        )
        .unwrap();
        let offline = owner.try_writer_lease();
        if admitted {
            assert!(matches!(offline, Err(CaptureError::Busy)));
        } else {
            assert!(offline.is_ok());
        }
        drop(app);
        drop(backup);
    }
}

pub(super) fn run(
    case: &str,
    fixture: &Fixture,
    listener: &Listener,
    inbox: &mut CoordinatorInbox,
    client: &Client<'_>,
) {
    let mut scenario = Scenario {
        fixture,
        listener,
        inbox,
        client,
    };
    match case {
        "restore" | "reload" => recovery(&mut scenario, case == "restore"),
        "echo" => scenario.serve(inputs(), echo),
        "audio" => scenario.serve(inputs(), |c, _| audio(c)),
        "schemas" => scenario.serve(inputs(), |c, _| schemas(c)),
        "event-timezones" => scenario.serve(inputs(), |c, _| event_timezones(c)),
        "live-status" => scenario.serve(inputs(), live_status::read),
        "maintenance-lease" => scenario.serve(inputs(), |client, _| maintenance::read(client)),
        "native-maintenance-session" => native::run(&mut scenario, case),
        "poll" => scenario.serve(inputs(), |c, rt| poll(c, fixture, rt)),
        "asterisk-managed" | "asterisk-private-pending" => {
            native::asterisk(&scenario, case == "asterisk-private-pending")
        }
        "standalone-managed" | "standalone-private-pending" => {
            native::standalone(&scenario, case == "standalone-private-pending")
        }
        "standalone-managed-direct-echo" => native::managed_echo::activation(&scenario),
        "standalone-managed-asl-activation" | "standalone-managed-asl-originate" => {
            native::managed_asl::activation(&scenario)
        }
        "native-private-authorized"
        | "native-private-auto-attach"
        | "native-private-retained-vad"
        | "native-private-managed-iax"
        | "native-private-inband"
        | "native-private-mailbox-access" => native::positive_phone(&scenario),
        "standalone-managed-phone-dial"
        | "standalone-managed-phone-pending"
        | "standalone-managed-phone-hangup"
        | "standalone-managed-phone-idle-rtp"
        | "standalone-managed-phone-progress"
        | "standalone-managed-phone-progress-terminal"
        | "standalone-managed-phone-rtp-handover" => {
            native::managed_phone_dial::run(&scenario, case)
        }
        "standalone-managed-phone-incoming" => native::managed_phone_incoming::run(&scenario),
        "managed-backup-first" => first_cold_start_after_backup(fixture),
        "public-pending-backup" => public_pending_after_backup(&mut scenario),
        "standalone-managed-phone-prompt-clock"
        | "standalone-managed-phone-login-timers"
        | "standalone-managed-phone-login-barge"
        | "standalone-managed-phone-mix-minus" => native::managed_phone_incoming::run(&scenario),
        "managed-bootstrap" | "managed-maintenance" | "managed-offline-first" | "inspection-r1" => {
            native::bootstrap(&scenario)
        }
        "native-runtime"
        | "native-site-resume-uncertain"
        | "native-site-zero"
        | "native-site-views"
        | "native-site-fence"
        | "native-site-fault-reload"
        | "native-site-claim-release"
        | "native-site-output-override"
        | "native-site-alarm-zero"
        | "native-site-alarm-live"
        | "native-site-action-fault"
        | "native-site-ptt-silence"
        | "native-inspection-registered"
        | "native-cold"
        | "native-reopen"
        | "native-two-live"
        | "native-confirm"
        | "native-confirm-failure"
        | "native-remove"
        | "native-disable"
        | "native-stop"
        | "native-bookkeeping"
        | "native-repair"
        | "native-new-fault"
        | "native-local" => native::run(&mut scenario, case),
        "station-keyboard" => scenario.serve(inputs(), |c, _| station::keyboard(c)),
        "station-serial" => scenario.serve(inputs(), |c, _| station::serial(c)),
        "station-invalid" => scenario.serve(inputs(), station::invalid),
        "station-sparse-keyboard" => scenario.serve(inputs(), station::sparse_keyboard),
        "station-invalid-serial" => scenario.serve(inputs(), station::invalid_serial),
        "station-source-fallback" => scenario.serve(inputs(), station::source_fallback),
        _ => panic!("invalid fixture case"),
    }
}
