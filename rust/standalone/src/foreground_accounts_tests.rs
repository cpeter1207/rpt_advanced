//! Production foreground account composition, without live providers or radio I/O.
use super::*;
use crate::{configuration::resolve_radio_nodes, iax::accounts::ProviderAccount};
use std::{fs, net::UdpSocket, os::unix::fs::PermissionsExt, sync::atomic::AtomicU64};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "rpt-phone-foreground-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("phone.secret");
        fs::write(&path, "fixture-secret\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        Self { directory }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("rpt_advanced.conf")
    }
    fn text(&self, port: u16) -> String {
        format!(
            "[general]\nnode_enabled=yes\niax_local_port={port}\n[desk]\nradio_channel=fixture\n\
             [phone_account office]\nlocal_node=desk\nprotocol=iax\nenabled=yes\n\
             server=127.0.0.1:4569\nusername=fixture-user\nsecrets_file=phone.secret\nregister=no\n"
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn test_client() -> crate::iax::IaxClient {
    crate::iax::IaxClient::load(Path::new("librptadviax2.so.2")).unwrap()
}

#[test]
fn configured_numeric_account_binds_named_node_listener() {
    let fixture = Fixture::new();
    let reserved = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    let text = fixture.text(port);
    let radios = resolve_radio_nodes(&text).unwrap();
    let accounts = ForegroundAccounts::default()
        .prepare(&fixture.path(), &text, &radios)
        .unwrap();
    assert_eq!(accounts.as_slice().len(), 1);
    drop(reserved);
    let mut listeners = bind_iax_listeners(&test_client(), &radios, &accounts).unwrap();
    assert_eq!(
        listeners.len(),
        1,
        "configured account must own its named-node listener"
    );
    let account = &accounts.as_slice()[0];
    let settings = account.settings();
    let conflicting = ProviderAccount {
        account: &settings.id,
        generation: account.generation(),
        local_binding: &settings.local_node,
        remote: account.remote(),
        username: &settings.username,
        secret: "changed-secret",
        authentication_timeout_ms: settings.authentication_timeout_ms,
    };
    assert_eq!(
        listeners
            .get_mut(&port)
            .unwrap()
            .configure_accounts(&[conflicting]),
        Err(crate::iax::IaxError::InvalidParameters),
        "the released provider must retain the exact configured account generation"
    );
    drop(listeners);
    UdpSocket::bind(("0.0.0.0", port)).unwrap();
}

#[test]
fn partial_account_listener_bind_failure_releases_earlier_ports() {
    let fixture = Fixture::new();
    let first = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    let second = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    let (earlier, occupied) =
        if first.local_addr().unwrap().port() < second.local_addr().unwrap().port() {
            (first, second)
        } else {
            (second, first)
        };
    let earlier_port = earlier.local_addr().unwrap().port();
    let occupied_port = occupied.local_addr().unwrap().port();
    let text = format!(
        "{}[other-node]\nradio_channel=fixture\niax_local_port={occupied_port}\n\
         [phone_account other]\nlocal_node=other-node\nprotocol=iax\nenabled=yes\n\
         server=127.0.0.1:4569\nusername=other-user\nsecrets_file=phone.secret\nregister=no\n",
        fixture.text(earlier_port)
    );
    let radios = resolve_radio_nodes(&text).unwrap();
    let accounts = ForegroundAccounts::default()
        .prepare(&fixture.path(), &text, &radios)
        .unwrap();
    drop(earlier);
    assert!(matches!(
        bind_iax_listeners(&test_client(), &radios, &accounts),
        Err(ForegroundError::Iax(crate::iax::IaxError::Bind))
    ));
    UdpSocket::bind(("0.0.0.0", earlier_port)).unwrap();
    drop(occupied);
}

#[test]
fn preparation_rejects_unsupported_profiles_without_replacing_active_accounts() {
    let fixture = Fixture::new();
    let text = fixture.text(4569);
    let radios = resolve_radio_nodes(&text).unwrap();
    let mut owner = ForegroundAccounts::default();
    owner.active = owner.prepare(&fixture.path(), &text, &radios).unwrap();
    let original = std::sync::Arc::clone(&owner.active.as_slice()[0]);
    for (candidate, diagnostic) in [
        (text.replace("protocol=iax", "protocol=sip"), "requires IAX"),
        (text.replace("register=no", "register=yes"), "registration"),
        (
            text.replace("127.0.0.1:4569", "provider.invalid:4569"),
            "numeric endpoint",
        ),
        (
            text.replace("phone.secret", "missing.secret"),
            "secrets file",
        ),
    ] {
        let error = match owner.prepare(&fixture.path(), &candidate, &radios) {
            Err(error) => error,
            Ok(_) => panic!("unsupported candidate must not activate"),
        };
        assert!(error.to_string().contains(diagnostic), "{error}");
        assert!(!error.to_string().contains("fixture-secret"));
        assert!(std::sync::Arc::ptr_eq(
            &owner.active.as_slice()[0],
            &original
        ));
    }
    let unchanged = owner.prepare(&fixture.path(), &text, &radios).unwrap();
    assert!(std::sync::Arc::ptr_eq(&unchanged.as_slice()[0], &original));
    fs::remove_file(fixture.directory.join("phone.secret")).unwrap();
    let disabled = text.replace("\nenabled=yes\n", "\nenabled=no\n");
    assert!(
        owner
            .prepare(&fixture.path(), &disabled, &radios)
            .unwrap()
            .as_slice()
            .is_empty()
    );
    let disabled_node =
        resolve_radio_nodes(&text.replace("node_enabled=yes", "node_enabled=no")).unwrap();
    assert!(
        owner
            .prepare(&fixture.path(), &text, &disabled_node)
            .unwrap()
            .as_slice()
            .is_empty()
    );
}

#[path = "foreground_incoming_tests.rs"]
mod incoming;
