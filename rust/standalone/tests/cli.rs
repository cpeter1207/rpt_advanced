use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn with_config<T>(text: &str, run: impl FnOnce(&str) -> T) -> T {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rpt-advanced-{stamp}.conf"));
    fs::write(&path, text).unwrap();
    let result = run(path.to_str().unwrap());
    fs::remove_file(path).unwrap();
    result
}

#[cfg(unix)]
fn with_secrets<T>(text: &str, run: impl FnOnce(&str) -> T) -> T {
    use std::os::unix::fs::PermissionsExt;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rpt-advanced-secrets-{stamp}.conf"));
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let result = run(path.to_str().unwrap());
    fs::remove_file(path).unwrap();
    result
}

#[test]
fn check_config_validates_without_opening_hardware() {
    with_config(
        include_str!("../../../examples/rpt_advanced.conf"),
        |path| {
            let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
                .args(["--check-config", path])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("configuration valid"));
        },
    );
}

#[test]
fn check_config_reports_invalid_configuration() {
    with_config("[node 1000]\n[permanent remote 2000]\n", |path| {
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-config", path])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown node"));
    });
}

#[test]
fn help_exits_successfully() {
    let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--check-config"));
    assert!(help.contains("--check-secrets-file"));
}

#[test]
fn invalid_arguments_are_rejected() {
    let binary = env!("CARGO_BIN_EXE_rpt-advanced");
    for arguments in [vec![], vec!["--unsupported".to_owned()]] {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    with_config("[1000]\n", |path| {
        for arguments in [
            vec!["--check-config".to_owned()],
            vec![
                "--check-config".to_owned(),
                path.to_owned(),
                "extra".to_owned(),
            ],
        ] {
            let output = Command::new(binary).args(arguments).output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
        }
    });
    #[cfg(unix)]
    {
        for arguments in [
            vec!["--check-secrets-file".to_owned()],
            vec![
                "--check-secrets-file".to_owned(),
                "/tmp/secrets.conf".to_owned(),
                "extra".to_owned(),
            ],
        ] {
            let output = Command::new(binary).args(arguments).output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
        }
    }
    for arguments in [
        vec!["--foreground".to_owned()],
        vec![
            "--foreground".to_owned(),
            "/etc/rpt.conf".to_owned(),
            "extra".to_owned(),
        ],
        vec![
            "--foreground".to_owned(),
            "/etc/rpt.conf".to_owned(),
            "--secrets-file".to_owned(),
        ],
    ] {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--foreground"));
    }
    let output = Command::new(binary)
        .args(["--check-providers", "extra"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--check-providers"));
}

#[cfg(unix)]
#[test]
fn foreground_refuses_root_before_opening_configuration() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
        .args(["--foreground", "/missing/rpt_advanced.conf"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unprivileged user"));
}

#[test]
fn missing_config_file_is_reported() {
    let path = std::env::temp_dir().join("rpt-advanced-config-does-not-exist.conf");
    let _ = fs::remove_file(&path);
    let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
        .args(["--check-config", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("No such file"));
}

#[test]
fn valid_configuration_warnings_are_printed() {
    with_config("[1000]\nunknown_setting=value\n", |path| {
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-config", path])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("warning:"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("configuration valid"));
    });
}

#[cfg(unix)]
#[test]
fn check_secrets_file_validates_without_printing_secrets() {
    with_secrets("[general]\niax_secret=private-secret-value\n", |path| {
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-secrets-file", path])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("secrets file valid"));
        assert!(!stdout.contains("private-secret-value"));
        assert!(!stderr.contains("private-secret-value"));
    });
}

#[cfg(unix)]
#[test]
fn check_secrets_file_rejects_permissive_mode() {
    use std::os::unix::fs::PermissionsExt;

    with_secrets("[general]\niax_secret=private-secret-value\n", |path| {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-secrets-file", path])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("mode 0600"));
        assert!(!stderr.contains("private-secret-value"));
    });
}

#[cfg(unix)]
#[test]
fn check_secrets_file_rejects_malformed_contents_without_leaking_them() {
    with_secrets("[general]\nunknown=private-secret-value\n", |path| {
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-secrets-file", path])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unknown option"));
        assert!(!stderr.contains("private-secret-value"));
    });
}
