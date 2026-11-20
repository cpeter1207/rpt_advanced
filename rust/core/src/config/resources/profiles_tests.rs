//! Hardware-free protocol resolution through real, registry-bound captures.

use super::*;
use crate::config::resources::candidate::{
    CapturedResource, ResourceKind, ResourceRegistry, ResourceScope,
};
use serde_json::json;

const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const FIRST: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SECOND: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const FIRST_KEY: &str = " /../Radio:é #;\t ";
const SECOND_KEY: &str = " /../radio:é #;\t ";

fn id(text: &str) -> ResourceId {
    text.parse().unwrap()
}

fn section(
    sequence: u8,
    scope: ResourceScope,
    kind: ResourceKind,
    configuration: Value,
) -> (ResourceBinding, Option<Value>) {
    (
        ResourceBinding {
            id: id(&format!("00000000-0000-4000-8000-{sequence:012x}")),
            scope,
            kind,
        },
        Some(configuration),
    )
}

fn resolve(
    sections: &[(ResourceBinding, Option<Value>)],
) -> Result<ResolvedProtocolProfiles, ProtocolProfileError> {
    let ports = [
        (id(FIRST), PortKey::new(FIRST_KEY)),
        (id(SECOND), PortKey::new(SECOND_KEY)),
    ];
    let registry = ResourceRegistry::new(
        id(SITE),
        &ports,
        &sections
            .iter()
            .map(|(binding, _)| *binding)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut bodies = Vec::new();
    for binding in registry.bindings() {
        let config = if binding.kind == ResourceKind::Metadata {
            match binding.scope {
                ResourceScope::Site(_) => Some(json!({"name": "Site", "inherit_from": null})),
                ResourceScope::Port(_) => Some(json!({"name": "Repeater", "inherit_from": "site"})),
            }
        } else {
            sections
                .iter()
                .find(|(resource, _)| *resource == binding)
                .and_then(|(_, value)| value.clone())
        };
        if let Some(config) = config {
            let mut envelope = json!({
                "id": binding.id.to_string(),
                "kind": binding.kind.as_str(),
                "configuration": config,
            });
            if binding.kind == ResourceKind::Metadata
                && let Some((_, key)) = ports.iter().find(|(id, _)| *id == binding.id)
            {
                envelope["key"] = json!(key.as_str());
            }
            bodies.push((binding, serde_json::to_vec(&envelope).unwrap()));
        }
    }
    let captures = bodies
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect::<Vec<_>>();
    ModularCandidate::assemble(&registry, &captures)
        .unwrap()
        .resolve_protocol_profiles()
}

fn port(text: &str) -> ResourceScope {
    ResourceScope::Port(id(text))
}

fn site() -> ResourceScope {
    ResourceScope::Site(id(SITE))
}

#[test]
fn normalized_locals_keep_valid_siblings_and_omit_authored_secrets() {
    let input = section(
        1,
        port(FIRST),
        ResourceKind::Allstarlink,
        json!({
            "node_number": 524950,
            "iax": {
                "registration_url": "https://sensitive:A-private-value@example.net/",
                "local_port": 4570
            },
            "credentials": {"password": "A-private-value"},
            "password_configured": true,
            "echolink": {"enabled": true},
            "processing": {"enabled": true},
            "static_directory_file": "A-private-value",
            "A-private-value": 123
        }),
    );
    let profiles = resolve(std::slice::from_ref(&input)).unwrap();
    let local = profiles
        .local(input.0.id)
        .map(|local| json!(local.configuration));
    assert_eq!(
        local,
        Some(json!({"node_number": 524950, "iax": {"local_port": 4570}}))
    );
    assert_eq!(profiles.warnings().len(), 7);
    assert!(
        profiles
            .warnings()
            .iter()
            .all(|warning| warning.resource == input.0)
    );
    assert!(!format!("{:?}", profiles.warnings()).contains("A-private-value"));
}

#[test]
fn site_defaults_do_not_attach_accounts_and_absent_ordinary_locals_remain_empty() {
    let default_asl = section(
        1,
        site(),
        ResourceKind::Allstarlink,
        json!({"node_number": 8}),
    );
    let explicit_echo = section(2, port(FIRST), ResourceKind::Echolink, json!({}));
    let mut empty_peer = section(3, port(FIRST), ResourceKind::PeerAudio, json!({}));
    empty_peer.1 = None;
    let empty_peer_id = empty_peer.0.id;
    let profiles = resolve(&[default_asl, explicit_echo, empty_peer]).unwrap();
    let first = profiles.port(id(FIRST)).unwrap();
    assert_eq!(first.key.as_str(), FIRST_KEY);
    assert_eq!(first.metadata.name, "Repeater");
    assert!(first.allstarlink.is_none());
    assert!(first.phone_patch.is_none());
    assert!(!first.echolink.as_ref().unwrap().settings.enabled);
    let second = profiles.port(id(SECOND)).unwrap();
    assert_eq!(second.key.as_str(), SECOND_KEY);
    assert!(second.allstarlink.is_none() && second.echolink.is_none());
    assert_eq!(first.peer_audio.unwrap().id, empty_peer_id);
}

#[test]
fn echolink_reuses_public_semantics_then_checks_final_transport() {
    let defaults = section(
        1,
        site(),
        ResourceKind::Echolink,
        json!({
            "aprs_server": "local.example:40000",
            "directory_servers": ["POOL.EXAMPLE.ORG", "backup.example.org"],
            "deny": ["k1*"],
            "timing": {"heartbeat_timeout_ms": 110000},
            "rx": {"gain_db": -5.0}
        }),
    );
    let direct = section(
        2,
        port(SECOND),
        ResourceKind::Echolink,
        json!({"transport": "direct", "proxy": {"host": "unused.example.org"}}),
    );
    let proxy = section(
        3,
        port(FIRST),
        ResourceKind::Echolink,
        json!({
            "enabled": true,
            "callsign": "n0call-l",
            "node_number": 424242,
            "transport": "proxy",
            "bind_address": "127.0.0.1",
            "audio_port": 65534,
            "latitude": 90.1,
            "directory_servers": ["new.example.org", "bad host"],
            "timing": {"connect_timeout_ms": 31000, "heartbeat_timeout_ms": "bad"},
            "rx": {"gain_db": {}},
            "tx": {"gain_db": -12.0},
            "antenna_direction_deg": -1
        }),
    );
    let direct_id = direct.0.id;
    let proxy_id = proxy.0.id;
    let site_binding = defaults.0;
    let child = section(
        4,
        port(FIRST),
        ResourceKind::EcholinkProxy,
        json!({"host": "Proxy.Example.NET", "port": 8101}),
    );
    let profiles = resolve(&[defaults, direct, proxy, child]).unwrap();
    let first = profiles.port(id(FIRST)).unwrap();
    let effective = &first.echolink.as_ref().unwrap().settings;
    let second = profiles.port(id(SECOND)).unwrap();
    assert_eq!(
        effective.directory_servers,
        ["pool.example.org", "backup.example.org"]
    );
    assert_eq!(effective.callsign, "N0CALL-L");
    assert_eq!(effective.deny, ["K1*"]);
    assert_eq!(effective.aprs_server, "aprs.echolink.org:5199");
    assert_eq!(
        second.echolink.as_ref().unwrap().settings.aprs_server,
        "local.example:40000"
    );
    assert_eq!(effective.heartbeat_timeout_ms, 110000);
    assert_eq!(effective.connect_timeout_ms, 31000);
    assert_eq!(effective.receive_gain_db, -5.0);
    assert_eq!(effective.transmit_gain_db, -12.0);
    assert_eq!(effective.antenna_direction_deg, -1.0);
    assert_eq!(effective.latitude, 0.0);
    assert!(effective.secrets_file.is_none());
    assert!(profiles.warnings().contains(&CandidateWarning {
        resource: site_binding,
        warning: super::super::ResourceWarning::InvalidValue("aprs_server"),
    }));
    assert!(
        !profiles
            .local(direct_id)
            .unwrap()
            .configuration
            .contains_key("proxy")
    );
    assert!(
        !profiles
            .local(proxy_id)
            .unwrap()
            .configuration
            .contains_key("audio_port")
    );
}

#[test]
fn jitter_retains_ordered_source_choices_and_revalidates_each_port() {
    let common = section(
        1,
        site(),
        ResourceKind::Allstarlink,
        json!({"jitter": {"timing": {"minimum_ms": 70, "initial_ms": 50, "maximum_ms": 60}}}),
    );
    let first = section(
        2,
        port(FIRST),
        ResourceKind::Allstarlink,
        json!({"jitter": {"timing": {"initial_ms": 80, "maximum_ms": 4294967296_u64}}}),
    );
    let second = section(3, port(SECOND), ResourceKind::Allstarlink, json!({}));
    let common_source = common.0;
    let first_source = first.0;
    let profiles = resolve(&[common, first, second]).unwrap();
    let first = profiles.port(id(FIRST)).unwrap();
    let second = profiles.port(id(SECOND)).unwrap();
    let first_jitter = &first.allstarlink.as_ref().unwrap().jitter;
    let second_jitter = &second.allstarlink.as_ref().unwrap().jitter;
    assert_eq!(
        first_jitter.timing(),
        JitterTiming {
            minimum_ms: 70,
            initial_ms: 80,
            maximum_ms: 100
        }
    );
    assert_eq!(
        second_jitter.timing(),
        JitterTiming {
            minimum_ms: 20,
            initial_ms: 50,
            maximum_ms: 60
        }
    );
    assert_eq!(
        first_jitter.sources(JitterField::Initial),
        [
            TimingSource {
                milliseconds: 80,
                resource: Some(first_source)
            },
            TimingSource {
                milliseconds: 50,
                resource: Some(common_source)
            },
            TimingSource {
                milliseconds: 40,
                resource: None
            },
        ]
    );
    assert_eq!(
        first_jitter.selected_source(JitterField::Maximum).resource,
        None
    );
    assert_eq!(
        first.warnings,
        [CandidateWarning {
            resource: common_source,
            warning: super::super::ResourceWarning::InvalidValue("maximum_ms"),
        }]
    );
    assert_eq!(
        second.warnings,
        [CandidateWarning {
            resource: common_source,
            warning: super::super::ResourceWarning::InvalidValue("minimum_ms"),
        }]
    );
    assert!(
        profiles
            .warnings()
            .iter()
            .any(|warning| warning.resource == first_source)
    );
}

#[test]
fn phone_patch_uses_context_vad_and_independent_playout_timing() {
    let site_phone = section(
        1,
        site(),
        ResourceKind::PhonePatch,
        json!({
            "provider": {"protocol":"sip","server":"SIP.Example.NET","port":5060},
            "timing": {"maximum_latency_ms": 150},
            "vad": {"mode": "kerchunk", "timing": {"hang_ms": 1500}}
        }),
    );
    let local_phone = section(
        2,
        port(FIRST),
        ResourceKind::PhonePatch,
        json!({
            "provider": {"username":"ExampleUser", "port":0, "protocol":"webrtc"},
            "playout_destination":"local_and_links",
            "isolate_peers":false,
            "timing":{"target_latency_ms":240,"check_deadline_ms":4000},
            "tx":{"keying_mode":"vox"},
            "allowed_local_nodes":["524950"],
            "max_attachments":8,
            "dtmf_inactivity_timeout_ms":4000
        }),
    );
    let other_phone = section(3, port(SECOND), ResourceKind::PhonePatch, json!({}));
    let phone_id = local_phone.0.id;
    let profiles = resolve(&[site_phone, local_phone, other_phone]).unwrap();
    let first = profiles
        .port(id(FIRST))
        .unwrap()
        .phone_patch
        .as_ref()
        .unwrap();
    let second = profiles
        .port(id(SECOND))
        .unwrap()
        .phone_patch
        .as_ref()
        .unwrap();
    assert_eq!(
        first.settings.provider.account_identity(),
        Some(PhoneAccountIdentity {
            protocol: PhoneProtocol::Sip,
            server: "sip.example.net",
            port: 5060,
            username: "ExampleUser",
        })
    );
    assert!(second.settings.provider.account_identity().is_none());
    assert_eq!(
        first.settings.playout_destination,
        PhonePlayoutDestination::LocalAndLinks
    );
    assert!(!first.settings.isolate_peers);
    assert_eq!(
        second.settings.playout_destination,
        PhonePlayoutDestination::Local
    );
    assert!(second.settings.isolate_peers);
    assert_eq!(
        first.settings.vad,
        PhoneVadSettings {
            enabled: true,
            mode: VadMode::Continuous,
            hang_ms: 1500
        }
    );
    assert_eq!(
        first.jitter.timing(),
        JitterTiming {
            minimum_ms: 20,
            initial_ms: 40,
            maximum_ms: 100
        }
    );
    assert_eq!(
        first.settings.timing,
        PhoneLatencyTiming {
            target_latency_ms: 240,
            maximum_latency_ms: 500
        }
    );
    assert_eq!(
        second.settings.timing,
        PhoneLatencyTiming {
            target_latency_ms: 250,
            maximum_latency_ms: 500
        }
    );
    assert_eq!(
        json!(profiles.local(phone_id).unwrap().configuration),
        json!({
            "provider":{"username":"ExampleUser"},
            "playout_destination":"local_and_links",
            "isolate_peers":false,
            "timing":{"target_latency_ms":240}
        })
    );
}

#[test]
fn account_identity_uses_public_endpoints_and_ignores_processing_and_display() {
    let profiles = |registration: &str, proxy_server: &str, callsign: &str, jitter: u32| {
        resolve(&[
            section(
                1,
                port(FIRST),
                ResourceKind::Allstarlink,
                json!({
                    "callsign": callsign, "node_number": 524950,
                    "allow_nodes": ["1234", "3515584", "1234"],
                    "deny_nodes": ["5678"],
                    "directory_file": "  /feed/name ; # .txt  ",
                    "lookup_method": "file",
                    "iax": {
                        "registration_url": registration,
                        "timing": {"registration_interval_s":90},
                        "local_port": 4570
                    },
                    "status_post": {"url":"http://HTTP.Example.NET:80/state", "timing":{"interval_s":120}},
                    "jitter": {"timing": {"initial_ms": jitter}}
                }),
            ),
            section(
                2,
                port(FIRST),
                ResourceKind::Echolink,
                json!({
                    "callsign": "n1echo-r",
                    "node_number": 100501,
                    "transport": "proxy",
                    "directory_servers": ["Directory.Example.NET"],
                    "credentials":{"password":"hidden"}
                }),
            ),
            section(3, port(FIRST), ResourceKind::EcholinkProxy,
                json!({"host": proxy_server, "port": 8102, "credentials":{"password":"hidden"}})),
        ])
        .unwrap()
    };

    let first = profiles("", "proxy-a.example.net", "N0ASL", 40);
    let changed = profiles("", "PROXY-B.Example.NET", "N1ASL", 60);
    let registered = profiles(
        "HTTPS://REGISTER.EXAMPLE.NET:443/#view",
        "proxy-a.example.net",
        "N0ASL",
        40,
    );
    let first = first.port(id(FIRST)).unwrap();
    let changed = changed.port(id(FIRST)).unwrap();
    let registered = registered.port(id(FIRST)).unwrap();
    let asl = first.allstarlink.as_ref().unwrap();
    let effective_asl = asl.settings.account_identity().unwrap();
    assert_eq!(effective_asl.node_number.get(), 524950);
    assert_eq!(effective_asl.registration_url, None);
    assert_eq!(asl.settings.allow_nodes, ["1234", "3515584"]);
    assert_eq!(asl.settings.deny_nodes, ["5678"]);
    assert_eq!(asl.settings.directory_file, "  /feed/name ; # .txt  ");
    assert_eq!(
        asl.settings.lookup_method,
        crate::config::LinkLookupMethod::File
    );
    assert_eq!(asl.settings.registration_interval_s, 90);
    assert_eq!(asl.settings.local_port, 4570);
    assert_eq!(
        asl.settings.status_post_url.as_deref(),
        Some("http://http.example.net/state")
    );
    assert_eq!(asl.settings.status_post_interval_s, 120);
    assert_eq!(
        asl.settings.account_identity(),
        changed
            .allstarlink
            .as_ref()
            .unwrap()
            .settings
            .account_identity()
    );
    assert_ne!(
        asl.jitter.timing(),
        changed.allstarlink.as_ref().unwrap().jitter.timing()
    );
    let new_identity = registered
        .allstarlink
        .as_ref()
        .unwrap()
        .settings
        .account_identity()
        .unwrap();
    assert_eq!(
        new_identity.registration_url,
        Some("https://register.example.net/")
    );
    assert_ne!(effective_asl, new_identity);
    assert_ne!(first.key.as_str(), "524950");

    let echo = first.echolink.as_ref().unwrap();
    let other = changed.echolink.as_ref().unwrap();
    assert!(!echo.settings.enabled);
    let directory = echo.directory_identity().unwrap();
    assert_eq!(directory.callsign, "N1ECHO-R");
    assert_eq!(directory.node_number, 100501);
    assert_eq!(directory.directory_servers, ["directory.example.net"]);
    assert_eq!(echo.directory_identity(), other.directory_identity());
    assert_eq!(first.proxy_identity().unwrap().host, "proxy-a.example.net");
    assert_eq!(
        changed.proxy_identity().unwrap().host,
        "proxy-b.example.net"
    );
    assert_ne!(first.proxy_identity(), changed.proxy_identity());
    assert!(!format!("{first:?} {changed:?}").contains("hidden"));
}

#[test]
fn incomplete_echo_proxy_stays_selected_but_safely_disabled() {
    let site_echo = section(
        1,
        site(),
        ResourceKind::Echolink,
        json!({"enabled": true, "max_stations": 7, "max_pending_handshakes": 21}),
    );
    let proxy = section(
        2,
        port(FIRST),
        ResourceKind::Echolink,
        json!({
            "transport":"proxy",
            "callsign":"N0CALL-R",
            "node_number":424242,
            "max_pending_handshakes":4294967295_u64
        }),
    );
    let site_binding = site_echo.0;
    let local = proxy.0;
    let profiles = resolve(&[site_echo, proxy]).unwrap();
    let port = profiles.port(id(FIRST)).unwrap();
    let echo = port.echolink.as_ref().unwrap();
    assert!(!echo.settings.enabled);
    assert_eq!(
        echo.settings.transport,
        crate::config::EchoLinkTransport::Proxy
    );
    assert_eq!(echo.settings.max_stations, 7);
    assert_eq!(echo.settings.max_pending_handshakes, 21);
    assert!(echo.directory_identity().is_some());
    assert!(port.proxy_identity().is_none());
    assert_eq!(
        port.warnings,
        [
            CandidateWarning {
                resource: local,
                warning: super::super::ResourceWarning::InvalidValue("max_pending_handshakes"),
            },
            CandidateWarning {
                resource: site_binding,
                warning: super::super::ResourceWarning::InvalidValue("enabled"),
            },
        ]
    );
}

#[test]
fn ownership_allows_shared_iax_port_but_rejects_ambiguous_active_bindings() {
    let asl = |id, port, node| {
        section(
            id,
            port,
            ResourceKind::Allstarlink,
            json!({"node_number":node}),
        )
    };
    let echo = |id, port, node, callsign, audio_port| {
        section(
            id,
            port,
            ResourceKind::Echolink,
            json!({
                "enabled":true, "callsign": callsign, "node_number": node, "audio_port": audio_port
            }),
        )
    };
    let first = asl(1, port(FIRST), 524950);
    let second = asl(2, port(SECOND), 524951);
    let profiles = resolve(&[first.clone(), second]).unwrap();
    for (_, port) in profiles.ports() {
        assert_eq!(port.allstarlink.as_ref().unwrap().settings.local_port, 4569);
    }
    let duplicate = asl(2, port(SECOND), 524950);
    let error = resolve(&[first.clone(), duplicate.clone()]).unwrap_err();
    assert_eq!(
        error.problem,
        ProtocolProfileProblem::DuplicateAllStarLinkNode
    );
    assert_eq!((error.other, error.resource), (first.0, duplicate.0));

    let first = echo(3, port(FIRST), 123456, "N0ABC", 5198);
    let duplicate = echo(4, port(SECOND), 123456, "N1ABC", 5200);
    let error = resolve(&[first.clone(), duplicate]).unwrap_err();
    assert_eq!(
        error.problem,
        ProtocolProfileProblem::DuplicateEchoLinkAccount
    );
    let shared = echo(4, port(SECOND), 654321, "N1ABC", 5199);
    let error = resolve(&[first.clone(), shared]).unwrap_err();
    assert_eq!(
        error.problem,
        ProtocolProfileProblem::EchoLinkEndpointConflict
    );
    assert!(!error.to_string().contains("5199"));
    let separate = echo(4, port(SECOND), 654321, "N1ABC", 5200);
    resolve(&[first, separate]).unwrap();
}

fn inherited_echo_resources(
    authored: Value,
    proxy: Value,
) -> (ResourceBinding, ResourceBinding, ResolvedProtocolProfiles) {
    let site = section(
        1,
        site(),
        ResourceKind::Echolink,
        json!({
            "directory_servers": ["primary.example.net", "backup.example.net"]
        }),
    );
    let account = section(2, port(FIRST), ResourceKind::Echolink, authored);
    let defaults = section(
        3,
        site.0.scope,
        ResourceKind::EcholinkProxy,
        json!({"host": "proxy.example.net", "port":8100}),
    );
    let child = section(4, port(FIRST), ResourceKind::EcholinkProxy, proxy);
    (
        account.0,
        child.0,
        resolve(&[site, account, defaults, child]).unwrap(),
    )
}

fn inherited_echo_hosts(authored: Value) -> (ResourceBinding, ResolvedProtocolProfiles) {
    let (account, _, profiles) = inherited_echo_resources(authored, json!({}));
    (account, profiles)
}

fn assert_echo_host_fallback(
    binding: ResourceBinding,
    profiles: &ResolvedProtocolProfiles,
    field: &'static str,
) {
    assert_eq!(
        profiles.warnings(),
        [CandidateWarning {
            resource: binding,
            warning: super::super::ResourceWarning::InvalidValue(field),
        }]
    );
    assert!(!profiles.warnings()[0].to_string().contains("example"));
}

#[test]
fn echo_directory_rejects_delimiters_introduced_by_host_normalization() {
    let (binding, profiles) = inherited_echo_hosts(json!({
        "directory_servers": ["new.example.net%2cother.example.net"],
        "name": "Retained station"
    }));
    let echo = &profiles
        .port(id(FIRST))
        .unwrap()
        .echolink
        .as_ref()
        .unwrap()
        .settings;
    assert_eq!(
        echo.directory_servers,
        ["primary.example.net", "backup.example.net"]
    );
    assert_eq!(echo.name, "Retained station");
    assert_eq!(
        json!(profiles.local(binding.id).unwrap().configuration),
        json!({"name":"Retained station"})
    );
    assert_echo_host_fallback(binding, &profiles, "directory_servers");
}

fn overlong_canonical_host() -> String {
    // Each one-letter UTF-8 label grows to the seven-byte xn--9ca label.
    std::iter::repeat_n("é", 32).collect::<Vec<_>>().join(".")
}

#[test]
fn echo_directory_rejects_host_whose_canonical_form_exceeds_limit() {
    let (binding, profiles) = inherited_echo_hosts(json!({
        "directory_servers": ["valid.example.net", overlong_canonical_host()],
        "name": "Retained station"
    }));
    let echo = &profiles
        .port(id(FIRST))
        .unwrap()
        .echolink
        .as_ref()
        .unwrap()
        .settings;
    assert_eq!(
        echo.directory_servers,
        ["primary.example.net", "backup.example.net"]
    );
    assert_eq!(echo.name, "Retained station");
    assert_eq!(
        json!(profiles.local(binding.id).unwrap().configuration),
        json!({"name":"Retained station"})
    );
    assert_echo_host_fallback(binding, &profiles, "directory_servers");
}

#[test]
fn echo_proxy_rejects_host_whose_canonical_form_exceeds_limit() {
    let (_, binding, profiles) = inherited_echo_resources(
        json!({"transport": "proxy"}),
        json!({"host": overlong_canonical_host(), "port":8101}),
    );
    let echo = &profiles
        .port(id(FIRST))
        .unwrap()
        .echolink
        .as_ref()
        .unwrap()
        .settings;
    assert_eq!(echo.proxy_host, "proxy.example.net");
    assert_eq!(echo.proxy_port, 8101);
    assert_eq!(
        json!(profiles.local(binding.id).unwrap().configuration),
        json!({"port":8101})
    );
    assert_echo_host_fallback(binding, &profiles, "host");
}

#[test]
fn echo_canonical_proxy_and_ordered_directory_pool_survive_local_readback() {
    let (account, child, source) = inherited_echo_resources(
        json!({"transport":"proxy","directory_servers":["bücher.Example.NET","Second.Example.ORG"]}),
        json!({"host":"Bücher.Example.ORG", "port":8101}),
    );
    let parent = json!(source.local(account.id).unwrap().configuration);
    let endpoint = json!(source.local(child.id).unwrap().configuration);
    assert_eq!(
        parent,
        json!({"transport":"proxy","directory_servers":["xn--bcher-kva.example.net","second.example.org"]})
    );
    assert_eq!(
        endpoint,
        json!({"host":"xn--bcher-kva.example.org","port":8101})
    );
    let (_, _, readback) = inherited_echo_resources(parent, endpoint);
    let projection = |profiles: &ResolvedProtocolProfiles| {
        profiles
            .port(id(FIRST))
            .unwrap()
            .echolink
            .as_ref()
            .unwrap()
            .settings
            .clone()
    };
    assert_eq!(projection(&readback), projection(&source));
    assert!(readback.warnings().is_empty());
}

#[test]
fn proxy_child_owns_its_endpoint_even_while_direct_transport_is_selected() {
    let account = section(
        1,
        port(FIRST),
        ResourceKind::Echolink,
        json!({
            "callsign": "n0call-l", "node_number": 424242, "transport": "direct",
            "proxy": {"host": "wrong.example.org"}
        }),
    );
    let defaults = section(
        2,
        site(),
        ResourceKind::EcholinkProxy,
        json!({"host": "Site.Example.Net", "port": 8101}),
    );
    let child = section(
        3,
        port(FIRST),
        ResourceKind::EcholinkProxy,
        json!({"host": "Bücher.Example.ORG"}),
    );
    let child_id = child.0.id;
    let profiles = resolve(&[account, defaults, child]).unwrap();
    let port = profiles.port(id(FIRST)).unwrap();
    assert_eq!(
        profiles.local(child_id).unwrap().configuration["host"],
        "xn--bcher-kva.example.org"
    );
    let identity = port.proxy_identity().unwrap();
    assert_eq!(identity.binding.id, child_id);
    assert_eq!(
        (identity.host, identity.port),
        ("xn--bcher-kva.example.org", 8101)
    );
    assert!(profiles.port(id(SECOND)).unwrap().echolink.is_none());
    assert_eq!(profiles.warnings().len(), 1);
}

#[test]
fn site_profiles_bind_credentials_even_without_a_port_instance() {
    let echo = section(
        1,
        site(),
        ResourceKind::Echolink,
        json!({"callsign":"N0TEST-R","node_number":42,"transport":"direct"}),
    );
    let proxy = section(
        2,
        site(),
        ResourceKind::EcholinkProxy,
        json!({"host":"example.org","port":8101}),
    );
    let phone = section(3, site(), ResourceKind::PhonePatch, json!({}));
    let asl = section(
        4,
        site(),
        ResourceKind::Allstarlink,
        json!({"node_number":524950}),
    );
    let child = proxy.0;
    let profiles = resolve(&[echo, proxy, phone, asl]).unwrap();
    let defaults = profiles.site_accounts();
    assert_eq!(
        defaults
            .echolink
            .as_ref()
            .unwrap()
            .directory_identity()
            .unwrap()
            .callsign,
        "N0TEST-R"
    );
    assert_eq!(defaults.proxy_identity().unwrap().binding, child);
    assert_eq!(
        defaults
            .allstarlink
            .as_ref()
            .unwrap()
            .settings
            .node_number
            .unwrap()
            .get(),
        524950
    );
    assert!(defaults.phone_patch.is_some());
    let port = profiles.port(id(FIRST)).unwrap();
    assert!(port.echolink.is_none() && port.proxy_identity().is_none());
}
