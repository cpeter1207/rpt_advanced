use super::{StatusSnapshot, request_url};
use rpt_advanced_core::link::{LinkStatus, Mode};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

fn link(name: &str, mode: Mode, connected: bool) -> LinkStatus {
    LinkStatus {
        name: name.to_owned(),
        mode,
        permanent: false,
        ended: !connected,
        retrying: false,
        paused: false,
        due_ms: None,
        topology_blocked: false,
    }
}

#[test]
fn request_includes_node_time_sequence_key_and_link_state() {
    let snapshot = StatusSnapshot::new(
        "1000",
        1_700_000_000,
        7,
        true,
        vec![
            link("2000", Mode::TRANSCEIVE, true),
            link("3000", Mode::MONITOR, true),
            link("4000", Mode::LOCAL_MONITOR, true),
            link("5000", Mode::TRANSCEIVE, false),
        ],
    );
    let url = request_url("https://stats.example/uhandler", &snapshot).unwrap();
    let pairs = url.query_pairs().collect::<Vec<_>>();

    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "node" && value == "1000")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "time" && value == "1700000000")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "seqno" && value == "7")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "keyed" && value == "1")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "nodes" && value == "T2000,R3000,L4000,C5000")
    );
}

#[test]
fn request_encodes_existing_query_and_reserved_values() {
    let snapshot = StatusSnapshot::new(
        "1000",
        42,
        9,
        false,
        vec![
            link("2000", Mode::TRANSCEIVE, true),
            link("3000", Mode::MONITOR, true),
        ],
    );
    let url = request_url(
        "https://stats.example/uhandler?site=west%20side&keep=a%26b",
        &snapshot,
    )
    .unwrap();
    let pairs = url.query_pairs().collect::<Vec<_>>();

    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "site" && value == "west side")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "keep" && value == "a&b")
    );
    assert!(
        pairs
            .iter()
            .any(|(key, value)| key == "nodes" && value == "T2000,R3000")
    );
    assert!(url.as_str().contains("nodes=T2000%2CR3000"));
}

#[test]
fn unsupported_status_fields_are_omitted() {
    let snapshot = StatusSnapshot::new("1000", 42, 1, false, Vec::new());
    let url = request_url("https://stats.example/uhandler", &snapshot).unwrap();
    let fields = url
        .query_pairs()
        .map(|(key, _)| key.into_owned())
        .collect::<Vec<_>>();

    assert!(fields.contains(&"node".to_owned()));
    assert!(fields.contains(&"time".to_owned()));
    assert!(fields.contains(&"seqno".to_owned()));
    assert!(fields.contains(&"keyed".to_owned()));
    assert!(fields.contains(&"nodes".to_owned()));
    for unsupported in [
        "apprptvers",
        "apprptuptime",
        "keytime",
        "totalkerchunks",
        "totalkeyups",
        "totaltxtime",
        "timeouts",
        "totalexecdcommands",
    ] {
        assert!(!fields.iter().any(|field| field == unsupported));
    }
}

#[test]
fn state_changes_coalesce_for_200_ms_and_keyed_refreshes_every_30_seconds() {
    let mut schedule = super::Schedule::with_sequence(60, 0);
    let initial = schedule.snapshot("1000", 0, 1, false, Vec::new()).unwrap();
    schedule.sent(0, &initial);
    assert!(
        schedule
            .snapshot("1000", 100, 2, true, Vec::new())
            .is_none()
    );
    assert!(
        schedule
            .snapshot("1000", 299, 3, true, Vec::new())
            .is_none()
    );
    let changed = schedule.snapshot("1000", 300, 4, true, Vec::new()).unwrap();
    schedule.sent(300, &changed);
    assert!(
        schedule
            .snapshot("1000", 30_299, 5, true, Vec::new())
            .is_none()
    );
    assert!(
        schedule
            .snapshot("1000", 30_300, 6, true, Vec::new())
            .is_some()
    );
}

