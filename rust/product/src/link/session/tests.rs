use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::{audio::LinkAudioProducer, link::PeerInput};
use std::{collections::VecDeque, ffi::c_void, ptr, sync::Mutex};

pub(crate) enum Input {
    Text(&'static [u8]),
    Digit(u8),
    Audio(Vec<f32>),
    Gap(usize),
    RadioKey,
    RadioUnkey,
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
    pub(crate) ready_without_input: bool,
    pub(crate) fail_write: bool,
    pub(crate) fail_bind: bool,
    pub(crate) bound_radio: Option<String>,
    pub(crate) pause: Option<Arc<std::sync::Barrier>>,
    pub(crate) drops: usize,
    pub(crate) async_attaches: usize,
    pub(crate) async_stops: usize,
    pub(crate) async_detaches: usize,
    pub(crate) fail_async_attach: bool,
    pub(crate) observe_calls: usize,
    pub(crate) fail_observe: bool,
    pub(crate) bad_observe_size: bool,
    pub(crate) bad_observe_version: bool,
}
type Shared = Arc<Mutex<State>>;

unsafe extern "C" fn bind_radio(_: *mut c_void, peer: *mut c_void, radio: *mut c_void) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    assert!(state.texts.is_empty() && state.writes.is_empty());
    if state.fail_bind {
        -1
    } else {
        state.bound_radio = Some(unsafe { crate::fixture::radio_name(radio) });
        0
    }
}

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
        i32::from(state.ready_without_input || !state.input.is_empty())
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
                Input::Gap(samples) => event.unwrap()(context, 6, ptr::null(), samples),
                Input::RadioKey => event.unwrap()(context, 4, ptr::null(), 0),
                Input::RadioUnkey => event.unwrap()(context, 5, ptr::null(), 0),
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
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.25; 960]),
        0
    );

    session.step(20).unwrap();
    session.step(40).unwrap();
    session.step(60).unwrap();

    let writes = state.lock().unwrap().writes.clone();
    assert_eq!(writes, [960, 0]);
}

#[test]
fn outbound_audio_keeps_up_when_driver_wakes_one_millisecond_late() {
    for driver_interval_ms in [10, 11] {
        let (mut session, _control, _state, mut output) = make_session("1000");
        for now_ms in 0..=4_000 {
            if now_ms % 20 == 0 {
                output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.25; 960]);
            }
            if now_ms % driver_interval_ms == 0 {
                session.step(now_ms).unwrap();
            }
        }
        assert_eq!(
            output.dropped_samples(),
            0,
            "continuous 48 kHz audio overflowed with {driver_interval_ms} ms driver steps"
        );
    }
}

#[test]
fn outbound_audio_rebases_after_late_start_and_idle() {
    let (mut session, _control, state, mut output) = make_session("1000");
    assert_eq!(
        output.write(
            rpt_advanced_core::audio::AudioSpan::default(),
            &[0.25; 1920]
        ),
        0
    );
    session.step(5_000).unwrap();
    session.step(5_001).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960]);

    session.step(5_020).unwrap();
    session.step(10_000).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960, 0]);

    assert_eq!(
        output.write(
            rpt_advanced_core::audio::AudioSpan::default(),
            &[0.25; 1920]
        ),
        0
    );
    session.step(10_001).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960, 0]);
    session.step(10_020).unwrap();
    session.step(10_021).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960, 0, 960]);
    session.step(10_040).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960, 0, 960, 960]);
}

#[test]
fn outbound_audio_catches_up_one_block_per_step_after_owner_stall() {
    let (mut session, _control, state, mut output) = make_session("1000");
    assert_eq!(
        output.write(
            rpt_advanced_core::audio::AudioSpan::default(),
            &[0.25; 2880]
        ),
        0
    );
    session.step(0).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960]);

    session.step(100).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960]);
    session.step(101).unwrap();
    assert_eq!(state.lock().unwrap().writes, [960, 960, 960]);
}

