use super::*;
use std::{
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Fake {
    requests: Vec<Value>,
    cleared: Vec<String>,
}
impl Backend for Fake {
    fn execute(&mut self, request: &Value) -> Result<Value, String> {
        self.requests.push(request.clone());
        Ok(json!("physical status"))
    }
    fn clear_manual(&mut self, node: &str) {
        self.cleared.push(node.to_owned());
    }
}

fn setup() -> (Server, UnixStream, Fake) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "rpt-radio-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut server = Server::bind(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let client = UnixStream::connect(&path).unwrap();
    let mut backend = Fake::default();
    server.poll(&mut backend);
    (server, client, backend)
}

fn frame(value: Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    frame
}

fn read_reply(server: &mut Server, client: &mut UnixStream, backend: &mut Fake) -> Value {
    for _ in 0..24 {
        server.poll(backend);
    }
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    let mut size = [0; 4];
    client.read_exact(&mut size).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
    client.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn fragmented_requests_preserve_one_session_and_cleanup_only_manual_intents() {
    let (mut server, mut client, mut backend) = setup();
    let request = frame(json!({"version":1,"operation":"transmit","node":"1000","keyed":true}));
    client.write_all(&request[..3]).unwrap();
    server.poll(&mut backend);
    assert!(backend.requests.is_empty());
    client.write_all(&request[3..]).unwrap();
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["ok"],
        true
    );
    client
        .write_all(&frame(
            json!({"version":1,"operation":"status","node":"2000"}),
        ))
        .unwrap();
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["result"],
        "physical status"
    );
    drop(client);
    server.poll(&mut backend);
    assert_eq!(backend.cleared, ["1000"]);
    let path = server.path.clone();
    drop(server);
    assert!(!path.exists());
}

#[test]
fn bad_requests_never_reach_backend_and_oversized_frames_close() {
    let (mut server, mut client, mut backend) = setup();
    for request in [
        json!({"version":2,"operation":"nodes"}),
        json!({"version":1,"operation":"shell","command":"x"}),
        json!({"version":1,"operation":"status","node":42}),
    ] {
        client.write_all(&frame(request)).unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            false
        );
    }
    assert!(backend.requests.is_empty());
    client
        .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
        .unwrap();
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["ok"],
        false
    );
    assert!(server.session.is_none());
}

#[test]
fn a_second_client_cannot_take_over_the_manual_lease_or_replace_socket() {
    let (mut server, _client, mut backend) = setup();
    assert!(Server::bind(&server.path).is_err());
    let mut second = UnixStream::connect(&server.path).unwrap();
    server.poll(&mut backend);
    let mut byte = [0];
    assert_eq!(second.read(&mut byte).unwrap(), 0);
    server.close(&mut backend);
    assert!(backend.cleared.is_empty());
    server.close(&mut backend);
}

#[test]
fn validation_and_manual_command_ownership_use_one_persistent_session() {
    let (mut server, mut client, mut backend) = setup();
    for request in [
        json!({"version":1}),
        json!({"version":1,"operation":"status"}),
        json!({"version":1,"operation":"status","node":[]}),
    ] {
        client.write_all(&frame(request)).unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            false
        );
    }
    client.write_all(&[0, 0, 0, 1, b'{']).unwrap();
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["ok"],
        false
    );
    assert!(backend.requests.is_empty());
    for operation in ["nodes", "configuration-path"] {
        client
            .write_all(&frame(json!({"version":1,"operation":operation})))
            .unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            true
        );
    }
    for (node, command) in [
        ("1000", crate::abi::URP_AST_COMMAND_SET_TEST_TONE),
        ("2000", crate::abi::URP_AST_COMMAND_SET_CTCSS_INHIBIT),
        ("3000", crate::abi::URP_AST_COMMAND_SET_SUBAUDIBLE_OVERRIDE),
        ("4000", crate::abi::URP_AST_COMMAND_GET_MIXER),
    ] {
        client
            .write_all(&frame(
                json!({"version":1,"operation":"command","node":node,"command":command}),
            ))
            .unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            true
        );
    }
    server.close(&mut backend);
    assert_eq!(backend.cleared, ["1000", "2000", "3000"]);
}

