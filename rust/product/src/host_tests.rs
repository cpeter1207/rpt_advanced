use super::*;
#[path = "egress_load_tests.rs"]
mod egress_load;
use crate::{
    abi,
    link::session::tests::{Input, peer, peer_with_async},
};
use rpt_advanced_core::{
    command::{Command, LinkAction},
    runtime::dtmf::{DigitEvent, DigitOperation},
};
use std::{ffi::c_void, path::Path, ptr, sync::Barrier, time::Duration};

unsafe extern "C" fn reaper() {}
fn media() -> NativeMediaPreparer {
    unsafe {
        NativeMediaPreparer::from_descriptors(
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            1000,
            (reaper, reaper),
        )
    }
    .unwrap()
}
fn services() -> HostServices {
    let mut table: abi::rptadv_host_services_v5 =
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
        "[1000]\nradio_channel={device}\nduplex=full\nlink_static_directory_file=fixture\n{extra}"
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

#[test]
fn remote_selection_precedes_the_next_digit_in_one_host_batch() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for asynchronous in [false, true] {
        for suffix in ["5", "#5"] {
            let mut host = host();
            host.runtime.incoming("1000", "2000", true).unwrap();
            let (io, state) = peer_with_async(48000, asynchronous);
            state.lock().unwrap().continuous_async = true;
            host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
                .unwrap();
            pump_until(&mut host, |host| {
                named_peer_ready(host, "2000") && host.runtime.status(10)[0].1.retiring.is_none()
            });
            let _other = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
            // The fixture directory resolves every numeric name, including "3".
            state
                .lock()
                .unwrap()
                .input
                .extend(format!("*43{suffix}").bytes().map(Input::Digit));
            let mut outcomes = Vec::new();
            for _ in 0..100 {
                host.pump(clock()).unwrap();
                outcomes.extend(host.take_operations());
                std::thread::sleep(Duration::from_millis(1));
            }
            if suffix == "5" {
                assert!(
                    matches!(outcomes.as_slice(), [(_, DigitOutcome::Operation(operation))]
                    if operation.command.node == "3" && operation.digit == Some('5')),
                    "{outcomes:?}"
                );
            } else {
                assert_eq!(
                    outcomes,
                    vec![("1000".into(), DigitOutcome::Regenerate('5'))]
                );
            }
            assert!(host.stop(20));
        }
    }
}

#[test]
fn remote_digits_use_shared_policy_for_polling_and_async_hosts() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for asynchronous in [false, true] {
        let mut host = host();
        host.runtime.incoming("1000", "2000", true).unwrap();
        let (io, state) = peer_with_async(48000, asynchronous);
        state.lock().unwrap().continuous_async = true;
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
            .unwrap();
        pump_until(&mut host, |host| {
            named_peer_ready(host, "2000") && host.runtime.status(10)[0].1.retiring.is_none()
        });
        let other = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
        state.lock().unwrap().input.extend([
            Input::Text(b"D 3000 4000 11 9"),
            Input::Text(b"D 1000 4000 12 5"),
        ]);
        pump_until(&mut host, |host| {
            !host.operations.is_empty()
                && other
                    .lock()
                    .unwrap()
                    .texts
                    .iter()
                    .any(|text| text == b"D 3000 4000 11 9")
        });
        assert_eq!(
            host.take_operations(),
            vec![("1000".into(), DigitOutcome::Regenerate('5'))]
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .texts
                .iter()
                .any(|text| text.starts_with(b"D "))
        );
        assert!(other.lock().unwrap().digits.is_empty());
        assert!(host.stop(20));
    }
}

#[test]
fn same_device_reload_refreshes_local_ring_without_reopening_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(host.status_text("1000").unwrap().contains("reserve=0ms"));
    host.reload(
        document(
            "usb",
            "squelch_delay_ms=20\nstatus_snapshot_interval_ms=1\n",
        ),
        clock(),
    )
    .unwrap();
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    let status = host.status_text("1000").unwrap();
    assert!(
        status.contains("reserve=20ms target=20ms configured-delay=20ms"),
        "{status}"
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops,
        "same-device settings must preserve the active hardware stream"
    );
    assert!(host.stop(10));
}

