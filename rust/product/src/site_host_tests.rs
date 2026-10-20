//! Shared Host uses the existing live radio handle for station site observations.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use rpt_advanced_core::{
    config::{NodeId, ResolvedSiteSettings},
    site::ReadingQuality,
};

struct BankFixture {
    generation: u64,
    mask: u32,
    high: u32,
    writes: Vec<(u32, u32)>,
}
static BANK: Mutex<BankFixture> = Mutex::new(BankFixture {
    generation: 7,
    mask: 0,
    high: 0,
    writes: Vec::new(),
});
unsafe extern "C" fn bank_snapshot(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    result: *mut abi::UrpAstSiteSnapshot,
) -> i32 {
    let state = BANK.lock().unwrap();
    unsafe {
        (*result).generation = state.generation;
        (*result).output_mask = 0xff;
        (*result).input_mask = 0xff;
        (*result).input_quality = 1;
        (*result).observed_at_valid = 1;
        (*result).claimed_mask = state.mask;
        (*result).requested_high_mask = state.high;
        (*result).submitted_high_mask = state.high;
        (*result).serviced_high_mask = state.high;
        (*result).submitted_generation = state.generation;
        (*result).serviced_generation = state.generation;
    }
    0
}
unsafe extern "C" fn bank_bind(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteBinding,
) -> i32 {
    let value = unsafe { &*value };
    let mut state = BANK.lock().unwrap();
    assert_eq!(value.generation, state.generation);
    let added = value.output_mask & !state.mask;
    state.high = (state.high & !added) | (value.initial_high_mask & added);
    state.mask = value.output_mask;
    state.generation += 13;
    let write = (state.mask, state.high);
    state.writes.push(write);
    0
}
unsafe extern "C" fn bank_publish(
    _: *mut c_void,
    _: *mut c_void,
    _: u32,
    value: *const abi::UrpAstSiteRequest,
) -> i32 {
    let value = unsafe { &*value };
    let mut state = BANK.lock().unwrap();
    assert_eq!(value.generation, state.generation);
    state.high = value.high_mask;
    let write = (state.mask, state.high);
    state.writes.push(write);
    0
}
fn bank_host() -> Host {
    bank_host_with("")
}
fn bank_host_with(extra: &str) -> Host {
    *BANK.lock().unwrap() = BankFixture {
        generation: 7,
        mask: 0,
        high: 0,
        writes: Vec::new(),
    };
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.radio_site_snapshot = Some(bank_snapshot);
    table.radio_site_bind = Some(bank_bind);
    table.radio_site_publish = Some(bank_publish);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let document = document(
        "usb",
        &format!(
            "[output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=7\nstartup_state=on\nsafe_state=off\n{extra}"
        ),
    );
    let radio = RadioSite::prepare(
        &[(
            "1000".into(),
            ResolvedSiteSettings::resolve(&document, &NodeId::new("1000").unwrap())
                .unwrap()
                .value,
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let mut host = Host::start(document, media(), services, Instant::now(), clock()).unwrap();
    host.install_sites(Acquisition::default(), radio);
    host.poll_sites(0);
    host.poll_sites(1);
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    host
}

#[test]
fn site_host_alarm_actions_do_not_wait_for_full_telemetry_queue() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=8\n\
         [alarm 1000 open]\nenabled=yes\nsource=door\ncondition=inactive\n\
         on_trip=output:relay:off,inhibit:on,macro:stop\ntrip_message=notice\n\
         repeat_message=notice\nrepeat_interval_ms=1\n\
         [macro stop]\naction=connect\ntarget_node=2000\n\
         [message notice]\ncontent={media}\nmorse_text=E\ndtmf_id=1\n",
    );
    for _ in 0..4 {
        host.runtime.queue_message("1000", "1", None).unwrap();
    }
    assert!(host.runtime.queue_message("1000", "1", None).is_err());
    for now_ms in 10..14 {
        host.pump(RuntimeClock { now_ms, ..clock() }).unwrap();
    }
    let snapshot = host.runtime.node("1000").unwrap().site().snapshot(14);
    assert!(snapshot.alarms["open"].active);
    assert!(snapshot.transmit_inhibited);
    assert_eq!(BANK.lock().unwrap().high, 0);
    let operations = host.take_site_links();
    assert_eq!(
        operations.len(),
        1,
        "repeated notices must not replay actions"
    );
    assert!(matches!(&operations[0].1, LinkEffect::Connect(attempt) if attempt.remote() == "2000"));
    drop(operations);
    // Site pins must be released while the radio owner still services commands.
    for now in 14..20 {
        if host.site_shutdown(now) == Ok(true) {
            break;
        }
    }
    assert_eq!(host.site_shutdown(20), Ok(true));
    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let mut audio = [0.25; 48];
    let keyed = owners
        .1
        .acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(true, false, &[], &mut audio);
    assert!(!keyed);
    assert_eq!(audio, [0.0; 48]);
    lease.lock().unwrap().owners = Some(owners);
    for now in 20..28 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.stop(28));
}

