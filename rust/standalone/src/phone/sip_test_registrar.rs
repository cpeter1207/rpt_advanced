//! Bounded synthetic UDP registrar shared by controller integration fixtures.
use std::{net::UdpSocket, time::Instant};

pub(crate) fn header<'a>(packet: &'a str, name: &str) -> &'a str {
    packet
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| {
                    key.eq_ignore_ascii_case(name)
                        || matches!(
                            (name, *key),
                            ("Via", "v")
                                | ("From", "f")
                                | ("To", "t")
                                | ("Call-ID", "i")
                                | ("Contact", "m")
                        )
                })
                .map(|(_, value)| value.trim())
        })
        .unwrap_or_else(|| {
            panic!(
                "missing {name} in fixture packet {}",
                packet.lines().next().unwrap_or("empty")
            )
        })
}
pub(crate) fn response(packet: &str, code: u16, challenge: bool) -> String {
    let mut reply = format!(
        "SIP/2.0 {code} Fixture\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=fixture\r\nCall-ID: {}\r\nCSeq: {}\r\n",
        header(packet, "Via"),
        header(packet, "From"),
        header(packet, "To"),
        header(packet, "Call-ID"),
        header(packet, "CSeq")
    );
    if challenge {
        reply.push_str("WWW-Authenticate: Digest realm=\"fixture\", nonce=\"fixture-nonce\", algorithm=MD5\r\n");
    } else if packet.starts_with("REGISTER ") {
        let contact = header(packet, "Contact")
            .split(';')
            .filter(|part| !part.starts_with("expires="))
            .collect::<Vec<_>>()
            .join(";");
        reply.push_str(&format!(
            "Contact: {contact};expires={}\r\nExpires: {}\r\n",
            header(packet, "Expires"),
            header(packet, "Expires")
        ));
    }
    reply.push_str("Content-Length: 0\r\n\r\n");
    reply
}

pub(crate) fn receive_register(
    socket: &UdpSocket,
    bytes: &mut [u8],
) -> std::io::Result<(String, std::net::SocketAddr)> {
    let deadline = Instant::now() + socket.read_timeout()?.expect("bounded fixture receive");
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "fixture receive deadline")
            })?;
        socket.set_read_timeout(Some(remaining))?;
        let (count, remote) = socket.recv_from(bytes)?;
        let packet = std::str::from_utf8(&bytes[..count]).unwrap();
        if packet.starts_with("OPTIONS ") {
            socket.send_to(response(packet, 200, false).as_bytes(), remote)?;
        } else {
            return Ok((packet.to_owned(), remote));
        }
    }
}
