use super::*;
use crate::{
    abi,
    link::session::tests::{Input, peer},
};
use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::dtmf::DigitEvent,
};
use std::{ffi::c_void, path::Path, ptr, time::Duration};

#[link(name = "rptadv_file_adapter")]
unsafe extern "C" {
    fn rptadv_file_adapter_descriptor() -> *const crate::media::FileDescriptor;
}
#[link(name = "rptadv_speech_adapter")]
unsafe extern "C" {
    fn rptadv_speech_adapter_descriptor() -> *const crate::media::SpeechDescriptor;
}
unsafe extern "C" fn reaper() {}
fn media() -> NativeMediaPreparer {
    unsafe {
        NativeMediaPreparer::from_descriptors(
            rptadv_file_adapter_descriptor(),
            rptadv_speech_adapter_descriptor(),
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            Path::new("/tmp"),
            1000,
            (reaper, reaper),
        )
    }
    .unwrap()
}
fn services() -> HostServices {
    let mut table: abi::rptadv_host_services_v3 =
        unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}
fn clock() -> RuntimeClock {
    RuntimeClock {
        now_ms: 10,
        wall_seconds: 0,
        civil: Some((
            rpt_advanced_core::schedule::CivilTime::new(
                2030,
                1,
                1,
                rpt_advanced_core::schedule::Weekday::Tuesday,
                9,
                7,
            )
            .unwrap(),
            3,
        )),
    }
}
fn document(device: &str, extra: &str) -> ConfigDocument {
    ConfigDocument::parse(&format!(
        "[1000]\nradio_channel={device}\nduplex=full\n{extra}"
    ))
    .unwrap()
}
fn host() -> Host {
    Host::start(
        document("usb", ""),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}
fn pump_until(host: &mut Host, mut condition: impl FnMut(&mut Host) -> bool) {
    for _ in 0..200 {
        host.pump(clock()).unwrap();
        if condition(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("host did not reach expected state");
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation {
        command: Command {
            action,
            node: remote.into(),
        },
        digit: None,
    }
}
fn attach(
    host: &mut Host,
    remote: &str,
    action: LinkAction,
    mode: Mode,
) -> Arc<Mutex<crate::link::session::tests::State>> {
    let effect = host
        .runtime
        .command("1000", operation(action, remote), clock(), false)
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected a new dial");
    };
    assert!(
        host.runtime
            .finish_connect("1000", attempt, true, clock())
            .unwrap()
    );
    let (io, state) = peer(48000);
    host.attach_peer("1000", remote, mode, io, clock()).unwrap();
    pump_until(host, |host| {
        host.peers
            .iter()
            .all(|peer| !peer.control.snapshot().unwrap().ended)
    });
    state
}

fn render_queued_telemetry(host: &mut Host, owners: &mut AudioOwners) -> bool {
    let mut heard = false;
    for block in 0..1024_u64 {
        let mut audio = [0.0; 960];
        owners
            .0
            .acquire()
            .unwrap()
            .state()
            .process(false, &mut audio, block * 20);
        let mut generation = owners.1.acquire().unwrap();
        let transmit = generation.state();
        let keyed = transmit
            .adapter
            .process(&mut transmit.controller, false, &mut audio)
            .unwrap();
        drop(generation);
        heard |= audio.iter().any(|sample| *sample != 0.0);
        host.pump(clock()).unwrap();
        if heard && !keyed {
            return true;
        }
    }
    false
}

#[test]
fn peer_attach_and_detach_queue_lifecycle_telemetry() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();

    assert!(render_queued_telemetry(&mut host, &mut owners));

    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::Disconnect, "2000"),
            clock(),
            false,
        )
        .unwrap();
    host.immediate("1000", effect, clock()).unwrap();
    assert!(render_queued_telemetry(&mut host, &mut owners));

    lease.lock().unwrap().owners = Some(owners);
    assert!(host.stop(30));
}

