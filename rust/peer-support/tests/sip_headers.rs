//! RFC-derived header semantics; no transaction or authentication decisions.
use rptadv_peer_support::sip::{MAX_FIELD, NameAddr, ParseError, Parser, StartLine, Transport};

#[test]
fn aliases_unfolding_and_repeated_list_order_follow_rfc4475_wsinv() {
    let wire = include_bytes!("fixtures/sip/rfc4475-wsinv.sip");
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    assert_eq!(message.body().len(), 150);
    let mut scratch = [0; MAX_FIELD];
    let subject = message.unique(b"Subject").unwrap().unwrap();
    assert!(subject.unfold(&mut scratch).is_empty());
    let mut vias = Vec::new();
    for header in message.headers().filter(|h| h.is(b"via")) {
        for value in header.values(&mut scratch) {
            vias.push(value.unwrap().to_vec());
        }
    }
    assert_eq!(vias.len(), 3);
    assert!(vias[0].windows(9).any(|v| v == b"192.0.2.2"));
    assert!(vias[1].windows(19).any(|v| v == b"spindle.example.com"));
    assert!(vias[2].windows(15).any(|v| v == b"192.168.255.111"));
    assert_eq!(
        message.cseq(&mut scratch).unwrap(),
        Some((9, b"INVITE".as_slice()))
    );
    let raw = message
        .unique(b"unknownheaderwithunusualvalue")
        .unwrap()
        .unwrap();
    let values: Vec<_> = raw.values(&mut scratch).collect();
    assert_eq!(values, [Ok(b";;,,;;,;".as_slice())]);
}

#[test]
fn quoted_comma_and_escaped_control_bytes_are_not_field_boundaries() {
    // RFC 4475 section 3.1.1.2 permits quoted-pair BEL, NUL and DEL.
    let wire = b"SIP/2.0 200 OK\r\nContact: \"BEL:\\\x07 NUL:\\\0 DEL:\\\x7f, x\"<sip:a@b>, <sip:c@d>\r\nCall-ID: intmeth.word%ZK-!.*_+'@word`~)(><:\\\"][?}{\r\nl:0\r\n\r\n";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    let mut scratch = [0; MAX_FIELD];
    let field = message.unique(b"m").unwrap().unwrap();
    let values: Vec<_> = field.values(&mut scratch).collect();
    assert_eq!(values.len(), 2);
    let address = NameAddr::parse(values[0].unwrap()).unwrap();
    assert_eq!(address.uri(), b"sip:a@b");
    assert!(address.display_name().contains(&0));
    assert_eq!(
        message.call_id(&mut scratch).unwrap(),
        Some(b"intmeth.word%ZK-!.*_+'@word`~)(><:\\\"][?}{".as_slice())
    );
}

#[test]
fn folding_inside_quotes_preserves_escapes_and_authentication_commas() {
    let wire = b"SIP/2.0 401 Unauthorized\r\nFrom: \"A,\r\n B\\\"\" <sip:a@b> ; tag = abc\r\nWWW-Authenticate: Digest realm=\"a,b\", nonce=\"n\", qop=\"auth,auth-int\"\r\nl:0\r\n\r\n";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    let mut scratch = [0; MAX_FIELD];
    let field = message.unique(b"from").unwrap().unwrap();
    let unfolded = field.unfold(&mut scratch);
    assert_eq!(unfolded, b"\"A, B\\\"\" <sip:a@b> ; tag = abc");
    let address = NameAddr::parse(unfolded).unwrap();
    assert_eq!(address.parameters(), b"; tag = abc");
    assert_eq!(
        message
            .unique(b"www-authenticate")
            .unwrap()
            .unwrap()
            .values(&mut scratch)
            .count(),
        1
    );
}

