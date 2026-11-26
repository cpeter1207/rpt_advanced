//! Confirm the exact edit and numeric call on the actual standalone foreground.
use super::*;
use rpt_advanced_standalone::iax::{
    IaxClient, IaxEvent, IaxPeer,
    managed::{Authority, Incoming, Listeners},
};
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::PathBuf,
};

const LOCAL: &str = "716701";
const REMOTE: &str = "716799";

pub(super) fn start(document: &Path) -> Foreground {
    let directory = PathBuf::from(std::env::var_os("RPTADV_TEST_STATION_EDGE").unwrap())
        .canonicalize()
        .unwrap();
    for name in [
        "librptadv_portaudio_alsa_adapter.so.2",
        "librptadv_gpio_adapter.so.1",
    ] {
        let file = directory.join(name).canonicalize().unwrap();
        assert!(
            file.starts_with(&directory) && file.is_file(),
            "isolated station edge required"
        );
    }
    let prior = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
    let search = std::env::join_paths(
        std::iter::once(directory)
            .chain(std::env::split_paths(&prior).filter(|path| !path.as_os_str().is_empty())),
    )
    .unwrap();
    Foreground(
        Command::new(std::env::var_os("RPTADV_TEST_STANDALONE_BIN").unwrap())
            .arg("--foreground")
            .arg(document)
            .env("LD_LIBRARY_PATH", search)
            .spawn()
            .unwrap(),
    )
}

pub(super) fn wait_service(client: &Client<'_>, address: SocketAddr, process: &mut Foreground) {
    let until = Instant::now() + Duration::from_secs(15);
    while std::net::TcpStream::connect_timeout(&address, Duration::from_millis(40)).is_err() {
        process.running(until);
    }
    client.ready();
}

pub(super) fn confirmed(client: &Client<'_>, revision: &str, station: &str) {
    client.ready();
    let health = &client.get("/api/v1/health/").body["status"];
    assert_eq!(health["desired_revision"], revision);
    assert_eq!(health["active_revision"], revision);
    assert_eq!(health["historical_activation"], "confirmed");
    assert!(
        health["owners"][station]
            .as_u64()
            .is_some_and(|generation| generation != 0)
    );
}

fn reported(statistics: &TcpListener, nodes: &str, process: &mut Foreground) {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        match statistics.accept() {
            Ok((mut socket, _)) => {
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&socket).read_line(&mut request).unwrap();
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let target = request.split_ascii_whitespace().nth(1).unwrap();
                let url = url::Url::parse("http://127.0.0.1/")
                    .unwrap()
                    .join(target)
                    .unwrap();
                let select = |name: &str, value: &str| {
                    url.query_pairs()
                        .any(|(key, actual)| key == name && actual == value)
                };
                if select("node", LOCAL) && select("nodes", nodes) {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("statistics listener failed: {error}"),
        }
        process.running(until);
    }
}

fn directory_row(path: &Path, port: u16) {
    fs::write(
        path,
        format!("[extnodes]\n{REMOTE}=radio@127.0.0.1:{port}/{REMOTE},127.0.0.1\n"),
    )
    .unwrap();
}

struct Remote(Option<IaxPeer>);
impl Incoming for Remote {
    fn authorize(&mut self, local: Authority, remote: &str, source: &str) -> bool {
        local.wire.get() == 716799 && remote == LOCAL && source == "127.0.0.1"
    }
    fn accept(&mut self, _: Authority, _: &str, _: &str, peer: IaxPeer) {
        self.0 = Some(peer);
    }
}

fn originate(
    client: &Client<'_>,
    directory: &Path,
    station: &str,
    stats: &TcpListener,
    foreground: &mut Foreground,
) {
    use std::sync::mpsc;
    let (bound, address) = mpsc::sync_channel(1);
    let (audio, received) = mpsc::sync_channel(1);
    let (stay, done) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let client = IaxClient::load(Path::new("librptadviax2.so.2")).unwrap();
            let reserved = UdpSocket::bind("127.0.0.1:0").unwrap();
            let port = reserved.local_addr().unwrap().port();
            let local = Authority {
                station: [1; 16],
                account: [2; 16],
                generation: 1_u64.try_into().unwrap(),
                wire: 716799_u64.try_into().unwrap(),
                port: port.try_into().unwrap(),
            };
            let mut listeners = Listeners::default();
            listeners.prepare(None, local.generation, &[local]).unwrap();
            drop(reserved);
            listeners
                .apply(local.generation, &client, |_| Ok(()))
                .unwrap();
            listeners.publish(local.generation).unwrap();
            listeners.finish(local.generation).unwrap();
            bound.send(port).unwrap();
            let until = Instant::now() + Duration::from_secs(15);
            let mut remote = Remote(None);
            let mut pcm = vec![0.0; 4096];
            let mut text = [0; 2048];
            loop {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as u32;
                listeners.poll(now, &mut remote).unwrap();
                if let Some(peer) = &mut remote.0 {
                    match peer.poll(&mut pcm, &mut text).unwrap() {
                        IaxEvent::Audio(n)
                            if pcm[..n].iter().any(|sample| sample.abs() > 0.0001) =>
                        {
                            break;
                        }
                        IaxEvent::NeedCapacity(n) => pcm.resize(n, 0.0),
                        IaxEvent::Hangup => panic!("outbound call ended before fixture audio"),
                        _ => {}
                    }
                }
                assert!(
                    Instant::now() < until,
                    "no outgoing numeric station audio; accepted={}",
                    remote.0.is_some()
                );
                thread::sleep(Duration::from_millis(2));
            }
            audio.send(()).unwrap();
            let _ = done.recv_timeout(Duration::from_secs(15));
        });
        directory_row(
            directory,
            address.recv_timeout(Duration::from_secs(5)).unwrap(),
        );
        let outcome = operation_outcome(
            client,
            station,
            json!({"action":"connect","mode":"transceive","parameter":REMOTE}),
        );
        assert_eq!(outcome, "admitted");
        received
            .recv_timeout(Duration::from_secs(16))
            .expect("actual outgoing call and nonzero PCM");
        reported(stats, &format!("T{REMOTE}"), foreground);
        drop(stay);
        worker.join().unwrap();
    });
}