#[test]
fn peers_survive_reload_route_events_and_detach_before_generation_reclamation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    assert!(
        host.status_text("1000")
            .unwrap()
            .contains("no active links")
    );
    let local = host.status_text("1000").unwrap();
    assert!(
        local.contains("local-rx:"),
        "status must identify the local receive ring"
    );
    assert!(local.contains("reset-failures=0"));
    assert!(local.contains("pending-reset-dropped=0"));
    assert!(matches!(
        host.status_text("missing"),
        Err(RuntimeError::MissingNode)
    ));
    assert_eq!(
        host.lookup("1000", "2000", Some("127.0.0.1")).unwrap(),
        "radio@fixture/2000"
    );
    assert!(matches!(
        host.lookup("missing", "2000", None),
        Err(RuntimeError::MissingNode)
    ));
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert!(
        host.status_text("1000")
            .unwrap()
            .contains("2000: transceive")
    );
    host.immediate(
        "1000",
        LinkEffect::Text(vec![("2000".into(), "L T3000".into())]),
        clock(),
    )
    .unwrap();
    host.immediate(
        "1000",
        LinkEffect::RemoteDigit {
            remote: "2000".into(),
            digit: '5',
        },
        clock(),
    )
    .unwrap();
    host.immediate("1000", LinkEffect::None, clock()).unwrap();
    host.immediate("1000", LinkEffect::SelectedRemote, clock())
        .unwrap();
    state.lock().unwrap().input.extend([
        Input::Text(b"invalid"),
        Input::Text(b"L T3000"),
        Input::Digit(b'*'),
        Input::Digit(b'7'),
        Input::Digit(b'0'),
    ]);
    pump_until(&mut host, |host| !host.operations.is_empty());
    assert_eq!(
        host.take_operations()[0].1.command.action,
        LinkAction::Status
    );
    assert!(host.take_operations().is_empty());
    assert!(host.status_text("1000").unwrap().contains("3000"));
    assert_eq!(state.lock().unwrap().digits, [b'5']);
    host.runtime.digit(
        "1000",
        DigitEvent::Digit {
            digit: '*',
            now_ms: 10,
        },
    );
    host.lost_digits();
    host.reload(document("usb", "callsign=W1NEW\n"), clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(state.lock().unwrap().drops, 0);
    for action in [
        LinkAction::Status,
        LinkAction::FullStatus,
        LinkAction::LastKeyed,
        LinkAction::Time,
    ] {
        host.immediate("1000", LinkEffect::Telemetry(action), clock())
            .unwrap();
    }
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!!DISCONNECT!!"));
    pump_until(&mut host, |host| host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(
        host.status_text("1000")
            .unwrap()
            .contains("no active links")
    );
    assert!(host.stop(20));
}

#[test]
fn status_reports_modes_permanent_retries_and_failed_resource_preparation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    for (remote, action, mode, expected) in [
        ("2000", LinkAction::Monitor, Mode::MONITOR, "monitor"),
        (
            "3000",
            LinkAction::PermanentLocalMonitor,
            Mode::LOCAL_MONITOR,
            "local-monitor (permanent)",
        ),
    ] {
        attach(&mut host, remote, action, mode);
        pump_until(&mut host, |host| {
            host.runtime
                .status(10)
                .iter()
                .all(|(_, status)| status.retiring.is_none())
        });
        assert!(
            host.status_text("1000")
                .unwrap()
                .contains(&format!("{remote}: {expected}"))
        );
    }
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::PermanentTransceive, "4000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected dial");
    };
    assert!(
        !host
            .runtime
            .finish_connect("1000", attempt, false, clock())
            .unwrap()
    );
    assert!(host.status_text("1000").unwrap().contains("(retrying)"));
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::DisconnectAll, ""),
            clock(),
            false,
        )
        .unwrap();
    host.immediate("1000", effect, clock()).unwrap();
    assert!(host.status_text("1000").unwrap().contains("(paused)"));
    assert!(
        host.immediate(
            "1000",
            LinkEffect::RemoteDigit {
                remote: "missing".into(),
                digit: '1'
            },
            clock()
        )
        .is_err()
    );
    assert!(host.text("1000", "missing", "L", false).is_err());
    assert!(host.refresh("missing", clock()).is_err());
    let (io, state) = peer(0);
    assert!(
        host.attach_peer("1000", "5000", Mode::TRANSCEIVE, io, clock())
            .is_err()
    );
    assert_eq!(state.lock().unwrap().drops, 1);
    let (io, state) = peer(48000);
    assert!(
        host.attach_peer("missing", "5000", Mode::TRANSCEIVE, io, clock())
            .is_err()
    );
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.stop(30));
}

