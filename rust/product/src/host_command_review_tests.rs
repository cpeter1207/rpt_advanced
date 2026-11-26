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

    let (speech, morse) = {
        let owner = ResourceScope::Port(local.parse().map_err(|_| RuntimeError::MissingNode)?);
        let scope = runtime
            .modular_configuration()
            .and_then(|config| config.scopes.get(&owner))
            .ok_or(RuntimeError::MissingNode)?;
        (
            scope.telemetry.speech.clone(),
            scope.telemetry.morse.clone(),
        )
    };
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
    let config = configuration(
        "usb",
        &[
            (
                Kind::Command,
                "",
                json!({"mappings":{"mute":{"command":"AB","protected":false,"action":"rx_mute_toggle","target":"port"}}}),
            ),
            (
                Kind::Macro,
                "boot",
                json!({"actions":[{"action":"connect","parameter":"2000"},{"action":"pause","timing":{"duration_ms":15}},{"action":"message","message":"Ready","destination":"local"}]}),
            ),
            (
                Kind::Event,
                "start",
                json!({"start":{"at":"controller_start","macro":"boot"}}),
            ),
        ],
    );
    let mut host =
        Host::fixture_start(config, media(), services(), Instant::now(), clock()).unwrap();
    let Some((LinkEffect::Connect(dial), dial_receipt)) =
        host.runtime.advance_commands(LOCAL, at(10)).unwrap()
    else {
        panic!("startup dial")
    };
    assert!(
        host.runtime
            .digit(
                LOCAL,
                DigitEvent::Digit {
                    digit: 'A',
                    now_ms: 10
                }
            )
            .is_none()
    );
    assert!(
        host.runtime
            .finish_connect(LOCAL, dial, true, at(11))
            .unwrap()
    );
    let (io, _) = peer(48000);
    host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, io, at(11))
        .unwrap();
    assert!(
        host.runtime
            .advance_commands(LOCAL, at(11))
            .unwrap()
            .is_none()
    );
    assert!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .take_command_completion()
            .is_none()
    );
    dial_receipt.settle(true);
    assert!(
        host.runtime
            .advance_commands(LOCAL, at(12))
            .unwrap()
            .is_none()
    );
    assert!(
        host.runtime
            .digit(
                LOCAL,
                DigitEvent::Digit {
                    digit: 'B',
                    now_ms: 12
                }
            )
            .is_none()
    );
    let Some(DigitOutcome::Mapped(input)) = host.runtime.digit(
        LOCAL,
        DigitEvent::Digit {
            digit: '#',
            now_ms: 12,
        },
    ) else {
        panic!("partial RF command survived link attachment")
    };
    host.runtime.dispatch_mapping(LOCAL, input, at(12)).unwrap();
    assert!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .radio_receive_status()
            .muted
    );
    assert!(
        host.runtime
            .advance_commands(LOCAL, at(26))
            .unwrap()
            .is_none()
    );
    let Some((LinkEffect::None, message)) = host.runtime.advance_commands(LOCAL, at(27)).unwrap()
    else {
        panic!("message after pause")
    };
    assert!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .take_command_completion()
            .is_none()
    );
    message.settle(true);
    assert!(
        host.runtime
            .advance_commands(LOCAL, at(28))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        host.runtime
            .node(LOCAL)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        MacroOutcome::Completed
    );
    assert!(host.fixture_stop(32));
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
    host.runtime.incoming(LOCAL, "2000", true).unwrap();
    host.runtime
        .node(LOCAL)
        .unwrap()
        .links()
        .attach_ingress(
            PeerIdentity::from_operator("2000").unwrap(),
            control.reply_target(),
        )
        .unwrap();
    host.peers.push(PeerOwner {
        local: LOCAL.into(),
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
    let mut host = Host::fixture_start(
        configuration(
            "usb",
            &[(
                Kind::Allstarlink,
                "",
                json!({
                    "node_number":1000, "iax":{"registration_url":""},
                    "static_nodes":{
                        "2000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"},
                        "4000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"}
                    }
                }),
            )],
        ),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    install(
        &mut host,
        LOCAL,
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
        .node(LOCAL)
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
            .node(LOCAL)
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
    assert!(host.fixture_stop(20));
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
            LOCAL,
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
                .node(LOCAL)
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
                host.runtime.node(LOCAL).unwrap().links().cancel_connect(d);
            }
        }
        session.stop();
        assert!(host.fixture_stop(220));
    }
}

