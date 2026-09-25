use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::{audio::LinkAudioProducer, link::PeerInput};
use std::{collections::VecDeque, ffi::c_void, ptr, sync::Mutex};

pub(crate) enum Input {
    Text(&'static [u8]),
    Digit(u8),
    Audio(Vec<f32>),
}
#[derive(Default)]
pub(crate) struct State {
    pub(crate) rate: u32,
    pub(crate) input: VecDeque<Input>,
    pub(crate) texts: Vec<Vec<u8>>,
    pub(crate) digits: Vec<u8>,
    pub(crate) audio: Vec<f32>,
    pub(crate) writes: Vec<usize>,
    pub(crate) fail_text: bool,
    pub(crate) fail_read: bool,
    pub(crate) fail_ready: bool,
    pub(crate) fail_write: bool,
    pub(crate) pause: Option<Arc<std::sync::Barrier>>,
    pub(crate) drops: usize,
}
type Shared = Arc<Mutex<State>>;

unsafe fn state<'a>(peer: *const c_void) -> &'a Shared {
    unsafe { &*peer.cast::<Shared>() }
}
unsafe extern "C" fn rate(_: *mut c_void, peer: *const c_void) -> u32 {
    unsafe { state(peer) }.lock().unwrap().rate
}
unsafe extern "C" fn ready(_: *mut c_void, peer: *mut c_void) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    let pause = state.pause.take();
    let result = if state.fail_ready {
        -1
    } else {
        i32::from(!state.input.is_empty())
    };
    drop(state);
    if let Some(pause) = pause {
        pause.wait();
        pause.wait();
    }
    result
}
unsafe extern "C" fn read(
    _: *mut c_void,
    peer: *mut c_void,
    event: abi::rptadv_peer_event_v1,
    context: *mut c_void,
) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    if state.fail_read {
        return -1;
    }
    if let Some(input) = state.input.pop_front() {
        unsafe {
            match input {
                Input::Text(bytes) => {
                    event.unwrap()(context, 1, bytes.as_ptr().cast(), bytes.len())
                }
                Input::Digit(digit) => event.unwrap()(context, 2, ptr::from_ref(&digit).cast(), 1),
                Input::Audio(samples) => {
                    event.unwrap()(context, 3, samples.as_ptr().cast(), samples.len())
                }
            }
        }
    }
    0
}
unsafe extern "C" fn text(
    _: *mut c_void,
    peer: *mut c_void,
    bytes: *const std::ffi::c_char,
    count: usize,
) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    state
        .texts
        .push(unsafe { std::slice::from_raw_parts(bytes.cast(), count) }.to_vec());
    if state.fail_text { -1 } else { 0 }
}
unsafe extern "C" fn digit(_: *mut c_void, peer: *mut c_void, digit: u8) -> i32 {
    unsafe { state(peer) }.lock().unwrap().digits.push(digit);
    0
}
unsafe extern "C" fn write(
    _: *mut c_void,
    peer: *mut c_void,
    samples: *const f32,
    count: usize,
) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    if state.fail_write {
        return -1;
    }
    state.writes.push(count);
    state
        .audio
        .extend_from_slice(unsafe { std::slice::from_raw_parts(samples, count) });
    0
}