#[test]
fn local_ring_reload_retains_update_until_worker_can_accept_it() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let initial = host.runtime.status(10)[0].1.active.unwrap();
    {
        let mut lease = host.leases[0].1.lock().unwrap();
        assert!(
            lease
                .worker
                .as_mut()
                .unwrap()
                .update_local_ring(initial, LocalRingUpdate::prepare(0).unwrap())
                .is_ok()
        );
    }
    host.reload(document("usb", "squelch_delay_ms=20\n"), clock())
        .unwrap();
    let active = host.runtime.status(10)[0].1.active.unwrap();
    let control = host.runtime.adapter_control_mut("1000", active).unwrap();
    assert!(control.awaiting_local);
    assert!(control.local_update.is_some());

    assert!(host.leases[0].1.lock().unwrap().quiesce());
    host.pump(clock()).unwrap();
    let control = host.runtime.adapter_control_mut("1000", active).unwrap();
    assert!(control.awaiting_local);
    assert!(control.local_update.is_some());
    assert!(host.stop(20));
}

#[test]
fn failed_local_ring_reload_preserves_the_active_configuration() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let generation = host.runtime.status(10)[0].1.active;
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    // The fixed local ring holds 300 ms, so this candidate cannot be prepared.
    assert!(
        host.reload(document("usb", "squelch_delay_ms=301\n"), clock())
            .is_err()
    );
    assert_eq!(host.runtime.status(10)[0].1.active, generation);
    assert!(
        host.status_text("1000")
            .unwrap()
            .contains("reserve=0ms target=0ms configured-delay=0ms")
    );
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    assert!(host.stop(10));
}

#[test]
fn egress_admission_host_reservation_failure_preserves_running_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let generation = host.runtime.status(10)[0].1.active;
    let mut probe = Vec::<PeerOwner>::new();
    probe.try_reserve(1).unwrap();
    let bytes = probe.capacity() * std::mem::size_of::<PeerOwner>();
    let (io, state) = peer(48000);
    let result = crate::fixture::fail_allocation(bytes, || {
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
    });
    assert_eq!(result, Err(RuntimeError::Preparation));
    assert_eq!(host.runtime.status(10)[0].1.active, generation);
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.stop(20));
}

