//! Isolated actual provider descriptor, without optional peer-construction helpers.
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}