#[test]
fn site_commands_preserve_protection_and_reject_during_owner_handoff() {
    use rpt_advanced_core::site::{OutputRequest, SiteCommand};
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    let mut host = bank_host_with(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=8\n\
         [alarm 1000 open]\nenabled=yes\nsource=door\ncondition=inactive\n\
         latched=yes\non_trip=inhibit:on\non_clear=inhibit:off\n",
    );
    for now_ms in 10..14 {
        host.pump(RuntimeClock { now_ms, ..clock() }).unwrap();
    }
    assert!(
        host.site_command("1000", SiteCommand::Inhibit(false), clock())
            .is_ok()
    );
    assert!(
        host.site_command("1000", SiteCommand::AlarmReset("open".into()), clock())
            .is_err()
    );
    assert!(
        host.site_command(
            "1000",
            SiteCommand::AlarmAcknowledge("open".into()),
            clock()
        )
        .is_ok()
    );
    host.site_command(
        "1000",
        SiteCommand::Output {
            name: "relay".into(),
            request: OutputRequest::Off,
        },
        clock(),
    )
    .unwrap();
    host.poll_sites(14);
    assert_eq!(BANK.lock().unwrap().high, 0);
    let snapshot = host.runtime.node("1000").unwrap().site().snapshot(14);
    assert!(snapshot.transmit_inhibited);
    assert!(snapshot.alarms["open"].active && snapshot.alarms["open"].acknowledged);
    assert!(
        host.site_command("missing", SiteCommand::Inhibit(false), clock())
            .is_err()
    );
    assert!(
        host.site_command(
            "1000",
            SiteCommand::Output {
                name: "ptt".into(),
                request: OutputRequest::On,
            },
            clock()
        )
        .is_err()
    );
    let _ = host.site_shutdown(15);
    assert!(
        host.site_command("1000", SiteCommand::Inhibit(false), clock())
            .is_err()
    );
    for now in 16..24 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.stop(24));
}

#[test]
fn site_host_status_separates_requested_submitted_and_unavailable_readback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=8\n[sensor 1000 battery]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\nchannel=in1_input\n",
    );
    host.pump(RuntimeClock {
        now_ms: 10,
        ..clock()
    })
    .unwrap();
    host.epoch = Instant::now() - std::time::Duration::from_secs(6);
    let text = host.status_text("1000").unwrap();
    assert!(text.contains("site transmit-inhibited=false"), "{text}");
    assert!(
        text.contains("input door value=Some(false) quality=Valid age-ms=Some("),
        "{text}"
    );
    assert!(
        text.contains("sensor battery value=None quality=Unavailable"),
        "{text}"
    );
    assert!(text.contains("output relay requested-on=true"), "{text}");
    assert!(text.contains("submitted=Some((1.0,"), "{text}");
    assert!(text.contains("readback=unavailable"), "{text}");
    for now in 6000..6008 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.stop(6008));
}

#[test]
fn site_host_shutdown_acknowledges_safe_release_before_destroying_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    for now in 2..8 {
        if host.stop(now) {
            break;
        }
        assert_eq!(
            crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
            drops
        );
    }
    assert!(host.stop(8));
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
}