#[test]
fn legacy_newkey_is_echoed_once_without_gating_audio() {
    let (mut session, _control, state, mut output) = make_session("1000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY!"));
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.5; 960]),
        0
    );
    session.step(0).unwrap();
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY!"));
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.25; 960]),
        0
    );
    session.step(20).unwrap();

    let state = state.lock().unwrap();
    assert_eq!(state.texts, [b"!NEWKEY1!".to_vec(), b"!NEWKEY!".to_vec()]);
    let mut expected_audio = vec![0.5; 960];
    expected_audio.extend(vec![0.25; 960]);
    assert_eq!(state.audio, expected_audio);
}

#[test]
fn radio_key_control_waits_for_peer_negotiation() {
    let (mut session, _control, state, _) = make_session("1000");
    session.step(0).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(1).unwrap();
    assert!(!session.inbound.signals().radio_keyed());

    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY!"));
    session.step(2).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(3).unwrap();
    assert!(session.inbound.signals().radio_keyed());
}

#[test]
fn radio_key_control_falls_back_after_two_seconds_without_newkey1() {
    let (mut session, _control, state, _) = make_session("1000");
    session.step(0).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(1).unwrap();
    assert!(!session.inbound.signals().radio_keyed());

    session.step(1_999).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(2_000).unwrap();
    assert!(session.inbound.signals().radio_keyed());
}

#[test]
fn newkey1_disables_radio_key_control_but_unkey_is_always_honored() {
    let (mut session, _control, state, _) = make_session("1000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY!"));
    session.step(0).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(1).unwrap();
    assert!(session.inbound.signals().radio_keyed());

    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY1!"));
    session.step(2).unwrap();
    assert!(!session.inbound.signals().radio_keyed());

    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(3).unwrap();
    assert!(!session.inbound.signals().radio_keyed());

    state.lock().unwrap().input.push_back(Input::RadioUnkey);
    session.step(4).unwrap();
    assert!(!session.inbound.signals().radio_keyed());

    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY!"));
    session.step(5).unwrap();
    state.lock().unwrap().input.push_back(Input::RadioKey);
    session.step(6).unwrap();
    assert!(!session.inbound.signals().radio_keyed());
}

#[test]
fn newkey1_does_not_suppress_outbound_audio() {
    let (mut session, _control, state, mut output) = make_session("1000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!NEWKEY1!"));
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.5; 960]),
        0
    );
    session.step(20).unwrap();
    assert_eq!(state.lock().unwrap().audio, vec![0.5; 960]);
}

#[test]
fn remote_control_digit_uses_the_asl_link_text_envelope() {
    let (mut session, mut control, state, _) = make_session("1000");
    control.send(Command::Digit('5')).ok().unwrap();
    control.send(Command::Digit('6')).ok().unwrap();

    session.step(20).unwrap();
    session.step(21).unwrap();

    let (text, digits) = {
        let state = state.lock().unwrap();
        (
            state
                .texts
                .iter()
                .filter(|text| text.starts_with(b"D "))
                .cloned()
                .collect::<Vec<_>>(),
            state.digits.clone(),
        )
    };
    drop(session);
    assert_eq!(
        text,
        [b"D 2000 1000 1 5".to_vec(), b"D 2000 1000 2 6".to_vec()]
    );
    assert!(digits.is_empty());
}

#[test]
fn addressed_asl_remote_dtmf_text_becomes_a_peer_digit_event() {
    let (mut session, mut control, _, _) = make_session("1000");

    session.text(b"D 9999 2000 16 6".to_vec(), 20).unwrap();
    session.text(b"D 1000 3000 17 6".to_vec(), 20).unwrap();
    assert!(control.event().is_none());
    session.text(b"D 1000 2000 17 5".to_vec(), 21).unwrap();

    assert!(matches!(control.event(), Some(Event::Digit('5'))));
    assert!(control.event().is_none());
}

