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

#[test]
fn packaged_example_requires_an_explicitly_provisioned_store_and_credentials() {
    with_config(
        include_str!("../../../examples/rpt_advanced.service.json"),
        |path| {
            let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
                .args(["--check-config", path])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("configuration invalid"));
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
        assert!(String::from_utf8_lossy(&output.stderr).contains("configuration invalid"));
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
    assert!(help.contains("--foreground <SERVICE-JSON>"));
    assert!(!help.contains("--secrets-file"));
}

#[test]
fn check_providers_validates_the_installed_runtime_composition() {
    let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
        .arg("--check-providers")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("runtime providers valid"));
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
        vec![
            "--foreground".to_owned(),
            "/etc/rpt.conf".to_owned(),
            "--secrets-file".to_owned(),
            "/tmp/secrets.conf".to_owned(),
            "extra".to_owned(),
        ],
    ] {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--foreground"));
    }
    let output = Command::new(binary)
        .args([
            "--foreground",
            "/missing/rpt_advanced.conf",
            "--secrets-file",
            "/missing/rpt_advanced-secrets.conf",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read service provisioning"));
}

#[test]
fn rejected_provisioning_never_echoes_secret_fields() {
    with_config(
        r#"{"store_path":"/bad","aead_key":"private-secret-value"}"#,
        |path| {
            let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
                .args(["--check-config", path])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("configuration invalid"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("private-secret-value"));
        },
    );
}

#[cfg(unix)]
#[test]
fn check_config_rejects_writable_and_symlinked_service_documents() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    with_config("{}", |path| {
        let link = format!("{path}.link");
        symlink(path, &link).unwrap();
        for (candidate, mode) in [(&link[..], 0o600), (path, 0o666)] {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            let result = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
                .args(["--check-config", candidate])
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(1));
            assert!(
                String::from_utf8_lossy(&result.stderr)
                    .contains("cannot read service provisioning")
            );
        }
        fs::remove_file(link).unwrap();
    });
}

#[test]
fn removed_secrets_checker_refuses_a_path_without_disclosing_contents() {
    with_config("private-secret-value", |path| {
        let output = Command::new(env!("CARGO_BIN_EXE_rpt-advanced"))
            .args(["--check-secrets-file", path])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-secret-value"));
        assert!(output.stdout.is_empty());
    });
}
