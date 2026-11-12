//! Independent framing behavior for the uncomposed SIP prerequisite.
use rptadv_peer_support::sip::{MAX_MESSAGE, ParseError, Parser, TcpDecoder, Transport};

#[test]
fn udp_content_length_preserves_binary_and_discards_suffix() {
    let wire = b"MESSAGE sip:b@example.com SIP/2.0\r\nl: 4\r\n\r\n\0\xff\r\ntrailing";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Udp).unwrap();
    assert_eq!(message.body(), b"\0\xff\r\n");
    assert_eq!(message.consumed(), wire.len());
    assert_eq!(message.discarded(), 8);
}

#[test]
fn udp_implicit_length_and_stream_explicit_length_are_distinct() {
    let wire = b"OPTIONS sip:b@example.com SIP/2.0\r\n\r\nbody";
    let mut parser = Parser::default();
    assert_eq!(parser.parse(wire, Transport::Udp).unwrap().body(), b"body");
    assert_eq!(
        parser.parse(wire, Transport::Tcp).err(),
        Some(ParseError::MissingLength)
    );
    let short = b"OPTIONS sip:b@example.com SIP/2.0\r\nl: 4\r\n\r\nb";
    assert_eq!(
        parser.parse(short, Transport::Udp).err(),
        Some(ParseError::Truncated)
    );
    assert_eq!(
        parser.parse(short, Transport::Tcp).err(),
        Some(ParseError::Incomplete)
    );
}

#[test]
fn framing_rejects_duplicate_signed_nondecimal_and_overflow_lengths() {
    for header in [
        "l: 0\r\nContent-Length: 0",
        "l: 0\r\nl: 1",
        "l: +1",
        "l: -1",
        "l: 1x",
        "l:",
        "l: 1,1",
        "l: 18446744073709551616",
        "l: 16385",
    ] {
        let wire = format!("SIP/2.0 200 OK\r\n{header}\r\n\r\n");
        assert!(
            Parser::default()
                .parse(wire.as_bytes(), Transport::Udp)
                .is_err(),
            "{header}"
        );
    }
    let wire = b"SIP/2.0 200 OK\r\n CoNtEnT-LeNgTh: 0\r\n\r\n";
    assert!(Parser::default().parse(wire, Transport::Udp).is_err());
}

#[test]
fn tcp_fragmentation_and_pipeline_preserve_first_frame() {
    let first = b"\r\nSIP/2.0 200 OK\r\nContent-Length: 4\r\n\r\nbody";
    let second = b"SIP/2.0 204 Empty\r\nl:0\r\n\r\n";
    for split in 0..=first.len() {
        let mut wire = [0; MAX_MESSAGE];
        let mut decoder = TcpDecoder::new(&mut wire);
        assert_eq!(decoder.push(&first[..split]).unwrap(), split);
        assert_eq!(decoder.message().is_some(), split == first.len());
        let tail = [first[split..].to_vec(), second.to_vec()].concat();
        assert_eq!(decoder.push(&tail).unwrap(), first.len() - split);
        assert_eq!(decoder.message().unwrap().body(), b"body");
        decoder.reset();
        assert!(decoder.message().is_none());
        assert_eq!(decoder.push(second).unwrap(), second.len());
        assert_eq!(decoder.message().unwrap().body(), b"");
    }
    let mut wire = [0; MAX_MESSAGE];
    let mut decoder = TcpDecoder::new(&mut wire);
    for byte in first {
        assert_eq!(decoder.push(&[*byte]).unwrap(), 1);
    }
    assert_eq!(decoder.message().unwrap().body(), b"body");
}

#[test]
fn tcp_parse_leaves_pipeline_and_reset_after_error_discards_stale_index() {
    let wire = b"SIP/2.0 200 OK\r\nl:0\r\n\r\nSIP/2.0 204 Empty\r\nl:0\r\n\r\n";
    let mut parser = Parser::default();
    let first = parser.parse(wire, Transport::Tcp).unwrap();
    assert_eq!(first.consumed(), 23);
    assert_eq!(first.discarded(), 0);
    let mut storage = [0; MAX_MESSAGE];
    let mut decoder = TcpDecoder::new(&mut storage);
    assert!(decoder.push(b"SIP/2.0 200 OK\r\nl:x\r\n\r\n").is_err());
    assert!(decoder.message().is_none());
    decoder.reset();
    assert_eq!(decoder.push(&wire[..23]).unwrap(), 23);
    assert_eq!(decoder.message().unwrap().body(), b"");
}