#[test]
fn nested_comments_apply_only_to_fields_with_comment_grammar() {
    for (value, valid) in [
        ("tool/1 (outer, (inner\\)text))", true),
        ("tool/1 (open", false),
        ("tool/1 )close(", false),
        ("tool/1 (bad\\)", false),
    ] {
        let wire = format!("SIP/2.0 200 OK\r\nServer: {value}\r\n\r\n");
        let mut parser = Parser::default();
        let message = parser.parse(wire.as_bytes(), Transport::Udp).unwrap();
        let mut scratch = [0; MAX_FIELD];
        assert_eq!(
            message
                .unique(b"server")
                .unwrap()
                .unwrap()
                .values(&mut scratch)
                .all(|v| v.is_ok()),
            valid
        );
    }
    let nested = format!(
        "SIP/2.0 200 OK\r\nServer: {}{}\r\n\r\n",
        "(".repeat(100),
        ")".repeat(100)
    );
    let mut parser = Parser::default();
    let message = parser.parse(nested.as_bytes(), Transport::Udp).unwrap();
    assert!(
        message
            .unique(b"server")
            .unwrap()
            .unwrap()
            .values(&mut [0; MAX_FIELD])
            .all(|v| v.is_ok())
    );
}

#[test]
fn name_address_respects_header_parameter_boundary_and_display_spacing() {
    let wire = include_bytes!("fixtures/sip/rfc4475-lwsdisp.sip");
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    let mut scratch = [0; MAX_FIELD];
    let address = NameAddr::parse(
        message
            .unique(b"from")
            .unwrap()
            .unwrap()
            .unfold(&mut scratch),
    )
    .unwrap();
    assert_eq!(address.display_name(), b"caller");
    assert_eq!(address.uri(), b"sip:caller@example.com");
    assert_eq!(address.parameters(), b";tag=323");
    let address = NameAddr::parse(b"sip:a@b;tag=123").unwrap();
    assert_eq!(address.uri(), b"sip:a@b");
    assert_eq!(address.parameters(), b";tag=123");
    for value in [
        b"<sip:a@b>;tag=\"unterminated".as_slice(),
        b"name <sip:a@b",
        b"<sip:a@b> trailing",
        b"sip:a@b?subject=x",
        b"<sip:a@b>;tag=",
        b"bad(name) <sip:a@b>",
    ] {
        assert!(NameAddr::parse(value).is_err());
    }
}

#[test]
fn singleton_typed_fields_reject_duplicates_and_invalid_cseq_without_normalizing_method() {
    for field in [
        "CSeq: 1 INVITE\r\nCSeq: 1 INVITE",
        "CSeq: 2147483648 INVITE",
        "CSeq: 1",
        "CSeq: 1 INVITE extra",
        "CSeq: -1 INVITE",
        "CSeq: 1 INV/ITE",
    ] {
        let wire = format!("SIP/2.0 200 OK\r\n{field}\r\n\r\n");
        let mut parser = Parser::default();
        assert!(
            parser
                .parse(wire.as_bytes(), Transport::Udp)
                .unwrap()
                .cseq(&mut [0; MAX_FIELD])
                .is_err()
        );
    }
    let wire = b"SIP/2.0 200 \r\ni: a@b\r\nCall-ID: c@d\r\nCSeq: 0009 IN%56ITE\r\n\r\n";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    assert_eq!(
        message.call_id(&mut [0; MAX_FIELD]),
        Err(ParseError::Duplicate)
    );
    assert_eq!(
        message.cseq(&mut [0; MAX_FIELD]).unwrap(),
        Some((9, b"IN%56ITE".as_slice()))
    );
    assert!(matches!(
        message.start_line(),
        StartLine::Response {
            status: 200,
            reason: b""
        }
    ));
}

#[test]
fn malformed_list_syntax_is_rejected_without_splitting_opaque_fields() {
    for value in [
        "\"unclosed <sip:a@b>",
        "<sip:a@b",
        "<sip:a@b>>",
        "<sip:a@b>,",
        ",<sip:a@b>",
        "\"bad\\",
    ] {
        let wire = format!("SIP/2.0 200 OK\r\nContact: {value}\r\n\r\n");
        let mut parser = Parser::default();
        let message = parser.parse(wire.as_bytes(), Transport::Udp).unwrap();
        assert!(
            message
                .unique(b"contact")
                .unwrap()
                .unwrap()
                .values(&mut [0; MAX_FIELD])
                .any(|v| v.is_err())
        );
    }
}