#[test]
fn site_host_reload_release_then_abort_restores_live_demand_without_reopening() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host();
    let revision = host.runtime.revision();
    let backends =
        Arc::new(crate::site::backends::SiteBackends::load(host.runtime.document()).unwrap());
    let mut candidate = Some(
        Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(RadioSite::default());
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        host.begin_site_reload(
            revision + 1,
            &mut candidate,
            &mut radio,
            &Default::default()
        ),
        Err(RuntimeError::Rejected)
    );
    assert!(candidate.is_some() && radio.is_some());
    host.begin_site_reload(revision, &mut candidate, &mut radio, &Default::default())
        .unwrap();
    let mut ready = false;
    for now in 2..8 {
        ready = host.site_barrier(now).unwrap();
        if ready {
            break;
        }
    }
    assert!(ready);
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
    drop(host.abort_sites());
    host.poll_sites(10);
    host.poll_sites(11);
    assert_eq!(BANK.lock().unwrap().high, 0x40);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    for now in 12..18 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.stop(18));
}

#[test]
fn site_host_reload_keeps_unchanged_output_timers_and_alarm_protection_running() {
    use rpt_advanced_core::site::{OutputRequest, SiteCommand};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = bank_host_with(
        "pulse_ms=20\n\
         [input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=8\n\
         [alarm 1000 open]\nenabled=yes\nsource=door\ncondition=inactive\n\
         trip_delay_ms=50\non_trip=inhibit:on\n",
    );
    host.advance_sites(RuntimeClock {
        now_ms: 0,
        ..clock()
    });
    for request in [OutputRequest::Off, OutputRequest::Pulse] {
        host.site_command(
            "1000",
            SiteCommand::Output {
                name: "relay".into(),
                request,
            },
            RuntimeClock {
                now_ms: 0,
                ..clock()
            },
        )
        .unwrap();
    }
    let backends =
        Arc::new(crate::site::backends::SiteBackends::load(host.runtime.document()).unwrap());
    let mut candidate = Some(
        Acquisition::prepare_reload(
            &Default::default(),
            backends,
            host.epoch,
            host.site_acquisition.keys(),
        )
        .unwrap(),
    );
    let mut radio = Some(
        RadioSite::prepare(
            &[(
                "1000".into(),
                ResolvedSiteSettings::resolve(
                    host.runtime.document(),
                    &NodeId::new("1000").unwrap(),
                )
                .unwrap()
                .value,
            )]
            .into_iter()
            .collect(),
        )
        .unwrap(),
    );
    host.begin_site_reload(
        host.runtime.revision(),
        &mut candidate,
        &mut radio,
        &Default::default(),
    )
    .unwrap();
    // Hold publication as though another physical owner has not acknowledged yet.
    host.advance_sites(RuntimeClock {
        now_ms: 100,
        ..clock()
    });
    host.poll_sites(101);
    let state = host.runtime.node("1000").unwrap().site().snapshot(101);
    assert!(!state.outputs["relay"].requested_on);
    assert_eq!(state.outputs["relay"].pulse_deadline_ms, None);
    assert!(state.alarms["open"].active);
    assert!(state.transmit_inhibited);
    assert_eq!(BANK.lock().unwrap().high, 0);
    drop(host.abort_sites());
    for now in 102..110 {
        if host.stop(now) {
            break;
        }
    }
    assert!(host.stop(110));
}

unsafe extern "C" fn observation(
    _: *mut c_void,
    handle: *mut c_void,
    bank: u32,
    output: *mut abi::UrpAstSiteSnapshot,
) -> i32 {
    assert_eq!(unsafe { crate::fixture::radio_name(handle) }, "1000\0usb");
    assert_eq!(bank, 0);
    unsafe {
        (*output).generation = 7;
        (*output).input_mask = 2;
        (*output).input_high_mask = 2;
        (*output).input_quality = 1;
        (*output).observed_at_ms = 950;
        (*output).observed_at_valid = 1;
        (*output).snapshot_at_ms = 1000;
    }
    0
}

