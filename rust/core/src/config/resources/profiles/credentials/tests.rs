use super::*;
use crate::config::resources::{
    PortKey,
    candidate::{CapturedResource, ResourceRegistry},
};

#[test]
fn private_identity_uses_complete_normalized_account_and_registry_owner() {
    use CredentialSlot::*;
    let id = |n: u32| {
        format!("929e52be-6854-4c46-85f2-{n:012}")
            .parse::<ResourceId>()
            .unwrap()
    };
    let site = ResourceScope::Site(id(0));
    let field = |n, kind| ResourceBinding {
        id: id(n),
        scope: site,
        kind,
    };
    let mailbox = field(5, ResourceKind::Mailbox);
    let rows = [
        (
            field(0, ResourceKind::Metadata),
            json!({"inherit_from":null,"name":"site"}),
        ),
        (
            field(1, ResourceKind::Allstarlink),
            json!({"node_number":123,"iax":{"registration_url":"https://example.org/"}}),
        ),
        (
            field(2, ResourceKind::Echolink),
            json!({"callsign":"n0test-r","node_number":42,"directory_servers":["a.example","B.EXAMPLE"]}),
        ),
        (
            field(3, ResourceKind::EcholinkProxy),
            json!({"host":"p.EXAMPLE","port":8101}),
        ),
        (
            field(4, ResourceKind::PhonePatch),
            json!({"provider":{"protocol":"sip","server":"S.EXAMPLE","port":5060,"username":"Exact"}}),
        ),
        (mailbox, json!({"first_name":"Ada","callsign":"N0TEST"})),
    ];
    let registry = ResourceRegistry::with_members(
        id(0),
        &[],
        &rows[1..5].iter().map(|r| r.0).collect::<Vec<_>>(),
        &[(mailbox, PortKey::new("001"))],
    )
    .unwrap();
    for complete in [true, false] {
        let docs = rows
            .iter()
            .map(|(b, body)| {
                let mut envelope =
                    json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":body});
                if b.kind == ResourceKind::Mailbox {
                    envelope["key"] = json!("001");
                    if !complete {
                        envelope["configuration"]
                            .as_object_mut()
                            .unwrap()
                            .remove("first_name");
                    }
                }
                (*b, serde_json::to_vec(&envelope).unwrap())
            })
            .collect::<Vec<_>>();
        let captures = docs
            .iter()
            .map(|(b, body)| CapturedResource {
                binding: *b,
                json: body,
            })
            .collect::<Vec<_>>();
        let owners = ModularCandidate::assemble(&registry, &captures)
            .unwrap()
            .credential_owners();
        if !complete {
            assert!(owners.is_err());
            continue;
        }
        let owners = owners.unwrap();
        for (n, slot, account) in [
            (1, AllStarLink, json!([123, "https://example.org/"])),
            (
                2,
                Directory,
                json!(["N0TEST-R", 42, ["a.example", "b.example"]]),
            ),
            (3, Proxy, json!(["N0TEST-R", 42, "p.example", 8101])),
            (4, Phone, json!(["sip", "s.example", 5060, "Exact"])),
            (5, Pin, json!(["001"])),
        ] {
            assert_eq!(owners[&(id(n), slot)].affinity, Some(account));
        }
        assert_eq!(owners[&(id(3), Proxy)].parent, Some(id(2)));
    }
}
