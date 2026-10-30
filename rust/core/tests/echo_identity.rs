use rpt_advanced_core::command::DtmfCommandMap;
use rpt_advanced_core::link::{EchoAlias, PeerIdentity, ProtocolId};

#[test]
fn absent_schedule_is_a_configuration_error_not_an_identity_panic() {
    use rpt_advanced_core::config::{
        ConfigDocument, ConfigError, NodeId, ResolvedScheduleSettings,
    };
    let document = ConfigDocument::parse("[1000]\n").unwrap();
    let error =
        ResolvedScheduleSettings::resolve(&document, &NodeId::new("1000").unwrap(), "missing")
            .unwrap_err();
    assert!(matches!(error, ConfigError::Structure { .. }));
}

#[test]
fn echo_aliases_are_canonical_before_configuration_and_access_comparisons() {
    use rpt_advanced_core::{
        access::AccessPolicy,
        config::{
            ConfigDocument, NodeId, ResolvedPermanentLinkSettings, ResolvedScheduleSettings, Schema,
        },
    };
    let doc = ConfigDocument::parse("[1000]\nradio_channel=usb\n[permanent 1000 home]\nremote_node=4000\n[permanent 1000 echo]\nremote_node=30123456\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    assert_eq!(
        ResolvedPermanentLinkSettings::resolve(&doc, &node, "echo")
            .unwrap()
            .value
            .remote_nodes,
        ["3123456"]
    );
    let schedule = "[1000]\nradio_channel=usb\n[permanent 1000 home]\nremote_node=4000\n[schedule 1000 one]\nremote_node=30123456\nreplace_permanent=home\ndays=monday\nstart_time=09:00\nend_time=10:00\n";
    let doc = ConfigDocument::parse(schedule).unwrap();
    assert_eq!(
        ResolvedScheduleSettings::resolve(&doc, &node, "one")
            .unwrap()
            .value
            .remote_nodes,
        ["3123456"]
    );
    for input in [
        "[1000]\n[permanent 1000 group]\nremote_node=30123456,3123456\n".to_owned(),
        "[1000]\n[permanent 1000 one]\nremote_node=30123456\n[permanent 1000 two]\nremote_node=3123456\n".to_owned(),
        format!("{schedule}[schedule 1000 two]\nremote_node=3123456\nreplace_permanent=home\ndays=monday\nstart_time=09:00\nend_time=10:00\n"),
    ] {
        assert!(Schema::validate(&ConfigDocument::parse(&input).unwrap()).is_err());
    }
    let deny = AccessPolicy::new("", "30123456").unwrap();
    assert!(!deny.allows("3123456", true));
    let allow = AccessPolicy::new("30123456", "").unwrap();
    assert!(allow.allows("3123456", true));
    assert!(allow.allows("30123456", true));
    assert!(
        AccessPolicy::new("00123456", "")
            .unwrap()
            .allows("00123456", true)
    );
    let doc = ConfigDocument::parse(
        "[1000]\n[courtesy 1000 echo]\ninput=link\nremote_node=30123456\nmorse_text=E\n",
    )
    .unwrap();
    assert_eq!(
        rpt_advanced_core::config::ResolvedCourtesySettings::resolve(&doc, &node, "echo")
            .unwrap()
            .value
            .remote_node,
        "3123456"
    );
    let duplicated = ConfigDocument::parse("[1000]\n[courtesy 1000 one]\ninput=link\nremote_node=30123456\nmorse_text=E\n[courtesy 1000 two]\ninput=link\nremote_node=3123456\nmorse_text=F\n").unwrap();
    assert!(Schema::validate(&duplicated).is_err());
    use rpt_advanced_core::runtime::link_schedule::{LinkScheduler, RouteSpec};
    let route = |remote: &str| RouteSpec {
        local: "1000".into(),
        remote: remote.into(),
        permanent: true,
        group_label: None,
        group_name: None,
        group_priority: None,
    };
    assert_eq!(
        LinkScheduler::new(1, vec![route("30123456")], vec![], None)
            .unwrap()
            .route_specs()[0]
            .remote,
        "3123456"
    );
    assert!(
        LinkScheduler::new(1, vec![route("30123456"), route("3123456")], vec![], None).is_err()
    );
}

