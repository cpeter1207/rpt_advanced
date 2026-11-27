//! Exercise signaling ownership with real principals admitted through one proxy leaf.
use super::*;
use rptadv_peer_support::https::{
    AdmittedRequest, CertificateDer, CoordinatorInbox, Listener, OriginPolicy, PrivateKeyDer,
    Response, ServerIdentity, ServiceId, ServiceTrust,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

struct BrowserFixture {
    listener: Listener,
    inbox: CoordinatorInbox,
    temp: Temp,
}

fn browser_fixture() -> BrowserFixture {
    let path = Command::new("mktemp").arg("-d").output().unwrap();
    assert!(path.status.success());
    let temp = Temp(String::from_utf8(path.stdout).unwrap().trim().into());
    assert!(
        Command::new("sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../peer-support/tests/https-pki.sh"
            ))
            .arg(&temp.0)
            .output()
            .unwrap()
            .status
            .success()
    );
    fn certificate(dir: &Path, file: &str) -> CertificateDer<'static> {
        let der = Command::new("openssl")
            .args(["x509", "-outform", "DER", "-in"])
            .arg(dir.join(file))
            .output()
            .unwrap();
        assert!(der.status.success());
        CertificateDer::from(der.stdout)
    }
    let key = Command::new("openssl")
        .args(["pkcs8", "-topk8", "-nocrypt", "-outform", "DER", "-in"])
        .arg(temp.0.join("server.key"))
        .output()
        .unwrap();
    assert!(key.status.success());
    let proxy_pin = ServiceTrust::leaf_pin(&certificate(&temp.0, "nginx.pem"));
    let trust = ServiceTrust::new(
        vec![certificate(&temp.0, "client.pem")],
        vec![(proxy_pin, ServiceId::new("fixture-proxy").unwrap())],
    )
    .unwrap()
    .with_browser_principal_proxies([proxy_pin])
    .unwrap();
    let (listener, inbox) = Listener::start(
        "127.0.0.1:0".parse().unwrap(),
        ServerIdentity::new(
            vec![certificate(&temp.0, "server.pem")],
            PrivateKeyDer::try_from(key.stdout).unwrap(),
        ),
        trust,
        OriginPolicy::new(vec!["localhost".into()], Vec::new()).unwrap(),
    )
    .unwrap();
    BrowserFixture {
        listener,
        inbox,
        temp,
    }
}

impl BrowserFixture {
    fn request<T>(
        &mut self,
        subject: Option<&str>,
        method: &str,
        path: &str,
        body: Option<&str>,
        handle: impl FnOnce(AdmittedRequest) -> T,
    ) -> T {
        let mut curl = Command::new("curl");
        curl.args([
            "--silent",
            "--max-time",
            "5",
            "--noproxy",
            "*",
            "--request",
            method,
        ])
        .args(["--header", "Host: localhost", "--cacert"])
        .arg(self.temp.0.join("backend.pem"))
        .arg("--cert")
        .arg(self.temp.0.join("nginx.pem"))
        .arg("--key")
        .arg(self.temp.0.join("nginx.key"));
        if let Some(subject) = subject {
            curl.args([
                "--header",
                "X-Rpt-Advanced-Oidc-Issuer: https://fixture.local/realms/test",
                "--header",
                &format!("X-Rpt-Advanced-Oidc-Subject: {subject}"),
            ]);
        }
        if let Some(body) = body {
            curl.args([
                "--header",
                "Content-Type: application/hal+json",
                "--data-binary",
                body,
            ]);
        }
        let mut curl = curl
            .arg(format!(
                "https://localhost:{}{path}",
                self.listener.local_addr().port()
            ))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        let request = loop {
            if let Some(request) = self.inbox.try_recv() {
                break request;
            }
            assert!(curl.try_wait().unwrap().is_none());
            assert!(
                Instant::now() < deadline,
                "browser principal did not arrive"
            );
            thread::sleep(Duration::from_millis(2));
        };
        let result = handle(request);
        assert!(curl.wait().unwrap().success());
        result
    }
}

