use super::*;
use crate::configuration::PhoneAccountProtocol;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "rptadv-phone-accounts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let fixture = Self { directory };
        fixture.secret(" literal;#secret ");
        fixture
    }
    fn secret(&self, value: &str) {
        let path = self.directory.join("phone.secret");
        fs::write(&path, value).unwrap();
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn configuration(&self) -> PathBuf {
        self.directory.join("rpt_advanced.conf")
    }
    fn record(&self) -> ResolvedPhoneAccount {
        ResolvedPhoneAccount {
            id: "provider-account".into(),
            local_node: "local-node".into(),
            protocol: PhoneAccountProtocol::Iax,
            enabled: true,
            server: "127.0.0.1:4569".into(),
            username: "provider-login".into(),
            secrets_file: "phone.secret".into(),
            register: true,
            registration_interval_s: 60,
            authentication_timeout_ms: 1000,
        }
    }
    fn prepare(
        &self,
        owner: &mut AccountPreparer,
        records: Vec<ResolvedPhoneAccount>,
        active: &PreparedAccounts,
    ) -> Result<PreparedAccounts, PreparationError> {
        owner.prepare(&self.configuration(), records, active, |_| {
            panic!("numeric endpoint used DNS")
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn literal_secret_preserves_symbols_and_spaces_with_one_terminal_newline() {
    let fixture = Fixture::new();
    for ending in [
        "", "
", "\r
",
    ] {
        fixture.secret(&format!(" literal;#secret {ending}"));
        let record = fixture.record();
        let prepared = fixture
            .prepare(
                &mut AccountPreparer::default(),
                vec![record.clone()],
                &PreparedAccounts::default(),
            )
            .unwrap();
        let account = &prepared.as_slice()[0];
        assert_eq!(account.secret(), " literal;#secret ");
        assert_eq!(account.settings(), &record);
        assert_eq!(account.remote(), "127.0.0.1:4569".parse().unwrap());
        assert_ne!(account.generation(), 0);
        let debug = format!("{account:?}");
        for hidden in ["literal", "127.0.0.1", "provider-login", "phone.secret"] {
            assert!(!debug.contains(hidden));
        }
    }
    let mut absolute = fixture.record();
    absolute.secrets_file = fixture
        .directory
        .join("phone.secret")
        .to_str()
        .unwrap()
        .into();
    assert!(
        fixture
            .prepare(
                &mut AccountPreparer::default(),
                vec![absolute],
                &PreparedAccounts::default()
            )
            .is_ok()
    );
}

#[test]
fn disabled_accounts_do_not_read_files_or_resolve_endpoints() {
    let mut record = Fixture::new().record();
    record.enabled = false;
    record.secrets_file = "/missing/private-file".into();
    record.server = "must-not-resolve.invalid:4569".into();
    let prepared = AccountPreparer::default()
        .prepare(
            Path::new("missing/config"),
            vec![record],
            &PreparedAccounts::default(),
            |_| panic!("disabled account resolved"),
        )
        .unwrap();
    assert!(prepared.as_slice().is_empty());
}

#[cfg(unix)]
#[test]
fn unchanged_complete_identity_reuses_the_exact_owned_generation() {
    let fixture = Fixture::new();
    let mut owner = AccountPreparer::default();
    let first = fixture.record();
    let mut second = first.clone();
    second.id = "another-provider".into();
    let active = fixture
        .prepare(
            &mut owner,
            vec![first.clone(), second.clone()],
            &PreparedAccounts::default(),
        )
        .unwrap();
    let candidate = fixture
        .prepare(&mut owner, vec![second, first], &active)
        .unwrap();
    assert_eq!(candidate.as_slice()[0].settings().id, "another-provider");
    for (old, next) in active.as_slice().iter().zip(candidate.as_slice()) {
        assert!(Arc::ptr_eq(old, next));
        assert_eq!(old.generation(), next.generation());
    }
}

#[cfg(unix)]
#[test]
fn every_resolved_setting_and_secret_change_gets_a_fresh_generation() {
    let fixture = Fixture::new();
    let mut owner = AccountPreparer::default();
    let original = fixture.record();
    let active = fixture
        .prepare(
            &mut owner,
            vec![original.clone()],
            &PreparedAccounts::default(),
        )
        .unwrap();
    let old = &active.as_slice()[0];
    for variant in 0..10 {
        fixture.secret(" literal;#secret ");
        let mut next = original.clone();
        match variant {
            0 => next.id = "changed-account".into(),
            1 => next.local_node = "changed-local-node".into(),
            2 => next.protocol = PhoneAccountProtocol::Sip,
            3 => next.server = "127.0.0.2:4569".into(),
            4 => next.username = "changed-login".into(),
            5 => {
                next.secrets_file = fixture
                    .directory
                    .join("phone.secret")
                    .to_str()
                    .unwrap()
                    .into()
            }
            6 => next.register = false,
            7 => next.registration_interval_s += 1,
            8 => next.authentication_timeout_ms += 1,
            _ => fixture.secret("changed-literal-secret"),
        }
        let prepared = fixture.prepare(&mut owner, vec![next], &active).unwrap();
        let changed = &prepared.as_slice()[0];
        assert!(!Arc::ptr_eq(old, changed), "variant {variant}");
        assert!(changed.generation() > old.generation());
    }
    assert_eq!(old.secret(), " literal;#secret ");
}

#[cfg(unix)]
#[test]
fn failed_or_dropped_preparation_never_mutates_active_accounts() {
    let fixture = Fixture::new();
    let mut owner = AccountPreparer::default();
    let original = fixture.record();
    let active = fixture
        .prepare(
            &mut owner,
            vec![original.clone()],
            &PreparedAccounts::default(),
        )
        .unwrap();
    let old = active.as_slice()[0].clone();
    let mut changed = original.clone();
    changed.username = "changed-login".into();
    let mut invalid = original.clone();
    invalid.id = "z-last".into();
    invalid.secrets_file = "missing-private-file".into();
    assert!(
        fixture
            .prepare(&mut owner, vec![changed.clone(), invalid], &active)
            .is_err()
    );
    assert!(Arc::ptr_eq(&old, &active.as_slice()[0]));
    assert_eq!(old.settings(), &original);
    assert_eq!(old.secret(), " literal;#secret ");
    let candidate = fixture
        .prepare(&mut owner, vec![changed.clone()], &active)
        .unwrap();
    let abandoned_generation = candidate.as_slice()[0].generation();
    drop(candidate);
    let retry = fixture.prepare(&mut owner, vec![changed], &active).unwrap();
    assert!(retry.as_slice()[0].generation() > abandoned_generation);
    assert!(Arc::ptr_eq(&old, &active.as_slice()[0]));
}

#[cfg(unix)]
#[test]
fn credentials_enforce_protocol_limits_controls_and_private_file_bound() {
    let fixture = Fixture::new();
    for value in [
        "".to_owned(),
        "
"
        .into(),
        "\r".into(),
        "value

"
        .into(),
        "value\t".into(),
        "value\0".into(),
        "x".repeat(256),
        "x".repeat(4097),
    ] {
        fixture.secret(&value);
        let error = fixture
            .prepare(
                &mut AccountPreparer::default(),
                vec![fixture.record()],
                &PreparedAccounts::default(),
            )
            .err()
            .expect("invalid credential accepted");
        let display = format!("{error:?}: {error}");
        assert!(!display.contains("value") && !display.contains("phone.secret"));
    }
    fixture.secret(&"x".repeat(255));
    assert!(
        fixture
            .prepare(
                &mut AccountPreparer::default(),
                vec![fixture.record()],
                &PreparedAccounts::default()
            )
            .is_ok()
    );
    fixture.secret(&"x".repeat(4096));
    let mut sip = fixture.record();
    sip.protocol = PhoneAccountProtocol::Sip;
    assert!(
        fixture
            .prepare(
                &mut AccountPreparer::default(),
                vec![sip],
                &PreparedAccounts::default()
            )
            .is_ok()
    );
}

#[cfg(unix)]
#[test]
fn resolver_is_caller_owned_and_endpoint_changes_advance_generation() {
    let fixture = Fixture::new();
    let mut record = fixture.record();
    record.server = "fixture.invalid:5060".into();
    let mut owner = AccountPreparer::default();
    let active = owner
        .prepare(
            &fixture.configuration(),
            vec![record.clone()],
            &PreparedAccounts::default(),
            |input| {
                assert_eq!(input, "fixture.invalid:5060");
                Ok("[::1]:5060".parse().unwrap())
            },
        )
        .unwrap();
    let changed = owner
        .prepare(
            &fixture.configuration(),
            vec![record.clone()],
            &active,
            |_| Ok("127.0.0.1:5060".parse().unwrap()),
        )
        .unwrap();
    assert!(changed.as_slice()[0].generation() > active.as_slice()[0].generation());
    let error = owner
        .prepare(&fixture.configuration(), vec![record], &active, |_| {
            Err(io::Error::other("private resolver detail"))
        })
        .err()
        .unwrap();
    assert!(!format!("{error:?}: {error}").contains("private"));
    for endpoint in ["0.0.0.0:4569", "127.0.0.1:0"] {
        let mut invalid = fixture.record();
        invalid.server = endpoint.into();
        assert!(fixture.prepare(&mut owner, vec![invalid], &active).is_err());
    }
}

#[test]
fn duplicate_enabled_labels_fail_before_any_external_preparation() {
    let fixture = Fixture::new();
    let mut record = fixture.record();
    record.secrets_file = "must-not-read-missing-secret".into();
    record.server = "must-not-resolve.invalid:4569".into();
    let error = fixture
        .prepare(
            &mut AccountPreparer::default(),
            vec![record.clone(), record],
            &PreparedAccounts::default(),
        )
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "duplicate phone account label");
}

#[cfg(unix)]
#[test]
fn exhausted_generation_space_never_reuses_an_old_identity() {
    let fixture = Fixture::new();
    let mut owner = AccountPreparer::default();
    let record = fixture.record();
    let active = fixture
        .prepare(
            &mut owner,
            vec![record.clone()],
            &PreparedAccounts::default(),
        )
        .unwrap();
    owner.last_generation = u64::MAX;
    let unchanged = fixture
        .prepare(&mut owner, vec![record.clone()], &active)
        .unwrap();
    assert!(Arc::ptr_eq(&unchanged.as_slice()[0], &active.as_slice()[0]));
    let mut changed = record;
    changed.username = "changed-login".into();
    assert_eq!(
        fixture
            .prepare(&mut owner, vec![changed], &active)
            .err()
            .unwrap()
            .to_string(),
        "phone account generation space exhausted"
    );
}
