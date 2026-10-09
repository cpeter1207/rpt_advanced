use super::*;

struct Directory {
    record: Option<String>,
    srv: Result<Option<(String, u16)>, BackendError>,
    addresses: Result<Vec<IpAddr>, BackendError>,
}
impl Backend for Directory {
    fn record(&self, path: &str, _: &str) -> Result<Option<String>, BackendError> {
        Ok(if path == "static" {
            self.record.clone()
        } else {
            Some("radio@external/123,192.0.2.1".into())
        })
    }
    fn srv(&self, service: &str) -> Result<Option<(String, u16)>, BackendError> {
        assert_eq!(service, "_iax._udp.123.nodes.allstarlink.org");
        self.srv.clone()
    }
    fn addresses(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, BackendError> {
        assert!(host == "123.nodes.allstarlink.org" || host == "srv.example");
        assert!(port == 4569 || port == 4571);
        self.addresses.clone()
    }
}
fn resolver(method: u32) -> DirectoryResolver<Directory> {
    DirectoryResolver::new(
        Directory {
            record: None,
            srv: Ok(None),
            addresses: Ok(vec![]),
        },
        method,
        "static",
        "external",
    )
}

#[test]
fn authoritative_static_records_and_authentication_never_fall_back() {
    let mut directory = resolver(0);
    for record in [
        "missing-separator",
        "wrong@host/123,192.0.2.1",
        "radio@/123,192.0.2.1",
        "radio@host/999,192.0.2.1",
        "radio@ho st/123,192.0.2.1",
        "radio@host\0/123,192.0.2.1",
        "radio@host/123,hostname",
        "radio@host/123,192.0.2.1 ",
        "radio@host/123,192.0.2.1,192.0.2.2",
    ] {
        directory.backend.record = Some(record.into());
        assert_eq!(
            directory.lookup("123", None),
            Err(DirectoryError::InvalidRecord),
            "{record}"
        );
    }
    directory.backend.record = Some("radio@static/123,::ffff:192.0.2.1".into());
    assert_eq!(
        directory.lookup("123", Some("192.0.2.1")),
        Ok("radio@static/123".into())
    );
    assert_eq!(
        directory.lookup("123", Some("192.0.2.2")),
        Err(DirectoryError::SourceMismatch)
    );
    for node in ["", "a", "123/1", "123\0", &"1".repeat(64)] {
        assert_eq!(
            directory.lookup(node, None),
            Err(DirectoryError::InvalidRecord)
        );
    }
    assert_eq!(
        directory.lookup("123", Some("not-an-ip")),
        Err(DirectoryError::InvalidRecord)
    );
    assert_eq!(
        resolver(3).lookup("123", None),
        Err(DirectoryError::InvalidRecord)
    );
}

#[test]
fn dns_precedence_uses_srv_port_any_matching_address_and_ipv6() {
    let mut directory = resolver(0);
    directory.backend.srv = Ok(Some(("srv.example".into(), 4571)));
    directory.backend.addresses = Ok(vec![
        "192.0.2.2".parse().unwrap(),
        "192.0.2.1".parse().unwrap(),
    ]);
    assert_eq!(
        directory.lookup("123", None),
        Ok("radio@192.0.2.2:4571/123".into())
    );
    assert_eq!(
        directory.lookup("123", Some("::ffff:192.0.2.1")),
        Ok("radio@192.0.2.1:4571/123".into())
    );
    assert_eq!(
        directory.lookup("123", Some("192.0.2.3")),
        Err(DirectoryError::SourceMismatch)
    );
    directory.backend.srv = Ok(None);
    directory.backend.addresses = Ok(vec!["2001:db8::1".parse().unwrap()]);
    assert_eq!(
        directory.lookup("123", Some("2001:db8::1")),
        Ok("radio@[2001:db8::1]:4569/123".into())
    );
}

#[test]
fn backend_error_translation_preserves_dns_fallback_and_diagnostics() {
    assert_eq!(
        resolver(0).lookup("123", Some("192.0.2.1")),
        Ok("radio@external/123".into()),
        "an empty DNS answer permits an authenticated external-file fallback"
    );
    for method in 0..=2 {
        let mut directory = resolver(method);
        let fallback = Ok("radio@external/123".into());
        assert_eq!(
            directory.lookup("123", None),
            if method == 1 {
                Err(DirectoryError::NotFound)
            } else {
                fallback.clone()
            }
        );
        for backend_error in [BackendError::Rejected, BackendError::Unavailable] {
            let expected = match (method, backend_error) {
                (2, _) | (0, BackendError::Unavailable) => fallback.clone(),
                (_, BackendError::Rejected) => Err(DirectoryError::InvalidRecord),
                _ => Err(DirectoryError::DnsFailed),
            };
            directory.backend.srv = Err(backend_error);
            assert_eq!(directory.lookup("123", None), expected);
            directory.backend.srv = Ok(None);
            directory.backend.addresses = Err(backend_error);
            assert_eq!(directory.lookup("123", None), expected);
            directory.backend.addresses = Ok(vec![]);
        }
    }
    let directory = DirectoryResolver::new(resolver(2).backend, 2, "", "");
    assert_eq!(directory.lookup("123", None), Err(DirectoryError::NotFound));
}
