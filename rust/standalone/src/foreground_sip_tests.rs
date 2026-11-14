//! Full foreground/private SIP lifecycle; only physical radio I/O is simulated.
use super::tests::{FOREGROUND_FIXTURE, spawn_foreground, wait_for_log};
use crate::phone::sip_test_registrar::{header, receive_register, response};
use std::{
    fs,
    net::UdpSocket,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    directory: PathBuf,
    runtime_created: bool,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
        if self.runtime_created {
            let _ = fs::remove_file(crate::local_control::SOCKET_PATH);
            let _ = fs::remove_dir(
                Path::new(crate::local_control::SOCKET_PATH)
                    .parent()
                    .unwrap(),
            );
        }
    }
}

fn change_owner(path: &Path, owner: u32, group: u32) {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), owner, group) }, 0);
}

#[test]
fn full_foreground_registers_reloads_and_stops_without_radio_output() {
    let _guard = FOREGROUND_FIXTURE.lock().unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("rpt-foreground-sip-{stamp}"));
    fs::create_dir(&directory).unwrap();
    let owner = if unsafe { libc::geteuid() } == 0 {
        65534
    } else {
        unsafe { libc::geteuid() }
    };
    let group = if unsafe { libc::getegid() } == 0 {
        65534
    } else {
        unsafe { libc::getegid() }
    };
    let runtime = Path::new(crate::local_control::SOCKET_PATH)
        .parent()
        .unwrap();
    let runtime_created = match fs::create_dir(runtime) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => panic!("cannot prepare test RuntimeDirectory: {error}"),
    };
    let fixture = Fixture {
        directory,
        runtime_created,
    };
    change_owner(&fixture.directory, owner, group);
    if runtime_created {
        change_owner(runtime, owner, group);
        fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Never disturb an existing control socket or runtime directory's ownership.
    assert!(!Path::new(crate::local_control::SOCKET_PATH).exists());
    let secret = fixture.directory.join("phone.secret");
    fs::write(&secret, "synthetic-foreground-secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    change_owner(&secret, owner, group);
    let registrar = UdpSocket::bind("127.0.0.1:0").unwrap();
    registrar
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // Keep the configured IAX port occupied: the nonnumeric node must not open it.
    let iax = UdpSocket::bind("0.0.0.0:0").unwrap();
    let text = format!(
        "[general]\nnode_enabled=yes\niax_local_port={}\n[desk]\nradio_channel=fixture\nduplex=full\n\
         [phone_account office]\nlocal_node=desk\nprotocol=sip\nenabled=yes\n\
         server={}\nusername=synthetic\nsecrets_file=phone.secret\nregister=yes\n\
         registration_interval_s=60\nauthentication_timeout_ms=3000\n\
         [morse]\nspeed_wpm=40\nfrequency_hz=800\nlevel_db=-6\n{}",
        iax.local_addr().unwrap().port(),
        registrar.local_addr().unwrap(),
        include_str!("../../product/src/tests/fixtures/phone_mailbox.conf")
    );
    let config = fixture.directory.join("rpt_advanced.conf");
    fs::write(&config, &text).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "foreground::tests::foreground_child_entry",
            "--nocapture",
        ])
        .env("RPT_ADVANCED_FOREGROUND_TEST_CONFIG", &config)
        .env("RPTADV_TEST_FOREGROUND_SIP_RADIO", "1")
        .env(
            "LLVM_PROFILE_FILE",
            fixture
                .directory
                .join(format!("rpt-foreground-sip-{stamp}-%p.profraw")),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // As in the existing foreground harness, the real process never runs as root.
    unsafe {
        command.pre_exec(|| {
            if libc::geteuid() == 0
                && (libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(65534) != 0
                    || libc::setuid(65534) != 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (mut child, logs, reader) = spawn_foreground(&mut command);
    wait_for_log(&logs, "standalone runtime started");
    let mut bytes = [0; 8192];
    let (first, remote) = receive_register(&registrar, &mut bytes).unwrap();
    assert!(first.starts_with("REGISTER sip:"));
    registrar
        .send_to(response(&first, 401, true).as_bytes(), remote)
        .unwrap();
    let (authenticated, remote) = receive_register(&registrar, &mut bytes).unwrap();
    assert!(header(&authenticated, "Authorization").contains("username=\"synthetic\""));
    assert!(!authenticated.contains("synthetic-foreground-secret"));
    assert_eq!(header(&authenticated, "Expires"), "60");
    registrar
        .send_to(response(&authenticated, 200, false).as_bytes(), remote)
        .unwrap();
    wait_for_log(&logs, "phone account office: SIP state 2 response 200");

    assert_eq!(
        unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
        0
    );
    wait_for_log(&logs, "configuration reloaded");
    registrar
        .set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    assert!(
        receive_register(&registrar, &mut bytes).is_err(),
        "unchanged reload must retain registration"
    );

    // Same literal caller and private-prompt assertions as the actual-product test,
    // now entering through the real foreground loop and account owner.
    crate::host_services::product_phone_tests::sip::caller(
        registrar.try_clone().unwrap(),
        Some(remote),
    );
    // Cloned UDP descriptors share O_NONBLOCK; restore the registrar's blocking mode.
    registrar.set_nonblocking(false).unwrap();

    fs::write(
        &config,
        text.replace("protocol=sip\nenabled=yes", "protocol=sip\nenabled=no"),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGHUP) },
        0
    );
    wait_for_log(&logs, "configuration reloaded");
    registrar
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let (removal, remote) = loop {
        let (packet, remote) = receive_register(&registrar, &mut bytes).unwrap();
        if header(&packet, "Expires") == "0" {
            break (packet, remote);
        }
        // A refresh may already be on the wire when the foreground commits removal.
        // Acknowledge it, but still require the explicit unregister below.
        assert_eq!(
            header(&packet, "Call-ID"),
            header(&authenticated, "Call-ID")
        );
        registrar
            .send_to(response(&packet, 200, false).as_bytes(), remote)
            .unwrap();
        assert!(
            Instant::now() < deadline,
            "account removal did not unregister"
        );
    };
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
    registrar
        .send_to(response(&removal, 200, false).as_bytes(), remote)
        .unwrap();
    wait_for_log(&logs, "phone account office: SIP state 7 response 200");
    assert_eq!(
        unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "foreground child failed: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "foreground shutdown did not join promptly"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_for_log(
        &logs,
        "foreground SIP fixture stopped; authorized voice output: false",
    );
    reader.join().unwrap();
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        let profiles: Vec<_> = fs::read_dir(&fixture.directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "profraw")
            })
            .collect();
        assert!(
            !profiles.is_empty(),
            "foreground SIP child must write coverage profiles"
        );
        let parent = std::env::var_os("LLVM_PROFILE_FILE").unwrap();
        for profile in profiles {
            let target = Path::new(&parent)
                .parent()
                .unwrap()
                .join(profile.file_name().unwrap());
            fs::copy(&profile, target).unwrap();
        }
    }
    assert!(!Path::new(crate::local_control::SOCKET_PATH).exists());
}
