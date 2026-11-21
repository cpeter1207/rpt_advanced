use super::*;
use rpt_advanced_core::{command_language::MacroOutcome, config::resources::candidate::ResourceId};
use serde_json::{Value, json};

pub(crate) fn install(
    host: &mut Host,
    local: &str,
    key: &str,
    account_number: Option<u32>,
    settings: Value,
    definitions: &[(&str, &str, Value)],
) -> ResourceId {
    install_with_other(
        &mut host.runtime,
        local,
        key,
        account_number,
        settings,
        definitions,
        None,
    )
    .unwrap()
}
pub(crate) fn install_with_other<A: Send, C: Send>(
    runtime: &mut rpt_advanced_core::runtime::Runtime<A, C>,
    local: &str,
    key: &str,
    account_number: Option<u32>,
    settings: Value,
    definitions: &[(&str, &str, Value)],
    other: Option<u32>,
) -> Result<ResourceId, RuntimeError> {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
            ResourceRegistry, ResourceScope,
        },
    };

    let site: ResourceId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap();
    let port: ResourceId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let owner = ResourceScope::Port(port);
    let commands = ResourceBinding {
        scope: owner,
        kind: ResourceKind::Command,
        id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".parse().unwrap(),
    };
    let account = ResourceBinding {
        scope: owner,
        kind: ResourceKind::Allstarlink,
        id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd".parse().unwrap(),
    };

    let mut fields = vec![
        (
            ResourceBinding {
                id: site,
                scope: ResourceScope::Site(site),
                kind: ResourceKind::Metadata,
            },
            serde_json::json!({"name":"Station","inherit_from":null}),
        ),
        (
            ResourceBinding {
                id: port,
                scope: owner,
                kind: ResourceKind::Metadata,
            },
            serde_json::json!({"name":"North","inherit_from":"site"}),
        ),
        (commands, settings),
        (
            account,
            match account_number {
                Some(n) => serde_json::json!({"node_number":n}),
                None => serde_json::json!({}),
            },
        ),
    ];
    let mut kinds = vec!["metadata", "metadata", "command", "allstarlink"];
    let mut names = vec![(port, PortKey::new(key))];
    let mut singles = vec![commands, account];
    if let Some(number) = other {
        let other_port = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".parse().unwrap();
        let scope = ResourceScope::Port(other_port);
        let other_account = ResourceBinding {
            id: "ffffffff-ffff-4fff-8fff-ffffffffffff".parse().unwrap(),
            kind: ResourceKind::Allstarlink,
            scope,
        };
        names.push((other_port, PortKey::new("West")));
        singles.push(other_account);
        fields.push((
            ResourceBinding {
                id: other_port,
                kind: ResourceKind::Metadata,
                scope,
            },
            json!({"name":"West","inherit_from":"site"}),
        ));
        fields.push((other_account, json!({"node_number":number})));
        kinds.extend(["metadata", "allstarlink"]);
    }
    let mut members = Vec::new();
    for (i, (kind, key, body)) in definitions.iter().enumerate() {
        let binding = ResourceBinding {
            id: format!("00000000-0000-4000-8000-{i:012}").parse().unwrap(),
            scope: owner,
            kind: match *kind {
                "macro" => ResourceKind::Macro,
                "event" => ResourceKind::Event,
                "message" => ResourceKind::Message,
                _ => panic!("fixture kind"),
            },
        };
        fields.push((binding, body.clone()));
        kinds.push(kind);
        members.push((binding, PortKey::new(*key)));
    }
    let registry = ResourceRegistry::with_members(site, &names, &singles, &members).unwrap();
    let bodies: Vec<_> = fields
        .iter()
        .zip(kinds)
        .map(|((binding, settings), kind)| {
            let mut body = if binding.kind == ResourceKind::Event {
                settings.clone()
            } else {
                serde_json::json!({"configuration":settings})
            };
            body["id"] = serde_json::json!(binding.id.to_string());
            body["kind"] = serde_json::json!(kind);
            if let Some((_, key)) = members.iter().find(|(m, _)| m == binding) {
                body["key"] = serde_json::json!(key.as_str());
            }
            serde_json::to_vec(&body).unwrap()
        })
        .collect();
    let captures: Vec<_> = fields
        .iter()
        .zip(&bodies)
        .map(|((binding, _), json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let candidate = ModularCandidate::assemble(&registry, &captures).unwrap();
    let validated = candidate
        .command_language(owner)
        .expect("prepared command language");
    candidate
        .event_profiles(&validated, Some("UTC"))
        .expect("prepared event profiles");

    let node = rpt_advanced_core::config::NodeId::new(local).unwrap();
    let speech =
        rpt_advanced_core::config::ResolvedSpeechSettings::resolve(runtime.document(), &node)
            .unwrap()
            .value;
    let morse =
        rpt_advanced_core::config::ResolvedMorseSettings::resolve(runtime.document(), &node)
            .unwrap()
            .value;
    let language = "en-US".parse().unwrap();
    let renderers = rpt_advanced_core::runtime::CommandRenderers {
        catalog: rpt_advanced_core::messages::MessageCatalog::load("en-US").unwrap(),
        language: &language,
        speech: &speech,
        morse: &morse,
    };
    runtime.node(local).unwrap().install_command_profile(
        &candidate,
        port,
        Some("UTC"),
        clock(),
        renderers,
    )?;
    Ok(port)
}

fn at(ms: u64) -> RuntimeClock {
    RuntimeClock {
        now_ms: ms,
        ..clock()
    }
}

#[test]
fn registered_startup_connect_survives_answer_and_local_partial_command() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    install(
        &mut host,
        "1000",
        "North é",
        Some(1000),
        json!({"mappings":{"mute":{"command":"AB","protected":false,"action":"rx_mute_toggle","target":"port"}}}),
        &[
            (
                "macro",
                "boot",
                json!({"actions":[{"action":"connect","parameter":"2000"},{"action":"pause","timing":{"duration_ms":15}},{"action":"message","message":"Ready","destination":"local"}]}),
            ),
            (
                "event",
                "start",
                json!({"start":{"at":"controller_start","macro":"boot"}}),
            ),
        ],
    );
    let Some((LinkEffect::Connect(dial), dial_receipt)) =
        host.runtime.advance_commands("1000", at(10)).unwrap()
    else {
        panic!("startup dial")
    };
    assert!(
        host.runtime
            .digit(
                "1000",
                DigitEvent::Digit {
                    digit: 'A',
                    now_ms: 10
                }
            )
            .is_none()
    );
    assert!(
        host.runtime
            .finish_connect("1000", dial, true, at(11))
            .unwrap()
    );
    let (io, _) = peer(48000);
    host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, at(11))
        .unwrap();
    assert!(
        host.runtime
            .advance_commands("1000", at(11))
            .unwrap()
            .is_none()
    );
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .take_command_completion()
            .is_none()
    );
    dial_receipt.settle(true);
    assert!(
        host.runtime
            .advance_commands("1000", at(12))
            .unwrap()
            .is_none()
    );
    assert!(
        host.runtime
            .digit(
                "1000",
                DigitEvent::Digit {
                    digit: 'B',
                    now_ms: 12
                }
            )
            .is_none()
    );
    let Some(DigitOutcome::Mapped(input)) = host.runtime.digit(
        "1000",
        DigitEvent::Digit {
            digit: '#',
            now_ms: 12,
        },
    ) else {
        panic!("partial RF command survived link attachment")
    };
    host.runtime
        .dispatch_mapping("1000", input, at(12))
        .unwrap();
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .radio_receive_status()
            .muted
    );
    assert!(
        host.runtime
            .advance_commands("1000", at(26))
            .unwrap()
            .is_none()
    );
    let Some((LinkEffect::None, message)) = host.runtime.advance_commands("1000", at(27)).unwrap()
    else {
        panic!("message after pause")
    };
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .take_command_completion()
            .is_none()
    );
    message.settle(true);
    assert!(
        host.runtime
            .advance_commands("1000", at(28))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        host.runtime
            .node("1000")
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        MacroOutcome::Completed
    );
    assert!(host.stop(32));
}

