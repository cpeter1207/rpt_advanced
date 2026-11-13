//! Real controller owner against an independent synthetic UDP registrar.
use super::*;
use crate::{configuration::ResolvedPhoneAccount, phone_accounts::AccountPreparer};
use std::{fs, net::UdpSocket, os::unix::fs::PermissionsExt};

fn header<'a>(packet: &'a str, name: &str) -> &'a str {
    packet
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| {
                    key.eq_ignore_ascii_case(name)
                        || matches!(
                            (name, *key),
                            ("Via", "v")
                                | ("From", "f")
                                | ("To", "t")
                                | ("Call-ID", "i")
                                | ("Contact", "m")
                        )
                })
                .map(|(_, value)| value.trim())
        })
        .unwrap_or_else(|| {
            panic!(
                "missing {name} in fixture packet {}",
                packet.lines().next().unwrap_or("empty")
            )
        })
}
fn response(packet: &str, code: u16, challenge: bool) -> String {
    let mut reply = format!(
        "SIP/2.0 {code} Fixture\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=fixture\r\nCall-ID: {}\r\nCSeq: {}\r\n",
        header(packet, "Via"),
        header(packet, "From"),
        header(packet, "To"),
        header(packet, "Call-ID"),
        header(packet, "CSeq")
    );
    if challenge {
        reply.push_str("WWW-Authenticate: Digest realm=\"fixture\", nonce=\"fixture-nonce\", algorithm=MD5\r\n");
    } else if packet.starts_with("REGISTER ") {
        let contact = header(packet, "Contact")
            .split(';')
            .filter(|part| !part.starts_with("expires="))
            .collect::<Vec<_>>()
            .join(";");
        reply.push_str(&format!(
            "Contact: {contact};expires={}\r\nExpires: {}\r\n",
            header(packet, "Expires"),
            header(packet, "Expires")
        ));
    }
    reply.push_str("Content-Length: 0\r\n\r\n");
    reply
}

fn receive_register(
    socket: &UdpSocket,
    bytes: &mut [u8],
) -> std::io::Result<(String, std::net::SocketAddr)> {
    let deadline = Instant::now() + socket.read_timeout()?.expect("bounded fixture receive");
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "fixture receive deadline")
            })?;
        socket.set_read_timeout(Some(remaining))?;
        let (count, remote) = socket.recv_from(bytes)?;
        let packet = std::str::from_utf8(&bytes[..count]).unwrap();
        if packet.starts_with("OPTIONS ") {
            socket.send_to(response(packet, 200, false).as_bytes(), remote)?;
        } else {
            return Ok((packet.to_owned(), remote));
        }
    }
}

