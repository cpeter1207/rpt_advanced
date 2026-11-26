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
fn managed_directory_proof_rechecks_source_account_and_retired_selection() {
    let source: IpAddr = "192.0.2.1".parse().unwrap();
    let unauthorized: IpAddr = "192.0.2.2".parse().unwrap();
    let row =
        AllStarLinkStaticNode::new("registered.example", 4570_u16.try_into().unwrap(), source)
            .unwrap();
    let selected = Arc::new(ManagedDirectory {
        station: [1; 16],
        account: [2; 16],
        generation: 1_u64.try_into().unwrap(),
        method: 0,
        external: "external".into(),
        registered: Arc::new(BTreeMap::from([("123".to_owned(), row)])),
    });
    let proof = selected
        .resolve(resolver(0).backend, "123", Some(source))
        .unwrap();
    assert_eq!(
        proof.destination(&selected, "123", Some(source)),
        Some("radio@registered.example:4570/123")
    );
    assert!(
        proof
            .destination(&selected, "123", Some(unauthorized))
            .is_none()
    );
    assert!(proof.destination(&selected, "124", Some(source)).is_none());
    assert!(matches!(
        selected.resolve(resolver(0).backend, "123", Some(unauthorized)),
        Err(DirectoryError::SourceMismatch)
    ));
    let recreated = Arc::new(ManagedDirectory {
        account: [3; 16],
        registered: Arc::new(BTreeMap::new()),
        ..(*selected).clone()
    });
    assert!(proof.destination(&recreated, "123", Some(source)).is_none());
    let fallback = recreated
        .resolve(resolver(0).backend, "123", Some(source))
        .unwrap();
    assert_eq!(
        fallback.destination(&recreated, "123", Some(source)),
        Some("radio@external/123")
    );
}

#[test]
fn managed_directory_uses_json_rows_before_the_external_feed() {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceKind, ResourceRegistry,
            ResourceScope,
        },
    };
    use serde_json::{Value, json};

    let site: ResourceId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap();
    let station: ResourceId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let account: ResourceId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc".parse().unwrap();
    let binding = ResourceBinding {
        id: account,
        scope: ResourceScope::Port(station),
        kind: ResourceKind::Allstarlink,
    };
    let registry =
        ResourceRegistry::new(site, &[(station, PortKey::new("1000"))], &[binding]).unwrap();
    let selected = |static_nodes: Value| {
        let encoded = registry
            .bindings()
            .map(|owner| {
                let mut body = json!({"id":owner.id.to_string(),"kind":owner.kind.as_str()});
                body["configuration"] = if owner == binding {
                    json!({"node_number":1000,"directory_file":"external","static_nodes":static_nodes})
                } else if owner.id == station {
                    json!({"name":"Station","inherit_from":"site"})
                } else {
                    json!({"name":"Site","inherit_from":null})
                };
                if owner.id == station {
                    body["key"] = json!("1000");
                }
                (owner, serde_json::to_vec(&body).unwrap())
            })
            .collect::<Vec<_>>();
        let capture = encoded
            .iter()
            .map(|(owner, json)| CapturedResource {
                binding: *owner,
                json,
            })
            .collect::<Vec<_>>();
        let profiles = ModularCandidate::assemble(&registry, &capture)
            .unwrap()
            .resolve_protocol_profiles()
            .unwrap();
        let setting = profiles
            .port(station)
            .unwrap()
            .allstarlink
            .as_ref()
            .unwrap();
        let plan = AccountPlan {
            station,
            account: setting.binding.id,
            wire: setting.settings.node_number.unwrap(),
            settings: &setting.settings,
            jitter: &setting.jitter,
        };
        Arc::new(ManagedDirectory::for_account(&plan, NonZeroU64::MIN))
    };
    let current = selected(json!({
        "123": {"host":"2001:db8::2","port":4570,"source":"::ffff:192.0.2.2"}
    }));
    let source: IpAddr = "192.0.2.2".parse().unwrap();
    let fallback_source: IpAddr = "192.0.2.1".parse().unwrap();
    let proof = current
        .resolve(resolver(0).backend, "123", Some(source))
        .unwrap();
    assert_eq!(
        proof.destination(&current, "123", Some(source)),
        Some("radio@[2001:db8::2]:4570/123")
    );
    assert!(matches!(
        current.resolve(resolver(0).backend, "123", Some(fallback_source)),
        Err(DirectoryError::SourceMismatch)
    ));
    #[cfg(target_os = "linux")]
    assert_eq!(
        current
            .resolve(resolver(0).backend, "123", None)
            .unwrap()
            .outbound_endpoint(&resolver(0).backend)
            .unwrap(),
        "[2001:db8::2]:4570".parse().unwrap()
    );

    let cleared = selected(json!({}));
    assert!(proof.destination(&cleared, "123", Some(source)).is_none());
    let fallback = cleared
        .resolve(resolver(0).backend, "123", Some(fallback_source))
        .unwrap();
    assert_eq!(
        fallback.destination(&cleared, "123", Some(fallback_source)),
        Some("radio@external/123")
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
