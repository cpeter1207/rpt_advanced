//! URI grammar cases are independent literals, not serializer round trips.
use rptadv_peer_support::sip::{
    Host, NameAddr, ParseError, Parser, RouteError, Scheme, SipUri, StartLine, Transport,
};

#[test]
fn uri_components_preserve_userinfo_delimiters_and_percent_bytes() {
    // RFC 4475 sections 3.1.1.2, 3.1.1.4 and 3.1.1.9.
    let uri = SipUri::parse(b"sip:1_unusual.URI~(to-be!sure)&isn't+it$/crazy?,/;;*:&it+has=1,weird!*pas$wo~d_too.(doesn't-it)@example.com;unknownparam").unwrap();
    assert_eq!(
        uri.user(),
        Some(b"1_unusual.URI~(to-be!sure)&isn't+it$/crazy?,/;;*".as_slice())
    );
    assert_eq!(
        uri.password(),
        Some(b"&it+has=1,weird!*pas$wo~d_too.(doesn't-it)".as_slice())
    );
    assert!(matches!(uri.host(), Host::Name(b"example.com")));
    assert_eq!(uri.parameter(b"unknownparam"), Some(None));
    let uri = SipUri::parse(
        b"sip:null%00%40;par=u?x@EXAMPLE.com.:0005060;transport=TCP?subject=a%0D%0A&x=",
    )
    .unwrap();
    assert_eq!(uri.user(), Some(b"null%00%40;par=u?x".as_slice()));
    assert_eq!(uri.port(), Some(5060));
    assert_eq!(uri.headers(), Some(b"subject=a%0D%0A&x=".as_slice()));
    assert_eq!(uri.require_cleartext_routing(), Err(RouteError::UriHeaders));
}

#[test]
fn sips_is_valid_syntax_but_never_downgrades_to_cleartext() {
    let uri = SipUri::parse(b"SiPs:alice@example.com;transport=tcp").unwrap();
    assert_eq!(uri.scheme(), Scheme::Sips);
    assert_eq!(
        uri.require_cleartext_routing(),
        Err(RouteError::SecureRouting)
    );
    for target in [
        b"sip:a@b;transport=tls".as_slice(),
        b"sip:a@b;transport=sctp",
        b"sip:a@b;transport=other",
    ] {
        assert_eq!(
            SipUri::parse(target).unwrap().require_cleartext_routing(),
            Err(RouteError::Transport)
        );
    }
    for target in [
        b"sip:a@b".as_slice(),
        b"sip:a@b;transport=UDP",
        b"sip:a@b;transport=tcp",
    ] {
        assert_eq!(
            SipUri::parse(target).unwrap().require_cleartext_routing(),
            Ok(())
        );
    }
    assert_eq!(
        SipUri::parse(b"sip:a@b:0")
            .unwrap()
            .require_cleartext_routing(),
        Err(RouteError::Port)
    );
}

#[test]
fn rfc5118_messages_distinguish_framing_from_ipv6_target_validation() {
    for (wire, valid) in [
        (
            include_bytes!("fixtures/sip/rfc5118-ipv6-good.sip").as_slice(),
            true,
        ),
        (
            include_bytes!("fixtures/sip/rfc5118-ipv6-bad.sip").as_slice(),
            false,
        ),
    ] {
        let mut parser = Parser::default();
        let message = parser.parse(wire, Transport::Udp).unwrap();
        let StartLine::Request { target, .. } = message.start_line() else {
            panic!("request expected")
        };
        assert_eq!(SipUri::parse(target).is_ok(), valid);
    }
    // RFC 5118 4.3: 5070 inside brackets is an address group, not a port.
    let uri = SipUri::parse(b"sip:[2001:db8::10:5070]").unwrap();
    assert_eq!(uri.port(), None);
    assert!(matches!(uri.host(), Host::Ipv6(ip) if ip.segments()[7] == 0x5070));
    let uri = SipUri::parse(b"sip:[2001:db8::10]:5070").unwrap();
    assert_eq!(uri.port(), Some(5070));
    for target in [
        b"sip:[::ffff:192.0.2.2]".as_slice(),
        b"sip:[::]",
        b"sip:[2001:db8:1:2:3:4:5:6]",
    ] {
        assert!(SipUri::parse(target).is_ok());
    }
    for target in [
        b"sip:[:::192.0.2.2]".as_slice(),
        b"sip:[1:2:3:4:5:6:7:8:9]",
        b"sip:[2001:db8::1%eth0]",
        b"sip:[::ffff:192.00.2.2]",
    ] {
        assert!(SipUri::parse(target).is_err());
    }
}

