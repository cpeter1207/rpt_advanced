//! Observe a configured local command ending one established outgoing SIP dialog.
use super::*;

pub(super) const CODE: &str = "*507";

#[derive(Default)]
pub(super) struct Observation {
    pub(super) mapping_http: u16,
    pub(super) effective: bool,
    operation_http: u16,
    admitted: bool,
    early_bye: bool,
    controller_bye: bool,
}

impl Observation {
    pub(super) fn request(
        &mut self,
        client: &Client<'_>,
        station: &str,
        revision: &Value,
        terminator: &str,
        socket: &UdpSocket,
        invite: &Invite,
    ) -> bool {
        self.early_bye = receive_bye(socket, invite, Duration::from_millis(100));
        if self.early_bye {
            return true;
        }
        let result = operation(
            client,
            json!({"submitted":true,"targets":[station],
            "expected_active_revision":revision,
            "action":{"action":"execute_dtmf","digits":format!("{CODE}{terminator}")}}),
        );
        self.operation_http = result.code;
        let receipt = &result.body["receipt"]["outcomes"][0];
        self.admitted = receipt["target"] == station && receipt["outcome"] == "admitted";
        self.controller_bye = receive_bye(socket, invite, Duration::from_secs(4));
        self.controller_bye
    }

    pub(super) fn verify(&self) {
        eprintln!(
            "RF_HANGUP_CLOSED dial=true ack=true both_pcm=true media_retired=true withdrawal_200=true foreground_joined=true writer_released=true mapping_http={} effective={} operation_http={} admitted={} early_bye={} controller_bye={}",
            self.mapping_http,
            self.effective,
            self.operation_http,
            self.admitted,
            self.early_bye,
            self.controller_bye
        );
        assert!(
            !self.early_bye,
            "established outgoing dialog ended before the local command"
        );
        assert!(
            self.controller_bye,
            "configured local hangup must send BYE on the established outgoing dialog before fixture cleanup"
        );
        assert!(
            self.mapping_http == 202 && self.effective && self.admitted,
            "the configured local hangup must be effective and admitted on its initiating station"
        );
    }
}

fn dialog_tag(header: &str) -> Option<&str> {
    header
        .split(';')
        .find_map(|field| field.trim().strip_prefix("tag="))
}

fn receive_bye(socket: &UdpSocket, invite: &Invite, wait: Duration) -> bool {
    let mut wire = [0; 8192];
    socket.set_read_timeout(Some(wait)).unwrap();
    let (packet, source) = match registrar::receive_register(socket, &mut wire) {
        Ok(received) => received,
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            return false;
        }
        Err(error) => panic!("SIP hangup observation failed: {error}"),
    };
    assert_eq!(source, invite.source);
    assert!(
        packet.starts_with("BYE "),
        "unexpected signaling in established dialog"
    );
    assert_eq!(registrar::header(&packet, "Call-ID"), invite.call_id);
    for (name, expected) in [("From", &invite.from), ("To", &invite.to)] {
        let tag = dialog_tag(expected).expect("established SIP dialog tag");
        assert_eq!(dialog_tag(registrar::header(&packet, name)), Some(tag));
    }
    let sequence = registrar::header(&packet, "CSeq").split_once(' ').unwrap();
    assert_eq!(sequence.1, "BYE");
    assert!(sequence.0.parse::<u32>().unwrap() > invite.sequence.parse::<u32>().unwrap());
    // In-dialog requests already carry both tags. Echo them unchanged.
    let mut response = String::from("SIP/2.0 200 OK\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        response.push_str(&format!("{name}: {}\r\n", registrar::header(&packet, name)));
    }
    response.push_str("Content-Length: 0\r\n\r\n");
    socket.send_to(response.as_bytes(), source).unwrap();
    true
}
