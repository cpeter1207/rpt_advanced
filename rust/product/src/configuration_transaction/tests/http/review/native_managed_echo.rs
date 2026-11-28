//! Managed direct EchoLink through the foreground, isolated directory and direct UDP peer.
use super::*;
use std::net::UdpSocket;

fn local_pair() -> (UdpSocket, UdpSocket) {
    for _ in 0..32 {
        let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
        if let Some(port) = audio.local_addr().unwrap().port().checked_add(1) {
            if let Ok(control) = UdpSocket::bind(("127.0.0.1", port)) {
                return (audio, control);
            }
        }
    }
    panic!("no adjacent ephemeral UDP pair");
}

fn wait_marker(directory: &Path, name: &str, fixture: &mut Foreground) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !directory.join(name).is_file() {
        fixture.running(deadline);
    }
}

pub(in super::super) fn activation(scenario: &Scenario<'_>) {
    let parent =
        std::env::var_os("RPTADV_TEST_PARENT_NETNS").expect("outer net namespace identity");
    let here = fs::read_link("/proc/self/ns/net").unwrap();
    assert!(
        parent
            .to_str()
            .is_some_and(|id| id.starts_with("net:[") && id.ends_with(']'))
    );
    assert_ne!(here.as_os_str(), parent.as_os_str());
    let interfaces = fs::read_to_string("/proc/net/dev").unwrap();
    assert_eq!(
        interfaces
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim())
            .collect::<Vec<_>>(),
        ["lo"]
    );

    let (audio, control) = local_pair();
    let audio_port = audio.local_addr().unwrap().port();
    let events = scenario.client.pki.join("direct-echo-events");
    fs::create_dir(&events).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/managed_direct_echo.py");
    let mut peer = Foreground(
        Command::new("python3")
            .args(["-E", "-B"])
            .arg(script)
            .arg(&events)
            .arg(audio_port.to_string())
            .spawn()
            .unwrap(),
    );
    wait_marker(&events, "ready", &mut peer);

    let (provision, address) = provisioning(scenario);
    let document = scenario.client.pki.join("managed-direct-echo.json");
    fs::write(&document, serde_json::to_vec(&provision).unwrap()).unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    let mut process = managed_asl::start(&document);
    managed_asl::wait_service(&client, address, &mut process);
    let port = port(&client);
    let station = port.trim_end_matches('/').rsplit('/').next().unwrap();
    let accounts = format!("{port}protocols/echolink/");
    put(
        &client,
        &format!("{port}interface/"),
        json!({"audio":{"device_identifier":"3-1"}}),
    );
    put(
        &client,
        &format!("{SITE_PATH}protocol-defaults/echolink/"),
        json!({
            "enabled":true, "transport":"direct", "directory_servers":["127.0.0.3"],
            "bind_address":"127.0.0.1", "audio_port":audio_port,
            "timing":{"directory_refresh_ms":500}
        }),
    );
    drop((audio, control));
    let enable = json!({"name":"Radio","inherit_from":"site","enabled":true});
    let result = edit(&client, "PUT", &port, json!({"configuration":enable}));
    managed_asl::confirmed(
        &client,
        result.body["desired_revision"].as_str().unwrap(),
        station,
    );
    assert!(
        client.get(&accounts).body["_links"]["item"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !events.join("contact").exists(),
        "site defaults opened an Echo account"
    );
    let audio = UdpSocket::bind(("127.0.0.1", audio_port)).unwrap();
    let control = UdpSocket::bind(("127.0.0.1", audio_port + 1)).unwrap();
    put(
        &client,
        &port,
        json!({"name":"Radio","inherit_from":"site","enabled":false}),
    );
    let attach = client.edit(
        "POST",
        &accounts,
        Some(&json!({"configuration":{
            "enabled":true, "callsign":"K1ABC-R", "node_number":716701,
            "credentials":{"password":"fixture-writeonly"}
        }})),
    );
    assert_eq!(attach.code, 201);
    client.ready();
    let account = &attach.headers["location"];
    let public = client.get(account);
    assert_eq!(public.code, 200);
    assert!(!public.body.to_string().contains("fixture-writeonly"));
    assert!(
        !events.join("contact").exists(),
        "disabled radio opened an Echo account"
    );
    drop((audio, control));
    let accepted = client.edit("PUT", &port, Some(&json!({"configuration":enable})));
    if accepted.code != 202 {
        process.signal("-TERM");
        wait_stopped(&mut process);
    }
    assert_eq!(
        accepted.code, 202,
        "explicit managed direct EchoLink activation"
    );
    let revision = accepted.body["desired_revision"].as_str().unwrap();
    let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
    let status = account.replacen("/configuration/", "/status/", 1);
    let mut live_registration = true;
    for boot in 1..=2 {
        if boot == 2 {
            process = managed_asl::start(&document);
            managed_asl::wait_service(&client, address, &mut process);
        }
        managed_asl::confirmed(&client, revision, station);
        wait_marker(&events, &format!("fresh-{boot}"), &mut peer);
        live_registration &= live_status::wait_for(&client, &status, |doc| {
            let account = &doc["status"]["registration"];
            account["state"] == "online" && account["registered"] == true
        });
        let provider = fs::canonicalize(
            std::env::var_os("RPTADV_TEST_ECHO_PROVIDER").expect("selected real EchoLink provider"),
        )
        .unwrap();
        let maps = fs::read_to_string(format!("/proc/{}/maps", process.0.id())).unwrap();
        let suffix = format!(" {}", provider.display());
        assert!(
            maps.lines().any(|line| line.ends_with(&suffix)),
            "selected Echo provider unmapped"
        );
        let outcome = operation_outcome(
            &client,
            station,
            json!({"action":"connect", "mode":"transceive", "parameter":"3123456"}),
        );
        assert_eq!(outcome, "admitted");
        wait_marker(&events, &format!("pcm-{boot}"), &mut peer);
        process.signal("-TERM");
        wait_stopped(&mut process);
        if boot == 1 {
            wait_marker(&events, "logout-1", &mut peer);
        }
        assert!(store.try_writer_lease().is_ok());
        let audio = UdpSocket::bind(("127.0.0.1", audio_port)).unwrap();
        let control = UdpSocket::bind(("127.0.0.1", audio_port + 1)).unwrap();
        drop((audio, control));
    }
    wait_stopped(&mut peer);
    assert!(events.join("logout-2").exists());
    assert!(
        live_registration,
        "HAL must report actual EchoLink directory registration on both boots"
    );
}