#[test]
fn worker_posts_to_a_local_http_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let length = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..length]).into_owned();
        let _ = send.send(request);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let worker = super::StatusPostWorker::new().unwrap();
    worker.replace(
        1,
        vec![super::PostConfig {
            node: "1000".into(),
            url: format!("http://{address}/status"),
            interval_seconds: 60,
        }],
    );
    assert!(worker.submit(1, &StatusSnapshot::new("1000", 123, 1, false, Vec::new()),));
    let request = receive.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(request.starts_with("GET /status?node=1000&time=123&seqno=1&keyed=0&nodes= HTTP/1.1"));
    drop(worker);
    server.join().unwrap();
}

#[test]
fn worker_discards_snapshots_from_replaced_config_generations() {
    let worker = super::StatusPostWorker::new().unwrap();
    worker.replace(
        2,
        vec![super::PostConfig {
            node: "1000".into(),
            url: "http://127.0.0.1:1/status".into(),
            interval_seconds: 60,
        }],
    );
    assert!(!worker.submit(1, &StatusSnapshot::new("1000", 123, 1, false, Vec::new()),));
}

#[test]
fn submit_drops_when_mailbox_is_busy_instead_of_blocking_the_pump() {
    let worker = super::StatusPostWorker::new().unwrap();
    worker.replace(
        1,
        vec![super::PostConfig {
            node: "1000".into(),
            url: "http://127.0.0.1:1/status".into(),
            interval_seconds: 60,
        }],
    );
    let _mailbox = worker
        .mailbox
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(!worker.submit(1, &StatusSnapshot::new("1000", 123, 1, false, Vec::new()),));
}

#[test]
fn reload_preserves_sequence_and_empty_url_disables_publication() {
    let mut service = super::StatusPostService::default();
    let config = || {
        vec![super::PostConfig {
            node: "1000".into(),
            url: "http://127.0.0.1:1/status".into(),
            interval_seconds: 60,
        }]
    };
    service.configure(config());
    service.observe("1000", 0, 10, false, Vec::new());
    assert_eq!(service.sequences.get("1000"), Some(&1));
    service.configure(config());
    service.observe("1000", 1, 11, false, Vec::new());
    assert_eq!(service.sequences.get("1000"), Some(&2));
    service.configure(Vec::new());
    assert!(service.schedules.is_empty());
    assert!(service.worker.is_some());
    assert!(
        service
            .worker
            .as_ref()
            .unwrap()
            .mailbox
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .destinations
            .is_empty()
    );
}

#[test]
fn service_retries_a_status_if_nonblocking_submission_is_busy() {
    let mut service = super::StatusPostService::default();
    service.configure(vec![super::PostConfig {
        node: "1000".into(),
        url: "http://127.0.0.1:1/status".into(),
        interval_seconds: 60,
    }]);
    let mailbox = service.worker.as_ref().unwrap().mailbox.clone();
    let guard = mailbox.0.lock().unwrap_or_else(|error| error.into_inner());
    service.observe("1000", 0, 10, false, Vec::new());
    assert!(!service.sequences.contains_key("1000"));
    drop(guard);
    service.observe("1000", 1, 11, false, Vec::new());
    assert_eq!(service.sequences.get("1000"), Some(&1));
}

#[test]
fn service_recreates_a_worker_if_initial_thread_creation_was_unavailable() {
    let mut service = super::StatusPostService::default();
    service.configure(vec![super::PostConfig {
        node: "1000".into(),
        url: "http://127.0.0.1:1/status".into(),
        interval_seconds: 60,
    }]);
    drop(service.worker.take());

    service.observe("1000", 0, 10, false, Vec::new());

    assert!(service.worker.is_some());
    assert_eq!(service.sequences.get("1000"), Some(&1));
}

#[test]
fn stopping_a_worker_does_not_wait_for_its_in_flight_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, waiting) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        let _ = accepted.send(());
        std::thread::sleep(Duration::from_millis(300));
    });
    let worker = super::StatusPostWorker::new().unwrap();
    worker.replace(
        1,
        vec![super::PostConfig {
            node: "1000".into(),
            url: format!("http://{address}/status"),
            interval_seconds: 60,
        }],
    );
    assert!(worker.submit(1, &StatusSnapshot::new("1000", 123, 1, false, Vec::new()),));
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();

    let started = std::time::Instant::now();
    worker.request_stop();
    assert!(started.elapsed() < Duration::from_millis(100));

    drop(worker);
    server.join().unwrap();
}
