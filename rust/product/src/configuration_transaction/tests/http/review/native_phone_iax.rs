//! This opt-in product fixture uses Asterisk as the remote dynamic IAX peer.
use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

struct Caller {
    pbx: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    port: u16,
    channel: Option<String>,
}

impl Caller {
    fn start() -> Self {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../private-phone-host/tests/support/iax_pbx.py");
        let mut pbx = Command::new("python3")
            .arg(script)
            .arg("--managed-call")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = pbx.stdin.take().unwrap();
        let mut output = BufReader::new(pbx.stdout.take().unwrap());
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        let port = line.trim().parse().expect("private IAX registrar bound a UDP port");
        Self { pbx, input: Some(input), output, port, channel: None }
    }

    fn step(&mut self, name: &str) {
        self.step_observed(name, || {});
    }

    fn step_observed(&mut self, name: &str, after_query: impl FnOnce()) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{name}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        let length = self.output.read_line(&mut line).unwrap();
        after_query();
        assert_ne!(length, 0, "Asterisk ended before {name}");
        let (completed, channel) = line.trim_end().split_once(' ').unwrap();
        assert_eq!(completed, name);
        assert!(channel.starts_with("IAX2/club-account-"));
        if let Some(original) = &self.channel {
            assert_eq!(original, channel);
        } else {
            self.channel = Some(channel.to_owned());
        }
    }
}

impl Drop for Caller {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.pbx.wait();
    }
}

// Called from native_phone_positive::positive_phone immediately after the shared mTLS
// port, station observation and protected mailbox are provisioned in this child.
pub(super) fn positive_iax(
    client: &Client<'_>,
    path: &str,
    mailbox: &str,
    owners: &super::phone_owner::Owners,
    output: &fixture::ReviewOutput,
) {
    let mut caller = Caller::start();
    edit(client, "POST", &format!("{path}protocols/phone-patch/"), json!({"configuration": {
        "provider": {"protocol": "iax2", "server": "127.0.0.1", "port": caller.port, "username": "club-account"},
        "credentials": {"password": "fixture-only"}, "vad": {"enabled": false}
    }}));
    put(client, path, json!({"name":"Call","inherit_from":"site","enabled":true}));
    let observe=|step:&str|{
        owners.trace();
        let counters=[
            owners.read_count.load(Ordering::Acquire),
            fixture::PHONE_EVENT_DIGITS.load(Ordering::Acquire),
            fixture::PHONE_COMMAND_DIGITS.load(Ordering::Acquire),
            fixture::PHONE_MAILBOX_REJECTED.load(Ordering::Acquire),
            fixture::PHONE_MAILBOX_VERIFIED.load(Ordering::Acquire),
            fixture::PHONE_ROUTE_INSTALLS.load(Ordering::Acquire),
            fixture::PHONE_WIRE_ORIGINAL.load(Ordering::Acquire),
            fixture::PHONE_FRESH.load(Ordering::Acquire),
            fixture::PHONE_ACCEPTED.load(Ordering::Acquire),
            output.keyed.load(Ordering::Acquire),output.audible.load(Ordering::Acquire),
        ];
        eprintln!("IAX {step} reads/events/commands/rejected/authenticated/routes/wire/fresh/credited/keyed/audible {counters:?}");
        counters
    };
    caller.step("dial");
    let opened=observe("answered");
    let before=(opened[9],opened[10]);
    caller.step("wrong");
    caller.step("beforeauth");
    let rejected=observe("wrong credential and first playback");
    assert_eq!((rejected[9],rejected[10]),before);
    assert!(rejected[0]>0 && rejected[1]>=9 && rejected[2]>=9 && rejected[3]>0 && rejected[4]==0,
        "wrong credentials must traverse the real native events and scoped verifier");
    caller.step("login");
    caller.step("beforeattach");
    let authenticated=observe("login and second playback");
    assert!(authenticated[4]>rejected[4],"the real mailbox did not authenticate");
    assert_eq!((authenticated[9],authenticated[10]),before);
    caller.step("attach");
    caller.step("joined");
    let attached=observe("attached playback");
    assert!(attached[5]>authenticated[5],"the authenticated mapped attach did not install a radio route");
    assert!(attached[6]>authenticated[6] && attached[8]>0,"actual IAX timed audio never reached the live radio ring");
    assert!(attached[9]>before.0 && attached[10]>before.1);
    let identity = owners.read_identity();
    assert_eq!(identity.source.port(), caller.port);
    assert!(identity.source.ip().is_loopback());
    assert_ne!(identity.clock_epoch, 0);
    caller.step("mute");
    observe("mute completed");
    wait_until(|| output.value.load(Ordering::Acquire) == 0);
    let quiet = (output.keyed.load(Ordering::Acquire), output.audible.load(Ordering::Acquire));
    caller.step("muted");
    observe("muted completed");
    assert_eq!((output.keyed.load(Ordering::Acquire), output.audible.load(Ordering::Acquire)), quiet);
    caller.step("unmute");
    observe("unmute completed");
    caller.step("restored");
    observe("restored completed");
    assert!(output.keyed.load(Ordering::Acquire) > quiet.0);
    assert!(output.audible.load(Ordering::Acquire) > quiet.1);
    assert_eq!(owners.read_identity(), identity);
    let before_delete = owners.retirement();
    assert_eq!(client.edit("DELETE", mailbox, None).code, 202);
    client.ready();
    caller.step_observed("channel-ended", || {
        let after = owners.retirement();
        let changes: [usize; 3] = std::array::from_fn(|i| after[i] - before_delete[i]);
        eprintln!("private PHONE retirement at PBX query return: native stop/joined detach/peer destroy {changes:?}");
    });
    // The remote checked the original channel independently. A delivered HANGUP
    // can precede the local native destructor's return; require both before any
    // account edit or global teardown can release another owner.
    wait_until(|| {
        let after = owners.retirement();
        (0..3).all(|i| after[i] > before_delete[i])
    });
    let after = owners.retirement();
    assert_eq!(std::array::from_fn::<_,3,_>(|i| after[i] - before_delete[i]), [1, 1, 1]);
    eprintln!("private PHONE mailbox withdrawal: original PBX channel gone, native stop/joined detach/peer destroy exactly once");
    wait_until(|| output.value.load(Ordering::Acquire) == 0);
    put(client, path, json!({"name":"Call","inherit_from":"site","enabled":false}));
    eprintln!("actual Asterisk channel {:?}, authenticated IAX owner {:?}, station callback keys {} audible {}",
        caller.channel, identity, output.keyed.load(Ordering::Acquire), output.audible.load(Ordering::Acquire));
}