// Step the actual provider session at a deterministic receipt time. The host owns its normal
// control endpoint and drains the unchanged bounded handoff on its next pump.
fn attached_session(
    host: &mut Host,
) -> (PeerSession, Arc<Mutex<crate::link::session::tests::State>>) {
    use rpt_advanced_core::link::PeerIdentity;
    let (io, state) = peer(48_000);
    let (_, out) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
    let (session, control) = PeerSession::prepare(io, out, "1000", "2000", false).unwrap();
    host.runtime.incoming("1000", "2000", true).unwrap();
    host.runtime
        .node("1000")
        .unwrap()
        .links()
        .attach_ingress(
            PeerIdentity::from_operator("2000").unwrap(),
            control.reply_target(),
        )
        .unwrap();
    host.peers.push(PeerOwner {
        local: "1000".into(),
        remote: "2000".into(),
        mode: Mode::TRANSCEIVE,
        group: None,
        announced: true,
        superseded: false,
        control,
    });
    (session, state)
}

#[test]
fn registered_same_batch_unlock_protected_and_select_forward_use_own_ingress() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    install(
        &mut host,
        "1000",
        "North é",
        Some(1000),
        json!({"mappings":{
            "mute":{"command":"B","action":"rx_mute_toggle","target":"port","protected":true},
            "choose":{"command":"C","protected":false,"action":"select_command_target"}
        }}),
        &[],
    );
    let hash =
        "$argon2id$v=19$m=1024,t=2,p=1$Zml4dHVyZTE$B1VnpG+RsPUNLprBqRc964o5W2vBdnP8VyhbqpcwHyE";
    host.runtime
        .node("1000")
        .unwrap()
        .links()
        .configure_admin(hash, hash, 60000);
    let (mut session, state) = attached_session(&mut host);
    for digit in b"B#D#*8001234#B#C4000#5" {
        state.lock().unwrap().input.push_back(Input::Digit(*digit));
        session.step(10).unwrap();
    }
    host.pump(at(11)).unwrap();
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .radio_receive_status()
            .muted
    );
    let reply = host.peers[0].control.reply_target();
    let ops = host.take_operations();
    assert!(
        matches!(ops.as_slice(),[(_,DigitOutcome::Operation(op))] if op.command.node=="4000" && op.digit==Some('5') && op.reply.as_ref()==Some(&reply)),
        "{ops:?}"
    );
    session.stop();
    assert!(host.stop(20));
}