#[test]
fn call_id_lists_do_not_apply_quoted_string_or_angle_grouping() {
    let mut parser = Parser::default();
    let message = parser
        .parse(
            b"SIP/2.0 200 OK\r\nIn-Reply-To: a\"(><@b,c@d\r\n\r\n",
            Transport::Udp,
        )
        .unwrap();
    let values: Vec<_> = message
        .unique(b"in-reply-to")
        .unwrap()
        .unwrap()
        .values(&mut [0; MAX_FIELD])
        .map(|v| v.map(<[u8]>::to_vec))
        .collect();
    assert_eq!(values, [Ok(b"a\"(><@b".to_vec()), Ok(b"c@d".to_vec())]);
}

#[test]
fn generic_parameter_hosts_and_quoted_utf8_are_validated_in_their_grammar() {
    for value in [
        b"<sip:a@b>;p=[bad]".as_slice(),
        b"<sip:a@b>;p=foo:bar",
        b"<sip:a@b>;p=::1",
        b"\"\xff\"<sip:a@b>",
    ] {
        assert!(NameAddr::parse(value).is_err());
    }
    assert!(NameAddr::parse(b"<sip:a@b>;p=[2001:db8::1];q=\"utf8:\xc3\xa9\"").is_ok());
}

#[test]
fn folding_cannot_turn_an_illegal_quoted_pair_into_a_valid_escape() {
    // RFC 3261 section 25 excludes CR and LF from quoted-pair. Ordinary
    // backslashes in Call-ID/unknown fields must not inherit quoted semantics.
    for field in [
        b"Contact: \"bad\\\r\n name\"<sip:a@b>".as_slice(),
        b"From: \"bad\\\r\n name\"<sip:a@b>",
        b"Server: tool (bad\\\r\n comment)",
        b"Contact: <sip:a@b>;p=\"bad\\\r\n value\"",
    ] {
        let mut wire = b"SIP/2.0 200 OK\r\n".to_vec();
        wire.extend_from_slice(field);
        wire.extend_from_slice(b"\r\n\r\n");
        let mut parser = Parser::default();
        let message = parser.parse(&wire, Transport::Udp).unwrap();
        let header = message.headers().next().unwrap();
        assert!(
            header
                .values(&mut [0; MAX_FIELD])
                .any(|value| value.is_err())
        );
    }
    let mut parser = Parser::default();
    let message = parser
        .parse(
            b"SIP/2.0 200 OK\r\nUnknown: plain\\\r\n text\r\n\r\n",
            Transport::Udp,
        )
        .unwrap();
    assert_eq!(
        message
            .headers()
            .next()
            .unwrap()
            .values(&mut [0; MAX_FIELD])
            .next()
            .unwrap()
            .unwrap(),
        b"plain\\ text"
    );
}

#[test]
fn compact_names_match_long_names_without_rewriting_original_bytes() {
    let wire = b"SIP/2.0 200 OK\r\ni:a@b\r\nm:<sip:a@b>\r\ne:gzip\r\nl:0\r\nc:application/sdp\r\nf:sip:a@b\r\ns:subject\r\nk:timer\r\nt:sip:a@b\r\nv:SIP/2.0/UDP host\r\n\r\n";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    for (short, long) in [
        (b"i".as_slice(), b"Call-ID".as_slice()),
        (b"m", b"Contact"),
        (b"e", b"Content-Encoding"),
        (b"l", b"Content-Length"),
        (b"c", b"Content-Type"),
        (b"f", b"From"),
        (b"s", b"Subject"),
        (b"k", b"Supported"),
        (b"t", b"To"),
        (b"v", b"Via"),
    ] {
        let field = message.unique(long).unwrap().unwrap();
        assert_eq!(field.name(), short);
        assert!(field.is(short));
    }
    assert!(message.unique(b"x").unwrap().is_none());
}