#[test]
fn site_host_publishes_site_replacement_with_runtime_before_post_reload_polling() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for notification_error in [false, true] {
        let mut host = bank_host();
        let backends =
            Arc::new(crate::site::backends::SiteBackends::load(host.runtime.document()).unwrap());
        let mut candidate = Some(
            Acquisition::prepare_reload(
                &Default::default(),
                backends,
                host.epoch,
                host.site_acquisition.keys(),
            )
            .unwrap(),
        );
        let mut radio = Some(RadioSite::default());
        host.begin_site_reload(
            host.runtime.revision(),
            &mut candidate,
            &mut radio,
            &Default::default(),
        )
        .unwrap();
        for now in 2..8 {
            if host.site_barrier(now).unwrap() {
                break;
            }
        }
        let mut session = None;
        if notification_error {
            host.runtime.incoming("1000", "2000", true).unwrap();
            host.runtime.incoming("1000", "3000", true).unwrap();
            let (io, state) = peer(48000);
            let (_producer, outbound) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
            let (mut pending, control) =
                PeerSession::prepare(io, outbound, "1000", "2000").unwrap();
            host.peers.push(PeerOwner {
                local: "1000".into(),
                remote: "2000".into(),
                mode: Mode::TRANSCEIVE,
                group: None,
                announced: true,
                control,
            });
            state
                .lock()
                .unwrap()
                .input
                .push_back(Input::Text(b"D 3000 4000 11 9"));
            pending.step(clock().now_ms).unwrap();
            session = Some(pending);
        }
        let revision = host.runtime.revision();
        let result = host.reload(document("usb", ""), clock());
        assert_eq!(
            result,
            if notification_error {
                Err(RuntimeError::Rejected)
            } else {
                Ok(())
            }
        );
        assert!(host.runtime.revision() > revision);
        host.poll_sites(12);
        let removed = host.site_radio.output_status("1000", "relay").is_none();
        drop(host.abort_sites());
        for now in 13..20 {
            if host.stop(now) {
                break;
            }
        }
        assert!(host.stop(20));
        assert!(
            removed,
            "published configuration must not retain the removed site binding"
        );
        drop(session);
    }
}

#[test]
fn site_engine_shutdown_drives_safe_barriers_before_stopping_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let api = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    *BANK.lock().unwrap() = BankFixture {
        generation: 7,
        mask: 0,
        high: 0,
        writes: Vec::new(),
    };
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.radio_site_snapshot = Some(bank_snapshot);
    table.radio_site_bind = Some(bank_bind);
    table.radio_site_publish = Some(bank_publish);
    let configuration = "[1000]\nradio_channel=usb\nduplex=full\n[output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=7\nstartup_state=on\nsafe_state=off\n";
    assert_eq!(
        unsafe {
            api.start.unwrap()(
                Box::leak(Box::new(table)),
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                configuration.as_ptr().cast(),
                configuration.len(),
            )
        },
        0
    );
    let (engine, call) = crate::lifecycle::selected().unwrap();
    engine
        .run(|host| {
            host.poll_sites(1);
            host.poll_sites(2);
            Ok(())
        })
        .unwrap();
    drop(call);
    let stopped = unsafe { api.stop.unwrap()() };
    // Clean the real fixture even while the missing lifecycle drive is under test.
    for _ in 0..5 {
        if unsafe { api.stop.unwrap()() } == 0 {
            break;
        }
    }
    assert_eq!(stopped, 0);
    assert_eq!(
        BANK.lock().unwrap().writes,
        [(0x40, 0x40), (0x40, 0), (0, 0)]
    );
}

#[test]
fn site_host_polls_configured_owner_without_a_second_radio_reservation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(1);
    table.radio_site_snapshot = Some(observation);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    let document = document(
        "usb",
        "[input 1000 door]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=2\nactive_level=low\n",
    );
    let site = ResolvedSiteSettings::resolve(&document, &NodeId::new("1000").unwrap())
        .unwrap()
        .value;
    let radio = RadioSite::prepare(&[("1000".to_owned(), site)].into_iter().collect()).unwrap();
    let mut host = Host::start(document, media(), services, Instant::now(), clock()).unwrap();
    let drops = crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed);
    host.install_sites(Acquisition::default(), radio);
    let values = host.poll_sites(2000);
    let input = &values["1000"].inputs["door"];
    assert_eq!(input.value, Some(false));
    assert_eq!(input.observed_at_ms, Some(1950));
    assert_eq!(input.quality, ReadingQuality::Valid);
    assert_eq!(
        crate::fixture::RADIO_DROPS.load(std::sync::atomic::Ordering::Relaxed),
        drops
    );
    assert!(host.stop(2100));
}