#[test]
fn registered_private_slot_binds_incoming_account_and_writes_only_it_in_asl_query() {
    const SLOT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::fixture_start(
        configuration("usb", &[]),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    install(&mut host, SLOT, "North é", Some(1000), json!({}), &[]);
    let (io, state) = peer(48000);
    // Source admission is covered by the registered ASL boundary fixture. This
    // unit control starts at its verified handoff and checks wire/slot separation.
    host.accept_verified_peer(SLOT, "2000", io, clock())
        .unwrap();
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
    assert!(host.fixture_stop(30));
    let mut numeric = super::host();
    assert!(matches!(
        install_with_other(
            &mut numeric.runtime,
            "missing-slot",
            "1000",
            Some(1001),
            json!({}),
            &[],
            Some(1000)
        ),
        Err(RuntimeError::MissingNode)
    ));
    assert!(numeric.fixture_stop(30));
}

#[test]
fn registered_peer_hint_and_unkey_leave_nonradio_candidate_for_its_real_terminator_or_deadline() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    install(
        &mut host,
        LOCAL,
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
            .node(LOCAL)
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
    assert!(host.fixture_stop(8030));
}

#[test]
fn round2_retained_port_reload_preserves_start_and_recovery_but_revokes_command_work() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut defs = vec![
        (
            Kind::Macro,
            "boot",
            json!({"actions":[{"action":"connect","parameter":"2011"}]}),
        ),
        (
            Kind::Macro,
            "recover",
            json!({"actions":[{"action":"connect","parameter":"2012"}]}),
        ),
        (
            Kind::Macro,
            "end",
            json!({"actions":[{"action":"connect","parameter":"2013"}]}),
        ),
        (
            Kind::Macro,
            "wait",
            json!({"actions":[{"action":"pause","timing":{"duration_ms":300}},{"action":"connect","parameter":"2099"}]}),
        ),
        (
            Kind::Event,
            "start",
            json!({"start":{"at":"controller_start","macro":"boot"}}),
        ),
        (
            Kind::Event,
            "window",
            json!({"restart_recovery":true,"start":{"at":"every day at 23:55 in UTC","macro":"recover"},"end":{"at":"every day at 0:10 in UTC","macro":"end"}}),
        ),
    ];
    let mut host = Host::fixture_start(
        configuration("usb", &defs),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let port = LOCAL.parse::<ResourceId>().unwrap();
    let work = host.runtime.node(LOCAL).unwrap().command_work().unwrap();
    let drain = |host: &mut Host, clock| {
        let mut dials = Vec::new();
        for _ in 0..8 {
            if let Some((LinkEffect::Connect(dial), receipt)) =
                host.runtime.advance_commands(LOCAL, clock).unwrap()
            {
                dials.push(dial.remote().to_owned());
                host.runtime
                    .node(LOCAL)
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
            .advance_commands(LOCAL, clock())
            .unwrap()
            .is_none()
    );
    assert_eq!(pending.outcome(), None);
    defs.push((
        Kind::TxSignaling,
        "",
        json!({"ptt":{"timing":{"hang_ms":12}}}),
    ));
    host.fixture_reload(configuration("usb", &defs), clock())
        .unwrap();
    assert!(!work.is_current());
    assert_eq!(pending.outcome(), Some(MacroOutcome::Cancelled));
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
    assert!(host.fixture_stop(601000));
}

#[test]
fn delayed_directory_selection_preserves_ingress_order_and_rejects_a_retired_connection() {
    use crate::link::session::tests::State;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    };

    // Only this synthetic file lookup can block. The bound also detects an implementation
    // that holds the control owner while waiting for the file.
    static LOOKUP: Mutex<Option<(SyncSender<()>, Receiver<()>)>> = Mutex::new(None);
    static ANSWERED: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn record(
        _: *mut c_void,
        _: *const std::ffi::c_char,
        _: usize,
        remote: *const std::ffi::c_char,
        length: usize,
        sink: abi::rptadv_text_sink_v1,
        context: *mut c_void,
    ) -> i32 {
        if unsafe { std::slice::from_raw_parts(remote.cast::<u8>(), length) } != b"4000" {
            return -1;
        }
        let gate = LOOKUP.lock().unwrap().take();
        let Some((entered, release)) = gate else {
            return -1;
        };
        if entered.send(()).is_err() || release.recv_timeout(Duration::from_secs(4)).is_err() {
            return -1;
        }
        let Some(sink) = sink else {
            return -1;
        };
        let route = b"radio@127.0.0.1:4569/4000,127.0.0.1";
        unsafe {
            sink(context, route.as_ptr().cast(), route.len());
        }
        ANSWERED.store(true, Ordering::Release);
        0
    }
    fn wait(mut ready: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(1);
        while !ready() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }
    fn input(peer: &Mutex<State>, symbols: &[u8]) {
        peer.lock()
            .unwrap()
            .input
            .extend(symbols.iter().copied().map(Input::Digit));
    }
    fn digits(peer: &Mutex<State>, target: &str) -> Vec<u8> {
        let prefix = format!("D {target} 1000 ");
        peer.lock()
            .unwrap()
            .texts
            .iter()
            .filter_map(|line| {
                line.strip_prefix(prefix.as_bytes())
                    .and_then(|body| body.last().copied())
            })
            .collect()
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut passed = Vec::new();
    for retire in [false, true] {
        ANSWERED.store(false, Ordering::Release);
        let (signal, entered) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        *LOOKUP.lock().unwrap() = Some((signal, gate));
        let mut table = service_table();
        table.directory_record = Some(record);
        let config = configuration(
            "usb",
            &[
                (
                    Kind::Allstarlink,
                    "",
                    json!({"lookup_method":"file","directory_file":"fixture",
                "iax":{"registration_url":""},
                "static_nodes":{"6000":{"host":"127.0.0.1","port":4569,"source":"127.0.0.1"}}}),
                ),
                (
                    Kind::Command,
                    "",
                    json!({"mappings":{
                        "choose":{"command":"C","protected":false,"action":"select_command_target"},
                        "mute":{"command":"B","protected":false,"action":"rx_mute_toggle","target":"port"}
                    }}),
                ),
            ],
        );
        let engine = unsafe {
            crate::lifecycle::tests::fixture_engine(
                config,
                &table,
                crate::fixture::control_descriptor(),
            )
        }
        .unwrap();
        let (origin, observer) = engine
            .run(|host| {
                Ok((
                    attach(host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE),
                    attach(host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE),
                ))
            })
            .unwrap();
        // Verify the relay and the same source's static operation before the gated selection.
        input(&origin, b"C6000#1#");
        let static_control = wait(|| digits(&observer, "6000") == b"1");
        input(&origin, b"C4000#56");
        let started = entered.recv_timeout(Duration::from_secs(1)).is_ok();
        input(&observer, b"B#");
        let independent = wait(|| {
            engine
                .run(|host| {
                    Ok(host
                        .runtime
                        .node(LOCAL)
                        .unwrap()
                        .radio_receive_status()
                        .muted)
                })
                .unwrap()
        });
        let mut replacement = None;
        if retire {
            let now = engine.elapsed_ms();
            replacement = Some(
                engine
                    .run(move |host| {
                        let previous = host
                            .peers
                            .iter()
                            .find(|peer| peer.remote == "2000")
                            .unwrap()
                            .control
                            .reply_target();
                        host.detach(LOCAL, "2000", now);
                        let next = attach(host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
                        let current = host
                            .peers
                            .iter()
                            .find(|peer| peer.remote == "2000")
                            .unwrap()
                            .control
                            .reply_target();
                        assert!(previous.ended() && previous != current);
                        Ok(next)
                    })
                    .unwrap(),
            );
        }
        // Establish the successor's own target while the old lookup is still blocked.
        let replacement_selected = replacement.as_ref().is_none_or(|peer| {
            input(peer, b"C6000#8");
            wait(|| digits(&observer, "6000") == b"18")
        });
        let _ = release.send(());
        let resolved = started && wait(|| ANSWERED.load(Ordering::Acquire));
        // The callback may return before the worker posts its result. Once every
        // registered lookup job has finished, let the real owner consume that result.
        let drained = wait(|| crate::lifecycle::tests::fixture_jobs_finished(&engine));
        let now = engine.clock();
        engine.run(move |host| host.pump(now)).unwrap();
        let forwarded = if let Some(replacement) = replacement {
            input(&replacement, b"9");
            wait(|| digits(&observer, "6000") == b"189") && digits(&observer, "4000").is_empty()
        } else {
            wait(|| digits(&observer, "4000") == b"56")
        };
        let stopped = crate::lifecycle::tests::fixture_stop() == 0;
        LOOKUP.lock().unwrap().take();
        eprintln!(
            "ASL_DIRECTORY_SELECTION retire={retire} static={static_control} started={started} independent={independent} replacement_selected={replacement_selected} answered={resolved} drained={drained} forwarded={forwarded} stopped={stopped}"
        );
        assert!(
            static_control && independent && replacement_selected && drained && stopped,
            "static selection, independent ingress, worker drain and shutdown are prerequisites"
        );
        passed.push(started && resolved && forwarded);
    }
    assert_eq!(
        passed,
        [true, true],
        "a blocked ASL command target must retain ordered symbols only for its current ingress"
    );
}