#[test]
fn typed_field_absence_and_invalid_scalar_boundaries_remain_distinct() {
    let mut parser = Parser::default();
    let message = parser
        .parse(b"SIP/2.0 200 OK\r\n\r\n", Transport::Udp)
        .unwrap();
    assert_eq!(message.cseq(&mut [0; MAX_FIELD]).unwrap(), None);
    assert_eq!(message.call_id(&mut [0; MAX_FIELD]).unwrap(), None);
    for value in ["", "@a", "a@", "a@b@c", "a b", "a;b"] {
        let wire = format!("SIP/2.0 200 OK\r\nCall-ID:{value}\r\n\r\n");
        let message = parser.parse(wire.as_bytes(), Transport::Udp).unwrap();
        assert!(message.call_id(&mut [0; MAX_FIELD]).is_err());
    }
    for value in [
        b"".as_slice(),
        b"\"display\"sip:a@b",
        b"<sip:a@b>;",
        b"<sip:a@b>;=x",
        b"<sip:a@b>;p=[bad",
        b"\"bad\0\"<sip:a@b>",
        b"\"bad\\\xff\"<sip:a@b>",
    ] {
        assert!(NameAddr::parse(value).is_err());
    }
    assert!(NameAddr::parse(&[b'a'; 4097]).is_err());
}

#[test]
fn generic_header_parameter_names_are_unique_without_uri_percent_decoding() {
    for value in [
        b"<sip:a@b>;tag=one;TAG=two".as_slice(),
        b"sip:a@b;tag=one;tag=two",
        b"<sip:a@b>;p;p",
        b"<sip:a@b>;p=\"a;b\";P=x",
    ] {
        assert!(matches!(NameAddr::parse(value), Err(ParseError::Duplicate)));
    }
    assert!(NameAddr::parse(b"<sip:a@b>;tag=one;%74ag=two;p=\"a;tag=not-a-parameter\"").is_ok());
}

#[test]
fn typed_wire_address_validates_odd_and_even_escapes_before_unfolding() {
    for slashes in 1..=4 {
        for parameter in [false, true] {
            let mut wire = b"SIP/2.0 200 OK\r\nFrom: ".to_vec();
            wire.extend_from_slice(if parameter {
                b"<sip:a@b>;p=\"bad"
            } else {
                b"\"bad"
            });
            wire.extend(std::iter::repeat_n(b'\\', slashes));
            wire.extend_from_slice(if parameter {
                b"\r\n text\"\r\n\r\n"
            } else {
                b"\r\n text\"<sip:a@b>\r\n\r\n"
            });
            let mut parser = Parser::default();
            let message = parser.parse(&wire, Transport::Udp).unwrap();
            let header = message.headers().next().unwrap();
            let mut scratch = [0; MAX_FIELD];
            // Normalization alone loses the distinction. Wire callers must use
            // name_addr, or the checked values path before NameAddr::parse.
            assert!(NameAddr::parse(header.unfold(&mut scratch)).is_ok());
            assert_eq!(header.name_addr(&mut scratch).is_ok(), slashes % 2 == 0);
            assert_eq!(
                header.values(&mut scratch).all(|v| v.is_ok()),
                slashes % 2 == 0
            );
        }
        let mut wire = b"SIP/2.0 200 OK\r\nServer: tool (a".to_vec();
        wire.extend(std::iter::repeat_n(b'\\', slashes));
        wire.extend_from_slice(b"\r\n b)\r\n\r\n");
        let mut parser = Parser::default();
        let message = parser.parse(&wire, Transport::Udp).unwrap();
        assert_eq!(
            message
                .headers()
                .next()
                .unwrap()
                .values(&mut [0; MAX_FIELD])
                .all(|v| v.is_ok()),
            slashes % 2 == 0
        );
    }
    let mut parser = Parser::default();
    let message = parser
        .parse(
            b"SIP/2.0 200 OK\r\ni:a\\\r\n b\r\nX-Literal:a\\\r\n b\r\n\r\n",
            Transport::Udp,
        )
        .unwrap();
    for header in message.headers() {
        assert_eq!(
            header.values(&mut [0; MAX_FIELD]).next().unwrap().unwrap(),
            b"a\\ b"
        );
    }
}