#[test]
fn configured_node_identities_follow_the_controller_contract() {
    let (mut server, mut client, mut backend) = setup();
    for node in ["local-radio".to_owned(), "1".repeat(63)] {
        client
            .write_all(&frame(
                json!({"version":1,"operation":"status","node":node}),
            ))
            .unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            true
        );
    }
    assert_eq!(backend.requests.len(), 2);
}

#[test]
fn incomplete_and_invalid_frame_bounds_and_oversized_replies_are_bounded() {
    for bytes in [
        vec![0, 0, 0, 0],
        ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
    ] {
        let (mut server, mut client, mut backend) = setup();
        client.write_all(&bytes).unwrap();
        assert_eq!(
            read_reply(&mut server, &mut client, &mut backend)["ok"],
            false
        );
        assert!(server.session.is_none());
    }
    let (mut server, mut client, mut backend) = setup();
    client.write_all(&[0, 0, 0, 2, b'{']).unwrap();
    server.poll(&mut backend);
    assert!(backend.requests.is_empty());
    client.write_all(b"}").unwrap();
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["ok"],
        false
    );
    let session = server.session.as_mut().unwrap();
    session.input = vec![0; MAX_FRAME + 5];
    session.input[..4].copy_from_slice(&1_u32.to_be_bytes());
    server.poll(&mut backend);
    assert_eq!(
        read_reply(&mut server, &mut client, &mut backend)["ok"],
        false
    );
    assert!(server.session.is_none());

    let (mut server, mut client, mut backend) = setup();
    server
        .session
        .as_mut()
        .unwrap()
        .reply(Ok(json!("a".repeat(MAX_FRAME))));
    let reply = read_reply(&mut server, &mut client, &mut backend);
    assert_eq!(reply["error"], "local control result exceeds frame limit");
    assert!(server.session.is_some());
}

#[test]
fn slow_and_aborted_clients_never_block_polling_or_retain_manual_intent() {
    use std::{net::Shutdown, os::fd::AsRawFd};
    let (mut server, client, mut backend) = setup();
    let session = server.session.as_mut().unwrap();
    let size: libc::c_int = 4096;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                session.stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                std::ptr::from_ref(&size).cast(),
                size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    session.output = vec![0; MAX_FRAME];
    session.manual.insert("1000".into());
    server.poll(&mut backend);
    let progress = server.session.as_ref().unwrap().written;
    assert!(progress > 0 && progress < MAX_FRAME);
    server.poll(&mut backend);
    assert_eq!(server.session.as_ref().unwrap().written, progress);
    assert!(backend.cleared.is_empty());
    client.shutdown(Shutdown::Read).unwrap();
    server.poll(&mut backend);
    assert!(server.session.is_none());
    assert_eq!(backend.cleared, ["1000"]);

    let (mut server, client, mut backend) = setup();
    server
        .session
        .as_mut()
        .unwrap()
        .stream
        .write_all(b"unread reply")
        .unwrap();
    drop(client);
    server.poll(&mut backend);
    assert!(server.session.is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn accepted_stream_setup_failure_rejects_the_new_lease() {
    const CHILD: &str = "RPT_RADIO_TEST_DENY_IOCTL";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_control::tests::accepted_stream_setup_failure_rejects_the_new_lease",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        return;
    }
    let (mut server, client, mut backend) = setup();
    server.close(&mut backend);
    drop(client);
    let mut next = UnixStream::connect(&server.path).unwrap();
    // Deny ioctl only in this disposable child's test thread. This exercises the
    // actual accepted descriptor's FIONBIO failure without corrupting an owned fd.
    let mut filters = [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_ioctl as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filters.len() as u16,
        filter: filters.as_mut_ptr(),
    };
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                std::ptr::from_ref(&program),
            )
        },
        0
    );
    server.poll(&mut backend);
    assert!(server.session.is_none());
    assert!(backend.requests.is_empty());
    assert_eq!(next.read(&mut [0]).unwrap(), 0);
}