#[test]
fn reload_handoff_rolls_back_failed_open_and_releases_removed_devices() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    host.reload(document("usb2", "link_lookup_method=dns\n"), clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(
        host.lookup("1000", "2000", None).unwrap(),
        "radio@fixture/2000"
    );
    let revision = host.runtime.revision();
    crate::fixture::RADIO_OPEN_RESULT.store(1, std::sync::atomic::Ordering::Release);
    assert!(
        host.reload(document("usb3", "link_lookup_method=file\n"), clock())
            .is_err()
    );
    assert_eq!(host.runtime.revision(), revision);
    host.reload(document("usb2", "link_lookup_method=file\n"), clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert_eq!(
        host.lookup("1000", "2000", None).unwrap(),
        "radio@fixture/2000"
    );
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(host.runtime.node_names().is_empty());
    assert!(host.stop(40));
    assert!(host.leases.is_empty());
}

#[test]
fn queued_redirect_retries_when_full_and_withdrawn_peers_release_acknowledgments() {
    struct Resume(Arc<std::sync::Barrier>);
    impl Drop for Resume {
        fn drop(&mut self) {
            self.0.wait();
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    let pause = Arc::new(std::sync::Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    for _ in 0..64 {
        host.peers[0]
            .control
            .send(PeerCommand::Digit('1'))
            .ok()
            .unwrap();
    }
    assert!(host.text("1000", "2000", "\0", false).is_err());
    assert!(host.text("1000", "2000", "L", false).is_err());
    assert!(
        host.immediate(
            "1000",
            LinkEffect::RemoteDigit {
                remote: "2000".into(),
                digit: '2'
            },
            clock()
        )
        .is_err()
    );
    host.reload(document("usb", "callsign=W1NEW\n"), clock())
        .unwrap();
    let active = host.runtime.status(10)[0].1.active.unwrap();
    assert_eq!(
        host.runtime
            .adapter_control_mut("1000", active)
            .unwrap()
            .redirects
            .len(),
        1
    );
    drop(resume);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });

    // Direct publication needs quiescent audio; keep the peer reader live for the pending ack.
    assert!(host.leases[0].1.lock().unwrap().quiesce());
    let candidate = prepared("1000", host.runtime.settings("1000").unwrap(), &host.peers).unwrap();
    host.runtime.replace_adapter("1000", candidate, 11).unwrap();
    host.reject_peer("1000", "2000", 12);
    host.pump(clock()).unwrap();
    assert_eq!(state.lock().unwrap().drops, 1);
    host.reject_peer("1000", "missing", 12);
    assert!(host.stop(13));
}

#[test]
fn ended_ingress_is_omitted_and_unknown_peer_events_do_not_change_routes() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    host.runtime.node("1000").unwrap().links().ended("2000");
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"K? * 3000 0 0"));
    std::thread::sleep(Duration::from_millis(5));
    host.pump(clock()).unwrap();
    host.peers[0].control.stop();
    while !host.peers[0].reader.ended() {
        std::thread::yield_now();
    }
    let candidate = prepared("1000", host.runtime.settings("1000").unwrap(), &host.peers).unwrap();
    assert!(candidate.control.redirects.is_empty());
    host.pump(clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(host.stop(20));
}

#[test]
fn new_node_reload_and_missing_or_busy_operations_leave_live_owners_intact() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = Host::start(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    host.reload(
        document("usb", "[2000]\nradio_channel=usb2\nduplex=full\n"),
        clock(),
    )
    .unwrap();
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    assert!(host.immediate("1000", effect, clock()).is_err());
    let first = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let (io, second) = peer(48000);
    host.runtime.incoming("2000", "3000", true).unwrap();
    host.attach_peer("2000", "3000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    host.immediate(
        "2000",
        LinkEffect::RemoteDigit {
            remote: "3000".into(),
            digit: '9',
        },
        clock(),
    )
    .unwrap();
    host.text("2000", "3000", "L", false).unwrap();
    assert!(
        host.status_text("2000")
            .unwrap()
            .contains("3000: transceive")
    );
    pump_until(&mut host, |_| !second.lock().unwrap().digits.is_empty());
    assert!(first.lock().unwrap().digits.is_empty());
    assert_eq!(second.lock().unwrap().digits, [b'9']);
    host.reject_peer("2000", "3000", 15);
    assert_eq!(second.lock().unwrap().drops, 1);
    assert_eq!(first.lock().unwrap().drops, 0);
    assert!(host.refresh("missing", clock()).is_err());
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(host.peers.is_empty());
    assert!(host.stop(20));
}

