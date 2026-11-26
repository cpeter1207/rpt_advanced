//! Pending outgoing ownership through the normal RF command and real SIP registrar.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Observation {
    admitted: usize,
    held: usize,
    threads: usize,
    acknowledged: usize,
    elapsed: std::time::Duration,
    admitted_mask: u32,
    invited_mask: u32,
}

impl Observation {
    pub(super) fn verify(self) {
        let Self {
            admitted,
            held,
            threads,
            acknowledged,
            elapsed,
            admitted_mask,
            invited_mask,
        } = self;
        // Bits refer only to the 17 fixed synthetic ordinals generated below.
        eprintln!("PHONE_PENDING_ORDINALS admitted={admitted_mask:05x} invited={invited_mask:05x}");
        eprintln!(
            "PHONE_PENDING_CLOSED submitted=17 admitted={admitted} held={held} dial_threads={threads} \
             refused_acks={acknowledged} elapsed_ms={} withdrew=true joined=true writer=true",
            elapsed.as_millis()
        );
        assert_eq!(
            admitted, 17,
            "all DTMF commands must be admitted by the selected mapping"
        );
        assert_eq!(
            acknowledged, held,
            "every pending wire attempt must finish its refusal/ACK"
        );
        assert!(
            elapsed < Duration::from_secs(35),
            "fixture exceeded the provider dial deadline"
        );
        assert_eq!(
            (held, threads),
            (17, 0),
            "seventeen pending attempts must coexist without a per-call dial thread"
        );
    }
}

fn dial_threads(pid: u32) -> usize {
    let tasks = fs::read_dir(format!("/proc/{pid}/task")).unwrap();
    tasks
        .map(|task| task.unwrap().path().join("comm"))
        .filter(|path| match fs::read_to_string(path) {
            Ok(name) => name.trim_end() == "rpt-phone-dial",
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => panic!("foreground thread-name read: {error}"),
        })
        .count()
}

pub(super) fn exercise(
    client: &Client<'_>,
    station: &str,
    active: &serde_json::Value,
    terminator: &str,
    socket: &UdpSocket,
    contact: SocketAddr,
    pid: u32,
) -> Observation {
    let start = Instant::now();
    let authority = socket.local_addr().unwrap();
    let mut wanted = BTreeSet::new();
    let mut admitted = 0;
    let mut admitted_mask = 0;
    assert_eq!(
        dial_threads(pid),
        0,
        "fresh foreground already owns an outbound dial"
    );
    for n in 0..17 {
        let number = format!("018{n:02}");
        wanted.insert(format!("sip:{number}@{authority}"));
        let reply = operation(
            client,
            json!({
                "submitted": true, "targets": [station], "expected_active_revision": active,
                "action": {"action": "execute_dtmf", "digits": format!("{DIAL_CODE}{number}{terminator}")}
            }),
        );
        let outcome = &reply.body["receipt"]["outcomes"][0];
        if reply.code == 200 && outcome["target"] == station && outcome["outcome"] == "admitted" {
            admitted += 1;
            admitted_mask |= 1 << n;
        }
    }
    let end = start + Duration::from_secs(15);
    let mut offers = BTreeMap::new();
    let mut targets = BTreeSet::new();
    let mut wire = [0; 8192];
    while offers.len() < 17 {
        let Some(left) = end.checked_duration_since(Instant::now()) else {
            break;
        };
        if left.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(left)).unwrap();
        let (message, source) =
            match registrar::receive_with_keepalives(socket, &mut wire, |source| {
                assert_eq!(source, contact)
            }) {
                Ok(packet) => packet,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
                Err(e) => panic!("pending INVITE read: {e}"),
            };
        assert_eq!(source, contact);
        assert!(
            message.starts_with("INVITE "),
            "unexpected method before refusal"
        );
        let target = message.split_ascii_whitespace().nth(1).unwrap().to_owned();
        assert!(wanted.contains(&target));
        let id = registrar::header(&message, "Call-ID").to_owned();
        offers.entry(id).or_insert_with(|| {
            assert!(targets.insert(target), "second dialog for the same command");
            message.clone()
        });
        // Keep each transaction pending. This indication does not report ringing.
        socket
            .send_to(registrar::response(&message, 100, false).as_bytes(), source)
            .unwrap();
    }
    let held = offers.len();
    let threads = dial_threads(pid);
    for offer in offers.values() {
        socket
            .send_to(registrar::response(offer, 486, false).as_bytes(), contact)
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut acks = BTreeSet::new();
    while acks.len() != offers.len() {
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("refused INVITE ACK deadline");
        socket.set_read_timeout(Some(left)).unwrap();
        let (message, source) = registrar::receive_with_keepalives(socket, &mut wire, |source| {
            assert_eq!(source, contact)
        })
        .unwrap();
        assert_eq!(source, contact);
        let id = registrar::header(&message, "Call-ID");
        let offer = offers.get(id).expect("unknown transaction after refusal");
        if message.starts_with("INVITE ") {
            socket
                .send_to(registrar::response(offer, 486, false).as_bytes(), source)
                .unwrap();
        } else {
            assert!(message.starts_with("ACK "));
            let number = registrar::header(offer, "CSeq")
                .split_whitespace()
                .next()
                .unwrap();
            assert_eq!(registrar::header(&message, "CSeq"), format!("{number} ACK"));
            acks.insert(id.to_owned());
        }
    }
    let invited_mask = (0..17).fold(0u32, |mask, n| {
        mask | (u32::from(targets.contains(&format!("sip:018{n:02}@{authority}"))) << n)
    });
    Observation {
        admitted,
        held,
        threads,
        acknowledged: acks.len(),
        elapsed: start.elapsed(),
        admitted_mask,
        invited_mask,
    }
}