#[test]
fn limits_apply_at_exact_logical_header_and_body_boundaries() {
    let mut headers = String::from("SIP/2.0 200 OK\r\n");
    for _ in 0..128 {
        headers.push_str("X: a\r\n");
    }
    let mut parser = Parser::default();
    assert!(
        parser
            .parse(format!("{headers}\r\n").as_bytes(), Transport::Udp)
            .is_ok()
    );
    assert!(
        parser
            .parse(format!("{headers}X: a\r\n\r\n").as_bytes(), Transport::Udp)
            .is_err()
    );
    for (length, valid) in [(16384, true), (16385, false)] {
        let mut wire = format!("SIP/2.0 200 OK\r\nl: {length}\r\n\r\n").into_bytes();
        wire.resize(wire.len() + length, 0xff);
        assert_eq!(parser.parse(&wire, Transport::Udp).is_ok(), valid);
    }
    assert!(
        parser
            .parse(&[b'x'; MAX_MESSAGE + 1], Transport::Udp)
            .is_err()
    );
}

#[test]
fn line_grammar_and_stream_truncation_fail_closed() {
    for wire in [
        b"SIP/2.0 200 OK\n\n".as_slice(),
        b"SIP/2.0 200 OK\r\nBad Name:a\r\n\r\n",
        b"INVITE  sip:a@b SIP/2.0\r\n\r\n",
        b"SIP/2.0 20 OK\r\n\r\n",
        b"SIP/2.0 200 OK\r\nMissingColon\r\n\r\n",
    ] {
        assert!(Parser::default().parse(wire, Transport::Udp).is_err());
    }
    assert_eq!(
        Parser::default().parse(b"SIP/2.0", Transport::Tcp).err(),
        Some(ParseError::Incomplete)
    );
    assert_eq!(
        Parser::default().parse(b"SIP/2.0", Transport::Udp).err(),
        Some(ParseError::Truncated)
    );
}

#[test]
fn declared_bounds_are_resource_errors_not_syntax_errors() {
    assert_eq!(
        Parser::default()
            .parse(b"SIP/2.0 200 OK\r\nl:16385\r\n\r\n", Transport::Udp)
            .err(),
        Some(ParseError::Limit)
    );
    let mut wire = b"SIP/2.0 200 OK\r\nX:".to_vec();
    wire.resize(16_383, b'a');
    wire.extend_from_slice(b"\r\n\r\n");
    assert_eq!(
        Parser::default().parse(&wire, Transport::Udp).err(),
        Some(ParseError::Limit)
    );
}

fn sized_headers(size: usize, length: usize) -> Vec<u8> {
    let mut wire = format!("SIP/2.0 200 OK\r\nl:{length}\r\n").into_bytes();
    while wire.len() + 2 < size {
        let remaining = size - wire.len() - 2;
        let part = if remaining > 4000 { 3996 } else { remaining };
        assert!(part >= 4);
        wire.extend_from_slice(b"X:");
        wire.resize(wire.len() + part - 4, b'a');
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"\r\n");
    assert_eq!(wire.len(), size);
    wire
}

#[test]
fn exact_wire_header_and_unfolded_field_boundaries() {
    let mut wire = sized_headers(16_384, 16_384);
    wire.resize(32_768, 0xff);
    let mut parser = Parser::default();
    assert_eq!(
        parser.parse(&wire, Transport::Udp).unwrap().body().len(),
        16_384
    );
    wire.push(0);
    assert_eq!(
        parser.parse(&wire, Transport::Udp).err(),
        Some(ParseError::Limit)
    );
    assert_eq!(
        parser
            .parse(&sized_headers(16_385, 0), Transport::Udp)
            .err(),
        Some(ParseError::Limit)
    );
    for (tail, valid) in [(2047, true), (2048, false)] {
        let mut wire = b"SIP/2.0 200 OK\r\nX:".to_vec();
        wire.extend(std::iter::repeat_n(b'a', 2048));
        wire.extend_from_slice(b"\r\n\t");
        wire.extend(std::iter::repeat_n(b'b', tail));
        wire.extend_from_slice(b"\r\n\r\n");
        assert_eq!(parser.parse(&wire, Transport::Udp).is_ok(), valid);
    }
    assert!(std::mem::size_of::<TcpDecoder>() + 4096 <= 8192);
    assert!(std::mem::size_of::<Parser>() + 4096 <= 8192);
}

#[test]
fn framing_ignores_http_transfer_coding_and_never_decodes_chunked_bodies() {
    let wire = b"SIP/2.0 200 OK\r\nTransfer-Encoding: chunked\r\nl:4\r\n\r\n0\r\n\r\n";
    let mut parser = Parser::default();
    let message = parser.parse(wire, Transport::Tcp).unwrap();
    assert_eq!(message.body(), b"0\r\n\r");
    assert_eq!(&wire[message.consumed()..], b"\n");
    assert_eq!(
        parser
            .parse(
                b"SIP/2.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
                Transport::Tcp
            )
            .err(),
        Some(ParseError::MissingLength)
    );
}

