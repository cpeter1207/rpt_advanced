use std::{collections::BTreeMap, fmt, fs::File, io::Read, path::Path};

use rpt_advanced_core::config::ConfigDocument;

/// IAX secrets parsed from an owner-only configuration file.
#[derive(Debug, PartialEq, Eq)]
pub struct SecretsFile {
    general: Option<String>,
    nodes: BTreeMap<String, String>,
}

impl SecretsFile {
    /// Parse global and per-node IAX secrets without exposing values in errors.
    pub fn parse(text: &str) -> Result<Self, SecretError> {
        let document = ConfigDocument::parse(text).map_err(|_| SecretError("invalid syntax"))?;
        let mut secrets = Self {
            general: None,
            nodes: BTreeMap::new(),
        };
        for entry in document.entries() {
            if entry.key != "iax_secret" {
                return Err(SecretError("unknown option"));
            }
            if entry.value.is_empty() {
                return Err(SecretError("empty secret"));
            }
            if entry.section == "general" {
                secrets.general = Some(entry.value.clone());
            } else if entry.section.len() <= 63
                && entry.section.bytes().all(|byte| byte.is_ascii_digit())
            {
                secrets
                    .nodes
                    .insert(entry.section.clone(), entry.value.clone());
            } else {
                return Err(SecretError("invalid section"));
            }
        }
        Ok(secrets)
    }

    /// Return a node-specific secret, falling back to the global default.
    pub fn for_node(&self, node: &str) -> Option<&str> {
        self.nodes
            .get(node)
            .or(self.general.as_ref())
            .map(String::as_str)
    }

    /// Load a secrets file after enforcing owner-only Unix permissions.
    pub fn load(path: &Path) -> Result<Self, SecretError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(path)
            .map_err(|_| SecretError("cannot open secrets file"))?;
        validate_file(&file)?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|_| SecretError("cannot read secrets file"))?;
        Self::parse(&text)
    }
}

fn validate_file(file: &File) -> Result<(), SecretError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file
            .metadata()
            .map_err(|_| SecretError("cannot inspect secrets file"))?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(SecretError(
                "secrets file must be owned by this user with mode 0600",
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Err(SecretError("secrets files require Unix permissions"))
    }
}

/// A safe-to-display secrets-file validation error that never includes a secret.
#[derive(Debug, PartialEq, Eq)]
pub struct SecretError(&'static str);

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for SecretError {}

#[cfg(test)]
mod tests {
    use super::SecretsFile;

    #[test]
    fn node_secret_overrides_general_default() {
        let secrets = SecretsFile::parse(
            "[general]\niax_secret=global-secret\n[524950]\niax_secret=node-secret\n",
        )
        .unwrap();

        assert_eq!(secrets.for_node("524950"), Some("node-secret"));
        assert_eq!(secrets.for_node("508422"), Some("global-secret"));
    }

    #[test]
    fn empty_secret_file_has_no_default_secret() {
        let secrets = SecretsFile::parse("").unwrap();

        assert_eq!(secrets.for_node("524950"), None);
    }

    #[test]
    fn unknown_option_is_rejected_without_echoing_secret_text() {
        let source = "[general]\nprivate-value=do-not-print-this-secret\n";
        let error = SecretsFile::parse(source).unwrap_err().to_string();

        assert!(error.contains("unknown"));
        assert!(!error.contains("do-not-print-this-secret"));
    }

    #[test]
    fn empty_secret_value_is_rejected_without_echoing_value() {
        let error = SecretsFile::parse("[524950]\niax_secret=\n")
            .unwrap_err()
            .to_string();

        assert!(error.contains("empty"));
        assert!(!error.contains("iax_secret="));
    }

    #[test]
    fn malformed_syntax_and_non_node_sections_are_rejected() {
        assert_eq!(
            SecretsFile::parse("not a configuration entry")
                .unwrap_err()
                .to_string(),
            "invalid syntax"
        );
        assert_eq!(
            SecretsFile::parse("[]\niax_secret=private\n")
                .unwrap_err()
                .to_string(),
            "invalid syntax"
        );
        for section in ["invalid".to_owned(), "1".repeat(64)] {
            let source = format!("[{section}]\niax_secret=private\n");
            assert_eq!(
                SecretsFile::parse(&source).unwrap_err().to_string(),
                "invalid section"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn secrets_file_rejects_symlinks_and_non_utf8_contents() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
            time::{SystemTime, UNIX_EPOCH},
        };

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let original = std::env::temp_dir().join(format!("rpt-advanced-secret-{stamp}"));
        let link = std::env::temp_dir().join(format!("rpt-advanced-secret-link-{stamp}"));
        fs::write(&original, "[general]\niax_secret=example\n").unwrap();
        fs::set_permissions(&original, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&original, &link).unwrap();
        assert_eq!(
            SecretsFile::load(&link).unwrap_err().to_string(),
            "cannot open secrets file"
        );
        fs::remove_file(&link).unwrap();

        fs::write(&original, [0xff]).unwrap();
        assert_eq!(
            SecretsFile::load(&original).unwrap_err().to_string(),
            "cannot read secrets file"
        );
        fs::remove_file(original).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secrets_file_must_be_mode_0600_and_owned_by_current_user() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            path::PathBuf,
            time::{SystemTime, UNIX_EPOCH},
        };

        let path = std::env::temp_dir().join(format!(
            "rpt-advanced-secrets-{}.conf",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(
            SecretsFile::load(&std::env::temp_dir())
                .unwrap_err()
                .to_string(),
            "secrets file must be owned by this user with mode 0600"
        );
        write_secrets(&path, 0o600);
        assert!(SecretsFile::load(&path).is_ok());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(SecretsFile::load(&path).is_err());

        unsafe extern "C" {
            fn geteuid() -> u32;
            fn fchown(file_descriptor: i32, owner: u32, group: u32) -> i32;
        }
        if unsafe { geteuid() } == 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let file = fs::File::open(&path).unwrap();
            assert_eq!(
                unsafe { fchown(std::os::fd::AsRawFd::as_raw_fd(&file), 1, 1) },
                0
            );
            assert!(SecretsFile::load(&path).is_err());
        }
        fs::remove_file(path).unwrap();

        fn write_secrets(path: &PathBuf, mode: u32) {
            use std::os::unix::fs::PermissionsExt;
            fs::write(path, "[general]\niax_secret=example\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}