#[test]
fn uri_rejects_invalid_escapes_host_port_and_component_boundaries() {
    for target in [
        b"sip:".as_slice(),
        b"sip:@b",
        b"sip:a@@b",
        b"sip:a%2@b",
        b"sip:a%ZZ@b",
        b"sip:a b@c",
        b"sip:a:b:c@d",
        b"sip:-bad.example",
        b"sip:bad-.example",
        b"sip:a..example",
        b"sip:a_1.example",
        b"sip:example.123",
        b"sip:999.1.2.3",
        b"sip:[::1",
        b"sip:[]",
        b"sip:a:",
        b"sip:a:-1",
        b"sip:a:65536",
        b"sip:a:999999999999999999999999",
        b"sip:a;",
        b"sip:a;p=",
        b"sip:a;p=%",
        b"sip:a?",
        b"sip:a?=x",
        b"sip:a?x",
        b"sip:a?x=y&",
        b"sip:a?x=y;z",
        b"sip:a#fragment",
    ] {
        assert!(SipUri::parse(target).is_err(), "{target:?}");
    }
    assert_eq!(
        SipUri::parse(b"tel:1234").err(),
        Some(ParseError::UnsupportedScheme)
    );
    let mut huge = b"sip:".to_vec();
    huge.resize(4097, b'a');
    assert_eq!(SipUri::parse(&huge).err(), Some(ParseError::Limit));
}

#[test]
fn duplicate_and_malformed_known_parameters_cannot_smuggle_routing() {
    for target in [
        b"sip:a;transport=tcp;Transport=udp".as_slice(),
        b"sip:a;transport=tcp;%74ransport=tls",
        b"sip:a;p=1;%70=2",
        b"sip:a;transport",
        b"sip:a;user=",
        b"sip:a;method=IN/VITE",
        b"sip:a;ttl=256",
        b"sip:a;ttl=-1",
        b"sip:a;ttl=0000",
        b"sip:a;maddr=[bad]",
        b"sip:a;lr=x",
    ] {
        assert!(SipUri::parse(target).is_err(), "{target:?}");
    }
    let uri = SipUri::parse(
        b"sip:a;transport=TCP;user=phone;method=INVITE;ttl=255;maddr=[2001:db8::1];lr;p=%3b",
    )
    .unwrap();
    assert_eq!(uri.parameter(b"transport"), Some(Some(b"TCP".as_slice())));
    assert_eq!(uri.parameter(b"absent"), None);
    assert_eq!(uri.parameter(b"p"), Some(Some(b"%3b".as_slice())));
}

#[test]
fn nameaddr_uri_and_header_parameters_never_share_ownership() {
    let address =
        NameAddr::parse(b"\"Caller, x\"<sip:a;b?c@host;transport=tcp?subject=x>;tag=123").unwrap();
    let uri = SipUri::parse(address.uri()).unwrap();
    assert_eq!(uri.user(), Some(b"a;b?c".as_slice()));
    assert_eq!(uri.parameter(b"transport"), Some(Some(b"tcp".as_slice())));
    assert_eq!(address.parameters(), b";tag=123");
    assert_eq!(uri.require_cleartext_routing(), Err(RouteError::UriHeaders));
}

#[test]
fn percent_equivalent_transport_values_do_not_change_profile_meaning() {
    assert_eq!(
        SipUri::parse(b"sip:a;transport=%75%64p")
            .unwrap()
            .require_cleartext_routing(),
        Ok(())
    );
    assert_eq!(
        SipUri::parse(b"sip:a;transport=%74ls")
            .unwrap()
            .require_cleartext_routing(),
        Err(RouteError::Transport)
    );
    assert!(
        matches!(SipUri::parse(b"sip:192.168.001.001").unwrap().host(), Host::Ipv4(ip) if ip.octets() == [192,168,1,1])
    );
}
