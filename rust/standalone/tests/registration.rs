use rpt_advanced_standalone::{registration, secrets::SecretsFile};

#[test]
fn registration_targets_follow_node_overrides_and_require_a_configured_secret() {
    let configuration = "[general]\niax_registration_url=https://register.example/\niax_registration_interval_s=120\niax_local_port=4570\n[1000]\n[2000]\niax_registration_url=https://backup.example/\niax_registration_interval_s=300\niax_local_port=4569\n[3000]\niax_registration_url=https://register.example/\n";
    let secrets =
        SecretsFile::parse("[general]\niax_secret=global-secret\n[2000]\niax_secret=node-secret\n")
            .unwrap();

    let targets = registration::targets(configuration, &secrets).unwrap();

    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].node(), "1000");
    assert_eq!(targets[0].url(), "https://register.example/");
    assert_eq!(targets[0].local_port(), 4570);
    assert_eq!(targets[0].interval_seconds(), 120);
    assert_eq!(targets[1].node(), "2000");
    assert_eq!(targets[1].url(), "https://backup.example/");
    assert_eq!(targets[1].local_port(), 4569);
    assert_eq!(targets[1].interval_seconds(), 300);
    assert_eq!(targets[2].node(), "3000");
    assert_eq!(targets[2].interval_seconds(), 120);
}

#[test]
fn registration_is_disabled_when_endpoint_is_empty() {
    let targets = registration::targets(
        "[1000]\niax_registration_url=\n",
        &SecretsFile::parse("[general]\niax_secret=secret\n").unwrap(),
    )
    .unwrap();

    assert!(targets.is_empty());
}

#[test]
fn registration_is_skipped_when_no_secret_is_configured() {
    let targets = registration::targets(
        "[1000]\niax_registration_url=https://register.example/\n",
        &SecretsFile::parse("").unwrap(),
    )
    .unwrap();

    assert!(targets.is_empty());
}