#[test]
fn audio_burst_ends_with_exactly_one_idle_marker() {
    let (mut session, _control, state, mut output) = make_session("1000");
    assert_eq!(output.write(&[0.25; 960]), 0);

    session.step(20).unwrap();
    session.step(40).unwrap();
    session.step(60).unwrap();

    let writes = state.lock().unwrap().writes.clone();
    assert_eq!(writes, [960, 0]);
}
unsafe extern "C" fn destroy(_: *mut c_void, peer: *mut c_void) {
    let state = unsafe { Box::from_raw(peer.cast::<Shared>()) };
    state.lock().unwrap().drops += 1;
}
pub(crate) fn peer(rate_hz: u32) -> (PeerIo, Shared) {
    let state = Arc::new(Mutex::new(State {
        rate: rate_hz,
        ..State::default()
    }));
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_rate = Some(rate);
    table.peer_ready = Some(ready);
    table.peer_read = Some(read);
    table.peer_send_text = Some(text);
    table.peer_send_digit = Some(digit);
    table.peer_write = Some(write);
    table.peer_destroy = Some(destroy);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let handle = Box::into_raw(Box::new(state.clone())).cast();
    (unsafe { services.peer(handle) }.unwrap(), state)
}
fn make_session(local: &str) -> (PeerSession, PeerControl, Shared, LinkAudioProducer) {
    let (io, state) = peer(48000);
    let (output, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(9600)
        .unwrap()
        .into_endpoints();
    let (session, control) = PeerSession::prepare(io, outbound, local, "2000").unwrap();
    (session, control, state, output)
}

#[test]
fn serial_reader_routes_protocol_digits_audio_and_exact_redirect_ack() {
    let (mut session, mut control, state, mut output) = make_session("1000");
    assert_eq!(state.lock().unwrap().texts, [b"!NEWKEY1!".to_vec()]);
    for bytes in [
        &b"bad"[..],
        b"!NEWKEY1!",
        b"!IAXKEY!",
        b"L T3000",
        b"K? * 3000 0 0",
    ] {
        state.lock().unwrap().input.push_back(Input::Text(bytes));
        session.step(0).unwrap();
    }
    assert!(
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"!IAXKEY! 1 1 0 0")
    );
    assert!(matches!(control.event(), Some(Event::Text(text)) if text == b"L T3000"));
    assert!(matches!(control.event(), Some(Event::Text(text)) if text == b"K? * 3000 0 0"));
    assert!(control.event().is_none());
    control.send(Command::Digit('5')).ok().unwrap();
    control
        .send(Command::Text {
            text: c"L 0".into(),
            advisory: false,
        })
        .ok()
        .unwrap();
    assert_eq!(output.write(&[0.25; 960]), 0);
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 4000]));
    session.step(20).unwrap();
    assert_eq!(state.lock().unwrap().digits, [b'5']);
    assert_eq!(state.lock().unwrap().audio, vec![0.25; 960]);
    assert!(control.snapshot().unwrap().ring.available_samples > 0);

    state.lock().unwrap().input.push_back(Input::Digit(b'6'));
    session.step(21).unwrap();
    assert!(matches!(control.event(), Some(Event::Digit('6'))));
    session.step(3021).unwrap();
    assert!(matches!(control.event(), Some(Event::Digit('#'))));
    let old = control.observer.clone();
    let (inbound, input) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    let observer = inbound.observer();
    let (_, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960)
        .unwrap()
        .into_endpoints();
    control
        .send(Command::Redirect { inbound, outbound })
        .ok()
        .unwrap();
    session.step(3022).unwrap();
    assert!(old.signals().ended());
    assert!(
        matches!(control.event(), Some(Event::Redirected(ack)) if ack.same_generation(&observer))
    );
    assert!(!control.snapshot().unwrap().ended);
    assert!(!input.signals().ended());
    control.stop();
    assert_eq!(session.step(3023), Err(Error::Hangup));
    assert!(input.signals().ended());
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn keyed_source_accepts_only_current_active_query_and_retains_last_selection() {
    let (mut session, mut control, state, _) = make_session("1000");
    session.text(b"K 1000 3000 1 0".to_vec()).unwrap();
    session.inbound.signals().set_active(true);
    // A response before the reader observes the new activity epoch is advisory only.
    session.text(b"K 1000 3000 1 0".to_vec()).unwrap();
    session.step(10).unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"K? * 1000 0 0")
    );
    session.text(b"K 9999 3000 1 0".to_vec()).unwrap();
    session.text(b"K 1000 3000 1 0".to_vec()).unwrap();
    session.text(b"K 1000 4000 1 0".to_vec()).unwrap();
    assert_eq!(
        control.snapshot().unwrap().selected_source.as_deref(),
        Some("3000")
    );
    session.inbound.signals().set_active(false);
    session.step(11).unwrap();
    assert_eq!(
        control.snapshot().unwrap().selected_source.as_deref(),
        Some("3000")
    );
    while control.event().is_some() {}

    state.lock().unwrap().fail_text = true;
    session.inbound.signals().set_active(true);
    session.step(12).unwrap();
    assert!(session.query_epoch.is_none());
    session.text(b"K 1000 4000 1 0".to_vec()).unwrap();
    assert!(control.snapshot().unwrap().selected_source.is_none());
    let (mut local, _local_control, local_state, _) = make_session("usb/test");
    local.inbound.signals().set_active(true);
    local.step(0).unwrap();
    assert_eq!(local_state.lock().unwrap().texts.len(), 1);
}

