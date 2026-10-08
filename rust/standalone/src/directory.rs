//! Standalone static and DNS lookup for numeric AllStarLink node identities.

use std::{
    fs, io,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
};

/// A static AllStarLink record was invalid or did not match the requested source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryError {
    /// Node identity, target, address, or record structure is invalid.
    InvalidRecord,
    /// The record address differs from the authenticated incoming source.
    SourceMismatch,
    /// The selected directory has no destination for this node.
    NotFound,
    /// DNS was the only selected source and did not resolve the node.
    DnsFailed,
}

/// Resolve a node by static directory, DNS, and/or external extnodes file.
///
/// DNS uses the ASL node hostname on UDP 4569. This standard-library resolver does not query
/// SRV records; deployments requiring a non-default port should provide an extnodes record.
pub fn lookup(
    method: u32,
    static_file: &str,
    external_file: &str,
    remote: &str,
    source: Option<&str>,
) -> Result<String, DirectoryError> {
    lookup_with(
        method,
        static_file,
        external_file,
        remote,
        source,
        resolve_system,
    )
}

fn resolve_system(hostname: &str) -> io::Result<Vec<SocketAddr>> {
    hostname
        .to_socket_addrs()
        .map(|addresses| addresses.collect())
}

fn lookup_with(
    method: u32,
    static_file: &str,
    external_file: &str,
    remote: &str,
    source: Option<&str>,
    mut resolve: impl FnMut(&str) -> io::Result<Vec<SocketAddr>>,
) -> Result<String, DirectoryError> {
    if remote.is_empty() || remote.len() > 63 || !remote.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DirectoryError::InvalidRecord);
    }
    if method > 2 || source.is_some_and(|source| source.parse::<IpAddr>().is_err()) {
        return Err(DirectoryError::InvalidRecord);
    }
    if let Some(record) = file_record(static_file, remote)? {
        return resolve_static_record(remote, &record, source);
    }
    if method != 2 {
        let hostname = format!("{remote}.nodes.allstarlink.org:4569");
        match resolve(&hostname) {
            Ok(addresses) => {
                if let Some(address) = addresses.iter().find(|address| {
                    source.is_none_or(|source| {
                        source.parse::<IpAddr>().ok().map(normalize)
                            == Some(normalize(address.ip()))
                    })
                }) {
                    return Ok(format!("radio@{address}/{remote}"));
                }
                if source.is_some() && !addresses.is_empty() {
                    return Err(DirectoryError::SourceMismatch);
                }
                if method == 1 {
                    return Err(DirectoryError::DnsFailed);
                }
            }
            Err(_) if method == 1 => return Err(DirectoryError::DnsFailed),
            Err(_) => {}
        }
    }
    if let Some(record) = file_record(external_file, remote)? {
        return resolve_static_record(remote, &record, source);
    }
    Err(DirectoryError::NotFound)
}

fn file_record(path: &str, remote: &str) -> Result<Option<String>, DirectoryError> {
    if path.is_empty() {
        return Ok(None);
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut extnodes = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            extnodes = &line[1..line.len() - 1] == "extnodes";
            continue;
        }
        if !extnodes || line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        let Some((node, value)) = line.split_once('=') else {
            continue;
        };
        if node.trim() == remote {
            return Ok(Some(value.trim().to_owned()));
        }
    }
    Ok(None)
}

/// Validate and return one standard `radio@host/node,address` extnodes record.
pub fn resolve_static_record(
    remote: &str,
    record: &str,
    source: Option<&str>,
) -> Result<String, DirectoryError> {
    if remote.is_empty() || remote.len() > 63 || !remote.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DirectoryError::InvalidRecord);
    }
    if record
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(DirectoryError::InvalidRecord);
    }
    let (target, address) = record
        .split_once(',')
        .filter(|(_, address)| !address.contains(','))
        .ok_or(DirectoryError::InvalidRecord)?;
    let Some((host, node)) = target
        .strip_prefix("radio@")
        .and_then(|target| target.rsplit_once('/'))
    else {
        return Err(DirectoryError::InvalidRecord);
    };
    if host.is_empty() || node != remote {
        return Err(DirectoryError::InvalidRecord);
    }
    let address = normalized_ip(address)?;
    if source.is_some_and(|source| source.parse::<IpAddr>().ok().map(normalize) != Some(address)) {
        return Err(DirectoryError::SourceMismatch);
    }
    Ok(target.to_owned())
}

