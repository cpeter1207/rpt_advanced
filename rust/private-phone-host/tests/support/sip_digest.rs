use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

pub(crate) fn check(authorization: &str) {
    // Use the independent UDP registrar's digest verifier before sending 200.
    let mut verifier = Command::new("python3")
        .args(["-c", "import sys; from test_sip_registration import digest_ok; sys.exit(0 if digest_ok(sys.stdin.read(), 'REGISTER', ('desk', 'fixture-only')) else 1)"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    verifier
        .stdin
        .take()
        .unwrap()
        .write_all(authorization.as_bytes())
        .unwrap();
    assert!(
        verifier.wait().unwrap().success(),
        "native registrar rejected digest"
    );
}