pub(in super::super) fn activation(scenario: &Scenario<'_>) {
    let (provision, address) = provisioning(scenario);
    let document = scenario.client.pki.join("managed-asl-service.json");
    fs::write(&document, serde_json::to_vec(&provision).unwrap()).unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    let mut process = start(&document);
    wait_service(&client, address, &mut process);
    let port = port(&client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    put(
        &client,
        &format!("{port}interface/"),
        json!({"audio":{"device_identifier":"3-1"}}),
    );
    let enable = json!({"name":"Radio","inherit_from":"site","enabled":true});
    let control = edit(&client, "PUT", &port, json!({"configuration":enable}));
    confirmed(
        &client,
        control.body["desired_revision"].as_str().unwrap(),
        station,
    );
    put(
        &client,
        &port,
        json!({"name":"Radio","inherit_from":"site","enabled":false}),
    );

    let reserved = UdpSocket::bind("127.0.0.1:0").unwrap();
    let iax = reserved.local_addr().unwrap();
    let directory = scenario.fixture.0.join("peer-directory");
    directory_row(&directory, 4569);
    let stats = TcpListener::bind("127.0.0.1:0").unwrap();
    stats.set_nonblocking(true).unwrap();
    let account = edit(
        &client,
        "POST",
        &format!("{port}protocols/allstarlink/"),
        json!({"configuration":{
            "node_number":716701, "allow_nodes":[REMOTE], "lookup_method":"file", "directory_file":directory,
            "iax":{"local_port":iax.port(),"registration_url":""},
            "status_post":{"url":format!("http://{}/stats",stats.local_addr().unwrap())}
        }}),
    );
    assert_ne!(
        station,
        account.headers["location"]
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap()
    );
    drop(reserved);
    let accepted = client.edit("PUT", &port, Some(&json!({"configuration":enable})));
    assert_eq!(accepted.code, 202, "{}", accepted.body);
    let revision = accepted.body["desired_revision"].as_str().unwrap();
    let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
    let account_status = account.headers["location"].replacen("/configuration/", "/status/", 1);
    let peer_status = format!(
        "{}peers/?offset=0&limit=1",
        port.replacen("/configuration/", "/status/", 1)
    );
    let mut registrar_disabled = true;
    let mut direct_peer_visible = true;
    for restart in [false, true] {
        if restart {
            process = start(&document);
            wait_service(&client, address, &mut process);
        }
        confirmed(&client, revision, station);
        reported(&stats, "", &mut process);
        registrar_disabled &= live_status::wait_for(&client, &account_status, |doc| {
            doc["status"]["registration"]["state"] == "disabled"
                && doc["status"]["owner_generation"]
                    .as_u64()
                    .is_some_and(|owner| owner != 0)
        });
        if std::env::var(CASE).as_deref() == Ok("standalone-managed-asl-originate") {
            originate(&client, &directory, station, &stats, &mut process);
            process.signal("-TERM");
            wait_stopped(&mut process);
            assert!(store.try_writer_lease().is_ok());
            drop(UdpSocket::bind(iax).unwrap());
            break;
        }
        let provider = IaxClient::load(Path::new("librptadviax2.so.2")).unwrap();
        let peer = provider.dial(iax, REMOTE, LOCAL, "", 4000).unwrap();
        assert!(peer.sample_rate_hz() > 0);
        reported(&stats, &format!("T{REMOTE}"), &mut process);
        direct_peer_visible &= live_status::wait_for(&client, &peer_status, |doc| {
            let peer = &doc["_embedded"]["item"][0];
            peer["type"] == "allstarlink"
                && peer["node"] == REMOTE
                && peer["muted"] == false
                && peer["active"].is_boolean()
        });
        process.signal("-TERM");
        wait_stopped(&mut process);
        drop(peer);
        assert!(store.try_writer_lease().is_ok());
        drop(UdpSocket::bind(iax).expect("standalone did not release the managed listener"));
    }
    assert!(
        registrar_disabled,
        "HAL must distinguish disabled ASL registration from an unavailable observation"
    );
    assert!(
        direct_peer_visible,
        "HAL must expose the actual direct ASL ingress and its mute state"
    );
}