fn normalized_ip(value: &str) -> Result<IpAddr, DirectoryError> {
    value
        .parse::<IpAddr>()
        .map(normalize)
        .map_err(|_| DirectoryError::InvalidRecord)
}

fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectoryError, lookup, lookup_with, resolve_static_record, resolve_system};
    use std::{io, net::SocketAddr};

    fn file(contents: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rpt-advanced-directory-{}-{}.conf",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn lookup_prefers_static_extnodes_and_authenticates_incoming_source() {
        let path = file(
            "[other]\n506315=wrong\n[extnodes]\n506315=radio@192.0.2.5:4569/506315,192.0.2.5\n",
        );
        assert_eq!(
            lookup(0, &path, "", "506315", Some("192.0.2.5")),
            Ok("radio@192.0.2.5:4569/506315".into())
        );
        assert_eq!(
            lookup(0, &path, "", "506315", Some("192.0.2.6")),
            Err(DirectoryError::SourceMismatch)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_only_lookup_uses_external_extnodes_without_dns() {
        let path = file("[extnodes]\n506316=radio@192.0.2.6:4569/506316,192.0.2.6\n");
        assert_eq!(
            lookup(2, "", &path, "506316", None),
            Ok("radio@192.0.2.6:4569/506316".into())
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_static_record_is_not_hidden_by_dns_fallback() {
        let path = file("[extnodes]\n506315=radio@192.0.2.5:4569/506316,192.0.2.5\n");
        assert_eq!(
            lookup(0, &path, "", "506315", None),
            Err(DirectoryError::InvalidRecord)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn static_record_checks_identity_and_optional_source_before_returning_target() {
        let record = "radio@192.0.2.5:4569/506315,192.0.2.5";
        assert_eq!(
            resolve_static_record("506315", record, None),
            Ok("radio@192.0.2.5:4569/506315".into())
        );
        assert_eq!(
            resolve_static_record("506315", record, Some("192.0.2.6")),
            Err(DirectoryError::SourceMismatch)
        );
        assert_eq!(
            resolve_static_record("506316", record, None),
            Err(DirectoryError::InvalidRecord)
        );
    }

    #[test]
    fn static_record_rejects_untrusted_or_malformed_fields() {
        for record in [
            "radio@192.0.2.5:4569/506315",
            "radio@192.0.2.5:4569/506315,not-an-ip",
            "radio@192.0.2.5:4569/506315,192.0.2.5,extra",
            "radio@192.0.2.5:4569/506316,192.0.2.5",
            "radio@192.0.2.5:4569/506315,192.0.2.5\n[evil]",
        ] {
            assert_eq!(
                resolve_static_record("506315", record, None),
                Err(DirectoryError::InvalidRecord),
                "record {record:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_lookup_inputs_without_resolving_dns() {
        for (method, remote, source) in [
            (0, "", None),
            (0, "50631x", None),
            (0, &"1".repeat(64), None),
            (3, "506315", None),
            (0, "506315", Some("not-an-ip")),
        ] {
            assert_eq!(
                lookup_with(method, "", "", remote, source, |_| panic!(
                    "DNS not expected"
                )),
                Err(DirectoryError::InvalidRecord)
            );
        }
    }

    #[test]
    fn dns_lookup_selects_matching_source_and_formats_the_target() {
        let address: SocketAddr = "192.0.2.10:4569".parse().unwrap();
        assert_eq!(
            lookup_with(0, "", "", "506315", Some("192.0.2.10"), |hostname| {
                assert_eq!(hostname, "506315.nodes.allstarlink.org:4569");
                Ok(vec![address])
            }),
            Ok("radio@192.0.2.10:4569/506315".into())
        );
        assert_eq!(
            lookup_with(0, "", "", "506315", None, |_| Ok(vec![address])),
            Ok("radio@192.0.2.10:4569/506315".into())
        );
    }

    #[test]
    fn system_resolver_supports_numeric_socket_addresses_without_network_access() {
        assert_eq!(
            resolve_system("127.0.0.1:4569").unwrap(),
            vec!["127.0.0.1:4569".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn dns_failure_mismatch_and_method_fallback_are_deterministic() {
        let address: SocketAddr = "192.0.2.10:4569".parse().unwrap();
        let dns_error = || Err(io::Error::new(io::ErrorKind::NotFound, "no DNS record"));
        assert_eq!(
            lookup_with(1, "", "", "506315", None, |_| dns_error()),
            Err(DirectoryError::DnsFailed)
        );
        assert_eq!(
            lookup_with(1, "", "", "506315", None, |_| Ok(vec![])),
            Err(DirectoryError::DnsFailed)
        );
        assert_eq!(
            lookup_with(0, "", "", "506315", Some("192.0.2.11"), |_| Ok(vec![
                address
            ])),
            Err(DirectoryError::SourceMismatch)
        );
        assert_eq!(
            lookup_with(0, "", "", "506315", Some("192.0.2.11"), |_| Ok(vec![])),
            Err(DirectoryError::NotFound)
        );

        let path = file("[extnodes]\n506315=radio@192.0.2.12:4569/506315,192.0.2.12\n");
        assert_eq!(
            lookup_with(0, "", &path, "506315", None, |_| dns_error()),
            Ok("radio@192.0.2.12:4569/506315".into())
        );
        let mut dns_called = false;
        assert_eq!(
            lookup_with(2, "", &path, "506315", None, |_| {
                dns_called = true;
                Ok(vec![address])
            }),
            Ok("radio@192.0.2.12:4569/506315".into())
        );
        assert!(!dns_called);
        assert_eq!(
            lookup_with(0, "", "", "506315", None, |_| Ok(vec![])),
            Err(DirectoryError::NotFound)
        );
        assert_eq!(
            lookup_with(2, "", "", "506315", None, |_| panic!("DNS must be skipped")),
            Err(DirectoryError::NotFound)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_record_skips_other_sections_comments_and_malformed_lines() {
        let path = file(
            "[other]\n506315=radio@192.0.2.1:4569/506315,192.0.2.1\n[extnodes\n506315=ignored\n[extnodes]\n\n; comment\n# comment\nnot-a-record\n506300 radio@invalid\n506300=radio@192.0.2.2:4569/506300,192.0.2.2\n506315 = radio@192.0.2.13:4569/506315,192.0.2.13\n",
        );
        assert_eq!(
            lookup(2, &path, "", "506315", None),
            Ok("radio@192.0.2.13:4569/506315".into())
        );
        assert_eq!(
            lookup(2, "", "", "506315", None),
            Err(DirectoryError::NotFound)
        );
        let other_node = file("[extnodes]\n506300=radio@192.0.2.3:4569/506300,192.0.2.3\n");
        assert_eq!(
            lookup(2, &other_node, "", "506315", None),
            Err(DirectoryError::NotFound)
        );
        std::fs::remove_file(other_node).unwrap();
        assert_eq!(
            lookup_with(1, "missing-file", "", "506315", None, |_| {
                Err(io::Error::new(io::ErrorKind::NotFound, "no DNS record"))
            }),
            Err(DirectoryError::DnsFailed)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn static_records_reject_bad_host_syntax_and_accept_mapped_ipv4_source() {
        for record in [
            "radio@/506315,192.0.2.1",
            "other@192.0.2.1/506315,192.0.2.1",
            "radio@192.0.2.1:4569,192.0.2.1",
            "radio@192.0.2.1:4569/506315,192.0.2.1\0",
        ] {
            assert_eq!(
                resolve_static_record("506315", record, None),
                Err(DirectoryError::InvalidRecord),
                "record {record:?}"
            );
        }
        assert_eq!(
            resolve_static_record(
                "506315",
                "radio@192.0.2.1:4569/506315,::ffff:192.0.2.1",
                Some("192.0.2.1")
            ),
            Ok("radio@192.0.2.1:4569/506315".into())
        );
        for remote in ["", "50631x", &"1".repeat(64)] {
            assert_eq!(
                resolve_static_record(remote, "radio@192.0.2.1:4569/506315,192.0.2.1", None),
                Err(DirectoryError::InvalidRecord)
            );
        }
        assert_eq!(
            resolve_static_record(
                "506315",
                "radio@[2001:db8::1]/506315,2001:db8::1",
                Some("2001:db8::1")
            ),
            Ok("radio@[2001:db8::1]/506315".into())
        );
    }
}