#[test]
fn registered_delayed_transport_batch_expires_at_exact_digit_and_addressed_envelope_time() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for envelopes in [false, true] {
        let mut host = host();
        install(
            &mut host,
            "1000",
            "North é",
            Some(1000),
            json!({"inactivity_timeout_ms":10,"mappings":{
                "mute_rf":{"command":"A","protected":false,"action":"rx_mute_toggle","target":"port"},
                "dial":{"command":"B","action":"connect","protected":false}
            }}),
            &[],
        );
        let (mut session, state) = attached_session(&mut host);
        for (time, digit, text) in [
            (10, b'A', b"D 1000 2000 1 A".as_slice()),
            (20, b'B', b"D 1000 2000 2 B"),
            (21, b'6', b"D 1000 2000 3 6"),
            (22, b'#', b"D 1000 2000 4 #"),
        ] {
            state.lock().unwrap().input.push_back(if envelopes {
                Input::Text(text)
            } else {
                Input::Digit(digit)
            });
            session.step(time).unwrap();
        }
        host.pump(at(200)).unwrap();
        assert!(
            host.runtime
                .node("1000")
                .unwrap()
                .radio_receive_status()
                .muted,
            "envelopes={envelopes}"
        );
        let queued = host.take_site_links();
        assert!(
            matches!(queued.as_slice(),[(_,LinkEffect::Connect(d))] if d.remote()=="6"),
            "envelopes={envelopes}"
        );
        for (_, link) in queued {
            if let LinkEffect::Connect(d) = link {
                host.runtime.node("1000").unwrap().links().cancel_connect(d);
            }
        }
        session.stop();
        assert!(host.stop(220));
    }
}

#[test]
fn registered_private_slot_binds_incoming_account_and_writes_only_it_in_asl_query() {
    const SLOT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::start(
        ConfigDocument::parse(&format!(
            "[{SLOT}]\nradio_channel=usb\nduplex=full\nlink_static_directory_file=fixture\n"
        ))
        .unwrap(),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    install(&mut host, SLOT, "North é", Some(1000), json!({}), &[]);
    let (io, state) = peer(48000);
    host.incoming_peer(io, "127.0.0.1", clock()).unwrap();
    state
        .lock()
        .unwrap()
        .input
        .extend([Input::Text(b"!NEWKEY!"), Input::RadioKey]);
    pump_until(&mut host, |_| {
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"K? * 1000 0 0")
    });
    assert!(host.peers.iter().all(|p| p.local == SLOT));
    assert!(
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .all(|t| !String::from_utf8_lossy(t).contains(SLOT))
    );
    host.detach(SLOT, "2000", 20);
    install(&mut host, SLOT, "North é", None, json!({}), &[]);
    let (io, _) = peer(48000);
    assert!(matches!(
        host.incoming_peer(io, "127.0.0.1", at(21)),
        Err(RuntimeError::MissingNode)
    ));
    assert!(host.stop(30));
    let mut numeric = super::host();
    assert!(matches!(
        install_with_other(
            &mut numeric.runtime,
            "1000",
            "1000",
            Some(1001),
            json!({}),
            &[],
            Some(1000)
        ),
        Err(RuntimeError::MissingNode)
    ));
    assert!(numeric.stop(30));
}