fn principals() -> [BrowserPrincipal; 2] {
    let mut fixture = browser_fixture();
    ["first-browser", "second-browser"].map(|subject| {
        fixture.request(Some(subject), "GET", "/", None, |request| {
            let principal = request.browser_principal().unwrap().clone();
            request.respond(Response::builder().status(204).body(Vec::new()).unwrap());
            principal
        })
    })
}

fn material(bytes: &[u8]) -> SecretBytes {
    SecretBytes::new(bytes.to_vec())
}

#[test]
fn webrtc_session_work_binds_browser_port_runtime_and_ice_generation() {
    let [principal, other] = principals();
    let port = fresh().unwrap();
    let now = Instant::now();
    let until = now + Duration::from_secs(1);
    let active = Observed {
        status_ready: true,
        active_revision: Some("revision-1".into()),
        owners: [(port, 42)].into(),
        ..Observed::default()
    };
    let first = Access {
        principal: &principal,
        active: &active,
        now,
    };
    let second = Access {
        principal: &other,
        ..first
    };
    let receive = Approval {
        direction: Direction::ReceiveOnly,
        until,
    };
    let mut book = Book::default();
    assert_eq!(
        book.offer(
            first,
            port,
            Direction::Transceive,
            receive,
            material(b"offer")
        )
        .err(),
        Some(Fault::Forbidden)
    );
    let offer_receive = |book: &mut Book| {
        book.offer(
            first,
            port,
            Direction::ReceiveOnly,
            receive,
            material(b"offer"),
        )
        .unwrap()
    };
    let offer = offer_receive(&mut book);
    let key = offer.key;
    assert_eq!(offer.direction, Direction::ReceiveOnly);
    assert_eq!(offer.sdp.expose_bytes(), b"offer");
    assert!(matches!(book.state(first, key), Ok(State::Pending(None))));
    assert!(matches!(book.state(second, key), Err(Fault::Forbidden)));
    assert!(matches!(
        book.state(
            first,
            WorkKey {
                port: fresh().unwrap(),
                ..key
            }
        ),
        Err(Fault::Forbidden)
    ));

    assert_eq!(
        book.candidate(
            first,
            key,
            0,
            Some(SecretBytes::new(vec![0; MAX_CANDIDATE_BYTES + 1]))
        )
        .err(),
        Some(Fault::TooLarge)
    );
    let candidate = book
        .candidate(first, key, 0, Some(material(b"candidate-a")))
        .unwrap()
        .unwrap();
    assert_eq!(
        candidate.contents.as_ref().unwrap().expose_bytes(),
        b"candidate-a"
    );
    assert!(
        book.candidate(first, key, 0, Some(material(b"candidate-a")))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        book.candidate(first, key, 0, Some(material(b"different")))
            .err(),
        Some(Fault::Conflict)
    );
    assert_eq!(
        book.candidate(first, key, 2, None).err(),
        Some(Fault::Conflict)
    );
    let end = book.candidate(first, key, 1, None).unwrap().unwrap();
    assert!(end.contents.is_none());
    assert_eq!(
        book.candidate(first, key, 2, None).err(),
        Some(Fault::Conflict)
    );
    assert_eq!(
        book.acknowledge_candidate(end.key, &active, now),
        Err(Fault::Conflict)
    );
    book.acknowledge_candidate(candidate.key, &active, now)
        .unwrap();
    book.acknowledge_candidate(candidate.key, &active, now)
        .unwrap();
    book.acknowledge_candidate(end.key, &active, now).unwrap();

    assert_eq!(book.activate(key, &active, now), Err(Fault::Conflict));
    book.answer_ready(key, &active, now, material(b"answer"))
        .unwrap();
    assert!(matches!(
        book.state(first, key),
        Ok(State::Pending(Some(sdp))) if sdp.expose_bytes() == b"answer"
    ));
    book.activate(key, &active, now).unwrap();
    assert!(
        matches!(book.state(first, key), Ok(State::Active(sdp)) if sdp.expose_bytes() == b"answer")
    );
    assert_eq!(
        book.restart(second, key, receive, material(b"another offer"))
            .err(),
        Some(Fault::Forbidden)
    );
    assert_eq!(
        book.restart(first, key, receive, material(b"")).err(),
        Some(Fault::Invalid)
    );
    assert!(matches!(book.state(first, key), Ok(State::Active(_))));
    let renewed = book
        .restart(first, key, receive, material(b"another offer"))
        .unwrap();
    assert_eq!(renewed.key.session, key.session);
    assert!(renewed.key.ice_generation > key.ice_generation);
    assert_eq!(
        book.current_key(first, port, key.session)
            .unwrap()
            .ice_generation,
        renewed.key.ice_generation
    );
    assert!(matches!(
        book.state(first, renewed.key),
        Ok(State::Pending(None))
    ));
    assert_eq!(book.activate(key, &active, now), Err(Fault::Conflict));
    assert_eq!(
        book.acknowledge_candidate(candidate.key, &active, now),
        Err(Fault::Conflict)
    );
    assert_eq!(book.revoke(key), Err(Fault::Conflict));

    let later = now + Duration::from_millis(1);
    let extended = Approval {
        until: until + Duration::from_secs(1),
        ..receive
    };
    book.renew(
        Access {
            now: later,
            ..first
        },
        renewed.key,
        extended,
    )
    .unwrap();
    assert!(matches!(
        book.state(
            Access {
                now: until,
                ..first
            },
            renewed.key
        ),
        Ok(State::Pending(None))
    ));
    book.close(first, renewed.key).unwrap();
    book.close(first, renewed.key).unwrap();
    assert!(matches!(book.state(first, renewed.key), Ok(State::Closed)));
    assert_eq!(book.activate(renewed.key, &active, now), Err(Fault::Gone));

    let transceive = Approval {
        direction: Direction::Transceive,
        until,
    };
    let tx = book
        .offer(
            first,
            port,
            Direction::Transceive,
            transceive,
            material(b"offer"),
        )
        .unwrap();
    assert_eq!(book.renew(first, tx.key, receive), Err(Fault::Forbidden));
    assert!(matches!(book.state(first, tx.key), Ok(State::Closed)));
    let revoked = offer_receive(&mut book);
    book.revoke(revoked.key).unwrap();
    assert!(matches!(book.state(first, revoked.key), Ok(State::Closed)));

    let expired = offer_receive(&mut book);
    assert_eq!(book.activate(expired.key, &active, until), Err(Fault::Gone));
    let stale = offer_receive(&mut book);
    let new_owner = Observed {
        owners: [(port, 43)].into(),
        ..active.clone()
    };
    assert_eq!(book.activate(stale.key, &new_owner, now), Err(Fault::Gone));
    let old_revision = offer_receive(&mut book);
    let new_revision = Observed {
        active_revision: Some("revision-2".into()),
        ..active.clone()
    };
    let mut retired = Vec::new();
    book.sweep(&new_revision, now, |key| retired.push(key));
    assert!(retired.contains(&old_revision.key));
    assert!(matches!(
        book.state(first, old_revision.key),
        Ok(State::Closed)
    ));
    let old = retired.len();
    book.sweep(&active, extended.until, |key| retired.push(key));
    assert_eq!(retired.len(), old);
    assert!(book.0.is_empty());
}