#[test]
fn device_handoff_retains_unique_owners_and_refuses_failed_or_duplicate_open() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut first = host();
    let mut second = host();
    let first_lease = first.leases[0].1.clone();
    let second_lease = second.leases[0].1.clone();
    let settings = GenerationSettings {
        node: "1000".into(),
        device: "usb".into(),
        receive_maximum: MAXIMUM_FRAMES,
        transmit_maximum: MAXIMUM_FRAMES,
        squelch_delay_ms: 0,
    };
    let mut device = Device {
        lease: first_lease.clone(),
        services: services(),
    };
    assert!(!device.open(&settings));
    assert!(device.quiesce());
    assert!(!device.open(&settings));
    let owners = first_lease.lock().unwrap().owners.take().unwrap();
    assert!(!first_lease.lock().unwrap().attach(owners));
    device.close();
    assert!(device.open(&settings));
    {
        let mut lease = second_lease.lock().unwrap();
        assert!(lease.quiesce());
        let owners = lease.owners.take().unwrap();
        assert!(!first_lease.lock().unwrap().attach(owners));
        lease.owners = first_lease.lock().unwrap().owners.take();
    }
    assert!(first.stop(10));
    assert!(second.stop(10));

    let lease = Arc::new(Mutex::new(Lease {
        quiesced: false,
        owners: None,
        worker: None,
        epoch: Instant::now(),
        status: Default::default(),
    }));
    let mut device = Device {
        lease: lease.clone(),
        services: services(),
    };
    RadioWorker::fail_next_for_test(crate::worker::TestFailure::Prepare);
    let before = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(!device.open(&settings));
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        before + 1
    );
    assert!(device.open(&settings));
    assert!(device.quiesce());
    assert!(lease.lock().unwrap().owners.is_none());
    device.close();
    assert!(device.open(&settings));
    device.close();
}

#[test]
fn pending_audio_generation_bounds_adapter_refresh_retries() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    assert!(host.leases[0].1.lock().unwrap().quiesce());
    host.reload(document("usb", "callsign=W1NEW\n"), clock())
        .unwrap();
    assert!(matches!(
        host.refresh("1000", clock()),
        Err(RuntimeError::Busy)
    ));
    assert!(host.stop(20));
}

#[test]
fn queued_peer_events_after_runtime_stop_cannot_reenter_removed_node_policy() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    assert!(!host.runtime.stop(20));
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"K? * 3000 0 0"));
    for _ in 0..100 {
        if state.lock().unwrap().input.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(1));
    host.pump(clock()).unwrap();
    assert!(host.take_operations().is_empty());
    assert!(host.stop(30));
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn failed_audio_preparation_and_attachment_preserve_start_and_reload_state() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    RadioWorker::fail_next_for_test(crate::worker::TestFailure::Prepare);
    assert!(matches!(
        Host::start(
            document("usb", ""),
            media(),
            services(),
            Instant::now(),
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    let mut host = host();
    let old_document = host.runtime.document().clone();
    let revision = host.runtime.revision();
    let lease = Arc::clone(&host.leases[0].1);
    crate::fixture::RADIO_ACTIVATE_RESULT.store(1, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        host.reload(
            document("usb", "[2000]\nradio_channel=usb2\nduplex=full\n"),
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    assert_eq!(host.runtime.document(), &old_document);
    assert_eq!(host.runtime.revision(), revision);
    assert_eq!(host.runtime.node_names(), ["1000"]);
    assert!(Arc::ptr_eq(&host.leases[0].1, &lease));
    assert!(lease.lock().unwrap().worker.is_some());
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .register_audio()
            .is_none()
    );
    assert!(host.stop(20));
}

#[test]
fn failed_device_restoration_leaves_inactive_generations_safe_to_pump_and_stop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FAIL: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn open(
        context: *mut c_void,
        name: *const std::ffi::c_char,
        length: usize,
        maximum: usize,
        output: *mut *mut c_void,
    ) -> i32 {
        if FAIL.load(Ordering::Acquire) {
            unsafe {
                output.write(std::ptr::null_mut());
            }
            -1
        } else {
            unsafe {
                (*crate::fixture::host_descriptor()).radio_open.unwrap()(
                    context, name, length, maximum, output,
                )
            }
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    FAIL.store(false, Ordering::Release);
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.radio_open = Some(open);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let mut host = Host::start(
        document("usb", ""),
        media(),
        services,
        Instant::now(),
        clock(),
    )
    .unwrap();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });
    FAIL.store(true, Ordering::Release);
    assert!(matches!(
        host.reload(document("unavailable", ""), clock()),
        Err(RuntimeError::Restore)
    ));
    assert!(host.runtime.status(10)[0].1.active.is_none());
    assert!(!host.status_text("1000").unwrap().contains("local-rx:"));
    // A late reader acknowledgment cannot reactivate a failed radio generation.
    let (inbound, _input) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    let (_, outbound) = LinkAudioQueue::new(960).unwrap().into_endpoints();
    host.peers[0]
        .control
        .send(PeerCommand::Redirect { inbound, outbound })
        .ok()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2));
    pump_until(&mut host, |host| {
        !host.peers[0].control.snapshot().unwrap().ended
    });
    host.pump(clock()).unwrap();
    assert!(host.stop(20));
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn stopped_host_status_does_not_retain_a_retired_radio_lease() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    assert!(host.stop(20));
    host.prune_leases();
    assert!(host.leases.is_empty());
    assert_eq!(host.status_text("1000"), Err(RuntimeError::MissingNode));
}