unsafe extern "C" fn destroy(_: *mut c_void, peer: *mut c_void) {
    let state = unsafe { Box::from_raw(peer.cast::<Shared>()) };
    state.lock().unwrap().drops += 1;
}
struct AsyncRegistration {
    state: Shared,
    thread: Option<JoinHandle<i32>>,
}
unsafe extern "C" fn async_attach(
    _: *mut c_void,
    peer: *mut c_void,
    step: abi::rptadv_peer_step_v1,
    step_context: *mut c_void,
    registration: *mut *mut c_void,
) -> i32 {
    let state = unsafe { state(peer) }.clone();
    if state.lock().unwrap().fail_async_attach {
        return -1;
    }
    state.lock().unwrap().async_attaches += 1;
    let address = step_context as usize;
    let thread = thread::spawn(move || unsafe { step.unwrap()(address as *mut c_void) });
    unsafe {
        *registration = Box::into_raw(Box::new(AsyncRegistration {
            state,
            thread: Some(thread),
        }))
        .cast();
    }
    0
}
unsafe extern "C" fn async_stop(_: *mut c_void, registration: *mut c_void) {
    let registration = unsafe { &*registration.cast::<AsyncRegistration>() };
    registration.state.lock().unwrap().async_stops += 1;
}
unsafe extern "C" fn async_detach(_: *mut c_void, registration: *mut c_void) {
    let mut registration = unsafe { Box::from_raw(registration.cast::<AsyncRegistration>()) };
    registration.thread.take().unwrap().join().unwrap();
    registration.state.lock().unwrap().async_detaches += 1;
}
unsafe extern "C" fn observe_peer(
    _: *mut c_void,
    peer: *mut c_void,
    output: *mut abi::rptadv_peer_observation_v1,
) -> i32 {
    let mut state = unsafe { state(peer) }.lock().unwrap();
    state.observe_calls += 1;
    if state.fail_observe {
        return -1;
    }
    unsafe {
        *output = abi::rptadv_peer_observation_v1 {
            struct_size: std::mem::size_of::<abi::rptadv_peer_observation_v1>() as u32,
            abi_version: abi::RPTADV_PEER_OBSERVATION_ABI_VERSION,
            jitter_queued_packets: 3,
            jitter_delay_samples: 480,
            jitter_buffered_span_samples: 640,
            jitter_duplicate_drops: 1,
            jitter_late_drops: 2,
            jitter_full_drops: 3,
            jitter_missing_samples: 160,
            jitter_insertion_samples: 80,
            jitter_skipped_samples: 40,
            jitter_limit_resets: 4,
            jitter_overload_drops: 5,
            jitter_overload_span_samples: 800,
            ingress_overflow_drops: 6,
            ingress_queued_packets: 7,
            ingress_capacity_packets: 64,
        };
        if state.bad_observe_size {
            (*output).struct_size -= 1;
        }
        if state.bad_observe_version {
            (*output).abi_version += 1;
        }
    }
    0
}
pub(crate) fn peer(rate_hz: u32) -> (PeerIo, Shared) {
    peer_with_async(rate_hz, false)
}
pub(crate) fn peer_with_async(rate_hz: u32, async_enabled: bool) -> (PeerIo, Shared) {
    let state = Arc::new(Mutex::new(State {
        rate: rate_hz,
        ..State::default()
    }));
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_rate = Some(rate);
    table.peer_bind_radio = Some(bind_radio);
    table.peer_ready = Some(ready);
    table.peer_read = Some(read);
    table.peer_send_text = Some(text);
    table.peer_send_digit = Some(digit);
    table.peer_write = Some(write);
    table.peer_destroy = Some(destroy);
    if async_enabled {
        table.peer_step_attach = Some(async_attach);
        table.peer_step_stop = Some(async_stop);
        table.peer_step_detach = Some(async_detach);
        table.peer_observe = Some(observe_peer);
    }
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let handle = Box::into_raw(Box::new(state.clone())).cast();
    (unsafe { services.peer(handle) }.unwrap(), state)
}
fn make_async_session(local: &str) -> (PeerSession, PeerControl, Shared, LinkAudioProducer) {
    let (io, state) = peer_with_async(48000, true);
    let (output, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(9600, 64)
        .unwrap()
        .into_endpoints();
    let (session, control) = PeerSession::prepare(io, outbound, local, "2000").unwrap();
    (session, control, state, output)
}
fn make_session(local: &str) -> (PeerSession, PeerControl, Shared, LinkAudioProducer) {
    let (io, state) = peer(48000);
    let (output, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(9600, 64)
        .unwrap()
        .into_endpoints();
    let (session, control) = PeerSession::prepare(io, outbound, local, "2000").unwrap();
    (session, control, state, output)
}

#[test]
fn shared_peer_io_stop_signals_active_session_before_drop() {
    let (session, control, state, _output) = make_session("1000");
    let pause = Arc::new(std::sync::Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    worker.attach(session).ok().unwrap();

    // The real I/O owner has moved this session into its active vector and is
    // inside its first step. Keep it there until shutdown has been published.
    pause.wait();
    let owner = control.owner_thread();
    let stopped_before_shutdown = control.stop.load(Ordering::Acquire);
    worker.stop.store(true, Ordering::Release);
    pause.wait();
    worker.stop();

    assert!(owner.is_some());
    assert!(!stopped_before_shutdown);
    assert!(control.stop.load(Ordering::Acquire));
    assert!(control.snapshot().unwrap().ended);
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn idle_peer_io_worker_does_not_start_a_poll_thread() {
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    assert!(worker.thread.is_none());
    worker.stop();
}

#[test]
fn disconnected_async_peer_is_reaped_before_the_worker_stops() {
    let (session, control, state, _) = make_async_session("1000");
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    worker.attach(session).ok().unwrap();
    control.stop();
    worker.reap();
    assert!(worker.async_peers.is_empty());
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.async_stops, state.async_detaches, state.drops),
            (1, 1, 1)
        );
    }
    worker.stop();
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn async_peer_drains_ready_gap_audio_and_control_in_one_bounded_turn() {
    let (mut session, mut control, state, _) = make_async_session("1000");
    {
        let mut state = state.lock().unwrap();
        state.input.push_back(Input::Gap(80));
        state.input.push_back(Input::Audio(vec![0.25; 160]));
        state.input.push_back(Input::Text(b"L T3000"));
    }
    session.step(0).unwrap();
    assert!(state.lock().unwrap().input.is_empty());
    assert!(matches!(control.event(), Some(Event::Text(bytes)) if bytes == b"L T3000"));
}

#[test]
fn peer_observation_is_copied_for_control_without_a_second_peer_borrow() {
    let (mut session, control, state, _) = make_async_session("1000");
    assert!(control.snapshot().unwrap().peer.is_none());
    session.step(0).unwrap();
    let observation = control.snapshot().unwrap().peer.unwrap();
    assert_eq!(observation.jitter_delay_samples, 480);
    assert_eq!(observation.jitter_missing_samples, 160);
    assert_eq!(observation.jitter_overload_span_samples, 800);
    assert_eq!(observation.ingress_queued_packets, 7);
    assert_eq!(state.lock().unwrap().observe_calls, 1);
    state.lock().unwrap().fail_observe = true;
    session.step(20).unwrap();
    assert_eq!(
        control
            .snapshot()
            .unwrap()
            .peer
            .unwrap()
            .jitter_delay_samples,
        480
    );
    assert_eq!(state.lock().unwrap().observe_calls, 2);
    state.lock().unwrap().fail_observe = false;
    state.lock().unwrap().bad_observe_size = true;
    session.step(40).unwrap();
    assert_eq!(
        control
            .snapshot()
            .unwrap()
            .peer
            .unwrap()
            .jitter_delay_samples,
        480
    );
    state.lock().unwrap().bad_observe_size = false;
    state.lock().unwrap().bad_observe_version = true;
    session.step(60).unwrap();
    assert_eq!(
        control
            .snapshot()
            .unwrap()
            .peer
            .unwrap()
            .jitter_delay_samples,
        480
    );
    assert_eq!(state.lock().unwrap().observe_calls, 4);
}

#[test]
fn async_peer_step_is_serial_and_detaches_before_handle_drop() {
    let (session, control, state, _output) = make_async_session("1000");
    let pause = Arc::new(std::sync::Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    worker.attach(session).ok().unwrap();
    worker.reap();
    assert_eq!(worker.async_peers.len(), 1);
    pause.wait();
    if worker.thread.is_some() {
        pause.wait();
        worker.stop();
        panic!("async peer used the synchronous polling thread");
    }
    assert!(worker.thread.is_none());
    assert_eq!(state.lock().unwrap().async_attaches, 1);
    let release_state = state.clone();
    let release = thread::spawn(move || {
        for _ in 0..1000 {
            if release_state.lock().unwrap().async_stops != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(release_state.lock().unwrap().drops, 0);
        pause.wait();
    });
    worker.stop();
    release.join().unwrap();
    assert!(control.snapshot().unwrap().ended);
    let state = state.lock().unwrap();
    assert_eq!(
        (state.async_stops, state.async_detaches, state.drops),
        (1, 1, 1)
    );
}

#[test]
fn retained_step_rejects_missing_context_and_unregistered_epoch() {
    assert_eq!(unsafe { retained_step(ptr::null_mut()) }, -1);
    let (mut session, control, _state, _) = make_async_session("1000");
    assert_eq!(
        unsafe { retained_step(ptr::from_mut(&mut session).cast()) },
        -1
    );
    assert!(control.snapshot().unwrap().ended);
}

#[test]
fn spurious_ready_without_an_event_yields_the_peer_turn() {
    let (mut session, _control, state, _) = make_session("1000");
    state.lock().unwrap().ready_without_input = true;
    session.step(0).unwrap();
    assert!(state.lock().unwrap().input.is_empty());
}

#[test]
fn failed_async_attach_never_retains_session_context() {
    let (session, _control, state, _) = make_async_session("1000");
    state.lock().unwrap().fail_async_attach = true;
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    let session = worker.attach(session).expect_err("host rejected attach");
    assert_eq!(state.lock().unwrap().async_attaches, 0);
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
    worker.stop();
}

#[test]
fn finished_synchronous_owner_rejects_a_new_session_without_consuming_it() {
    let (session, _control, state, _) = make_session("1000");
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    let finished = thread::spawn(|| {});
    while !finished.is_finished() {
        thread::yield_now();
    }
    worker.thread = Some(finished);
    let session = worker.attach(session).expect_err("owner already finished");
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
    worker.stop();
}

#[test]
fn async_capacity_rejects_before_host_registration() {
    let (session, _control, state, _) = make_async_session("1000");
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    worker.test_async_capacity = 0;
    let session = worker
        .attach(session)
        .expect_err("async peer limit reached");
    assert_eq!(state.lock().unwrap().async_attaches, 0);
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
    worker.stop();
}

#[test]
fn missing_sync_queue_and_spawn_failure_return_unconsumed_sessions() {
    let (session, _control, state, _) = make_session("1000");
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    drop(worker.incoming.take());
    let session = worker.attach(session).expect_err("sync queue missing");
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
    worker.stop();

    let (session, _control, state, _) = make_session("1000");
    let mut worker = PeerIoWorker::start(Instant::now()).unwrap();
    worker.fail_next_spawn = true;
    let session = worker.attach(session).expect_err("spawn failed");
    assert!(worker.stop.load(Ordering::Acquire));
    assert!(worker.thread.is_none());
    drop(session);
    assert_eq!(state.lock().unwrap().drops, 1);
    worker.stop();
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
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.25; 960]),
        0
    );
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 4000]));
    session.step(20).unwrap();
    assert!(state.lock().unwrap().digits.is_empty());
    assert!(
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"D 2000 1000 1 5")
    );
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
    let (_, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
        .unwrap()
        .into_endpoints();
    control
        .send(Command::Redirect { inbound, outbound })
        .ok()
        .unwrap();
    session.step(3022).unwrap();
    assert!(old.signals().ended());
    // Control may inspect status before consuming the redirect acknowledgment.
    // Retiring the old audio generation must not end the live connection.
    assert!(!control.snapshot().unwrap().ended);
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
fn oversized_peer_audio_is_rejected_before_touching_the_ring() {
    let (mut session, control, state, _) = make_session("1000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.25; MAX_PEER_INPUT_SAMPLES + 1]));
    assert!(matches!(session.step(0), Err(Error::InvalidFrame)));
    assert!(control.snapshot().unwrap().ended);
    assert!(session.pending_input.is_none());
}

#[test]
fn failed_or_overreported_ring_admission_ends_audio_and_gap_ingress() {
    for gap in [false, true] {
        for overreported in [false, true] {
            let (mut session, mut control, state, _) = make_session("1000");
            let (inbound, _consumer) =
                crate::link::ring::tests::invalid_ingress_endpoints(gap, overreported);
            control
                .send(Command::Redirect {
                    inbound,
                    outbound: rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
                        .unwrap()
                        .into_endpoints()
                        .1,
                })
                .ok()
                .unwrap();
            session.step(0).unwrap();
            state.lock().unwrap().input.push_back(if gap {
                Input::Gap(8)
            } else {
                Input::Audio(vec![0.25; 8])
            });
            assert!(matches!(session.step(1), Err(Error::Write)));
            assert!(session.inbound.signals().ended());
        }
    }
}

#[test]
fn pending_gap_rejects_a_ring_that_overreports_its_next_acceptance() {
    let (mut session, mut control, state, _) = make_session("1000");
    let (inbound, _consumer) = crate::link::ring::tests::pending_overreported_gap_endpoints();
    control
        .send(Command::Redirect {
            inbound,
            outbound: rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
                .unwrap()
                .into_endpoints()
                .1,
        })
        .ok()
        .unwrap();
    session.step(0).unwrap();
    state.lock().unwrap().input.push_back(Input::Gap(8));
    session.step(1).unwrap();
    assert!(matches!(session.pending_input, Some(PendingInput::Gap(8))));
    assert!(matches!(session.step(2), Err(Error::Write)));
    assert!(session.inbound.signals().ended());
}

#[test]
fn counted_gap_is_staged_before_following_peer_events_when_ring_is_full() {
    let (mut session, mut control, state, _) = make_session("1000");
    let (inbound, mut consumer) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    control
        .send(Command::Redirect {
            inbound,
            outbound: rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
                .unwrap()
                .into_endpoints()
                .1,
        })
        .ok()
        .unwrap();
    session.step(0).unwrap();
    for _ in 0..4 {
        state
            .lock()
            .unwrap()
            .input
            .push_back(Input::Audio(vec![0.25; 4096]));
    }
    state.lock().unwrap().input.push_back(Input::Gap(4096));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 64]));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"L T3000"));
    for now in 1..=4 {
        session.step(now).unwrap();
    }
    session.step(5).unwrap();
    assert_eq!(consumer.observe().unwrap().available_samples, 16576);
    assert_eq!(state.lock().unwrap().input.len(), 2);
    session.step(6).unwrap();
    assert_eq!(state.lock().unwrap().input.len(), 2);
    let mut output = [0.0; 4096];
    assert_eq!(consumer.render(&mut output), Ok(3916));
    session.step(7).unwrap();
    session.step(8).unwrap();
    assert_eq!(consumer.render(&mut output), Ok(4096));
    for now in 9..=12 {
        session.step(now).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert!(matches!(control.event(), Some(Event::Redirected(_))));
    assert!(matches!(control.event(), Some(Event::Text(bytes)) if bytes == b"L T3000"));
}

