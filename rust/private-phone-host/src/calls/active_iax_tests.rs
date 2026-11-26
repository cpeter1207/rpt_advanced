//! Exercise one real IAX registrar, provider-owned executor and connected G.711 Echo leg.
use super::{
    PrivateCall, Transport, abi,
    source_capture::{self, Audio},
};
use crate::{
    Error,
    phone::{AccountRow, PreparedAccount, Protocol},
    selection::{NativePaths, PhoneHost},
};
use std::{
    ffi::c_void,
    io::{BufRead, BufReader},
    mem::size_of,
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

struct Pbx(Child);
impl Drop for Pbx {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}
struct Turn {
    audio: Audio,
    next: Instant,
    encoded: [u8; 160],
    pending: Option<(usize, u32, u32)>,
    latest: Option<(u32, u32, u64, u64, u64, SocketAddr)>,
    error: bool,
}
struct Echo {
    call: PrivateCall,
    turn: Mutex<Turn>,
}
impl Echo {
    fn step(&self) -> Result<(), Error> {
        let mut turn = self.turn.lock().unwrap();
        let Turn {
            audio,
            next,
            encoded,
            pending,
            latest,
            ..
        } = &mut *turn;
        if pending.is_none() && Instant::now() >= *next {
            let pcm: [f32; 160] = std::array::from_fn(|i| if i % 32 < 16 { 0.25 } else { -0.25 });
            let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
            result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
            if self.call.prepare(&pcm, encoded, 0, &mut result)? != 0
                || result.consumed_frames != 160
                || result.packet_frames != 160
                || result.encoded_bytes != 160
            {
                return Err(Error::Provider);
            }
            *pending = Some((
                result.encoded_bytes,
                result.packet_frames,
                self.call.elapsed_ms()?,
            ));
        }
        if let Some((count, frames, stamp)) = *pending {
            match self
                .call
                .send(&encoded[..count], frames, stamp, stamp.saturating_add(200))?
            {
                0 => {
                    *pending = None;
                    *next += Duration::from_millis(20);
                }
                1 => {}
                _ => return Err(Error::Provider),
            }
        }
        source_capture::read(&self.call, audio)?;
        let Transport::Iax(call) = &self.call.0 else {
            return Err(Error::Provider);
        };
        let (o, epoch, local) = call.test_playout()?;
        *latest = Some((
            o.jitter_delay_samples,
            o.jitter_queued_packets,
            o.jitter_limit_resets,
            o.jitter_overload_drops,
            epoch,
            local,
        ));
        self.call.set_deadline(2_000_000)
    }
}
unsafe extern "C" fn echo_step(context: *mut c_void) -> i32 {
    let echo = unsafe { &*context.cast::<Echo>() };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| echo.step()))
        .is_ok_and(|s| s.is_ok())
    {
        0
    } else {
        echo.turn.lock().unwrap_or_else(|e| e.into_inner()).error = true;
        1
    }
}
fn wait_for(echo: &Echo, condition: impl Fn(&Turn) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let turn = echo.turn.lock().unwrap();
        assert!(
            !turn.error,
            "native callback failed before policy confirmation"
        );
        if condition(&turn) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native echo did not progress: {} frames, latest {:?}",
            turn.audio.qualified,
            turn.latest
        );
        drop(turn);
        thread::sleep(Duration::from_millis(2));
    }
}
fn drive(host: &PhoneHost, token: usize, reverse: bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !host.drive(token, reverse).unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn registered_iax_account_applies_and_reverses_policy_on_its_attached_echo_call() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/iax_pbx.py");
    let mut pbx = Pbx(Command::new("python3")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap());
    let mut address = String::new();
    BufReader::new(pbx.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let port = address
        .trim()
        .parse::<u16>()
        .expect("private Asterisk reported its bound UDP port");
    let path =
        |var, default| std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from);
    let host = PhoneHost::new(
        NativePaths {
            sip: "/absent/sip".into(),
            iax: path("RPTADV_TEST_PRIVATE_IAX", "librptadviax2.so.2"),
            g711: path("RPTADV_TEST_PRIVATE_G711", "librptadv_codec_g711.so.1"),
            jitter: path(
                "RPTADV_JITTER_TEST_PROVIDER",
                "librptadv_jitter_adapter.so.1",
            ),
        },
        Arc::new(|_| panic!("only the requested outgoing Echo call is admissible")),
    );
    let row = |jitter_ms| {
        PreparedAccount::copy(AccountRow {
            port_id: [1; 16],
            account_id: [2; 16],
            generation: 17,
            protocol: Protocol::Iax2,
            server: "127.0.0.1",
            port,
            username: "club-account",
            credential: b"fixture-only",
            jitter_ms,
        })
        .unwrap()
    };
    let initial = host.prepare(vec![row([30, 85, 140])]).unwrap();
    drive(&host, initial, false);
    host.release(initial).unwrap();
    assert_eq!(host.observe(&[1; 16], &[2; 16], 17).unwrap(), 2);
    let mut attempt = host.begin_dial([1; 16], [2; 16], 17, "999").unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    let call = loop {
        if let Some(call) = attempt.poll(&host).expect("actual authenticated Echo leg") {
            break call;
        }
        assert!(
            Instant::now() < deadline,
            "actual authenticated IAX call timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(attempt);
    let generation = call.generations();
    assert_eq!(generation.0, 17);
    assert_eq!(call.source(), SocketAddr::from(([127, 0, 0, 1], port)));
    let echo = Box::new(Echo {
        call,
        turn: Mutex::new(Turn {
            audio: Audio::default(),
            next: Instant::now(),
            encoded: [0; 160],
            pending: None,
            latest: None,
            error: false,
        }),
    });
    // Registration joins the callback while its exact Box and peer remain reachable.
    let registration = unsafe {
        echo.call.attach(
            echo_step,
            std::ptr::from_ref(echo.as_ref()).cast_mut().cast(),
        )
    }
    .unwrap();
    wait_for(&echo, |s| {
        s.audio.qualified >= 320 && s.latest.is_some_and(|o| o.0 == 680 && o.1 >= 2)
    });
    let (before, base) = {
        let s = echo.turn.lock().unwrap();
        (s.latest.unwrap(), s.audio.qualified)
    };
    eprintln!(
        "actual private IAX generation {:?} native epoch {} local {} delay {} queued {} qualified {}",
        generation, before.4, before.5, before.0, before.1, base
    );
    let change = host
        .prepare(vec![row([10, 20, 40])])
        .expect("retained actual IAX account must admit a live policy-only edit");
    drive(&host, change, false);
    wait_for(&echo, |s| {
        s.audio.qualified >= base + 1600 && s.latest.is_some_and(|o| o.0 <= 320)
    });
    let forward = {
        let s = echo.turn.lock().unwrap();
        (s.latest.unwrap(), s.audio.qualified)
    };
    assert_eq!(
        (forward.0.2, forward.0.3, forward.0.4, forward.0.5),
        (before.2, before.3, before.4, before.5)
    );
    drive(&host, change, true);
    host.release(change).unwrap();
    wait_for(&echo, |s| s.audio.qualified >= forward.1 + 640);
    assert_eq!(echo.call.generations(), generation);
    assert_eq!(host.observe(&[1; 16], &[2; 16], 17).unwrap(), 2);
    let s = echo.turn.lock().unwrap();
    let restored = s.latest.unwrap();
    assert_eq!(
        (restored.2, restored.3, restored.4, restored.5),
        (before.2, before.3, before.4, before.5)
    );
    eprintln!(
        "actual private IAX live delay {} restored epoch {} original frames {}",
        forward.0.0, restored.4, s.audio.qualified
    );
    drop(s);
    registration.request_stop();
    drop(registration);
    drop(echo);
    let remove = host.prepare(Vec::new()).unwrap();
    drive(&host, remove, false);
    host.release(remove).unwrap();
    assert!(host.observe(&[1; 16], &[2; 16], 17).is_err());
    let deadline = Instant::now() + Duration::from_secs(4);
    while !host.stop().unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn authenticated_incoming_iax_source_delivers_native_digits_and_playback() {
    use std::io::Write;
    use std::sync::mpsc;

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/iax_pbx.py");
    let mut pbx = Pbx(Command::new("python3")
        .arg(script)
        .arg("--managed-call")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap());
    let mut replies = BufReader::new(pbx.0.stdout.take().unwrap());
    let mut line = String::new();
    assert_ne!(replies.read_line(&mut line).unwrap(), 0);
    let port: u16 = line.trim().parse().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let sdk = |name| PathBuf::from(std::env::var_os(name).expect("qualified native SDK path"));
    let host = PhoneHost::new(
        NativePaths {
            sip: "/absent/sip".into(),
            iax: sdk("RPTADV_TEST_PRIVATE_IAX"),
            g711: sdk("RPTADV_TEST_PRIVATE_G711"),
            jitter: sdk("RPTADV_JITTER_TEST_PROVIDER"),
        },
        Arc::new(move |call| {
            assert!(
                send.try_send(call).is_ok(),
                "one authenticated incoming call"
            );
        }),
    );
    let row = PreparedAccount::copy(AccountRow {
        port_id: [1; 16],
        account_id: [2; 16],
        generation: 17,
        protocol: Protocol::Iax2,
        server: "127.0.0.1",
        port,
        username: "club-account",
        credential: b"fixture-only",
        jitter_ms: [30, 85, 140],
    })
    .unwrap();
    let token = host.prepare(vec![row]).unwrap();
    drive(&host, token, false);
    host.release(token).unwrap();
    assert_eq!(host.observe(&[1; 16], &[2; 16], 17).unwrap(), 2);

    let mut channel = None;
    let mut pbx_step = |step: &str| {
        let input = pbx.0.stdin.as_mut().unwrap();
        writeln!(input, "{step}").unwrap();
        input.flush().unwrap();
        line.clear();
        assert_ne!(replies.read_line(&mut line).unwrap(), 0);
        let (completed, name) = line.trim_end().split_once(' ').unwrap();
        assert_eq!(completed, step);
        assert!(name.starts_with("IAX2/club-account-"));
        if let Some(original) = &channel {
            assert_eq!(name, original);
        } else {
            channel = Some(name.to_owned());
        }
    };
    pbx_step("dial");
    let call = receive
        .recv_timeout(Duration::from_secs(6))
        .expect("authenticated incoming call");
    assert_eq!(call.generations().0, 17);
    assert_eq!(call.source(), SocketAddr::from(([127, 0, 0, 1], port)));
    let echo = Box::new(Echo {
        call,
        turn: Mutex::new(Turn {
            audio: Audio::default(),
            next: Instant::now(),
            encoded: [0; 160],
            pending: None,
            latest: None,
            error: false,
        }),
    });
    let registration = unsafe {
        echo.call.attach(
            echo_step,
            std::ptr::from_ref(echo.as_ref()).cast_mut().cast(),
        )
    }
    .unwrap();
    wait_for(&echo, |turn| turn.latest.is_some());
    pbx_step("wrong");
    pbx_step("beforeauth");
    let deadline = Instant::now() + Duration::from_secs(3);
    let (failure, digits, frames) = loop {
        let turn = echo.turn.lock().unwrap();
        let evidence = (turn.error, turn.audio.digits, turn.audio.qualified);
        if evidence.0 || (evidence.1 >= 9 && evidence.2 > 0) || Instant::now() >= deadline {
            break evidence;
        }
        drop(turn);
        thread::sleep(Duration::from_millis(2));
    };
    registration.request_stop();
    drop(registration);
    drop(echo);
    let remove = host.prepare(Vec::new()).unwrap();
    drive(&host, remove, false);
    host.release(remove).unwrap();
    eprintln!(
        "actual IAX incoming source terminal={failure} delivered-digit-count={digits} qualified-frames={frames}"
    );
    assert!(
        !failure && digits >= 9 && frames > 0,
        "authenticated native source lost SendDTMF or playback"
    );
}

/// An unanswered authenticated NEW must not block a second dial on the same account.
#[test]
fn registered_iax_second_new_completes_while_first_is_unanswered() {
    use std::io::Write;

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/iax_pbx.py");
    let mut pbx = Pbx(Command::new("python3")
        .arg(script)
        .arg("--concurrent-outgoing")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap());
    let mut replies = BufReader::new(pbx.0.stdout.take().unwrap());
    let mut line = String::new();
    assert_ne!(replies.read_line(&mut line).unwrap(), 0);
    let port: u16 = line.trim().parse().unwrap();
    let sdk = |name| PathBuf::from(std::env::var_os(name).expect("qualified native SDK path"));
    let host = PhoneHost::new(
        NativePaths {
            sip: "/absent/sip".into(),
            iax: sdk("RPTADV_TEST_PRIVATE_IAX"),
            g711: sdk("RPTADV_TEST_PRIVATE_G711"),
            jitter: sdk("RPTADV_JITTER_TEST_PROVIDER"),
        },
        Arc::new(|_| panic!("only the requested outgoing IAX legs are admissible")),
    );
    let row = PreparedAccount::copy(AccountRow {
        port_id: [1; 16],
        account_id: [2; 16],
        generation: 17,
        protocol: Protocol::Iax2,
        server: "127.0.0.1",
        port,
        username: "club-account",
        credential: b"fixture-only",
        jitter_ms: [30, 85, 140],
    })
    .unwrap();
    let token = host.prepare(vec![row]).unwrap();
    drive(&host, token, false);
    host.release(token).unwrap();
    assert_eq!(host.observe(&[1; 16], &[2; 16], 17).unwrap(), 2);

    let input = pbx.0.stdin.as_mut().unwrap();
    let mut pbx_command = |command: &str| {
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
        line.clear();
        assert_ne!(replies.read_line(&mut line).unwrap(), 0);
        line.trim_end().to_owned()
    };
    let mut first = host.begin_dial([1; 16], [2; 16], 17, "998").unwrap();
    let start = Instant::now();
    let held = loop {
        assert!(first.poll(&host).unwrap().is_none(), "held leg answered");
        let reply = pbx_command("held");
        if reply != "-" {
            break reply;
        }
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "PBX never ran the held extension"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(held.starts_with("IAX2/"));
    assert!(start.elapsed() < Duration::from_secs(4));
    let mut second = host.begin_dial([1; 16], [2; 16], 17, "999").unwrap();
    let limit = Instant::now() + Duration::from_secs(4);
    let mut echo_call = loop {
        assert!(first.poll(&host).unwrap().is_none(), "held leg answered");
        if let Some(call) = second.poll(&host).unwrap() {
            break Some(call);
        }
        if Instant::now() >= limit {
            break None;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        pbx_command("held"),
        held,
        "first authenticated call must still be held"
    );
    let second_while_first_pending = echo_call.is_some();
    drop(first);
    let first_ended_by_cancel = pbx_command("wait-held-ended") == "ended";
    // On the serial baseline the second leg can finish after the first provider timeout.
    // Keep the media and withdrawal witnesses on both sides of the final assertion.
    let limit = Instant::now() + Duration::from_secs(12);
    let call = loop {
        if let Some(call) = echo_call.take() {
            break call;
        }
        echo_call = second
            .poll(&host)
            .expect("second Echo dial reaches a terminal success");
        assert!(
            Instant::now() < limit,
            "second Echo dial did not complete after cancellation"
        );
        thread::sleep(Duration::from_millis(5));
    };
    drop(second);
    assert_eq!(call.generations().0, 17);
    assert_eq!(call.source(), SocketAddr::from(([127, 0, 0, 1], port)));
    let echo = Box::new(Echo {
        call,
        turn: Mutex::new(Turn {
            audio: Audio::default(),
            next: Instant::now(),
            encoded: [0; 160],
            pending: None,
            latest: None,
            error: false,
        }),
    });
    let registration = unsafe {
        echo.call.attach(
            echo_step,
            std::ptr::from_ref(echo.as_ref()).cast_mut().cast(),
        )
    }
    .unwrap();
    wait_for(&echo, |turn| turn.audio.qualified >= 320);
    let qualified_frames = echo.turn.lock().unwrap().audio.qualified;
    registration.request_stop();
    drop(registration);
    drop(echo);
    if !first_ended_by_cancel {
        assert_eq!(pbx_command("cleanup-held"), "ended");
    }
    assert_eq!(pbx_command("wait-all-ended"), "ended");
    let remove = host.prepare(Vec::new()).unwrap();
    drive(&host, remove, false);
    host.release(remove).unwrap();
    assert!(host.observe(&[1; 16], &[2; 16], 17).is_err());
    let limit = Instant::now() + Duration::from_secs(4);
    while !host.stop().unwrap() {
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(2));
    }
    drop(replies);
    drop(pbx.0.stdin.take());
    assert!(pbx.0.wait().unwrap().success());
    eprintln!(
        "IAX_TWO_NEW_CLOSED first_held=true second_connected=true second_while_first_pending={second_while_first_pending} first_ended_by_cancel={first_ended_by_cancel} g711_frames={qualified_frames} withdrawn=true account_joined=true pbx_joined=true"
    );
    assert!(
        second_while_first_pending && first_ended_by_cancel,
        "second IAX NEW must complete while the first waits, and cancellation must retire only the first"
    );
}