#[test]
fn echo_telemetry_has_no_default_dtmf_prefix_and_reuses_mapping_validation() {
    use rpt_advanced_core::{
        command::{CommandMapping, LinkAction},
        config::{ConfigDocument, NodeId, ResolvedNodeSettings},
    };
    assert!(DtmfCommandMap::standard().mappings().iter().any(|mapping| mapping.action == LinkAction::EchoTelemetry && mapping.digits.is_empty()));
    let doc = ConfigDocument::parse("[1000]\nradio_channel=usb\nlink_command_echo_telemetry=83\n")
        .unwrap();
    let settings = ResolvedNodeSettings::resolve(&doc, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    let map = settings.command_map().unwrap();
    let command = map.parse("833").unwrap();
    assert_eq!(
        (command.action, command.node.as_str()),
        (LinkAction::EchoTelemetry, "3")
    );
    assert!(
        DtmfCommandMap::new(vec![
            CommandMapping::new("83", LinkAction::EchoTelemetry),
            CommandMapping::new("83", LinkAction::Status)
        ])
        .is_err()
    );
}

#[test]
fn echo_telemetry_demand_is_command_scoped_and_fenced_by_connection() {
    use rpt_advanced_core::{
        config::EchoLinkTelemetry as Mode,
        link::{EchoTelemetryPolicy, PeerSignals},
    };
    let first = PeerSignals::new();
    let reply = first.reply_target();
    let second = PeerSignals::new().reply_target();
    let mut policy = EchoTelemetryPolicy::new(Mode::Demand, true);
    assert!(!policy.allows(&reply, 0, true));
    policy.requested(&reply, 10);
    assert!(policy.allows(&reply, 120009, true));
    assert!(!policy.allows(&reply, 120010, true));
    assert!(!policy.allows(&second, 11, true));
    first.end();
    policy.requested(&reply, 11);
    assert!(!policy.allows(&reply, 11, true));
    assert!(policy.select(Mode::Demand, 20));
    assert!(policy.allows(&second, 120019, true));
    assert!(!policy.allows(&second, 120020, true));
    for (mode, local, expected) in [
        (Mode::Off, true, false),
        (Mode::On, false, true),
        (Mode::Follow, false, false),
        (Mode::Follow, true, true),
    ] {
        assert!(policy.select(mode, 0));
        policy.requested(&second, 0);
        assert_eq!(policy.allows(&second, 1, local), expected);
    }
    let mut fixed = EchoTelemetryPolicy::new(Mode::Demand, false);
    assert!(!fixed.select(Mode::On, 0));
    fixed.requested(&second, 0);
    assert!(!fixed.allows(&second, 1, true));
}

#[test]
fn echo_announcements_use_configured_identity_and_localized_connection_wording() {
    use rpt_advanced_core::{
        config::EchoLinkAnnouncement as Mode,
        messages::{Message, MessageCatalog},
    };
    let catalog =
        MessageCatalog::from_sources("en-US", include_str!("../../../messages/en-US.ftl"), None)
            .unwrap();
    for (mode, raw, spoken) in [
        (Mode::Node, "node 123456", "node 1,2,3,4,5,6"),
        (Mode::Callsign, "W1XYZ-R", "W,1,X,Y,Z,-,R"),
        (
            Mode::Both,
            "node 123456 W1XYZ-R",
            "node 1,2,3,4,5,6 W,1,X,Y,Z,-,R",
        ),
    ] {
        for connected in [false, true] {
            let message = if connected {
                Message::PeerConnectedLocal { peer: "3123456" }
            } else {
                Message::PeerDisconnectedLocal { peer: "3123456" }
            };
            let forms = catalog
                .format_echo_peer(mode, "123456", "W1XYZ-R", &message)
                .unwrap()
                .unwrap();
            let verb = if connected {
                "connected"
            } else {
                "disconnected"
            };
            assert_eq!(forms.text, format!("{raw} {verb}"));
            assert_eq!(forms.tts, format!("{spoken} {verb}"));
            assert_eq!(
                forms.morse.to_ascii_lowercase(),
                format!("{} {verb}", raw.to_ascii_lowercase())
            );
        }
    }
    assert!(
        catalog
            .format_echo_peer(
                Mode::Off,
                "123456",
                "W1XYZ-R",
                &Message::PeerConnectedLocal { peer: "3123456" }
            )
            .unwrap()
            .is_none()
    );
    let third = catalog
        .format_echo_peer(
            Mode::Both,
            "123456",
            "W1XYZ-R",
            &Message::PeerDisconnectedThirdParty {
                first: "2000",
                second: "30123456",
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(third.text, "2000 DISCONNECTED FROM node 123456 W1XYZ-R");
    assert_eq!(
        third.tts,
        "2000 disconnected from node 1,2,3,4,5,6 W,1,X,Y,Z,-,R"
    );
    let third = catalog
        .format_echo_peer(
            Mode::Node,
            "123456",
            "W1XYZ-R",
            &Message::PeerConnectedThirdParty {
                first: "2000",
                second: "30123456",
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(third.text, "2000 CONNECTED TO node 123456");
    assert_eq!(third.tts, "2000 connected to node 1,2,3,4,5,6");
    assert!(
        catalog
            .format_echo_peer(Mode::Node, "123456", "W1XYZ-R", &Message::NoLinks)
            .is_err()
    );
}

#[test]
fn unsupported_remote_command_catalog_validates_and_renders_every_form() {
    use rpt_advanced_core::messages::{Message, MessageCatalog};
    let catalog =
        MessageCatalog::from_sources("en-US", include_str!("../../../messages/en-US.ftl"), None)
            .unwrap();
    let forms = catalog
        .format(&Message::RemoteCommandUnsupported { node: "3123456" })
        .unwrap();
    assert!(forms.text.contains("3123456"));
    assert!(forms.tts.contains("3123456"));
    assert!(forms.morse.contains("3123456"));
}

#[test]
fn echo_remote_commands_never_create_asl_control_envelopes() {
    use rpt_advanced_core::{
        access::AccessPolicy,
        command::{Command, LinkAction},
        runtime::{
            GenerationSettings, NodeHost, RuntimeGeneration,
            dtmf::{DigitEvent, DigitOperation},
            links::NodeLinkControl,
        },
    };
    let generation = RuntimeGeneration::prepare(
        1,
        GenerationSettings {
            node: "524950".into(),
            device: "radio0".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
        },
        (),
        (),
    )
    .unwrap();
    let mut host = NodeHost::new(generation);
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        None,
    )
    .unwrap();
    links.accept("3123456", true).unwrap();
    assert!(
        links
            .command(
                DigitOperation::new(
                    Command {
                        action: LinkAction::Transceive,
                        node: "30123456".into()
                    },
                    None
                ),
                host.control().work().unwrap(),
                0,
                true
            )
            .is_err(),
        "alternate spelling must not create a second connection"
    );
    for digit in [None, Some('5')] {
        let mut command = DigitOperation::new(
            Command {
                action: LinkAction::Command,
                node: "3123456".into(),
            },
            None,
        );
        command.digit = digit;
        assert!(
            links
                .command(command, host.control().work().unwrap(), 0, true)
                .is_err(),
            "EchoLink is not an ASL remote-command route"
        );
    }
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '5',
                now_ms: 1
            })
            .is_none(),
        "unsupported selection changed the local collector's remote destination"
    );
    // Reuse the same controller/grant path: a verified directory entry is not
    // administrative authorization, and this operation has no default prefix.
    use rpt_advanced_core::{
        config::EchoLinkTelemetry,
        runtime::{dtmf::DigitOutcome, links::LinkEffect},
    };
    assert!(
        links
            .command(
                DigitOperation::new(
                    Command {
                        action: LinkAction::EchoTelemetry,
                        node: "3".into()
                    },
                    None
                ),
                host.control().work().unwrap(),
                2,
                true
            )
            .is_err()
    );
    let mut mappings = DtmfCommandMap::standard().mappings().to_vec();
    mappings
        .iter_mut()
        .find(|mapping| mapping.action == LinkAction::EchoTelemetry)
        .unwrap()
        .digits = "83".into();
    links.reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::new(mappings).unwrap(),
        None,
    );
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    links.configure_admin(&digest, &digest, 1000);
    for digit in "*833#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 3 })
                .is_none()
        );
    }
    for digit in "*8001234#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 4 })
                .is_none()
        );
    }
    let mut selected = None;
    for digit in "*833#".chars() {
        selected = links
            .digit(DigitEvent::Digit { digit, now_ms: 5 })
            .or(selected);
    }
    let Some(DigitOutcome::Operation(selected)) = selected else {
        panic!("authorized typed operation")
    };
    assert!(matches!(
        links.command(selected, host.control().work().unwrap(), 6, false),
        Ok(LinkEffect::EchoTelemetry(EchoLinkTelemetry::Demand))
    ));
    let mut invalid = None;
    for digit in "*839#".chars() {
        invalid = links
            .digit(DigitEvent::Digit { digit, now_ms: 7 })
            .or(invalid);
    }
    let Some(DigitOutcome::Operation(invalid)) = invalid else {
        panic!("authorized invalid selection")
    };
    assert!(matches!(
        links.command(invalid, host.control().work().unwrap(), 8, false),
        Err(rpt_advanced_core::link::AdmissionError::Invalid)
    ));
}