#[test]
fn peer_ending_before_refresh_is_reaped_without_a_connected_announcement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let (io, state) = peer(48000);
    state.lock().unwrap().fail_ready = true;
    crate::fixture::PEER_WAIT_FOR_END.store(1, std::sync::atomic::Ordering::Release);
    assert_eq!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock()),
        Ok(())
    );
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    assert!(!render_queued_telemetry(&mut host, &mut owners));
    lease.lock().unwrap().owners = Some(owners);
    assert!(
        host.status_text("1000")
            .unwrap()
            .contains("no active links")
    );
    assert!(host.stop(20));
}

#[test]
fn replacement_retry_classification_preserves_non_busy_failures() {
    assert!(matches!(replacement_completed(Ok(1)), Ok(true)));
    assert!(matches!(
        replacement_completed(Err(RuntimeError::Busy)),
        Ok(false)
    ));
    for error in [
        RuntimeError::Preparation,
        RuntimeError::MissingNode,
        RuntimeError::Device,
    ] {
        assert_eq!(replacement_completed(Err(error.clone())), Err(error));
    }
}

#[test]
fn peer_without_a_live_local_radio_is_rejected_before_starting_media() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (local, expected) in [
        ("missing", RuntimeError::MissingNode),
        ("1000", RuntimeError::Device),
    ] {
        let mut host = host();
        assert!(host.leases[0].1.lock().unwrap().quiesce());
        let (io, state) = peer(48000);
        assert_eq!(
            host.attach_peer(local, "2000", Mode::TRANSCEIVE, io, clock()),
            Err(expected)
        );
        assert!(host.peers.is_empty());
        let state = state.lock().unwrap();
        assert!(
            state.texts.is_empty(),
            "link setup must not start before radio binding"
        );
        assert_eq!(state.drops, 1);
        assert!(host.stop(20));
    }
}

#[test]
fn rejected_radio_binding_drops_peer_without_starting_media_or_admitting_it() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = host();
    let (io, state) = peer(8000);
    {
        let mut state = state.lock().unwrap();
        state.fail_bind = true;
        state.input.push_back(Input::Audio(vec![0.25; 160]));
    }
    assert_eq!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(host.peers.is_empty());
    {
        let state = state.lock().unwrap();
        assert!(state.texts.is_empty() && state.audio.is_empty());
        assert_eq!(state.input.len(), 1, "no frame may be read before binding");
        assert_eq!(state.drops, 1);
    }
    assert!(host.stop(20));
}

#[test]
fn incoming_and_outgoing_peers_bind_to_their_nodes_existing_radio_leases() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = Host::start(
        document("west-radio", "[2000]\nradio_channel=east-radio\n"),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    let opens = crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed);
    for (local, remote, radio, incoming) in [
        ("1000", "3000", "west-radio", true),
        ("2000", "4000", "east-radio", false),
    ] {
        if incoming {
            host.runtime.incoming(local, remote, true).unwrap();
        } else {
            let LinkEffect::Connect(attempt) = host
                .runtime
                .command(
                    local,
                    operation(LinkAction::Transceive, remote),
                    clock(),
                    false,
                )
                .unwrap()
            else {
                panic!("expected outbound dial");
            };
            assert!(
                host.runtime
                    .finish_connect(local, attempt, true, clock())
                    .unwrap()
            );
        }
        let (io, state) = peer(8000);
        host.attach_peer(local, remote, Mode::TRANSCEIVE, io, clock())
            .unwrap();
        assert_eq!(state.lock().unwrap().bound_radio.as_deref(), Some(radio));
    }
    assert_eq!(
        crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
        opens,
        "binding must reuse existing radio reservations"
    );
    assert!(host.stop(20));
}