#[test]
fn fatal_io_and_full_events_end_ingress_but_advisory_failure_does_not() {
    for failure in ["ready", "read", "write", "text", "disconnect", "events"] {
        let (mut session, mut control, state, mut output) = make_session("1000");
        match failure {
            "ready" => state.lock().unwrap().fail_ready = true,
            "read" => {
                let mut state = state.lock().unwrap();
                state.fail_read = true;
                state.input.push_back(Input::Digit(b'1'));
            }
            "write" => {
                state.lock().unwrap().fail_write = true;
                assert_eq!(output.write(&[0.1; 960]), 0);
            }
            "text" => {
                state.lock().unwrap().fail_text = true;
                control
                    .send(Command::Text {
                        text: c"K? * 1000 0 0".into(),
                        advisory: true,
                    })
                    .ok()
                    .unwrap();
                session.step(0).unwrap();
                control
                    .send(Command::Text {
                        text: c"L 0".into(),
                        advisory: false,
                    })
                    .ok()
                    .unwrap();
            }
            "disconnect" => state
                .lock()
                .unwrap()
                .input
                .push_back(Input::Text(b"!!DISCONNECT!!")),
            "events" => {
                for _ in 0..64 {
                    session.event(Event::Digit('1')).unwrap();
                }
                state.lock().unwrap().input.push_back(Input::Digit(b'2'));
            }
            _ => unreachable!(),
        }
        assert!(session.step(20).is_err(), "{failure}");
        assert!(control.snapshot().unwrap().ended, "{failure}");
    }
    let (mut session, mut control, _, _) = make_session("1000");
    for _ in 0..64 {
        control.send(Command::Digit('1')).ok().unwrap();
    }
    assert!(matches!(
        control.send(Command::Digit('2')),
        Err(Command::Digit('2'))
    ));
    session.step(0).unwrap();
    let reader = session.start().unwrap();
    assert!(!reader.ended());
    control.stop();
    reader.join();
}

#[test]
fn preparation_rejects_invalid_identity_rate_and_negotiation_without_leaking_peer() {
    for (local, remote, rate, fail) in [
        ("1000", "invalid", 48000, false),
        ("", "2000", 48000, false),
        ("a\0b", "2000", 48000, false),
        ("1000", "2000", 0, false),
        ("1000", "2000", 48000, true),
    ] {
        let (io, state) = peer(rate);
        state.lock().unwrap().fail_text = fail;
        let (_, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960)
            .unwrap()
            .into_endpoints();
        assert!(PeerSession::prepare(io, outbound, local, remote).is_err());
        assert_eq!(state.lock().unwrap().drops, 1);
    }
}

#[test]
fn unkey_between_query_capture_and_send_cancels_the_advisory_request() {
    let (mut session, _control, state, _) = make_session("1000");
    let edge = session.inbound.signals().set_active(true);
    let epoch = session.peer.activity(true, 0).unwrap();
    session.inbound.signals().set_active(false);
    session.query(edge, epoch).unwrap();
    assert!(session.query_epoch.is_none());
    assert_eq!(state.lock().unwrap().texts, [b"!NEWKEY1!".to_vec()]);
}

#[test]
fn sub_codec_frame_does_not_send_empty_conversion_output() {
    let (io, state) = peer(8000);
    let (mut output, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960)
        .unwrap()
        .into_endpoints();
    let (mut session, _control) = PeerSession::prepare(io, outbound, "1000", "2000").unwrap();
    assert_eq!(output.write(&[0.5]), 0);
    session.step(0).unwrap();
    // SRC_LINEAR emits its initial interpolation position immediately; the next
    // source sample cannot advance another 48-to-8 kHz output position.
    let writes = state.lock().unwrap().writes.clone();
    assert_eq!(writes, [1]);
    assert_eq!(output.write(&[0.5]), 0);
    session.step(20).unwrap();
    let writes = state.lock().unwrap().writes.clone();
    assert_eq!(writes, [1]);
}
