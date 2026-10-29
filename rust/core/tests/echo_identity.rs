use rpt_advanced_core::command::DtmfCommandMap;
use rpt_advanced_core::link::{EchoAlias, PeerIdentity, ProtocolId};

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
}

#[test]
fn protocol_identity_does_not_collide() {
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
