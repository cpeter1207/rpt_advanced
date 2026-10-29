//! Native descriptor fixture shared by provider and host-dispatch tests.
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

pub struct Fixture(pub PathBuf);
impl Fixture {
    pub fn new(kind: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "echo-owner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let library = directory.join(format!("echo_fixture{}", std::env::consts::DLL_SUFFIX));
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("peer-support/tests/fixtures/echo_owner.rs");
        let result = Command::new("rustc")
            .args([
                "--edition=2024",
                "--crate-type=cdylib",
                "-Dwarnings",
                "--cfg",
                kind,
            ])
            .arg(source)
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

    pub fn peer(&self) -> std::sync::Arc<rptadv_peer_support::echolink::host::Peer> {
        use rptadv_peer_support::echolink::{abi, host::Provider};
        let library = unsafe { libloading::Library::new(&self.0) }.unwrap();
        let provider = unsafe { Provider::load(&self.0, &abi::RuntimeConfig::default()) }.unwrap();
        let account = unsafe {
            provider.prepare(
                &abi::AccountConfig {
                    enabled: 1,
                    local_node: abi::ByteSpan {
                        data: b"524950".as_ptr(),
                        length: 6,
                    },
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
        let create =
            unsafe { library.get::<unsafe extern "C" fn() -> *mut abi::Qso>(b"fixture_peer\0") }
                .unwrap();
        let mut taken = 0;
        unsafe {
            account.take_pending(&abi::Event {
                struct_size: std::mem::size_of::<abi::Event>() as u32,
                kind: 1,
                qso: create(),
                generation: 9,
                status: abi::Status::Pending,
                taken: &mut taken,
            })
        }
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}