#[test]
fn webrtc_hal_keeps_wire_state_typed_and_unregistered() {
    let offer = hal::offer(br#"{"direction":"receive_only","offer":"v=0\r\n"}"#).unwrap();
    assert_eq!(offer.direction, Direction::ReceiveOnly);
    assert_eq!(offer.sdp.expose_bytes(), b"v=0\r\n");
    assert_eq!(
        hal::offer(br#"{"direction":"receive_only","offer":"first","offer":"second"}"#).err(),
        Some(Fault::Malformed)
    );
    assert_eq!(
        hal::offer(br#"{"direction":"transceive","offer":"v=0","principal":"forged"}"#).err(),
        Some(Fault::Invalid)
    );
    assert!(matches!(
        hal::update(br#"{"action":"renew","ice_generation":"2"}"#),
        Ok(hal::Update::Renew { ice_generation: 2 })
    ));
    let restart = hal::update(br#"{"action":"restart","ice_generation":"2","offer":"v=0"}"#);
    assert!(matches!(
        restart,
        Ok(hal::Update::Restart { ice_generation: 2, sdp }) if sdp.expose_bytes() == b"v=0"
    ));
    assert_eq!(
        hal::update(br#"{"action":"restart","ice_generation":"02","offer":"v=0"}"#).err(),
        Some(Fault::Invalid)
    );

    let parsed =
        hal::candidate(br#"{"ice_generation":"2","sequence":0,"candidate":"candidate:fixture"}"#)
            .unwrap();
    assert_eq!((parsed.ice_generation, parsed.sequence), (2, 0));
    assert_eq!(
        parsed.contents.unwrap().expose_bytes(),
        b"candidate:fixture"
    );
    let end = hal::candidate(br#"{"ice_generation":"2","sequence":1,"candidate":null}"#).unwrap();
    assert!(end.contents.is_none());
    assert_eq!(hal::close_generation(Some("ice_generation=2")), Ok(2));
    assert_eq!(
        hal::close_generation(Some("ice_generation=0")),
        Err(Fault::Invalid)
    );
    assert_eq!(hal::close_generation(None), Err(Fault::Invalid));

    let api = crate::management::Api::new("/api/v1").unwrap();
    let boot = fresh().unwrap();
    let key = WorkKey {
        session: fresh().unwrap(),
        port: fresh().unwrap(),
        ice_generation: 2,
    };
    let collection = hal::collection_document(&api, key.port);
    let location = hal::collection_link(&api, key.port);
    assert_eq!(collection["_links"]["self"], location);
    assert!(api.lookup(location["href"].as_str().unwrap()).is_err());
    let pending = State::Pending(Some(material(b"v=0\r\n")));
    let result =
        serde_json::to_value(hal::session_document(&api, boot, key, &pending).unwrap()).unwrap();
    assert_eq!(result["phase"], "pending");
    assert_eq!(result["answer"], "v=0\r\n");
    assert_eq!(result["ice_generation"], "2");
    let self_url = result["_links"]["self"]["href"].as_str().unwrap();
    assert!(self_url.contains(&boot.to_string()));
    assert_eq!(
        result["_links"]["rpt:close"]["href"],
        format!("{self_url}?ice_generation=2")
    );
    assert_eq!(result["_links"]["up"], location);

    let items = [
        hal::LocalCandidate {
            sequence: 0,
            candidate: Some("candidate:fixture"),
        },
        hal::LocalCandidate {
            sequence: 1,
            candidate: None,
        },
    ];
    let page = serde_json::to_value(hal::candidate_document(&api, boot, key, &items)).unwrap();
    assert_eq!(page["_links"]["up"], result["_links"]["self"]);
    assert_eq!(
        page["_links"]["self"],
        result["_links"]["rpt:webrtc-candidates"]
    );
    assert_eq!(page["candidates"][0]["sequence"], 0);
    assert!(page["candidates"][1]["candidate"].is_null());

    let closed =
        serde_json::to_value(hal::session_document(&api, boot, key, &State::Closed).unwrap())
            .unwrap();
    assert_eq!(closed["phase"], "closed");
    assert!(closed.get("answer").is_none());
    assert!(closed["_links"].get("rpt:webrtc-candidates").is_none());
}

#[derive(Default)]
struct AsyncFixture {
    receipts: std::collections::VecDeque<Receipt>,
    submissions: usize,
    offers: Vec<WorkKey>,
    renewed: Vec<WorkKey>,
    restarted: Vec<(WorkKey, WorkKey)>,
    closed: Vec<WorkKey>,
}

struct FixtureTransport(std::sync::Arc<std::sync::Mutex<AsyncFixture>>);
impl Transport for FixtureTransport {
    fn approve(
        &mut self,
        _: &BrowserPrincipal,
        _: Id,
        direction: Direction,
        revision: &str,
        station_generation: u64,
        now: Instant,
    ) -> Result<Approval, Fault> {
        assert_eq!((revision, station_generation), ("active", 41));
        Ok(Approval {
            direction,
            until: now + Duration::from_secs(20),
        })
    }

    fn offer(
        &mut self,
        work: OfferWork,
        revision: &str,
        station_generation: u64,
        _: Instant,
    ) -> Result<(), Fault> {
        assert_eq!((revision, station_generation), ("active", 41));
        assert_eq!(work.direction, Direction::ReceiveOnly);
        assert_eq!(work.sdp.expose_bytes(), b"v=0");
        let key = work.key;
        let mut fixture = self.0.lock().unwrap();
        fixture.offers.push(key);
        fixture.receipts.extend([
            Receipt::Answer {
                key,
                sdp: material(b"v=0\r\n"),
            },
            Receipt::LocalCandidate {
                key,
                sequence: 0,
                contents: Some(material(b"candidate:local")),
            },
            Receipt::LocalCandidate {
                key,
                sequence: 1,
                contents: None,
            },
        ]);
        Ok(())
    }

    fn restart(&mut self, previous: WorkKey, work: OfferWork, until: Instant) -> Result<(), Fault> {
        self.0.lock().unwrap().restarted.push((previous, work.key));
        self.offer(work, "active", 41, until)
    }

    fn candidate(&mut self, work: CandidateWork) -> Result<(), Fault> {
        assert_eq!(work.contents.unwrap().expose_bytes(), b"candidate:remote");
        let mut fixture = self.0.lock().unwrap();
        fixture.submissions += 1;
        fixture
            .receipts
            .push_back(Receipt::CandidateAccepted(work.key));
        Ok(())
    }

    fn renew(&mut self, key: WorkKey, _: Instant) -> Result<(), Fault> {
        self.0.lock().unwrap().renewed.push(key);
        Ok(())
    }

    fn close(&mut self, key: WorkKey) {
        let mut fixture = self.0.lock().unwrap();
        fixture.closed.push(key);
        fixture.receipts.push_back(Receipt::Closed(key));
    }

    fn next_receipt(&mut self) -> Option<Receipt> {
        self.0.lock().unwrap().receipts.pop_front()
    }
}

#[test]
fn webrtc_hal_routes_isolate_browsers_and_drive_exact_async_work() {
    use crate::management::{Api, StatusCode};
    use serde_json::Value;

    let mut browser = browser_fixture();
    let api = Api::with_webrtc("/api/v1", true).unwrap();
    let port = fresh().unwrap();
    let boot = fresh().unwrap();
    let live = Observed {
        status_ready: true,
        active_revision: Some("active".into()),
        owners: [(port, 41)].into(),
        ..Observed::default()
    };
    let fixture = std::sync::Arc::new(std::sync::Mutex::new(AsyncFixture::default()));
    let mut signaling = Signaling::new(boot, Box::new(FixtureTransport(fixture.clone())));
    let mut call = |owner, method, path, body, status| {
        assert!(
            signaling.poll(&live, Instant::now()) || fixture.lock().unwrap().receipts.is_empty()
        );
        browser.request(owner, method, path, body, |request| {
            let (route, params) = api.lookup(request.target.path()).unwrap();
            let reply = signaling.request(&request, route, params, &api, &live, true);
            let (code, data) = match reply {
                Ok(reply) => {
                    assert_eq!(reply.headers()["cache-control"], "no-store");
                    let code = reply.status();
                    let data = serde_json::from_slice::<Value>(reply.body()).unwrap();
                    if code == StatusCode::CREATED {
                        assert_eq!(
                            reply.headers()["location"].to_str().unwrap(),
                            data["_links"]["self"]["href"].as_str().unwrap(),
                        );
                    }
                    request.respond(reply);
                    (code, data)
                }
                Err(fault) => {
                    let code = fault.wire().0;
                    request.respond(Response::builder().status(code).body(Vec::new()).unwrap());
                    (code, Value::Null)
                }
            };
            assert_eq!(code, status);
            data
        })
    };
    let collection = format!("/api/v1/webrtc/ports/{port}/sessions/");
    let alice = Some("first-browser");
    let bob = Some("second-browser");
    call(None, "GET", &collection, None, StatusCode::FORBIDDEN);
    call(alice, "GET", &collection, None, StatusCode::OK);
    let offered = call(
        alice,
        "POST",
        &collection,
        Some(r#"{"direction":"receive_only","offer":"v=0"}"#),
        StatusCode::CREATED,
    );
    assert_eq!(offered["phase"], "pending");
    assert!(offered.get("answer").is_none());
    let session = offered["_links"]["self"]["href"].as_str().unwrap();
    let candidates = offered["_links"]["rpt:webrtc-candidates"]["href"]
        .as_str()
        .unwrap();
    call(bob, "GET", session, None, StatusCode::FORBIDDEN);
    let ready = call(alice, "GET", session, None, StatusCode::OK);
    assert_eq!(ready["answer"], "v=0\r\n");
    assert_eq!(ready["phase"], "pending");
    let page = call(alice, "GET", candidates, None, StatusCode::OK);
    assert_eq!(page["candidates"][0]["candidate"], "candidate:local");
    assert!(page["candidates"][1]["candidate"].is_null());
    let ice = Some(r#"{"ice_generation":"1","sequence":0,"candidate":"candidate:remote"}"#);
    for _ in 0..2 {
        call(alice, "POST", candidates, ice, StatusCode::OK);
    }
    {
        let mut provider = fixture.lock().unwrap();
        assert_eq!(provider.submissions, 1);
        let current = provider.offers[0];
        provider.receipts.push_back(Receipt::Attached(current));
    }
    assert_eq!(
        call(alice, "GET", session, None, StatusCode::OK)["phase"],
        "active"
    );
    call(
        alice,
        "PUT",
        session,
        Some(r#"{"action":"renew","ice_generation":"1"}"#),
        StatusCode::OK,
    );
    let restarted = call(
        alice,
        "PUT",
        session,
        Some(r#"{"action":"restart","ice_generation":"1","offer":"v=0"}"#),
        StatusCode::OK,
    );
    assert_eq!(restarted["ice_generation"], "2");
    assert!(restarted.get("answer").is_none());
    call(alice, "POST", candidates, ice, StatusCode::CONFLICT);
    let current = restarted["_links"]["rpt:close"]["href"].as_str().unwrap();
    let prior = offered["_links"]["rpt:close"]["href"].as_str().unwrap();
    call(alice, "DELETE", prior, None, StatusCode::CONFLICT);
    call(bob, "DELETE", current, None, StatusCode::FORBIDDEN);
    for _ in 0..2 {
        let closed = call(alice, "DELETE", current, None, StatusCode::OK);
        assert_eq!(closed["phase"], "closed");
        assert!(closed.get("answer").is_none());
    }
    let provider = fixture.lock().unwrap();
    assert_eq!(provider.renewed, provider.offers[..1]);
    assert_eq!(
        provider.restarted,
        [(provider.offers[0], provider.offers[1])]
    );
    assert_eq!(provider.closed, provider.offers[1..]);
}