#[test]
fn protocol_identity_does_not_collide() {
    assert_eq!(
        PeerIdentity::from_operator("3").unwrap().protocol(),
        ProtocolId::AllStarLink
    );
    let asl = PeerIdentity::parse(ProtocolId::AllStarLink, "123456").unwrap();
    let echo = PeerIdentity::parse(ProtocolId::EchoLink, "123456").unwrap();
    assert_ne!(asl, echo);
    assert_eq!(echo.protocol(), ProtocolId::EchoLink);
    assert_eq!(echo.node(), "123456");
    assert_eq!(echo.operator_alias(), "3123456");
    assert_eq!(asl.operator_alias(), "123456");
    assert_eq!(PeerIdentity::from_operator("30123456"), Ok(echo.clone()));
    assert_eq!(PeerIdentity::from_operator("123456"), Ok(asl));
    assert_eq!(
        rpt_advanced_core::link::IdentityError.to_string(),
        "invalid peer identity"
    );
    assert_eq!(
        echo,
        PeerIdentity::parse(ProtocolId::EchoLink, "00123456").unwrap()
    );
    for invalid in ["", "0", "-1", "+1", "4294967296", "1a"] {
        assert!(PeerIdentity::parse(ProtocolId::EchoLink, invalid).is_err());
    }
    assert!(PeerIdentity::parse(ProtocolId::AllStarLink, "bad name").is_err());
    assert!(PeerIdentity::from_operator("3000000").is_err());
}

#[test]
fn echo_alias_round_trips_full_u32() {
    for (id, alias) in [
        (1, "3000001"),
        (123456, "3123456"),
        (1002775, "31002775"),
        (u32::MAX, "34294967295"),
    ] {
        assert_eq!(EchoAlias::format(id), alias);
        assert_eq!(EchoAlias::parse(alias), Ok(id));
    }
    for invalid in [
        "",
        "123456",
        "3123",
        "3000000",
        "34294967296",
        "31a2345",
        "3+123456",
        "300000000001",
    ] {
        assert!(EchoAlias::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn echo_dial_waits_for_terminator_or_unkey() {
    let map = DtmfCommandMap::standard();
    for terminator in ['#', '\0'] {
        let mut collector = map.collector();
        for digit in "*331002775".chars() {
            assert_eq!(collector.feed(digit, 0), None);
        }
        let command = collector.feed(terminator, 3000).unwrap();
        let destination = map.parse(&command).unwrap().node;
        assert_eq!(EchoAlias::parse(&destination), Ok(1002775));
    }
}