#[test]
fn partial_pcm_is_staged_before_a_following_counted_gap() {
    let (mut session, mut control, state, _) = make_session("1000");
    let (inbound, mut consumer) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    control
        .send(Command::Redirect {
            inbound,
            outbound: rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
                .unwrap()
                .into_endpoints()
                .1,
        })
        .ok()
        .unwrap();
    session.step(0).unwrap();
    for _ in 0..4 {
        state
            .lock()
            .unwrap()
            .input
            .push_back(Input::Audio(vec![0.25; 4096]));
    }
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Audio(vec![0.5; 256]));
    state.lock().unwrap().input.push_back(Input::Gap(64));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"L T3000"));
    for now in 1..=5 {
        session.step(now).unwrap();
    }
    assert_eq!(consumer.observe().unwrap().available_samples, 16576);
    assert_eq!(state.lock().unwrap().input.len(), 2);
    session.step(6).unwrap();
    assert_eq!(state.lock().unwrap().input.len(), 2);
    let mut output = [0.0; 4096];
    assert_eq!(consumer.render(&mut output), Ok(3916));
    for now in 7..=10 {
        session.step(now).unwrap();
    }
    assert!(state.lock().unwrap().input.is_empty());
    assert!(matches!(control.event(), Some(Event::Redirected(_))));
    assert!(matches!(control.event(), Some(Event::Text(bytes)) if bytes == b"L T3000"));
}

