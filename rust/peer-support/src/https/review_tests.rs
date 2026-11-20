//! Regression coverage for nonblocking admission, bounded replies and listener health.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
fn state() -> Shared {
    Arc::new(Mutex::new(State {
        snapshot: None,
        pending: VecDeque::new(),
        last_id: 0,
        refresh: Arc::new(Notify::new()),
    }))
}

#[test]
fn review_empty_poll_does_not_wait_for_network_owner() {
    let shared = state();
    let mut inbox = CoordinatorInbox(shared.clone());
    let (held, ready) = mpsc::sync_channel(0);
    let (release, released) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _guard = shared.lock().unwrap();
        held.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(1)).is_ok()
    });
    ready.recv().unwrap();
    assert!(inbox.try_recv().is_none());
    let _ = release.send(());
    assert!(
        worker.join().unwrap(),
        "control poll waited for the network lock"
    );
}

#[test]
fn review_reply_uses_transport_version_and_no_extensions() {
    let mut reply = Response::builder()
        .version(hyper::Version::HTTP_2)
        .status(201)
        .header("etag", "\"v1\"")
        .body(b"ok".to_vec())
        .unwrap();
    reply
        .extensions_mut()
        .insert(hyper::ext::ReasonPhrase::from_static(b"Saved"));
    let wire = http::encode(reply);
    assert_eq!(wire.version(), hyper::Version::HTTP_11);
    assert!(wire.extensions().is_empty());
    assert_eq!(wire.status(), 201);
    assert_eq!(wire.headers()["etag"], "\"v1\"");
}

#[test]
fn review_accept_failure_is_coalesced_recovered_and_stoppable() {
    let shared = state();
    let health = Arc::new(Health::default());
    let (stop, stopped) = oneshot::channel();
    let attempts = Arc::new(AtomicU32::new(0));
    let failing = Arc::new(AtomicBool::new(true));
    let calls = attempts.clone();
    let inject = failing.clone();
    let worker_health = health.clone();
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _peer = tokio::net::TcpStream::connect(socket.local_addr().unwrap())
                .await
                .unwrap();
            http::serve(
                async || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    if inject.load(Ordering::Acquire) {
                        return Err(std::io::ErrorKind::Other.into());
                    }
                    socket.accept().await
                },
                shared,
                OriginPolicy::new(vec!["localhost".into()], vec![]).unwrap(),
                stopped,
                worker_health,
            )
            .await;
        });
    });
    let until = Instant::now() + Duration::from_secs(2);
    while attempts.load(Ordering::Relaxed) < 3 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    let fault = health.take();
    let repeated = health.take();
    failing.store(false, Ordering::Release);
    // No trust snapshot in this fixture: after one healthy accept the worker exits.
    worker.join().unwrap();
    drop(stop);
    assert!(fault.accept_unavailable());
    assert_eq!(
        fault.events().collect::<Vec<_>>(),
        [TransportEvent::AcceptFailed]
    );
    assert!(repeated.accept_unavailable());
    assert_eq!(repeated.events().count(), 0);
    let recovery = health.take();
    assert!(!recovery.accept_unavailable());
    assert_eq!(
        recovery.events().collect::<Vec<_>>(),
        [TransportEvent::AcceptRecovered]
    );
}
#[test]
fn review_stop_joins_during_persistent_failure_and_reports_abnormal_task_once() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let health = Arc::new(Health::default());
        let (stop, stopped) = oneshot::channel();
        let (failed, mut first) = tokio::sync::mpsc::channel(1);
        let worker = tokio::spawn(http::serve(
            async move || {
                let _ = failed.try_send(());
                Err(std::io::ErrorKind::Other.into())
            },
            state(),
            OriginPolicy::new(vec!["localhost".into()], vec![]).unwrap(),
            stopped,
            health.clone(),
        ));
        first.recv().await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(health.take().accept_unavailable());
        for _ in 0..2 {
            let task = tokio::spawn(std::future::pending::<()>());
            task.abort();
            health.joined(task.await);
        }
        assert_eq!(
            health.take().events().collect::<Vec<_>>(),
            [TransportEvent::ConnectionTaskFailed]
        );
        assert_eq!(health.take().events().count(), 0);
    });
}
