use super::*;
use crate::registration::{ManagedRegistrar, RegistrarIdentity, RegistrarState, RegistrationWorker, exchange};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[test]
fn withdrawn_registrar_releases_credential_while_its_lookup_is_pending() {
    let guard = Duration::from_secs(12);
    let (entered, lookup_started) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    let calls = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let count = Arc::clone(&calls);
    let finished_lookup = Arc::clone(&finished);
    let dns = Arc::new(DnsWorker::start(move |netloc| {
        count.fetch_add(1, Ordering::Relaxed);
        if netloc == "registrar.invalid:443" {
            entered.try_send(()).unwrap();
            held.recv_timeout(guard).expect("lookup fixture release");
            finished_lookup.store(true, Ordering::Release);
        }
        Err(io::ErrorKind::NotFound.into())
    }).unwrap());

    let identity = RegistrarIdentity {
        station: [1; 16], account: [2; 16], record_version: [3; 16],
        attempt: 1_u64.try_into().unwrap(),
    };
    let mut registrar = ManagedRegistrar::prepare(
        identity, 716701_u64.try_into().unwrap(), Some("https://registrar.invalid/"),
        4569_u16.try_into().unwrap(), 60, Some("fixture-only"),
    ).unwrap();
    let credential = Arc::downgrade(&registrar.target.secret);
    let state = Arc::clone(&registrar.state);
    let (started, start) = mpsc::sync_channel(1);
    let (observed, lookup_result) = mpsc::sync_channel(1);
    let requester = Arc::clone(&dns);
    registrar.start_with(|targets| RegistrationWorker::start_with(targets, move |target| {
        let deadline = Instant::now() + Duration::from_secs(2);
        started.try_send(deadline).unwrap();
        let requester = Arc::clone(&requester);
        let observed = observed.clone();
        exchange(target, deadline, |deadline| agent_with(deadline, move |netloc, deadline| {
            let result = receive_addresses(requester.submit(netloc, deadline)?, deadline);
            observed.try_send(result.as_ref().err().map(io::Error::kind)).unwrap();
            result
        }))
    })).unwrap();

    let deadline = start.recv_timeout(guard).unwrap();
    lookup_started.recv_timeout(guard).unwrap();
    let queued = dns.submit("queued.invalid:443", deadline).unwrap();
    let full = dns.submit("third.invalid:443", Instant::now() + guard).unwrap_err();
    assert_eq!(full.kind(), io::ErrorKind::WouldBlock);
    assert!(!registrar.withdraw());
    assert_eq!(lookup_result.recv_timeout(guard).unwrap(), Some(io::ErrorKind::TimedOut));
    let finish_by = Instant::now() + guard;
    while !registrar.withdraw() {
        assert!(Instant::now() < finish_by, "registration worker did not drain");
        thread::yield_now();
    }
    assert_eq!(registrar.state(), RegistrarState::Withdrawn);
    drop(registrar);
    assert!(credential.upgrade().is_none());
    assert!(!finished.load(Ordering::Acquire));

    release.send(()).unwrap();
    assert_eq!(queued.recv_timeout(guard).unwrap().unwrap_err().kind(), io::ErrorKind::TimedOut);
    let DnsWorker { jobs, _thread: worker } = Arc::try_unwrap(dns).ok().unwrap();
    drop(jobs);
    worker.join().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(state.load(Ordering::Acquire), crate::registration::WITHDRAWN);
}

#[test]
fn numeric_addresses_bypass_the_lookup_queue() {
    for address in ["127.0.0.1:443", "[::1]:8443"] {
        let actual = resolve_registration(address, || Duration::from_secs(1), |_| {
            panic!("a numeric address entered the system resolver")
        }).unwrap();
        assert_eq!(actual, vec![address.parse::<SocketAddr>().unwrap()]);
    }
}

#[test]
fn tls_transport_checks_the_remaining_budget_for_read_write_and_flush() {
    #[derive(Debug)]
    struct MemoryIo(Arc<[AtomicUsize; 3]>);
    impl Read for MemoryIo {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.0[0].fetch_add(1, Ordering::Relaxed);
            b"x".as_slice().read(output)
        }
    }
    impl Write for MemoryIo {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.0[1].fetch_add(1, Ordering::Relaxed);
            Ok(input.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0[2].fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    impl ureq::ReadWrite for MemoryIo {
        fn socket(&self) -> Option<&TcpStream> { None }
    }

    let calls = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)]);
    let start = Instant::now();
    let mut stream = DeadlineIo {
        inner: Box::new(MemoryIo(Arc::clone(&calls))),
        deadline: start + Duration::from_secs(5),
    };
    assert_eq!(stream.remaining_at(start).unwrap(), Duration::from_secs(5));
    let mut output = [0];
    assert_eq!(stream.read(&mut output).unwrap(), 1);
    assert_eq!(&output, b"x");
    assert_eq!(stream.remaining_at(start + Duration::from_secs(3)).unwrap(), Duration::from_secs(2));
    assert_eq!(stream.write(b"y").unwrap(), 1);
    stream.flush().unwrap();
    stream.deadline = start;
    assert_eq!(stream.read(&mut output).unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(stream.write(b"z").unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(stream.flush().unwrap_err().kind(), io::ErrorKind::TimedOut);
    let tls = DeadlineTls { deadline: start, config: tls_config() };
    let transport = Box::new(MemoryIo(Arc::clone(&calls)));
    assert!(ureq::TlsConnector::connect(&tls, "fixture.invalid", transport).is_err());
    assert!(calls.iter().all(|count| count.load(Ordering::Relaxed) == 1));
}