#[test]
fn keyed_source_accepts_only_current_active_query_and_retains_last_selection() {
    let (mut session, mut control, state, _) = make_session("1000");
    session.text(b"K 1000 3000 1 0".to_vec(), 0).unwrap();
    session.inbound.signals().set_active(true);
    // A response before the reader observes the new activity epoch is advisory only.
    session.text(b"K 1000 3000 1 0".to_vec(), 0).unwrap();
    session.step(10).unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text == b"K? * 1000 0 0")
    );
    session.text(b"K 9999 3000 1 0".to_vec(), 0).unwrap();
    session.text(b"K 1000 3000 1 0".to_vec(), 0).unwrap();
    session.text(b"K 1000 4000 1 0".to_vec(), 0).unwrap();
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
    session.text(b"K 1000 4000 1 0".to_vec(), 0).unwrap();
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
                assert_eq!(
                    output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.1; 960]),
                    0
                );
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
    assert!(!control.snapshot().unwrap().ended);
    control.stop();
    assert!(control.snapshot().unwrap().ended);
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
        let (_, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
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
    let (mut output, outbound) = rpt_advanced_core::audio::LinkAudioQueue::new(960, 64)
        .unwrap()
        .into_endpoints();
    let (mut session, _control) = PeerSession::prepare(io, outbound, "1000", "2000").unwrap();
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.5]),
        0
    );
    session.step(0).unwrap();
    // A warmed converter may emit immediately or retain sub-frame input. Neither
    // case may create an empty network write; filter latency is not an ABI contract.
    let writes = state.lock().unwrap().writes.clone();
    assert!(writes.iter().all(|count| *count > 0));
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.5]),
        0
    );
    session.step(20).unwrap();
    let writes = state.lock().unwrap().writes.clone();
    assert!(writes.iter().all(|count| *count > 0));
    assert_eq!(
        output.write(rpt_advanced_core::audio::AudioSpan::default(), &[0.5; 960]),
        0
    );
    session.step(40).unwrap();
    let writes = state.lock().unwrap().writes.clone();
    assert!(!writes.is_empty());
    assert!(writes.iter().all(|count| *count > 0));
}
