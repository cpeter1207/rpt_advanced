//! Check real registered scope layering for public command and mailbox resources.
use super::{mailbox::MailboxError, *};
use crate::config::resources::{
    ResourceWarning,
    candidate::{CapturedResource, ResourceRegistry},
};
use serde_json::json;

fn id(n: u8) -> ResourceId {
    format!("929e52be-6854-4c46-85f2-{n:012}").parse().unwrap()
}

#[test]
fn command_and_mailbox_public_profiles_follow_exact_scopes_and_warn_without_private_values() {
    let site = ResourceScope::Site(id(1));
    let port = ResourceScope::Port(id(2));
    let binding = |n, scope, kind| ResourceBinding {
        id: id(n),
        scope,
        kind,
    };
    let site_command = binding(3, site, ResourceKind::Command);
    let port_command = binding(4, port, ResourceKind::Command);
    let site_box = binding(5, site, ResourceKind::Mailbox);
    let port_box = binding(6, port, ResourceKind::Mailbox);
    let key = PortKey::new("001");
    for (has_port, shadow, complete) in [
        (false, false, true),
        (true, false, true),
        (true, true, true),
        (true, true, false),
    ] {
        let mut ports = Vec::new();
        let mut sections = vec![site_command];
        let mut members = vec![(site_box, key.clone())];
        let mut rows = vec![
            (
                binding(1, site, ResourceKind::Metadata),
                json!({"inherit_from":null,"name":"site"}),
            ),
            (
                site_command,
                json!({"muting":false,"admin":{"timing":{"timeout_ms":222}},"regeneration":{"enabled":true,"level_dbfs":-60,"timing":{"duration_ms":333}}}),
            ),
            (
                site_box,
                json!({"first_name":"Ada","callsign":"N0TEST","extra":42}),
            ),
        ];
        if has_port {
            ports.push((id(2), PortKey::new("port")));
            sections.push(port_command);
            rows.extend([
                (binding(2,port,ResourceKind::Metadata),json!({"inherit_from":"site","name":"port"})),
                (port_command,json!({"muting":true,"admin":{"timing":{"timeout_ms":0}},"regeneration":{"timing":{"duration_ms":1001,"gap_ms":1},"level_dbfs":-61},"maximum_digits":17})),
            ]);
        }
        if shadow {
            members.push((port_box, key.clone()));
            let body = if complete {
                json!({"first_name":"Bea","callsign":"W0TEST"})
            } else {
                json!({"first_name":"Bea"})
            };
            rows.push((port_box, body));
        }
        let registry = ResourceRegistry::with_members(id(1), &ports, &sections, &members).unwrap();
        let docs: Vec<_> = rows
            .into_iter()
            .map(|(b, body)| {
                let mut doc =
                    json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":body});
                if b.kind == ResourceKind::Mailbox {
                    doc["key"] = json!(key.as_str());
                }
                (b, serde_json::to_vec(&doc).unwrap())
            })
            .collect();
        let captures = docs
            .iter()
            .map(|(binding, json)| CapturedResource {
                binding: *binding,
                json,
            })
            .collect::<Vec<_>>();
        let c = ModularCandidate::assemble(&registry, &captures).unwrap();
        let scope = if has_port { port } else { site };
        let policy = c.command_policy(scope).unwrap();
        assert_eq!(policy.admin_timeout_ms, 222);
        assert_eq!(policy.muting, has_port);
        assert_eq!(policy.regeneration.duration_ms, 333);
        assert_eq!(policy.regeneration.gap_ms, if has_port { 1 } else { 50 });
        assert_eq!(policy.regeneration.level_dbfs, -60);
        assert!(policy.regeneration.enabled);
        assert_eq!(
            policy.source(&["admin", "timing", "timeout_ms"]),
            Some(site_command)
        );
        assert_eq!(
            policy.source(&["regeneration", "timing", "gap_ms"]),
            has_port.then_some(port_command)
        );
        let views = policy.effective_fields();
        assert_eq!(views.len(), 3);
        assert_eq!(views["admin"], json!({"timing":{"timeout_ms":222}}));
        if has_port {
            assert_eq!(policy.warnings.len(), 3);
            assert!(policy.warnings.iter().all(|w| w.resource == port_command));
            assert_eq!(
                policy.local(id(4)).unwrap(),
                json!({"muting":true,"admin":{"timing":{}},"regeneration":{"timing":{"gap_ms":1}}})
                    .as_object()
                    .unwrap()
            );
        } else {
            assert!(policy.warnings.is_empty());
        }
        assert!(c.command_policy(ResourceScope::Port(id(99))).is_none());
        let boxes = c.mailbox_profiles(scope);
        if !complete {
            assert!(matches!(boxes, Err(MailboxError::Incomplete(b)) if b==port_box));
            assert!(c.credential_owners().is_err());
            continue;
        }
        let boxes = boxes.unwrap();
        let selected = &boxes.effective[&key];
        assert_eq!(selected.binding, if shadow { port_box } else { site_box });
        assert_eq!(selected.public_fields(),json!({"first_name":if shadow {"Bea"} else {"Ada"},"callsign":if shadow {"W0TEST"} else {"N0TEST"}}).as_object().unwrap().clone());
        assert_eq!(boxes.warnings.len(), 1);
        assert_eq!(boxes.warnings[0].warning, ResourceWarning::UnknownField);
        let owners = c.credential_owners().unwrap();
        let pin = &owners[&(selected.binding.id, credentials::CredentialSlot::Pin)];
        assert_eq!(pin.binding, selected.binding);
        assert_eq!(pin.affinity, Some(json!(["001"])));
    }
}