#[test]
fn folded_lengths_require_one_decimal_and_stream_errors_are_sticky() {
    let mut parser = Parser::default();
    assert_eq!(
        parser
            .parse(
                b"SIP/2.0 200 OK\r\nl:\r\n 0004 \t\r\n\r\nbody",
                Transport::Tcp
            )
            .unwrap()
            .body(),
        b"body"
    );
    for wire in [
        b"SIP/2.0 200 OK\r\nl:4\r\n 0\r\n\r\n".as_slice(),
        b"SIP/2.0 200 OK\r\nX:y\rX:z",
        b"SIP/2.0 200 \0\r\n\r\n",
        b"SIP/2.0 2x0 OK\r\n\r\n",
        b" SIP/2.0\r\n\r\n",
        b"INV/ITE sip:a@b SIP/2.0\r\n\r\n",
        b"INVITE sip:a\tb SIP/2.0\r\n\r\n",
        b"INVITE sip:a@b SIP/2.0 \r\n\r\n",
        b"SIP/2.0 200 OK\r\n: value\r\n\r\n",
    ] {
        assert!(parser.parse(wire, Transport::Udp).is_err());
    }
    for bad in [b"\n".as_slice(), b"\rx", b"SIP/2.0 200 OK\r\n\r\n"] {
        let mut storage = [0; MAX_MESSAGE];
        let mut decoder = TcpDecoder::new(&mut storage);
        let first = decoder.push(bad).unwrap_err();
        assert_eq!(decoder.push(b"next"), Err(first));
        assert!(decoder.message().is_none());
    }
    let mut storage = [0; MAX_MESSAGE];
    let mut decoder = TcpDecoder::new(&mut storage);
    assert_eq!(decoder.push(&[b'x'; 16_385]), Err(ParseError::Limit));
}

#[test]
fn full_header_capacity_without_terminator_fails_without_waiting_for_another_byte() {
    let mut storage = [0; MAX_MESSAGE];
    let mut decoder = TcpDecoder::new(&mut storage);
    assert_eq!(decoder.push(&[b'x'; 16_384]), Err(ParseError::Limit));
    assert!(decoder.message().is_none());
}

#[test]
fn reason_phrase_follows_rfc_byte_grammar_without_percent_decoding() {
    use rptadv_peer_support::sip::StartLine;
    let mut parser = Parser::default();
    let message = parser
        .parse(
            include_bytes!("fixtures/sip/rfc4475-unreason.sip"),
            Transport::Udp,
        )
        .unwrap();
    assert_eq!(message.body().len(), 154);
    assert!(
        matches!(message.start_line(),StartLine::Response{status:200,reason} if reason.starts_with(b"= 2**3 * 5**2 \xd0\xbd\xd0\xbe"))
    );
    for invalid in [
        b"%ZZ".as_slice(),
        b"%",
        b"%0",
        b"\xff",
        b"\xc2x",
        b"\"quote\"",
        b"\x7f",
    ] {
        let mut wire = b"SIP/2.0 200 ".to_vec();
        wire.extend_from_slice(invalid);
        wire.extend_from_slice(b"\r\nl:0\r\n\r\n");
        assert_eq!(
            parser.parse(&wire, Transport::Udp).err(),
            Some(ParseError::Syntax)
        );
    }
    // Literal section-25 UTF8-CONT and RFC2279 widths are preserved as bytes,
    // not decoded to Unicode strings or used for any routing/security decision.
    for valid in [
        b"%20\t".as_slice(),
        b"\x80",
        b"\xc2\xa9",
        b"\xe0\xa0\x80",
        b"\xf0\x90\x80\x80",
        b"\xf8\x88\x80\x80\x80",
        b"\xfc\x84\x80\x80\x80\x80",
    ] {
        let mut wire = b"SIP/2.0 200 ".to_vec();
        wire.extend_from_slice(valid);
        wire.extend_from_slice(b"\r\nl:0\r\n\r\n");
        let message = parser.parse(&wire, Transport::Udp).unwrap();
        assert!(matches!(message.start_line(),StartLine::Response{reason,..} if reason==valid));
    }
}

#[test]
fn incomplete_lines_implicit_body_limits_and_short_reason_octets_are_checked() {
    let mut parser = Parser::default();
    let mut wire = b"SIP/2.0 200 OK\r\n\r\n".to_vec();
    wire.resize(wire.len() + 16385, b'x');
    assert_eq!(
        parser.parse(&wire, Transport::Udp).err(),
        Some(ParseError::Limit)
    );
    for transport in [Transport::Udp, Transport::Tcp] {
        assert_eq!(
            parser.parse(&[b'x'; 16384], transport).err(),
            Some(ParseError::Limit)
        );
        assert_eq!(
            parser.parse(b"SIP/2.0 200 OK\r", transport).err(),
            Some(if transport == Transport::Udp {
                ParseError::Truncated
            } else {
                ParseError::Incomplete
            })
        );
        for wire in [
            b" sip:a SIP/2.0\r\n\r\n".as_slice(),
            b"SIP/2.0 200 \xc2\r\nl:0\r\n\r\n",
        ] {
            assert_eq!(
                parser.parse(wire, transport).err(),
                Some(ParseError::Syntax)
            );
        }
    }
}