#[test]
fn registered_peer_hint_and_unkey_leave_nonradio_candidate_for_its_real_terminator_or_deadline() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    install(
        &mut host,
        "1000",
        "North é",
        Some(1000),
        json!({"inactivity_timeout_ms":5000,"mappings":{
            "mute_rf":{"command":"A","protected":false,"action":"rx_mute_toggle","target":"port"}
        }}),
        &[],
    );
    let (mut session, state) = attached_session(&mut host);
    state.lock().unwrap().input.extend([
        Input::Text(b"!NEWKEY!"),
        Input::RadioKey,
        Input::Digit(b'A'),
    ]);
    for at in [8, 9, 10] {
        session.step(at).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    host.pump(at(10)).unwrap();
    session.step(3010).unwrap();
    host.pump(at(3010)).unwrap();
    let muted = |host: &mut Host| {
        host.runtime
            .node("1000")
            .unwrap()
            .radio_receive_status()
            .muted
    };
    assert!(!muted(&mut host), "the session hint is advisory");
    state.lock().unwrap().input.push_back(Input::RadioUnkey);
    session.step(3011).unwrap();
    session.step(3011).unwrap(); // Flush the pending source unkey on the next I/O turn.
    host.pump(at(3011)).unwrap();
    assert!(
        !muted(&mut host),
        "peer unkey completes radio grammars only"
    );
    state.lock().unwrap().input.push_back(Input::Digit(b'#'));
    session.step(3012).unwrap();
    host.pump(at(3012)).unwrap();
    assert!(
        muted(&mut host),
        "the genuine terminator executes the candidate"
    );
    state.lock().unwrap().input.push_back(Input::Digit(b'A'));
    session.step(3020).unwrap();
    host.pump(at(8019)).unwrap();
    assert!(muted(&mut host));
    host.pump(at(8020)).unwrap();
    assert!(
        !muted(&mut host),
        "the configured inactivity deadline executes it"
    );
    session.stop();
    assert!(host.stop(8030));
}

#[test]
fn round2_retained_port_reload_preserves_start_and_recovery_but_revokes_command_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let defs = [
        (
            "macro",
            "boot",
            json!({"actions":[{"action":"connect","parameter":"2011"}]}),
        ),
        (
            "macro",
            "recover",
            json!({"actions":[{"action":"connect","parameter":"2012"}]}),
        ),
        (
            "macro",
            "end",
            json!({"actions":[{"action":"connect","parameter":"2013"}]}),
        ),
        (
            "macro",
            "wait",
            json!({"actions":[{"action":"pause","timing":{"duration_ms":300}},{"action":"connect","parameter":"2099"}]}),
        ),
        (
            "event",
            "start",
            json!({"start":{"at":"controller_start","macro":"boot"}}),
        ),
        (
            "event",
            "window",
            json!({"restart_recovery":true,"start":{"at":"every day at 23:55 in UTC","macro":"recover"},"end":{"at":"every day at 0:10 in UTC","macro":"end"}}),
        ),
    ];
    let install_profile =
        |host: &mut Host| install(host, "1000", "North", Some(1000), json!({}), &defs);
    let port = install_profile(&mut host);
    let work = host.runtime.node("1000").unwrap().command_work().unwrap();
    let drain = |host: &mut Host, clock| {
        let mut dials = Vec::new();
        for _ in 0..8 {
            if let Some((LinkEffect::Connect(dial), receipt)) =
                host.runtime.advance_commands("1000", clock).unwrap()
            {
                dials.push(dial.remote().to_owned());
                host.runtime
                    .node("1000")
                    .unwrap()
                    .links()
                    .cancel_connect(dial);
                receipt.settle(true);
            }
        }
        dials
    };
    assert_eq!(drain(&mut host, clock()), ["2011", "2012"]);
    let call = rpt_advanced_core::command_language::prepare_runtime_action(
        &json!({"action":"call_macro","macro":"wait"}),
    )
    .unwrap();
    let (_, LinkEffect::CommandQueued(pending)) = host
        .runtime
        .management_action(port, "operator", &call, clock())
        .unwrap()
    else {
        panic!("waiting macro admission");
    };
    assert!(
        host.runtime
            .advance_commands("1000", clock())
            .unwrap()
            .is_none()
    );
    assert_eq!(pending.outcome(), None);
    host.reload(document("usb", "transmit_hang_ms=12\n"), clock())
        .unwrap();
    assert!(!work.is_current());
    assert_eq!(pending.outcome(), Some(MacroOutcome::Cancelled));
    install_profile(&mut host);
    assert!(
        drain(&mut host, clock()).is_empty(),
        "no second startup or recovered window"
    );
    let end = RuntimeClock {
        now_ms: 600010,
        wall_seconds: 600,
        ..clock()
    };
    assert_eq!(drain(&mut host, end), ["2013"]);
    assert!(host.stop(601000));
}
