//! Shared host owners keep the provider/context alive until accounts are retired.
#![cfg(unix)]
use rptadv_peer_support::echolink::{abi, host::Provider};
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(partial: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "echo-owner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let library = directory.join(format!("echo_fixture{}", std::env::consts::DLL_SUFFIX));
        let result = Command::new("rustc")
            .args(["--edition=2024", "--crate-type=cdylib", "-Dwarnings"])
            .args(if partial {
                &["--cfg", "partial"][..]
            } else {
                &[]
            })
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo_owner.rs"))
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Self(library)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}

#[test]
fn account_retains_context_after_the_host_releases_its_provider() {
    let fixture = Fixture::new(false);
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig::default(),
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    drop(provider);
    assert_eq!(account.stop(), Ok(()));
    drop(account);
    // A second open also verifies balanced context/account destruction in the fixture.
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    drop(provider);
}

#[test]
fn incomplete_network_group_never_creates_a_context() {
    let fixture = Fixture::new(true);
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    assert!(matches!(
        unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) },
        Err(rptadv_peer_support::echolink::host::Error::Contract)
    ));
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_contexts\0") }.unwrap();
    assert_eq!(unsafe { count() }, 0);
}

#[test]
fn enabled_account_requires_complete_network_and_media_before_preparation() {
    let fixture = Fixture::new(false);
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    assert!(matches!(
        unsafe {
            provider.prepare(
                &abi::AccountConfig {
                    enabled: 1,
                    ..abi::AccountConfig::default()
                },
                &abi::AccountSecrets {
                    struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                    abi_version: 1,
                    directory_password: abi::ByteSpan::default(),
                    proxy_password: abi::ByteSpan::default(),
                },
            )
        },
        Err(rptadv_peer_support::echolink::host::Error::Status(
            abi::Status::Unsupported
        ))
    ));
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }.unwrap();
    assert_eq!(unsafe { count() }, 0);
}

#[test]
fn failed_account_stop_keeps_its_foreign_state_and_provider_alive() {
    let fixture = Fixture::new(false);
    let library = unsafe { libloading::Library::new(&fixture.0) }.unwrap();
    let provider = unsafe { Provider::load(&fixture.0, &abi::RuntimeConfig::default()) }.unwrap();
    let account = unsafe {
        provider.prepare(
            &abi::AccountConfig {
                node_number: 99,
                ..abi::AccountConfig::default()
            },
            &abi::AccountSecrets {
                struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                abi_version: 1,
                directory_password: abi::ByteSpan::default(),
                proxy_password: abi::ByteSpan::default(),
            },
        )
    }
    .unwrap();
    drop(provider);
    assert!(account.stop().is_err());
    drop(account);
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_contexts\0") }.unwrap();
    assert_eq!(
        unsafe { count() },
        1,
        "failed cleanup must retain the DSO/context owner"
    );
    let count =
        unsafe { library.get::<unsafe extern "C" fn() -> usize>(b"fixture_accounts\0") }.unwrap();
    assert_eq!(
        unsafe { count() },
        1,
        "failed cleanup must not destroy the foreign account"
    );
}