#[test]
fn parrot_live_state_effect_needs_no_external_host_action() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    assert!(
        host.immediate("1000", LinkEffect::ParrotEnabled(true), clock())
            .is_ok()
    );
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
fn named_peer_ready(host: &Host, remote: &str) -> bool {
    host.peers.iter().any(|peer| {
        peer.local == "1000"
            && peer.remote == remote
            && peer.control.owner_thread().is_some()
            && peer
                .control
                .snapshot()
                .is_ok_and(|snapshot| !snapshot.ended)
    })
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation::new(
        Command {
            action,
            node: remote.into(),
        },
        None,
    )
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
        named_peer_ready(host, remote)
            && host.peers.iter().all(|peer| {
                peer.control
                    .snapshot()
                    .is_ok_and(|snapshot| !snapshot.ended)
            })
            && host
                .runtime
                .status(clock().now_ms)
                .iter()
                .any(|(node, status)| {
                    node == "1000" && status.active.is_some() && status.retiring.is_none()
                })
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
            .process(
                &mut transmit.controller,
                false,
                &mut audio,
                rpt_advanced_core::link::SourcePosition::default(),
            )
            .unwrap();
        drop(generation);
        heard |= audio.iter().any(|sample| *sample != 0.0);
        host.pump(clock()).unwrap();
        if heard && !keyed {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn composition_shared_host_pump_preserves_receive_interruption() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // Both composition hosts hand the same prepared program to this shared product.
    for (device, local) in [("native-cm119", true), ("RadioPlus/1000", false)] {
        let config = document(
            device,
            "kerchunk_max_ms=0\n[message notice]\ncontent={tone_sequence} {media}\ntone_sequence=600/1000\nmorse_text=E\ndtmf_id=1\n",
        );
        let mut host = Host::start(config, media(), services(), Instant::now(), clock()).unwrap();
        let lease = Arc::clone(&host.leases[0].1);
        assert!(lease.lock().unwrap().quiesce());
        let mut owners = lease.lock().unwrap().owners.take().unwrap();
        host.runtime.queue_message("1000", "1", None).unwrap();
        let mut heard_primary = false;
        for _ in 0..1000 {
            let mut audio = [0.0; 48];
            owners
                .1
                .acquire()
                .unwrap()
                .state()
                .controller
                .process_audio(false, false, &[], &mut audio);
            host.pump(clock()).unwrap();
            if audio.iter().any(|sample| *sample != 0.0) {
                heard_primary = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(heard_primary);
        // The receive edge cancels a one-second attention segment. Its full Morse
        // replacement must finish far sooner, even after receive falls again.
        owners
            .1
            .acquire()
            .unwrap()
            .state()
            .controller
            .process_audio(local, !local, &[], &mut [0.0; 48]);
        let mut heard_morse = false;
        let mut finished = false;
        let mut audible = 0;
        for _ in 0..1000 {
            let mut audio = [0.0; 48];
            let keyed = owners
                .1
                .acquire()
                .unwrap()
                .state()
                .controller
                .process_audio(false, false, &[], &mut audio);
            audible += audio.iter().filter(|sample| **sample != 0.0).count();
            heard_morse |= audible != 0;
            host.pump(clock()).unwrap();
            if heard_morse && !keyed {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            finished && audible < 10_000,
            "primary must not resume: {device}, audible={audible}"
        );
        lease.lock().unwrap().owners = Some(owners);
        assert!(host.stop(30));
    }
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
fn peer_sessions_share_one_io_owner_outside_the_control_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    attach(&mut host, "2001", LinkAction::Transceive, Mode::TRANSCEIVE);
    pump_until(&mut host, |host| {
        host.peers
            .iter()
            .all(|peer| peer.control.owner_thread().is_some())
    });
    let owners: Vec<_> = host
        .peers
        .iter()
        .map(|peer| peer.control.owner_thread().unwrap())
        .collect();
    assert!(owners.iter().all(|owner| *owner == owners[0]));
    assert_ne!(owners[0], std::thread::current().id());
    assert!(host.stop(30));
}

#[test]
fn blocked_peer_io_does_not_block_control_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let state = attach(&mut host, "2000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(Arc::clone(&pause));
    let release = std::thread::spawn(move || {
        pause.wait();
        std::thread::sleep(Duration::from_millis(100));
        pause.wait();
    });

    let started = Instant::now();
    host.pump(clock()).unwrap();
    let elapsed = started.elapsed();
    release.join().unwrap();
    assert!(elapsed < Duration::from_millis(50), "pump took {elapsed:?}");
    assert!(host.stop(30));
}

#[test]
fn status_reports_cached_peer_jitter_and_ingress_metrics() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let effect = host
        .runtime
        .command(
            "1000",
            operation(LinkAction::Transceive, "2000"),
            clock(),
            false,
        )
        .unwrap();
    let LinkEffect::Connect(attempt) = effect else {
        panic!("expected a new dial");
    };
    assert!(
        host.runtime
            .finish_connect("1000", attempt, true, clock())
            .unwrap()
    );
    let (io, state) = peer_with_async(48000, true);
    host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock())
        .unwrap();
    pump_until(&mut host, |host| {
        host.peers.iter().any(|peer| {
            peer.control
                .snapshot()
                .is_ok_and(|snapshot| snapshot.peer.is_some())
        })
    });
    let status = host.status_text("1000").unwrap();
    assert!(
        status.contains("jitter delay=10ms buffered-span=13ms queued=3"),
        "{status}"
    );
    assert!(status.contains("missing-samples=160 insertion-samples=80"));
    assert!(status.contains("overload-drops=5 overload-span-samples=800"));
    assert!(status.contains("ingress-queued=7/64 ingress-overflow-drops=6"));
    assert!(status.contains(
        "egress datagrams=10 socket-blocked=2 reliable-retries=1 reliable-pending=3 reply-pending=0"
    ));
    assert!(status.contains("media accepted=0 blocked=0 expired-native-samples=0"));
    assert_eq!(state.lock().unwrap().observe_calls, 1);
    assert!(host.stop(20));
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
        LinkEffect::ForwardDigits(vec![("2000".into(), "D 2000 1000 1 5".into())]),
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
    let (_, operation) = host.take_operations().pop().unwrap();
    let DigitOutcome::Operation(operation) = operation else {
        panic!("expected command")
    };
    assert_eq!(operation.command.action, LinkAction::Status);
    assert_eq!(operation.reply, Some(host.peers[0].control.reply_target()));
    assert!(host.take_operations().is_empty());
    assert!(host.status_text("1000").unwrap().contains("3000"));
    let guard = state.lock().unwrap();
    assert!(guard.texts.iter().any(|text| text == b"D 2000 1000 1 5"));
    assert!(guard.digits.is_empty());
    drop(guard);
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
        host.immediate("1000", LinkEffect::Telemetry(action, None), clock())
            .unwrap();
    }
    state
        .lock()
        .unwrap()
        .input
        .push_back(Input::Text(b"!!DISCONNECT!!"));
    pump_until(&mut host, |host| host.peers.is_empty());
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
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
            operation(LinkAction::DisconnectPermanentAll, ""),
            clock(),
            false,
        )
        .unwrap();
    host.immediate("1000", effect, clock()).unwrap();
    assert!(host.status_text("1000").unwrap().contains("(paused)"));
    assert!(
        host.immediate(
            "1000",
            LinkEffect::ForwardDigits(vec![("missing".into(), "D 3000 1000 1 1".into())]),
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
fn host_audio_owner_publishes_one_configured_group_winner_to_status() {
    use std::num::NonZeroUsize;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let config = ConfigDocument::parse(
        "[1000]\nradio_channel=usb\nduplex=full\n\
         [permanent 1000 network]\nremote_node=2000,3000\ngroup_name=Network\n",
    )
    .unwrap();
    let mut host = Host::start(config, media(), services(), Instant::now(), clock()).unwrap();
    let mut states = Vec::new();
    for (index, remote) in ["2000", "3000"].into_iter().enumerate() {
        host.runtime.incoming("1000", remote, true).unwrap();
        let (io, state) = peer(48000);
        host.attach_peer("1000", remote, Mode::TRANSCEIVE, io, clock())
            .unwrap();
        states.push(state);
        let expected = &["2000", "3000"][..=index];
        pump_until(&mut host, |host| {
            expected.iter().all(|remote| named_peer_ready(host, remote))
                && host.runtime.status(10).iter().any(|(node, status)| {
                    node == "1000" && status.active.is_some() && status.retiring.is_none()
                })
        });
    }
    let selection = host
        .peers
        .iter()
        .find(|peer| peer.remote == "2000")
        .and_then(|peer| peer.group.as_ref())
        .expect("configured group member is published")
        .selection()
        .clone();

    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let mut audio = [0.0; 960];
    owners
        .0
        .acquire()
        .unwrap()
        .state()
        .process(false, &mut audio, 0);
    assert_eq!(selection.active(), NonZeroUsize::new(1));
    let status = host.status_text("1000").unwrap();
    assert!(status.contains("2000: transceive"));
    assert!(status.contains("3000: monitor"));

    lease.lock().unwrap().owners = Some(owners);
    assert!(host.stop(30));
    drop(states);
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
    pump_until(&mut host, |host| host.runtime.status(40).is_empty());
    assert!(host.media_workers.is_empty());
    assert!(host.stop(40));
    assert!(host.leases.is_empty());
}

#[test]
fn egress_retirement_waits_for_dispatch_and_redirect() {
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
            .send(PeerCommand::Text {
                text: c"D 2000 1000 1 1".into(),
                advisory: false,
            })
            .ok()
            .unwrap();
    }
    assert!(host.text("1000", "2000", "\0", false).is_err());
    assert!(host.text("1000", "2000", "L", false).is_err());
    assert!(
        host.immediate(
            "1000",
            LinkEffect::ForwardDigits(vec![("2000".into(), "D 2000 1000 1 2".into())]),
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
    let retiring = host.runtime.status(10)[0].1.retiring.unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .adapter_control_mut("1000", retiring)
            .unwrap()
            .dispatcher
            .is_quiescent()
    });
    assert_eq!(
        host.runtime.status(10)[0].1.retiring,
        Some(retiring),
        "dispatcher quiescence cannot bypass the blocked peer's redirect acknowledgment"
    );
    drop(resume);
    pump_until(&mut host, |host| {
        host.runtime
            .status(10)
            .iter()
            .all(|(_, status)| status.retiring.is_none())
    });

    // Direct publication needs quiescent audio; retain the peer owner for its pending ack.
    assert!(host.leases[0].1.lock().unwrap().quiesce());
    let candidate = prepared(
        "1000",
        host.runtime.settings("1000").unwrap(),
        &host.peers,
        media_worker(&mut host.media_workers, "1000").unwrap(),
    )
    .unwrap();
    host.runtime.replace_adapter("1000", candidate, 11).unwrap();
    host.reject_peer("1000", "2000", 12);
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
    assert_eq!(state.lock().unwrap().drops, 1);
    host.reject_peer("1000", "missing", 12);
    assert!(host.stop(13));
}

#[test]
fn withdrawn_peer_clears_a_queued_redirect_and_acknowledgment() {
    struct Resume(Arc<Barrier>);
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
    let pause = Arc::new(Barrier::new(2));
    state.lock().unwrap().pause = Some(pause.clone());
    pause.wait();
    let resume = Resume(pause);
    for _ in 0..64 {
        host.peers[0]
            .control
            .send(PeerCommand::Text {
                text: c"D 2000 1000 1 1".into(),
                advisory: false,
            })
            .ok()
            .unwrap();
    }
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
    host.reject_peer("1000", "2000", 12);
    host.pump(clock()).unwrap();
    let control = host.runtime.adapter_control_mut("1000", active).unwrap();
    assert!(control.redirects.is_empty());
    assert!(control.awaiting.is_empty());
    drop(resume);
    assert!(host.stop(13));
    assert_eq!(state.lock().unwrap().drops, 1);
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
    let candidate = prepared(
        "1000",
        host.runtime.settings("1000").unwrap(),
        &host.peers,
        media_worker(&mut host.media_workers, "1000").unwrap(),
    )
    .unwrap();
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
        LinkEffect::ForwardDigits(vec![("3000".into(), "D 3000 2000 1 9".into())]),
        clock(),
    )
    .unwrap();
    host.text("2000", "3000", "L", false).unwrap();
    assert!(
        host.status_text("2000")
            .unwrap()
            .contains("3000: transceive")
    );
    pump_until(&mut host, |_| {
        second
            .lock()
            .unwrap()
            .texts
            .iter()
            .any(|text| text.starts_with(b"D "))
    });
    let first_state = first.lock().unwrap();
    let second_state = second.lock().unwrap();
    assert!(!first_state.texts.iter().any(|text| text.starts_with(b"D ")));
    assert!(
        second_state
            .texts
            .iter()
            .any(|text| text == b"D 3000 2000 1 9")
    );
    assert!(second_state.digits.is_empty());
    drop(second_state);
    drop(first_state);
    host.reject_peer("2000", "3000", 15);
    pump_until(&mut host, |_| second.lock().unwrap().drops == 1);
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
        status_snapshot_interval_ms: 50,
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
fn saturated_media_publication_keeps_the_running_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let active = host.runtime.status(10)[0].1.active;
    let mut held = Vec::new();
    // The worker retains two contexts plus two queued publications. The active
    // generation owns one slot; holding three candidates fills every other slot.
    for _ in 0..3 {
        loop {
            match prepared(
                "1000",
                host.runtime.settings("1000").unwrap(),
                &host.peers,
                media_worker(&mut host.media_workers, "1000").unwrap(),
            ) {
                Ok(candidate) => {
                    held.push(candidate);
                    break;
                }
                // Let a queued candidate move into an unused context slot.
                Err(RuntimeError::Busy) => std::thread::yield_now(),
                Err(error) => panic!("unexpected preparation failure: {error:?}"),
            }
        }
    }
    assert!(matches!(
        host.refresh("1000", clock()),
        Err(RuntimeError::Busy)
    ));
    assert_eq!(host.runtime.status(10)[0].1.active, active);
    drop(held);
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
    // A late peer acknowledgment cannot reactivate a failed radio generation.
    let (inbound, _input) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    let (_, outbound) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    host.peers[0]
        .control
        .send(PeerCommand::Redirect { inbound, outbound })
        .ok()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2));
    pump_until(&mut host, |host| {
        !host.peers[0].control.snapshot().unwrap().ended
    });
    pump_until(&mut host, |host| {
        host.runtime.status(10)[0].1.retiring.is_none()
    });
    // Failed restore retains the configured node after reclaiming its audio generation.
    assert!(host.runtime.status(10)[0].1.active.is_none());
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
    assert_eq!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock()),
        Ok(())
    );
    assert!(host.peers.is_empty());
    pump_until(&mut host, |_| state.lock().unwrap().drops == 1);
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
fn stopped_peer_io_owner_rejects_and_releases_a_prepared_session() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    host.peer_io.stop();
    let (io, state) = peer(48000);
    assert_eq!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, io, clock()),
        Err(RuntimeError::Rejected)
    );
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
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
        assert_eq!(
            state.lock().unwrap().bound_radio.as_deref(),
            Some(format!("{local}\0{radio}").as_str())
        );
    }
    assert_eq!(
        crate::fixture::RADIO_OPENS.load(std::sync::atomic::Ordering::Relaxed),
        opens,
        "binding must reuse existing radio reservations"
    );
    assert!(host.stop(20));
}
