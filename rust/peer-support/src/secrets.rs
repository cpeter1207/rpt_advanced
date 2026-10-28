//! Private credential-file loading shared by standalone and the Asterisk host.

use std::{
    fmt,
    path::{Path, PathBuf},
};

/// A content-free error safe for host diagnostics.
#[derive(Debug, PartialEq, Eq)]
pub struct SecretError(&'static str);

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for SecretError {}

/// Secrets are deliberately not printable and never inherit between accounts.
pub struct AccountSecrets {
    directory_password: String,
    proxy_password: Option<String>,
}

impl AccountSecrets {
    /// Borrow the directory password only when preparing the provider account.
    pub fn directory_password(&self) -> &str {
        &self.directory_password
    }

    /// Borrow the independent, optional user-supplied Proxy password.
    pub fn proxy_password(&self) -> Option<&str> {
        self.proxy_password.as_deref()
    }
}

/// Read an opened regular file owned by the effective host user with mode 0600.
pub fn read_private_file(path: &Path) -> Result<String, SecretError> {
    #[cfg(unix)]
    {
        use std::{
            io::Read,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        // Inspect the opened descriptor, not a pathname that can change underneath us.
        // Nonblocking open also lets us reject a FIFO without waiting for its writer.
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| SecretError("cannot open secrets file"))?;
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
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|_| SecretError("cannot read secrets file"))?;
        Ok(text)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(SecretError("secrets files require Unix permissions"))
    }
}

/// Resolve omitted and relative paths against the actual host configuration file.
pub fn account_secrets_path(configuration: &Path, configured: Option<&Path>) -> PathBuf {
    configuration
        .parent()
        .unwrap_or(Path::new(""))
        .join(configured.unwrap_or(Path::new("echolink-secrets.conf")))
}

/// Load only the selected local controller node's credentials, without defaults.
pub fn load_account_secrets(path: &Path, account: &str) -> Result<AccountSecrets, SecretError> {
    parse(&read_private_file(path)?, account)
}

fn parse(source: &str, account: &str) -> Result<AccountSecrets, SecretError> {
    use std::collections::BTreeMap;
    let invalid = || SecretError("invalid account secrets");
    let mut accounts = BTreeMap::<&str, AccountSecrets>::new();
    let mut current = None;
    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            if section.is_empty()
                || section.len() > 63
                || !section.bytes().all(|byte| byte.is_ascii_digit())
                || accounts.contains_key(section)
            {
                return Err(invalid());
            }
            accounts.insert(
                section,
                AccountSecrets {
                    directory_password: String::new(),
                    proxy_password: None,
                },
            );
            current = Some(section);
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(invalid)?;
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(invalid());
        }
        let secrets = accounts
            .get_mut(current.ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
        match key.trim() {
            "directory_password" if secrets.directory_password.is_empty() => {
                secrets.directory_password = value.to_owned()
            }
            "proxy_password" if secrets.proxy_password.is_none() => {
                secrets.proxy_password = Some(value.to_owned())
            }
            _ => return Err(invalid()),
        }
    }
    if accounts
        .values()
        .any(|secrets| secrets.directory_password.is_empty())
    {
        return Err(invalid());
    }
    accounts
        .remove(account)
        .ok_or(SecretError("missing account directory password"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_paths_follow_each_hosts_configuration_directory() {
        let configuration = Path::new("service/config/rpt_advanced.conf");
        assert_eq!(
            account_secrets_path(configuration, None),
            Path::new("service/config/echolink-secrets.conf")
        );
        assert_eq!(
            account_secrets_path(configuration, Some(Path::new("private/echo.conf"))),
            Path::new("service/config/private/echo.conf")
        );
        let absolute = std::env::temp_dir().join("echo-credentials.conf");
        assert_eq!(
            account_secrets_path(configuration, Some(&absolute)),
            absolute
        );
        assert_eq!(
            account_secrets_path(Path::new("rpt_advanced.conf"), None),
            Path::new("echolink-secrets.conf")
        );
    }

    #[test]
    fn secrets_select_one_account_without_inheritance_or_printable_values() {
        let source = "; private credentials\n[524950]\ndirectory_password = first value\nproxy_password = independent value\n[508422]\ndirectory_password = second value\n";
        let first = parse(source, "524950").ok().unwrap();
        assert_eq!(first.directory_password(), "first value");
        assert_eq!(first.proxy_password(), Some("independent value"));
        let second = parse(source, "508422").ok().unwrap();
        assert_eq!(second.directory_password(), "second value");
        assert_eq!(second.proxy_password(), None);
        assert_eq!(
            parse(source, "1").err().unwrap().to_string(),
            "missing account directory password"
        );
    }

    #[test]
    fn secrets_reject_malformed_or_ambiguous_records_without_content_in_errors() {
        for source in [
            "[general]\ndirectory_password=private value",
            "[524950]\ndirectory_password=",
            "[524950]\ndirectory_password=private value\nproxy_password=",
            "[524950]\ndirectory_password=private value\nunknown=private value",
            "directory_password=private value",
            "[invalid]\ndirectory_password=private value",
            "[524950]\ndirectory_password=private value\ndirectory_password=private value",
            "[524950]\ndirectory_password=private value\n[524950]\ndirectory_password=private value",
            "[524950]\ndirectory_password=private value\n[508422]\nproxy_password=private value",
            "[524950]\ndirectory_password=private\0value",
            "[524950\ndirectory_password=private value",
        ] {
            let error = parse(source, "524950")
                .err()
                .expect("invalid file accepted");
            assert!(!format!("{error:?}: {error}").contains("private"));
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn secrets_require_unix_file_permissions() {
        assert_eq!(
            read_private_file(Path::new("credentials.conf"))
                .unwrap_err()
                .to_string(),
            "secrets files require Unix permissions"
        );
    }

    #[cfg(unix)]
    #[test]
    fn secrets_file_checks_the_open_descriptor_and_never_follows_symlinks() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
            time::{SystemTime, UNIX_EPOCH},
        };
        let directory = std::env::temp_dir().join(format!(
            "rptadv-peer-secrets-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("credentials.conf");
        fs::write(&path, "[524950]\ndirectory_password=private value\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_account_secrets(&path, "524950")
                .ok()
                .unwrap()
                .directory_password(),
            "private value"
        );
        assert!(read_private_file(&directory).is_err());
        assert!(read_private_file(&directory.join("missing")).is_err());
        let fifo = directory.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_private_file(&fifo).is_err());
        fs::remove_file(fifo).unwrap();
        let link = directory.join("link");
        symlink(&path, &link).unwrap();
        assert_eq!(
            read_private_file(&link).unwrap_err().to_string(),
            "cannot open secrets file"
        );
        fs::remove_file(link).unwrap();
        for mode in [0o400, 0o640, 0o660, 0o1600] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(read_private_file(&path).is_err());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, [0xff]).unwrap();
        assert_eq!(
            read_private_file(&path).unwrap_err().to_string(),
            "cannot read secrets file"
        );
        if unsafe { libc::geteuid() } == 0 {
            use std::os::fd::AsRawFd;
            let file = fs::File::open(&path).unwrap();
            assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 1, 1) }, 0);
            assert!(read_private_file(&path).is_err());
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