#[test]
fn controller_registers_only_after_commit_and_unregisters_on_removal() {
    let directory = std::env::temp_dir().join(format!("rpt-sip-controller-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let secret = directory.join("secret");
    fs::write(&secret, "synthetic-test-secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(120)))
        .unwrap();
    let record = ResolvedPhoneAccount {
        id: "office".into(),
        local_node: "desk".into(),
        protocol: PhoneAccountProtocol::Sip,
        enabled: true,
        server: socket.local_addr().unwrap().to_string(),
        username: "synthetic".into(),
        secrets_file: "secret".into(),
        register: true,
        registration_interval_s: 60,
        authentication_timeout_ms: 2000,
    };
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt.conf"),
            vec![record],
            &PreparedAccounts::default(),
            |_| unreachable!(),
        )
        .unwrap();
    let path = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_sip_adapter.so.1");
    let mut worker = Worker::start(&path).unwrap();
    let mut bytes = [0; 8192];
    {
        let rejected = worker.prepare(&accounts).unwrap();
        assert!(
            socket.recv_from(&mut bytes).is_err(),
            "preparation must not send REGISTER"
        );
        drop(rejected);
    }
    worker.prepare(&accounts).unwrap().commit().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (first, remote) = receive_register(&socket, &mut bytes).unwrap();
    assert!(first.starts_with("REGISTER sip:"));
    socket
        .send_to(response(&first, 401, true).as_bytes(), remote)
        .unwrap();
    let (authenticated, remote) = receive_register(&socket, &mut bytes).unwrap();
    assert!(header(&authenticated, "Authorization").contains("username=\"synthetic\""));
    assert!(!authenticated.contains("synthetic-test-secret"));
    socket
        .send_to(response(&authenticated, 200, false).as_bytes(), remote)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let observed = worker.observe().unwrap();
        if observed[0].1.state == 2 {
            assert_eq!(observed[0].1.registrations, 1);
            assert_eq!(observed[0].1.accepted_expires, 60);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    // An unchanged generation must retain its registered context without another REGISTER.
    let prior = worker.observe().unwrap()[0].1;
    worker.prepare(&accounts).unwrap().commit().unwrap();
    let observation_started = Instant::now();
    socket
        .set_read_timeout(Some(Duration::from_millis(120)))
        .unwrap();
    let unchanged = receive_register(&socket, &mut bytes);
    assert!(
        unchanged.is_err(),
        "unchanged generation emitted {:?}",
        unchanged.as_ref().map(|(packet, _)| packet.lines().next())
    );
    let retained = worker.observe().unwrap()[0].1;
    assert_eq!(retained.generation, prior.generation);
    assert_eq!(retained.registrations, prior.registrations);
    assert_eq!(retained.accepted_expires, prior.accepted_expires);
    assert_eq!(retained.state, 2);
    assert!(observation_started.elapsed() < Duration::from_secs(2));
    worker
        .prepare(&PreparedAccounts::default())
        .unwrap()
        .commit()
        .unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (removal, remote) = receive_register(&socket, &mut bytes).unwrap();
    assert_eq!(header(&removal, "Expires"), "0");
    assert_eq!(
        header(&removal, "Call-ID"),
        header(&authenticated, "Call-ID")
    );
    let sequence = |packet: &str| {
        header(packet, "CSeq")
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap()
    };
    assert!(sequence(&removal) > sequence(&authenticated));
    socket
        .send_to(response(&removal, 200, false).as_bytes(), remote)
        .unwrap();
    worker.stop().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

fn native_fixture(failing: bool, id: &str) -> (Entry, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!("rpt-sip-owner-{}-{id}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("provider.c");
    let library = directory.join("provider.so");
    fs::write(
        &source,
        r#"
#include <stdlib.h>
#include <string.h>
#include "rptadv_sip_adapter.h"
typedef struct { rptadv_sip_status_v1 status; } Context;
static int create(const rptadv_sip_config_v1 *config, void **out) {
    Context *context = calloc(1, sizeof(*context));
    if (!context) return -4;
    context->status.struct_size = sizeof(context->status);
    context->status.generation = config->generation;
    *out = context; return 0;
}
static int service(void *opaque, uint64_t generation) {
    Context *context = opaque; (void)generation; (void)context;
#ifdef FAIL_SERVICE
    return -4;
#else
    if (context->status.state == 1 || context->status.state == 2) {
        context->status.state = 2; context->status.sip_status = 200;
        context->status.accepted_expires = 60; context->status.registrations++;
    } else if (context->status.state == 6) context->status.state = 7;
    context->status.sequence++; return 0;
#endif
}
static int status(void *opaque, uint64_t generation, rptadv_sip_status_v1 *out) {
    (void)generation; *out = ((Context *)opaque)->status; return 0;
}
static int event(void *opaque, uint64_t generation, rptadv_sip_status_v1 *out) {
    (void)opaque; (void)generation; (void)out; return 1;
}
static int activate(void *opaque, uint64_t generation) {
    (void)generation; ((Context *)opaque)->status.state = 1; return 0;
}
static int stop(void *opaque, uint64_t generation) {
    (void)generation; ((Context *)opaque)->status.state = 6; return 0;
}
static int destroy(void *opaque, uint64_t generation) {
    (void)generation;
    if (((Context *)opaque)->status.state != 7) return 1;
    free(opaque); return 0;
}
static const rptadv_sip_descriptor_v1 api = {
    sizeof(api), 1, "rptadv.sip", create, service, event, status,
    activate, stop, stop, destroy
};
const rptadv_sip_descriptor_v1 *rptadv_sip_adapter_descriptor_v1(void) { return &api; }
"#,
    )
    .unwrap();
    let mut command = std::process::Command::new("cc");
    command.args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"]);
    command
        .arg("-I")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../sip-adapter/include"));
    if failing {
        command.arg("-DFAIL_SERVICE");
    }
    assert!(
        command
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    fs::write(directory.join("secret"), "synthetic-secret").unwrap();
    fs::set_permissions(directory.join("secret"), fs::Permissions::from_mode(0o600)).unwrap();
    let record = ResolvedPhoneAccount {
        id: id.into(),
        local_node: "desk".into(),
        protocol: PhoneAccountProtocol::Sip,
        enabled: true,
        server: "127.0.0.1:5060".into(),
        username: "synthetic".into(),
        secrets_file: "secret".into(),
        register: true,
        registration_interval_s: 60,
        authentication_timeout_ms: 1000,
    };
    let accounts = AccountPreparer::default()
        .prepare(
            &directory.join("rpt.conf"),
            vec![record],
            &PreparedAccounts::default(),
            |_| unreachable!(),
        )
        .unwrap();
    let provider = unsafe { Provider::load(&library) }.unwrap();
    (
        Entry::new(&provider, Arc::clone(&accounts.as_slice()[0])).unwrap(),
        directory,
    )
}

#[test]
fn persistent_service_error_cannot_bypass_shutdown_deadline() {
    let (mut entry, directory) = native_fixture(true, "fault");
    let now = Instant::now();
    entry.service(now);
    assert!(entry.shutdown.is_some());
    entry.service(now + SHUTDOWN_LIMIT + Duration::from_secs(1));
    assert!(
        entry.done && entry.abandoned,
        "failed provider must not keep host join waiting forever"
    );
    drop(entry);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn late_success_cannot_extend_expired_authentication_or_binding() {
    for binding in [false, true] {
        let (mut entry, directory) =
            native_fixture(false, if binding { "binding" } else { "authentication" });
        let now = Instant::now();
        entry.activate(now).unwrap();
        if binding {
            entry.service(now);
            assert_eq!(entry.last.state, 2);
        }
        let expired = now + Duration::from_secs(if binding { 61 } else { 2 });
        entry.service(expired);
        assert!(
            entry.shutdown.is_some(),
            "late success must not replace an expired absolute deadline"
        );
        entry.service(expired + Duration::from_millis(10));
        assert!(entry.done);
        drop(entry);
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn stopped_reactor_reports_and_retains_a_still_blocked_preparation_job() {
    let path = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_sip_adapter.so.1");
    let mut worker = Worker::start(&path).unwrap();
    let lease = Arc::clone(&worker.resolver);
    let weak = Arc::downgrade(&lease);
    let (started, ready) = std::sync::mpsc::sync_channel(1);
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    worker.preparation = Some(worker.runtime.spawn_blocking(move || {
        let _lease = lease;
        started.send(()).unwrap();
        blocked.recv().unwrap();
    }));
    ready.recv().unwrap();
    let before = Instant::now();
    let result = worker.stop();
    let elapsed = before.elapsed();
    let job = worker.preparation.take().unwrap();
    drop(worker);
    let retained = weak.upgrade().is_some();
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !job.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(result, Err(Error::Preparation));
    assert!(
        elapsed < Duration::from_secs(2),
        "SIP reactor join must not await blocked native lookup"
    );
    assert!(
        retained,
        "actual blocked job must retain its resolver code lease"
    );
    assert!(job.is_finished() && weak.upgrade().is_none());
}
