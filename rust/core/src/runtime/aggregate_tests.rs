use crate::config::resources::candidate::ResourceKind;
use crate::station_fixture;
use serde_json::json;
use station_fixture::{FOURTH, PORT, SECOND, SITE, THIRD};

use super::*;
use crate::{
    command::LinkAction,
    config::ResolvedNodeSettings,
    media::{FileRequest, MediaError, PreparedAudio, SpeechRequest, StationMediaSession},
    schedule::{CivilTime, Weekday},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Media;
struct TestPcm(usize);
impl crate::audio::PcmStreamReader for TestPcm {
    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        let count = output.len().min(20_000_usize.saturating_sub(self.0));
        output[..count].fill(0.25);
        self.0 += count;
        if count == 0 {
            crate::audio::PcmRead::Finished
        } else if self.0 == 20_000 {
            crate::audio::PcmRead::FinalSamples(count)
        } else {
            crate::audio::PcmRead::Samples(count)
        }
    }
}
struct TestStation(Option<Arc<Mutex<Vec<String>>>>);
impl StationMediaSession for TestStation {
    fn register_program(
        &mut self,
        program: crate::media::MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        for source in program.primary {
            if let (true, Some(messages), Some(speech)) = (one_shot, &self.0, source.speech) {
                messages.lock().unwrap().push(speech.text);
            }
        }
        Ok(Box::new(TestPcm(0)))
    }
    fn register_prepared(
        &mut self,
        _: PreparedAudio,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        Ok(Box::new(TestPcm(0)))
    }
    fn start(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}
impl NativeFilePreparer for Media {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeSpeechPreparer for Media {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeMediaPreparer for Media {
    fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        Ok(Box::new(TestStation(None)))
    }
}
struct CapturingMedia(Arc<Mutex<Vec<String>>>);

#[test]
fn phone_diagnostics_cannot_succeed_in_the_radio_telemetry_queue() {
    use crate::link::{PhoneAccountId, PhoneSource};
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        1,
        "aabbccddeeff00112233445566778899".parse().unwrap(),
        2,
        3,
    )
    .unwrap();
    let signals = crate::link::PeerSignals::for_phone(source);
    assert!(signals.authenticate_phone());
    let reply = signals.reply_target();
    let before = messages.lock().unwrap().clone();
    assert!(matches!(
        runtime.prepare_phone_status(
            PORT,
            LinkAction::Status,
            None,
            clock(),
            &crate::link::PeerSignals::new().reply_target(),
        ),
        Err(RuntimeError::Rejected)
    ));
    assert_eq!(
        runtime.queue_macro_stopped(PORT, Some(reply.clone())),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_loop_rejected(PORT, "2000", Some(reply.clone())),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_remote_command_unsupported(PORT, "2000", Some(reply.clone())),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_status(PORT, LinkAction::Status, None, clock(), Some(reply)),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(*messages.lock().unwrap(), before);
    assert!(runtime.stop(0));
}

#[test]
fn dtmf_macro_reload_prepares_catalog_and_retires_already_queued_work() {
    use crate::command_language::MacroOutcome;
    use crate::runtime::dtmf::{DigitEvent, DigitOutcome};
    let mut runtime = command_runtime(
        json!({"mappings":{"status-macro":{"command":"*95","protected":false,"action":"call_macro","macro":"status"}}}),
        &[(
            "macro",
            "status",
            json!({"actions":[{"action":"pause","timing":{"duration_ms":25}},{"action":"link_status"}]}),
        )],
    );
    let mut admitted = None;
    for digit in "*95#".chars() {
        if let Some(DigitOutcome::Mapped(input)) = runtime.digit(
            PORT,
            DigitEvent::Digit {
                digit,
                now_ms: 1000,
            },
        ) {
            let links::LinkEffect::CommandQueued(reply) =
                runtime.dispatch_mapping(PORT, input, clock()).unwrap()
            else {
                panic!("macro admission")
            };
            admitted = Some(reply);
        }
    }
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    let reply = admitted.unwrap();
    station_fixture::reload(
        &mut runtime,
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
        RuntimeClock {
            now_ms: 1001,
            ..clock()
        },
    )
    .unwrap();
    assert_eq!(reply.outcome(), Some(MacroOutcome::Cancelled));
    assert!(
        runtime
            .advance_commands(
                PORT,
                RuntimeClock {
                    now_ms: 3000,
                    ..clock()
                }
            )
            .unwrap()
            .is_none()
    );
    assert!(runtime.stop(3001));
}

#[test]
fn manually_selected_message_does_not_renew_its_polite_wait_after_source_unkey() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(&[(PORT,"1000")], &[(PORT, ResourceKind::Message, "report", json!({"speech": {"text": "Report"}, "morse": {"text": "E"}, "dtmf_id": "1", "polite": {"timing": {"maximum_wait_ms": 20}, "expiry": "skip"}}))]),
        &CapturingMedia(messages.clone()), adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>), clock(),
    ).unwrap();
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = tx.acquire().unwrap();
    let controller = &mut generation.state().controller;
    controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    runtime
        .queue_message(PORT, "1", Some(peer.reply_target()))
        .unwrap();
    controller.process_audio(false, false, &[], &mut [0.0; 1920]);
    peer.set_radio_keyed(false);
    controller.process_audio(false, false, &[], &mut [0.0]);
    runtime.prepare_messages(clock());
    let mut output = [0.0; 20_000];
    controller.process_audio(false, false, &[], &mut output);
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "expired manual content must not play after source unkey"
    );
    assert!(messages.lock().unwrap().is_empty());
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn manual_identifier_and_tail_success_satisfy_their_automatic_opportunity() {
    for kind in [ResourceKind::Identifier, ResourceKind::Tail] {
        let family = kind.as_str();
        let mut fields = json!({"timing":{"interval_ms":1000},"speech":{"text":"Report"},"morse":{"text":"E"},"dtmf_id":"1"});
        if kind == ResourceKind::Identifier {
            fields["regardless_of_activity"] = json!(true);
        }
        let messages = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = station_fixture::start(
            station_fixture::configuration(
                &[(PORT, "radio")],
                &[
                    (PORT, ResourceKind::Radio, "", json!({"duplex_mode":"full"})),
                    (
                        PORT,
                        ResourceKind::TxSignaling,
                        "",
                        json!({"ptt":{"timing":{"hang_ms":0}}}),
                    ),
                    (
                        PORT,
                        ResourceKind::RxSignaling,
                        "",
                        json!({"timing":{"kerchunk_max_ms":0}}),
                    ),
                    (PORT, kind, "report", fields),
                ],
            ),
            &CapturingMedia(messages.clone()),
            adapter,
            |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
            clock(),
        )
        .unwrap();
        let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let mut generation = tx.acquire().unwrap();
        let controller = &mut generation.state().controller;
        controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
        runtime.queue_message(PORT, "1", None).unwrap();
        controller.process_audio(false, false, &[], &mut [0.0]);
        runtime.prepare_messages(clock());
        let mut played = [0.0; 20_000];
        controller.process_audio(false, false, &[], &mut played);
        assert_eq!(played, [0.25; 20_000]);
        if family == "tail" {
            controller.process_audio(true, false, &[], &mut [0.0]);
            controller.process_audio(false, false, &[], &mut [0.0; 12_001]);
        } else {
            controller.process_audio(false, false, &[], &mut [0.0; 21_000]);
        }
        runtime.prepare_messages(clock());
        assert_eq!(
            messages.lock().unwrap().len(),
            1,
            "manual {family} must not immediately play again automatically"
        );
        controller.process_audio(false, false, &[], &mut [0.0; 60_000]);
        if family == "tail" {
            controller.process_audio(true, false, &[], &mut [0.0]);
            controller.process_audio(false, false, &[], &mut [0.0]);
        }
        runtime.prepare_messages(clock());
        assert_eq!(
            messages.lock().unwrap().len(),
            2,
            "{family} becomes eligible after its normal interval"
        );
        drop(generation);
        assert!(runtime.stop(0));
    }
}

#[test]
fn site_manual_command_publishes_inhibit_without_waiting_for_poll() {
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Radio,
                "",
                json!({"duplex_mode": "full"}),
            )],
        ),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut actual = Vec::new();
    for inhibit in [true, false] {
        runtime
            .node(PORT)
            .unwrap()
            .site_command(crate::site::SiteCommand::Inhibit(inhibit), 0)
            .unwrap();
        runtime
            .node(PORT)
            .unwrap()
            .advance_site(0, &Default::default(), &Default::default());
        let mut guard = tx.acquire().unwrap();
        let mut audio = [0.25; 48];
        actual.push(
            guard
                .state()
                .controller
                .process_audio(true, false, &[], &mut audio),
        );
    }
    assert!(runtime.stop(0));
    assert_eq!(actual, [false, true]);
}

#[test]
fn site_inhibit_is_seeded_before_reloaded_audio_becomes_available() {
    let document = station_fixture::configuration(
        &[(PORT, "1000")],
        &[(
            PORT,
            ResourceKind::Radio,
            "",
            json!({"duplex_mode": "full"}),
        )],
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        document.clone(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let node = runtime.node(PORT).unwrap();
    node.site()
        .command(crate::site::SiteCommand::Inhibit(true), 0)
        .unwrap();
    station_fixture::reload(
        &mut runtime,
        document,
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    let mut audio = [0.25; 48];
    generation
        .transmit()
        .controller
        .process_audio(true, false, &[], &mut audio);
    let muted = audio.iter().all(|sample| *sample == 0.0);
    drop(generation);
    assert!(runtime.stop(0));
    assert!(
        muted,
        "replacement must start with the retained safety inhibit"
    );
}

#[test]
fn site_runtime_reload_keeps_history_and_original_pulse_deadline() {
    use crate::site::{
        FanOutput, Observation, OutputRequest, ReadingQuality, SiteCommand, SiteObservations,
    };
    let source = |pulse_ms: u64| {
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[
                (
                    PORT,
                    ResourceKind::Sensor,
                    "supply",
                    json!({"enabled":true,"source":{"type":"hwmon","device":"/sys/selected","channel":"in1_input"}}),
                ),
                (
                    PORT,
                    ResourceKind::Output,
                    "relay",
                    json!({"enabled":true,"source":{"type":"gpiod","device":"/dev/gpiochip0","line":"7"},"timing":{"pulse_ms":pulse_ms}}),
                ),
            ],
        )
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime =
        station_fixture::start(source(500), &Media, adapter, device, clock()).unwrap();
    let observations = SiteObservations {
        sensors: [(
            "supply".into(),
            Observation {
                value: Some(13.8),
                observed_at_ms: Some(1000),
                quality: ReadingQuality::Valid,
            },
        )]
        .into(),
        outputs: [(
            "relay".into(),
            FanOutput {
                available: true,
                pwm: false,
            },
        )]
        .into(),
        ..Default::default()
    };
    let site = runtime.node(PORT).unwrap().site();
    site.advance(1000, &observations, &Default::default());
    site.command(
        SiteCommand::Output {
            name: "relay".into(),
            request: OutputRequest::Pulse,
        },
        1000,
    )
    .unwrap();
    let mut now = clock();
    now.now_ms = 1200;
    station_fixture::reload(&mut runtime, source(900), &Media, adapter, device, now).unwrap();
    let snapshot = runtime.node(PORT).unwrap().site().snapshot(1200);
    assert_eq!(snapshot.outputs["relay"].pulse_deadline_ms, Some(1500));
    assert_eq!(snapshot.sensors["supply"].minimum.unwrap().value, 13.8);
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    runtime
        .node(PORT)
        .unwrap()
        .site()
        .advance(1500, &observations, &Default::default());
    assert!(!runtime.node(PORT).unwrap().site().snapshot(1500).outputs["relay"].requested_on);
    assert!(runtime.stop(1600));
}

#[test]
fn site_runtime_message_uses_current_measurement_and_reports_stale_as_unavailable() {
    use crate::site::{Observation, ReadingQuality, SiteObservations};
    for (now_ms, expected) in [
        (1000, "Temperature is 12.5."),
        (6000, "outdoor is unavailable."),
    ] {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = station_fixture::start(station_fixture::configuration(&[(PORT,"1000")], &[(PORT, ResourceKind::Sensor, "outdoor", json!({"enabled": true, "source": {"type": "hwmon", "device": "/sys/selected", "channel": "temp1_input"}})), (PORT, ResourceKind::Message, "site-temperature", json!({"bindings": {"temperature": "sensor:outdoor"}, "dtmf_id": "1"}))]), &CapturingMedia(messages.clone()), adapter, |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock()).unwrap();
        runtime.node(PORT).unwrap().site().advance(
            1000,
            &SiteObservations {
                sensors: [(
                    "outdoor".into(),
                    Observation {
                        value: Some(12.5),
                        observed_at_ms: Some(1000),
                        quality: ReadingQuality::Valid,
                    },
                )]
                .into(),
                ..Default::default()
            },
            &Default::default(),
        );
        runtime.queue_message(PORT, "1", None).unwrap();
        let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let mut generation = rx.acquire_pair(&mut tx).unwrap();
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut [0.0; 12001]);
        let mut now = clock();
        now.now_ms = now_ms;
        runtime.prepare_messages(now);
        assert_eq!(*messages.lock().unwrap(), [expected]);
        drop(generation);
        assert!(runtime.stop(now_ms));
    }
}

#[test]
fn site_report_captures_after_unkey_and_answers_only_the_requesting_peer() {
    use crate::site::{Observation, ReadingQuality, SiteObservations};
    for (selector, now_ms, expected) in [
        ("1", 1000, "Battery at 1,0,0,0 is 13.3 volts."),
        ("1", 6000, "Battery at 1,0,0,0 is unavailable."),
        ("2", 1000, "door is inactive."),
        ("99", 1000, "The requested site reading is unavailable."),
    ] {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = station_fixture::start(
            station_fixture::configuration(&[(PORT,"1000")], &[(PORT, ResourceKind::Sensor, "battery", json!({"enabled": true, "source": {"type": "hwmon", "device": "/sys/selected", "channel": "in1_input"}, "unit": "volts", "precision": 1, "dtmf_id": "1", "label": "Battery at { $node-key }"})), (PORT, ResourceKind::Input, "door", json!({"enabled": true, "source": {"type": "gpiod", "device": "/dev/gpiochip0", "line": "3"}, "dtmf_id": "2"}))]),
            &CapturingMedia(messages.clone()), adapter,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock(),
        ).unwrap();
        let peer = crate::link::PeerSignals::new();
        peer.set_radio_keyed(true);
        runtime
            .queue_site_report(PORT, selector, Some(peer.reply_target()))
            .unwrap();
        let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let mut generation = rx.acquire_pair(&mut tx).unwrap();
        let mut audio = [0.0; 960];
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
        runtime.prepare_messages(clock());
        assert!(messages.lock().unwrap().is_empty());
        runtime.node(PORT).unwrap().site().advance(
            1000,
            &SiteObservations {
                sensors: [(
                    "battery".into(),
                    Observation {
                        value: Some(13.26),
                        observed_at_ms: Some(1000),
                        quality: ReadingQuality::Valid,
                    },
                )]
                .into(),
                inputs: [(
                    "door".into(),
                    Observation {
                        value: Some(false),
                        observed_at_ms: Some(1000),
                        quality: ReadingQuality::Valid,
                    },
                )]
                .into(),
                ..Default::default()
            },
            &Default::default(),
        );
        peer.set_radio_keyed(false);
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
        runtime.prepare_messages(RuntimeClock { now_ms, ..clock() });
        assert_eq!(*messages.lock().unwrap(), [expected]);
        let mut parrot = [0.0; 960];
        let mut active = false;
        let mut replies = [crate::controller::PeerReplySample::default(); 960];
        generation.transmit().controller.process_audio_with_parrot(
            false,
            false,
            false,
            &[],
            &mut audio,
            crate::controller::ParrotOutput {
                samples: &mut parrot,
                active: &mut active,
                replies: Some(&mut replies),
                regeneration: None,
                local_media: Default::default(),
            },
        );
        assert!(audio.iter().all(|sample| *sample == 0.0));
        assert!(
            replies
                .iter()
                .any(|sample| sample.sample != 0.0 && sample.target == peer.reply_identity())
        );
        assert_eq!(
            runtime.node(PORT).unwrap().site().snapshot(1000).sensors["battery"]
                .current
                .value,
            Some(13.26),
            "display precision never rounds policy readings"
        );
        drop(generation);
        assert!(runtime.stop(now_ms));
    }
}

#[test]
fn site_report_rejects_empty_and_noncanonical_selectors_before_admission() {
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for selector in ["", "0", "01", "unknown"] {
        assert_eq!(
            runtime.queue_site_report(PORT, selector, None),
            Err(RuntimeError::Rejected)
        );
    }
    assert_eq!(
        runtime.queue_site_report("missing", "1", None),
        Err(RuntimeError::MissingNode)
    );
    for _ in 0..4 {
        runtime.queue_site_report(PORT, "99", None).unwrap();
    }
    assert_eq!(
        runtime.queue_site_report(PORT, "99", None),
        Err(RuntimeError::Rejected),
        "invalid selectors must not consume reply capacity"
    );
    assert!(runtime.stop(0));
}

#[test]
fn site_named_notification_retries_after_queue_pressure_and_plays_locally() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                SITE,
                ResourceKind::Message,
                "ready",
                json!({"content": "Ready", "dtmf_id": "1"}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert_eq!(
        runtime.cancel_message("missing"),
        Err(RuntimeError::MissingNode)
    );
    assert_eq!(
        runtime.queue_named_message("missing", "ready"),
        Err(RuntimeError::MissingNode)
    );
    assert_eq!(
        runtime.queue_named_message(PORT, "missing"),
        Err(RuntimeError::Preparation)
    );
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    for _ in 0..4 {
        runtime
            .queue_message(PORT, "1", Some(peer.reply_target()))
            .unwrap();
    }
    assert_eq!(runtime.queue_named_message(PORT, "ready"), Ok(false));
    runtime.prepare_messages(clock());
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    peer.end();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12001]);
    runtime.prepare_messages(clock());
    assert_eq!(runtime.queue_named_message(PORT, "ready"), Ok(true));
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0]);
    runtime.prepare_messages(clock());
    assert_eq!(*messages.lock().unwrap(), ["Ready"]);
    let mut audio = [0.0; 960];
    let mut parrot = [0.0; 960];
    let mut active = false;
    let mut replies = [crate::controller::PeerReplySample::default(); 960];
    generation.transmit().controller.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut audio,
        crate::controller::ParrotOutput {
            samples: &mut parrot,
            active: &mut active,
            replies: Some(&mut replies),
            regeneration: None,
            local_media: Default::default(),
        },
    );
    assert!(audio.iter().any(|sample| *sample != 0.0));
    assert!(replies.iter().all(|sample| sample.sample == 0.0));
    runtime.cancel_message(PORT).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut audio);
    assert_eq!(audio, [0.0; 960]);
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn site_alarm_uses_compiled_composition_and_registered_port_identity() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[
                (PORT, ResourceKind::Metadata, "", json!({"name":"Control room"})),
                (PORT, ResourceKind::Allstarlink, "", json!({"enabled":true,"node_number":98765})),
                (PORT, ResourceKind::Input, "door", json!({"enabled":true,"source":{"type":"gpiod","device":"/dev/gpiochip0","line":"3"}})),
                (
                    PORT,
                    ResourceKind::Alarm,
                    "door-open",
                    json!({"source":"door","condition":"inactive","trip_message":"{ station-identity }"}),
                ),
                (
                    PORT,
                    ResourceKind::Message,
                    "station-identity",
                    json!({"content": "{ $node-key } { $node-name } { $allstarlink-node-number }"}),
                ),
            ],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert_eq!(
        runtime.queue_alarm_message(PORT, "door-open", crate::site::AlarmEvent::Trip),
        Ok(true)
    );
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12001]);
    runtime.prepare_messages(clock());
    assert_eq!(
        *messages.lock().unwrap(),
        ["1,0,0,0 Control room 9,8,7,6,5"]
    );
    drop(generation);
    assert!(runtime.stop(1000));
}

#[test]
fn composition_runtime_ids_and_tails_keep_identification_with_unavailable_meter() {
    use crate::site::{Observation, ReadingQuality, SiteObservations};
    for family in ["identifier", "tail"] {
        for valid in [true, false] {
            let messages = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut fields = json!({"content":"Repeater. { site-temperature } { site-time }"});
            let kind = if family == "identifier" {
                fields["timing"] = json!({"interval_ms":1});
                fields["regardless_of_activity"] = json!(true);
                ResourceKind::Identifier
            } else {
                ResourceKind::Tail
            };
            let document = station_fixture::configuration(
                &[(PORT, "radio")],
                &[
                    (
                        PORT,
                        ResourceKind::TxSignaling,
                        "",
                        json!({"ptt":{"timing":{"hang_ms":0}}}),
                    ),
                    (
                        PORT,
                        ResourceKind::RxSignaling,
                        "",
                        json!({"timing":{"kerchunk_max_ms":0}}),
                    ),
                    (
                        PORT,
                        ResourceKind::Sensor,
                        "outdoor",
                        json!({"enabled":true,"source":{"type":"hwmon","device":"/sys/selected","channel":"temp1_input"}}),
                    ),
                    (
                        PORT,
                        ResourceKind::Message,
                        "site-temperature",
                        json!({"bindings":{"temperature":"sensor:outdoor"}}),
                    ),
                    (
                        PORT,
                        ResourceKind::Message,
                        "site-time",
                        json!({"bindings":{"time":"time"}}),
                    ),
                    (PORT, kind, "regular", fields),
                ],
            );
            let mut runtime = station_fixture::start(
                document,
                &CapturingMedia(messages.clone()),
                adapter,
                |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
                clock(),
            )
            .unwrap();
            let readings = SiteObservations {
                sensors: [(
                    "outdoor".into(),
                    Observation {
                        value: Some(12.5),
                        observed_at_ms: Some(1000),
                        quality: if valid {
                            ReadingQuality::Valid
                        } else {
                            ReadingQuality::Fault
                        },
                    },
                )]
                .into(),
                ..Default::default()
            };
            runtime
                .node(PORT)
                .unwrap()
                .site()
                .advance(1000, &readings, &Default::default());
            let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
            let mut generation = rx.acquire_pair(&mut tx).unwrap();
            generation.transmit().controller.process_audio(
                family == "tail",
                false,
                &[],
                &mut [0.0; 960],
            );
            generation
                .transmit()
                .controller
                .process_audio(false, false, &[], &mut [0.0; 960]);
            runtime.prepare_messages(RuntimeClock {
                now_ms: 1000,
                ..clock()
            });
            let reading = if valid {
                "Temperature is 12.5."
            } else {
                "outdoor is unavailable."
            };
            assert_eq!(
                *messages.lock().unwrap(),
                ["Repeater.", reading, "The time is 9:07 AM."],
                "{family} valid={valid}"
            );
            drop(generation);
            assert!(runtime.stop(1000));
        }
    }
}

#[test]
fn composition_runtime_tail_resolves_only_at_each_release_attempt() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[
                (
                    PORT,
                    ResourceKind::TxSignaling,
                    "",
                    json!({"ptt": {"timing": {"hang_ms": 0}}}),
                ),
                (
                    PORT,
                    ResourceKind::RxSignaling,
                    "",
                    json!({"timing": {"kerchunk_max_ms": 0}}),
                ),
                (
                    PORT,
                    ResourceKind::Tail,
                    "time",
                    json!({"content": "{ time }"}),
                ),
                (
                    PORT,
                    ResourceKind::Message,
                    "time",
                    json!({"content": "{ site-time }", "bindings": {"time": "time"}}),
                ),
            ],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    let mut audio = [0.0; 960];
    for minute in [7, 8] {
        generation
            .transmit()
            .controller
            .process_audio(true, false, &[], &mut audio);
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
        let mut now = clock();
        now.civil = Some((
            CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, minute).unwrap(),
            0,
        ));
        runtime.prepare_messages(now);
        for _ in 0..22 {
            generation
                .transmit()
                .controller
                .process_audio(false, false, &[], &mut audio);
        }
    }
    assert_eq!(
        *messages.lock().unwrap(),
        ["The time is 9:07 AM.", "The time is 9:08 AM."]
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn composition_runtime_recurring_id_resolves_at_each_attempt() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(&[(PORT,"1000")], &[(PORT, ResourceKind::Identifier, "regular", json!({"timing": {"interval_ms": 1}, "regardless_of_activity": true, "content": "{ time }"})), (PORT, ResourceKind::Message, "time", json!({"content": "{ site-time }", "bindings": {"time": "time"}}))]),
        &CapturingMedia(messages.clone()), adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock(),
    ).unwrap();
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    let mut audio = [0.0; 960];
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut audio);
    assert!(audio.iter().all(|value| *value == 0.0));
    runtime.prepare_messages(clock());
    assert_eq!(*messages.lock().unwrap(), ["The time is 9:07 AM."]);
    for _ in 0..22 {
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
    }
    let mut later = clock();
    later.civil = Some((
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 8).unwrap(),
        0,
    ));
    runtime.prepare_messages(later);
    assert_eq!(
        *messages.lock().unwrap(),
        ["The time is 9:07 AM.", "The time is 9:08 AM."]
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn composition_runtime_command_waits_for_source_unkey_and_uses_configured_audience() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Message,
                "time",
                json!({"content": "{ site-time }", "bindings": {"time": "time"}, "dtmf_id": "1"}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    runtime
        .queue_message(PORT, "1", Some(peer.reply_target()))
        .unwrap();
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    let mut audio = [0.0; 960];
    let mut parrot = [0.0; 960];
    let mut parrot_active = false;
    let mut replies = [crate::controller::PeerReplySample::default(); 960];
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut audio);
    runtime.prepare_messages(clock());
    assert!(messages.lock().unwrap().is_empty());
    peer.set_radio_keyed(false);
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut audio);
    runtime.prepare_messages(clock());
    assert_eq!(*messages.lock().unwrap(), ["The time is 9:07 AM."]);
    generation.transmit().controller.process_audio_with_parrot(
        false,
        false,
        false,
        &[],
        &mut audio,
        crate::controller::ParrotOutput {
            samples: &mut parrot,
            active: &mut parrot_active,
            replies: Some(&mut replies),
            regeneration: None,
            local_media: Default::default(),
        },
    );
    assert!(audio.contains(&0.25));
    assert!(replies.iter().all(|sample| sample.sample == 0.0));
    let other = crate::link::PeerSignals::new();
    peer.end();
    assert_eq!(
        runtime.queue_message(PORT, "1", Some(peer.reply_target())),
        Err(RuntimeError::Rejected)
    );
    assert!(
        runtime
            .queue_message(PORT, "1", Some(other.reply_target()))
            .is_ok()
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn composition_runtime_full_queue_retries_after_adapter_replacement() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Message,
                "time",
                json!({"content": "{ site-time }", "bindings": {"time": "time"}, "dtmf_id": "1"}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    for _ in 0..4 {
        runtime
            .queue_message(PORT, "1", Some(peer.reply_target()))
            .unwrap();
    }
    assert_eq!(
        runtime.queue_message(PORT, "1", None),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_message(PORT, "2", None),
        Err(RuntimeError::Rejected)
    );
    runtime.prepare_messages(clock());
    assert!(messages.lock().unwrap().is_empty());
    let settings = runtime.settings(PORT).unwrap().clone();
    assert_eq!(
        runtime.replace_adapter(PORT, adapter("1000", &settings).unwrap(), 1001),
        Ok(2)
    );
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    peer.end();
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12001]);
    runtime.prepare_messages(clock());
    runtime.queue_message(PORT, "1", None).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0]);
    let mut later = clock();
    later.civil = Some((
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 8).unwrap(),
        0,
    ));
    runtime.prepare_messages(later);
    assert_eq!(*messages.lock().unwrap(), ["The time is 9:08 AM."]);
    drop(generation);
    assert!(runtime.stop(1002));
}

/// Complete a registered digit sequence through the same collector and dispatch owner as production.
fn enter_mapping(
    runtime: &mut Runtime,
    local: &str,
    peer: Option<&crate::link::PeerReply>,
    digits: &str,
    now_ms: u64,
) -> Result<links::LinkEffect, RuntimeError> {
    let mut result = None;
    for digit in digits.chars() {
        let outcome = match peer {
            Some(reply) => runtime.node(local).unwrap().links().peer_digit(
                "2000",
                digit,
                now_ms,
                reply.clone(),
            ),
            None => runtime.digit(local, super::dtmf::DigitEvent::Digit { digit, now_ms }),
        };
        if let Some(super::dtmf::DigitOutcome::Mapped(input)) = outcome {
            assert!(result.is_none());
            result =
                Some(runtime.dispatch_mapping(local, input, RuntimeClock { now_ms, ..clock() }));
        }
    }
    result.expect("one completed registered command")
}

#[test]
fn pad_test_queues_one_local_stream_after_unkey() {
    use crate::runtime::dtmf::{DigitEvent, DigitOutcome, PadTestResult};
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*82#", 1),
        Ok(links::LinkEffect::None)
    ));
    for digit in "*#ABCD".chars() {
        assert!(
            runtime
                .digit(PORT, DigitEvent::Digit { digit, now_ms: 1 })
                .is_none()
        );
    }
    assert!(messages.lock().unwrap().is_empty());
    let Some(DigitOutcome::PadTest(report)) = runtime.digit(PORT, DigitEvent::Unkey { now_ms: 2 })
    else {
        panic!("pad result")
    };
    runtime.queue_pad_test(PORT, &report).unwrap();
    assert_eq!(*messages.lock().unwrap(), ["star pound A B C D"]);
    assert!(
        runtime
            .digit(PORT, DigitEvent::Unkey { now_ms: 3 })
            .is_none()
    );
    for _ in 0..3 {
        runtime.queue_pad_test(PORT, &report).unwrap();
    }
    assert_eq!(
        runtime.queue_pad_test(PORT, &report),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_pad_test("missing", &report),
        Err(RuntimeError::MissingNode)
    );
    assert_eq!(
        runtime.queue_pad_test(
            PORT,
            &PadTestResult {
                digits: "invalid".into(),
                overflowed: false
            }
        ),
        Err(RuntimeError::Preparation)
    );
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    let mut audio = [0.0; 960];
    for _ in 0..15 {
        generation
            .transmit()
            .controller
            .process_audio(true, false, &[], &mut audio);
        assert!(audio.iter().all(|sample| *sample == 0.0));
    }
    for _ in 0..14 {
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
    }
    assert!(audio.iter().any(|sample| *sample != 0.0));
    drop(generation);
    assert!(runtime.stop(0));
}
impl NativeFilePreparer for CapturingMedia {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeSpeechPreparer for CapturingMedia {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeMediaPreparer for CapturingMedia {
    fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        Ok(Box::new(TestStation(Some(self.0.clone()))))
    }
}
struct Device(Arc<Mutex<Vec<String>>>);
impl DeviceHandoff for Device {
    fn quiesce(&mut self) -> bool {
        self.0.lock().unwrap().push("quiesce".into());
        true
    }
    fn close(&mut self) {
        self.0.lock().unwrap().push("close".into());
    }
    fn open(&mut self, settings: &GenerationSettings) -> bool {
        self.0.lock().unwrap().push(settings.device.clone());
        settings.device != "fail"
    }
}
fn clock() -> RuntimeClock {
    RuntimeClock {
        now_ms: 1000,
        wall_seconds: 100,
        wall_subsecond_ms: 0,
        civil: Some((
            CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 7).unwrap(),
            0,
        )),
    }
}
fn adapter(_: &str, _: &ResolvedNodeSettings) -> Result<PreparedAdapter<()>, RuntimeError> {
    Ok(PreparedAdapter {
        remote_commands: None,
        state: (),
        control: (),
        receive_maximum: 960,
        transmit_maximum: 960,
    })
}

#[test]
fn registered_radio_mapping_uses_shared_owner_and_rejects_collisions_before_device_acquisition() {
    use crate::{
        remote_base_command::{RemoteCommand, RemoteCommandConfiguration, RemoteDtmfAccess},
        runtime::dtmf::{DigitEvent, DigitOutcome},
    };
    for key in ["*651", "*6651"] {
        let config = station_fixture::configuration(
            &[(PORT, "radio")],
            &[(
                PORT,
                ResourceKind::Command,
                "",
                json!({"mappings":{"radio_status_51":{"command":key,"protected":false,"action":"radio","operation":"status","detail":"long"}}}),
            )],
        );
        let mut runtime = station_fixture::start(
            config,
            &Media,
            |n, s| {
                let mut adapter = adapter(n, s)?;
                adapter.remote_commands = Some(RemoteCommandConfiguration {
                    prefix: "65".into(),
                    access: RemoteDtmfAccess::Open,
                });
                Ok(adapter)
            },
            |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
            clock(),
        )
        .unwrap();
        let mut mapped = None;
        for digit in key.chars().chain(['#']) {
            mapped = runtime.digit(
                PORT,
                DigitEvent::Digit {
                    digit,
                    now_ms: 1000,
                },
            );
        }
        let Some(DigitOutcome::Mapped(input)) = mapped else {
            panic!("complete radio mapping")
        };
        let links::LinkEffect::RemoteBase(op) =
            runtime.dispatch_mapping(PORT, input, clock()).unwrap()
        else {
            panic!("radio owner")
        };
        assert_eq!(op.remote, Some(RemoteCommand::Status { long: true }));
        assert!(runtime.stop(1002));
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let duplicate = crate::capture::Fixture::new(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::Command,
            "",
            json!({"mappings":{
                "radio_status_51":{"command":"*70","protected":false,"action":"radio","operation":"status","detail":"long"}
            }}),
        )],
    );
    assert!(matches!(
        duplicate.candidate().command_language(
            crate::config::resources::candidate::ResourceScope::Port(PORT.parse().unwrap())
        ),
        Err(crate::command_language::LanguageError::CodeCollision)
    ));
    let bad = |name: &str, settings: &ResolvedNodeSettings| {
        let mut prepared = adapter(name, settings)?;
        prepared.remote_commands = Some(RemoteCommandConfiguration {
            prefix: "#".into(),
            access: RemoteDtmfAccess::Open,
        });
        Ok(prepared)
    };
    assert!(
        station_fixture::start(
            station_fixture::single_port(true),
            &Media,
            bad,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
            clock()
        )
        .is_err()
    );
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn remote_completions_renew_only_the_requesting_peer_and_compose_its_reply() {
    use crate::remote_base_command::{RemoteCommand, RemoteDtmfAccess, RemoteReplyPart};
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "radio")],
            &[(
                PORT,
                ResourceKind::Echolink,
                "",
                json!({"telemetry":"demand"}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.incoming(PORT, "2000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    let links = runtime.node(PORT).unwrap().links();
    links.configure_admin(&digest, &digest, 100);
    links
        .configure_radio_owner(Some(
            &crate::remote_base_command::RemoteCommandConfiguration {
                prefix: "6".into(),
                access: RemoteDtmfAccess::Locked,
            },
        ))
        .unwrap();
    links
        .attach_ingress(
            crate::link::PeerIdentity::from_operator("2000").unwrap(),
            reply.clone(),
        )
        .unwrap();
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, Some(&reply), "*8001234#", 1001),
        Ok(links::LinkEffect::None)
    ));
    let links::LinkEffect::RemoteBase(operation) =
        enter_mapping(&mut runtime, PORT, Some(&reply), "*661#", 1002).unwrap()
    else {
        panic!("authorized remote request")
    };
    let revision = runtime.revision();
    assert!(runtime.remote_command_authorized(PORT, revision, &operation, 1003));
    assert!(!runtime.remote_command_authorized(PORT, revision + 1, &operation, 1003));
    assert!(!runtime.remote_command_authorized("missing", revision, &operation, 1003));
    runtime.complete_remote_command("missing", &operation, 1050);
    runtime.complete_remote_command(PORT, &operation, 1050);
    assert!(runtime.remote_command_authorized(PORT, revision, &operation, 1149));
    let mut query = operation.clone();
    query.remote = Some(RemoteCommand::Status { long: true });
    runtime.complete_remote_command(PORT, &query, 1149);
    assert!(!runtime.remote_command_authorized(PORT, revision, &operation, 1150));
    let parts = [
        RemoteReplyPart {
            id: "remote-applied",
            arguments: Default::default(),
        },
        RemoteReplyPart {
            id: "remote-applied",
            arguments: Default::default(),
        },
    ];
    runtime
        .queue_remote_reply(PORT, &parts, Some(reply.clone()), 1151)
        .unwrap();
    assert_eq!(
        messages.lock().unwrap().last().map(String::as_str),
        Some("Remote radio command completed. Remote radio command completed.")
    );
    assert!(runtime.echo_telemetry_allows(PORT, &reply, 1151));
    runtime.node(PORT).unwrap().links().ended("2000");
    runtime.peer_detached(PORT, "2000", 1152);
    assert!(runtime.stop(1152));
    assert!(!runtime.remote_command_authorized(PORT, revision, &query, 1153));
}

struct SnapshotDevice {
    node: String,
    snapshots: Arc<Mutex<Vec<(String, u64)>>>,
}
impl DeviceHandoff for SnapshotDevice {
    fn quiesce(&mut self) -> bool {
        true
    }
    fn close(&mut self) {}
    fn open(&mut self, settings: &GenerationSettings) -> bool {
        self.snapshots
            .lock()
            .unwrap()
            .push((self.node.clone(), settings.status_snapshot_interval_ms));
        true
    }
}

#[test]
fn runtime_passes_resolved_snapshot_interval_to_each_device_generation() {
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000"), (SECOND, "2000")],
            &[
                (
                    SITE,
                    ResourceKind::Radio,
                    "",
                    json!({"timing": {"status_snapshot_interval_ms": 80}}),
                ),
                (
                    SECOND,
                    ResourceKind::Radio,
                    "",
                    json!({"timing": {"status_snapshot_interval_ms": 25}}),
                ),
            ],
        ),
        &Media,
        adapter,
        |name, _| {
            Ok(Box::new(SnapshotDevice {
                node: name.to_owned(),
                snapshots: snapshots.clone(),
            }) as Box<dyn DeviceHandoff>)
        },
        clock(),
    )
    .unwrap();

    assert_eq!(
        *snapshots.lock().unwrap(),
        vec![(PORT.to_owned(), 80), (SECOND.to_owned(), 25)]
    );
    assert!(runtime.stop(0));
}

#[test]
fn reload_waits_for_activation_and_restores_old_generation_after_a_delayed_failure() {
    let mut devices = station_fixture::DevicePlans::default();
    struct OpeningDevice {
        current: String,
        ready: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
    }
    impl DeviceHandoff for OpeningDevice {
        fn quiesce(&mut self) -> bool {
            self.current != "second" || self.release.load(Ordering::Acquire)
        }
        fn close(&mut self) {
            self.log
                .lock()
                .unwrap()
                .push(format!("close:{}", self.current));
        }
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.current.clone_from(&settings.device);
            self.log
                .lock()
                .unwrap()
                .push(format!("open:{}", self.current));
            true
        }
        fn activation_ready(&mut self) -> Result<bool, RuntimeError> {
            if self.current != "second" {
                return Ok(true);
            }
            match self.ready.load(Ordering::Acquire) {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(RuntimeError::Device),
            }
        }
    }
    for (success, abandon_pending) in [
        (None, false),
        (None, true),
        (Some(false), false),
        (Some(true), false),
    ] {
        let ready = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(OpeningDevice {
                current: String::new(),
                ready: ready.clone(),
                release: release.clone(),
                log: log.clone(),
            }) as Box<dyn DeviceHandoff>)
        };
        let initial = station_fixture::configuration(
            &[(PORT, "1000"), (SECOND, "2000")],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "first"}}),
                ),
                (
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "other"}}),
                ),
            ],
        );
        let mut runtime = devices
            .start(initial, &Media, adapter, device, clock())
            .unwrap();
        let revision = runtime.revision();
        let candidate = station_fixture::configuration(
            &[(PORT, "1000"), (SECOND, "2000")],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "second"}}),
                ),
                (
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "other"}}),
                ),
            ],
        );
        devices
            .reload(&mut runtime, candidate, &Media, adapter, device, clock())
            .unwrap();
        assert!(runtime.reload_pending());
        assert_eq!(
            devices.reload(
                &mut runtime,
                station_fixture::single_port(true),
                &Media,
                adapter,
                device,
                clock(),
            ),
            Err(RuntimeError::Busy),
            "a pending handoff cannot be replaced by another reload"
        );
        assert_eq!(runtime.revision(), revision);
        assert_eq!(runtime.poll_reload(clock()), Ok(false));
        assert_eq!(
            station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
            "first"
        );
        // An unrelated active node keeps its control/link path during this wait.
        runtime.tick_links(clock());
        assert!(runtime.node(SECOND).unwrap().status(0).active.is_some());
        let Some(success) = success else {
            assert!(!runtime.stop(0));
            assert!(runtime.reload_pending());
            if abandon_pending {
                let retained_owners = Arc::strong_count(&ready);
                drop(runtime);
                assert_eq!(Arc::strong_count(&ready), retained_owners);
                assert!(
                    !log.lock()
                        .unwrap()
                        .iter()
                        .any(|entry| entry == "close:second"),
                    "dropping a runtime cannot close an unquiesced replacement"
                );
                continue;
            }
            release.store(true, Ordering::Release);
            assert!(runtime.stop(0));
            assert!(!runtime.reload_pending());
            continue;
        };
        ready.store(if success { 1 } else { 2 }, Ordering::Release);
        if success {
            assert_eq!(runtime.poll_reload(clock()), Ok(true));
            assert_eq!(runtime.revision(), revision + 1);
            assert_eq!(
                station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
                "second"
            );
        } else {
            assert_eq!(
                runtime.poll_reload(clock()),
                Ok(false),
                "retain replacement until safe to close"
            );
            assert!(runtime.reload_pending());
            release.store(true, Ordering::Release);
            assert_eq!(runtime.poll_reload(clock()), Err(RuntimeError::Device));
            assert_eq!(runtime.revision(), revision);
            assert_eq!(
                station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
                "first"
            );
            assert!(
                log.lock()
                    .unwrap()
                    .ends_with(&["close:second".into(), "open:first".into()])
            );
        }
        release.store(true, Ordering::Release);
        assert!(!runtime.reload_pending());
        assert!(runtime.stop(0));
    }
}

#[test]
fn changed_radio_settings_reopen_the_device_during_configuration_reload() {
    let mut devices = station_fixture::DevicePlans::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fallback = Device(log.clone());
    assert_eq!(fallback.finish_update(false), Ok(()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = devices
        .start(
            station_fixture::single_port(true),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    log.lock().unwrap().clear();

    devices
        .reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"gpio": {"wiring_profile": "nhrc"}}),
                )],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();

    let events = log.lock().unwrap().clone();
    assert_eq!(events, ["quiesce", "close", "1000"]);
    assert_eq!(
        runtime
            .modular_configuration()
            .unwrap()
            .radio
            .port(PORT.parse().unwrap())
            .unwrap()
            .interface
            .gpio
            .wiring_profile,
        crate::config::resources::profiles::radio::WiringProfile::Nhrc
    );
    assert!(runtime.stop(0));
}

#[test]
fn reload_retains_new_leases_until_reverse_order_cleanup_is_safe() {
    let mut devices = station_fixture::DevicePlans::default();
    struct StagedDevice {
        channel: String,
        release: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
    }
    impl DeviceHandoff for StagedDevice {
        fn quiesce(&mut self) -> bool {
            self.channel != "fail" || self.release.load(Ordering::Acquire)
        }
        fn close(&mut self) {
            self.log.lock().unwrap().push(self.channel.clone());
        }
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.channel.clone_from(&settings.device);
            self.channel != "fail"
        }
        fn activation_ready(&mut self) -> Result<bool, RuntimeError> {
            Ok(self.channel != "pending")
        }
    }
    let release = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(StagedDevice {
            channel: String::new(),
            release: release.clone(),
            log: log.clone(),
        }) as Box<dyn DeviceHandoff>)
    };
    let initial = station_fixture::single_port(true);
    let mut runtime = devices
        .start(initial.clone(), &Media, adapter, device, clock())
        .unwrap();
    devices
        .reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000"), (THIRD, "3000")],
                &[
                    (
                        SECOND,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "pending"}}),
                    ),
                    (
                        THIRD,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "fail"}}),
                    ),
                ],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert!(runtime.reload_pending());
    assert!(Arc::ptr_eq(
        runtime.modular_configuration().unwrap(),
        &initial
    ));
    assert_eq!(runtime.revision(), 1);
    assert!(log.lock().unwrap().is_empty());
    assert!(runtime.node(PORT).unwrap().status(0).active.is_some());
    release.store(true, Ordering::Release);
    assert_eq!(runtime.poll_reload(clock()), Err(RuntimeError::Device));
    assert_eq!(*log.lock().unwrap(), ["fail", "pending"]);
    assert!(!runtime.reload_pending());
    assert!(runtime.stop(0));
}

#[test]
fn reload_waits_for_restored_readiness_and_retains_failed_restoration_until_safe() {
    let mut devices = station_fixture::DevicePlans::default();
    struct RestoringDevice {
        channel: String,
        opens: usize,
        failed: bool,
        ready: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
    }
    impl DeviceHandoff for RestoringDevice {
        fn quiesce(&mut self) -> bool {
            !self.failed || self.release.load(Ordering::Acquire)
        }
        fn close(&mut self) {}
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.channel.clone_from(&settings.device);
            self.opens += 1;
            self.channel != "fail"
        }
        fn activation_ready(&mut self) -> Result<bool, RuntimeError> {
            if self.opens == 1 {
                return Ok(true);
            }
            match self.ready.load(Ordering::Acquire) {
                0 => Ok(false),
                1 => Ok(true),
                _ => {
                    self.failed = true;
                    Err(RuntimeError::Device)
                }
            }
        }
    }
    for success in [true, false] {
        let ready = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(RestoringDevice {
                channel: String::new(),
                opens: 0,
                failed: false,
                ready: ready.clone(),
                release: release.clone(),
            }) as Box<dyn DeviceHandoff>)
        };
        let initial = station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "old"}}),
            )],
        );
        let mut runtime = devices
            .start(initial.clone(), &Media, adapter, device, clock())
            .unwrap();
        let (mut rx, _tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        devices
            .reload(
                &mut runtime,
                station_fixture::configuration(
                    &[(PORT, "1000")],
                    &[(
                        PORT,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "fail"}}),
                    )],
                ),
                &Media,
                adapter,
                device,
                clock(),
            )
            .unwrap();
        assert!(runtime.reload_pending());
        assert!(rx.acquire().is_none());
        assert!(
            runtime.node(PORT).unwrap().work().is_none(),
            "restoration admitted incumbent work before physical readiness"
        );
        assert!(Arc::ptr_eq(
            runtime.modular_configuration().unwrap(),
            &initial
        ));
        ready.store(if success { 1 } else { 2 }, Ordering::Release);
        if !success {
            assert_eq!(runtime.poll_reload(clock()), Ok(false));
            assert!(runtime.reload_pending());
            assert!(rx.acquire().is_none());
            release.store(true, Ordering::Release);
        }
        assert_eq!(
            runtime.poll_reload(clock()),
            Err(if success {
                RuntimeError::Device
            } else {
                RuntimeError::Restore
            })
        );
        assert_eq!(runtime.revision(), 1);
        assert!(Arc::ptr_eq(
            runtime.modular_configuration().unwrap(),
            &initial
        ));
        assert_eq!(rx.acquire().is_some(), success);
        assert_eq!(
            runtime.node(PORT).unwrap().work().map(|permit| permit.id()),
            success.then_some(1)
        );
        runtime.detached(PORT, 1);
        assert!(runtime.stop(0));
    }
}

#[test]
fn reload_waits_for_a_callback_acquired_during_device_quiescence() {
    use std::sync::Barrier;
    struct RacingDevice {
        armed: Arc<AtomicBool>,
        enter: Arc<Barrier>,
        acquired: Arc<Barrier>,
        closes: Arc<AtomicUsize>,
    }
    impl DeviceHandoff for RacingDevice {
        fn quiesce(&mut self) -> bool {
            if self.armed.swap(false, Ordering::AcqRel) {
                self.enter.wait();
                self.acquired.wait();
            }
            true
        }
        fn close(&mut self) {
            self.closes.fetch_add(1, Ordering::Relaxed);
        }
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
    }
    let armed = Arc::new(AtomicBool::new(false));
    let enter = Arc::new(Barrier::new(2));
    let acquired = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let closes = Arc::new(AtomicUsize::new(0));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(RacingDevice {
            armed: armed.clone(),
            enter: enter.clone(),
            acquired: acquired.clone(),
            closes: closes.clone(),
        }) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "old"}}),
            )],
        ),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let (mut rx, _tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    armed.store(true, Ordering::Release);
    std::thread::scope(|scope| {
        let callback = scope.spawn(|| {
            enter.wait();
            let guard = rx.acquire().unwrap();
            acquired.wait();
            release.wait();
            drop(guard);
        });
        station_fixture::reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "new"}}),
                )],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
        assert!(runtime.reload_pending());
        assert_eq!(closes.load(Ordering::Relaxed), 0);
        release.wait();
        callback.join().unwrap();
    });
    assert_eq!(runtime.poll_reload(clock()), Ok(true));
    assert_eq!(
        station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
        "new"
    );
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert!(runtime.detached(PORT, 1));
    assert!(runtime.stop(0));
}

#[test]
fn supported_radio_updates_keep_the_device_and_rollback_if_a_later_open_fails() {
    let mut devices = station_fixture::DevicePlans::default();
    struct LiveDevice {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
        failure: u8,
    }
    impl DeviceHandoff for LiveDevice {
        fn quiesce(&mut self) -> bool {
            self.log
                .lock()
                .unwrap()
                .push(format!("quiesce:{}", self.name));
            true
        }
        fn close(&mut self) {
            self.log
                .lock()
                .unwrap()
                .push(format!("close:{}", self.name));
        }
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.log
                .lock()
                .unwrap()
                .push(format!("open:{}", settings.device));
            settings.device != "fail"
        }
        fn update(&mut self, settings: &GenerationSettings) -> Result<bool, RuntimeError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("update:{}", settings.device));
            if self.failure >= 3 {
                Err(RuntimeError::Device)
            } else {
                Ok(true)
            }
        }
        fn finish_update(&mut self, commit: bool) -> Result<(), RuntimeError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("finish:{}:{commit}", self.name));
            if self.name == "1000"
                && ((commit && self.failure == 2) || (!commit && self.failure == 4))
            {
                Err(RuntimeError::Restore)
            } else {
                Ok(())
            }
        }
    }
    for failure in 0..7 {
        let fail_later = failure == 1;
        let commit_failure = failure == 2;
        let log = Arc::new(Mutex::new(Vec::new()));
        let device = |name: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(LiveDevice {
                name: if name == PORT {
                    "1000".into()
                } else if name == SECOND {
                    "2000".into()
                } else if name == THIRD {
                    "3000".into()
                } else {
                    "4000".into()
                },
                log: log.clone(),
                failure,
            }) as Box<dyn DeviceHandoff>)
        };
        let configuration = |second: &str, gain: i32, fourth: bool| {
            let mut ports = vec![(PORT, "1000"), (SECOND, "2000"), (THIRD, "3000")];
            let mut resources = vec![
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"device_identifier":"first","input_gain_db":gain}}),
                ),
                (
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"device_identifier":second}}),
                ),
                (
                    THIRD,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"device_identifier":"third"}}),
                ),
            ];
            if fourth {
                ports.push((FOURTH, "4000"));
                resources.push((
                    FOURTH,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"device_identifier":"fourth"}}),
                ));
            }
            station_fixture::configuration(&ports, &resources)
        };
        let initial = configuration("second", 0, false);
        let candidate_bounds = std::cell::Cell::new(false);
        let runtime_adapter = |name: &str, settings: &ResolvedNodeSettings| {
            let mut prepared = adapter(name, settings)?;
            if candidate_bounds.get() && name == PORT {
                match failure {
                    5 => prepared.receive_maximum += 1,
                    6 => prepared.transmit_maximum += 1,
                    _ => {}
                }
            }
            Ok(prepared)
        };
        let mut runtime = devices
            .start(initial.clone(), &Media, runtime_adapter, device, clock())
            .unwrap();
        log.lock().unwrap().clear();
        let candidate = configuration(
            if fail_later {
                "fail"
            } else if commit_failure {
                "replacement"
            } else {
                "second"
            },
            -6,
            commit_failure,
        );
        candidate_bounds.set(true);
        let result = devices.reload(
            &mut runtime,
            candidate,
            &Media,
            runtime_adapter,
            device,
            clock(),
        );
        let changes = log.lock().unwrap().clone();
        assert_eq!(
            changes.iter().any(|value| value == "update:first"),
            failure < 5
        );
        if failure >= 5 {
            assert!(changes.contains(&"close:1000".to_owned()));
            assert!(changes.contains(&"open:first".to_owned()));
        }
        if !commit_failure && failure != 4 && failure < 5 {
            assert!(!changes.iter().any(|value| value == "quiesce:1000"
                || value == "close:1000"
                || value == "open:first"));
        }
        if matches!(failure, 3 | 4) {
            assert_eq!(
                result,
                Err(if failure == 4 {
                    RuntimeError::Restore
                } else {
                    RuntimeError::Device
                })
            );
            assert!(changes.contains(&"finish:1000:false".to_owned()));
            assert_eq!(changes.contains(&"close:1000".to_owned()), failure == 4);
            assert_eq!(runtime.revision(), 1);
            assert!(Arc::ptr_eq(
                runtime.modular_configuration().unwrap(),
                &initial
            ));
        } else if commit_failure {
            assert_eq!(result, Err(RuntimeError::Restore));
            for name in ["1000", "2000", "4000"] {
                assert!(
                    changes.contains(&format!("close:{name}")),
                    "{name}: {changes:?}"
                );
            }
            assert!(!changes.contains(&"close:3000".to_owned()));
            assert_eq!(runtime.revision(), 1);
        } else if fail_later {
            assert_eq!(result, Err(RuntimeError::Device));
            assert!(changes.iter().any(|value| value == "finish:1000:false"));
            assert_eq!(runtime.revision(), 1);
            assert!(Arc::ptr_eq(
                runtime.modular_configuration().unwrap(),
                &initial
            ));
        } else {
            assert_eq!(result, Ok(()));
            assert_eq!(runtime.revision(), 2);
        }
        assert!(runtime.stop(0));
    }
}

#[test]
fn admission_loss_discards_partial_commands_on_every_active_node() {
    use crate::runtime::dtmf::DigitEvent;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000"), (SECOND, "2000")],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "first"}}),
                ),
                (
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "second"}}),
                ),
            ],
        ),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for local in [PORT, SECOND] {
        for digit in ['*', '7'] {
            assert!(
                runtime
                    .digit(local, DigitEvent::Digit { digit, now_ms: 1 })
                    .is_none()
            );
        }
    }
    runtime.discard_digits();
    for local in [PORT, SECOND] {
        assert!(
            runtime
                .digit(
                    local,
                    DigitEvent::Digit {
                        digit: '0',
                        now_ms: 2
                    }
                )
                .is_none()
        );
        assert!(
            runtime
                .digit(local, DigitEvent::Unkey { now_ms: 2 })
                .is_none(),
            "discarded input must not become a command"
        );
        assert!(matches!(
            enter_mapping(&mut runtime, local, None, "*70#", 3),
            Ok(links::LinkEffect::Telemetry(LinkAction::Status, None))
        ));
    }
    assert!(runtime.stop(5));
    runtime.discard_digits();
}

#[test]
fn message_only_event_has_no_link_effect_and_unexposed_handoff_reclaims_automatically() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = station_fixture::start(
        scheduled_configuration(
            json!([{"action":"message","message":"Hello","destination":"local"}]),
            0,
        ),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    assert!(runtime.next_link().is_none());
    let at = event_clock(120);
    let Some((links::LinkEffect::None, receipt)) = runtime.advance_commands(PORT, at).unwrap()
    else {
        panic!("standalone message")
    };
    receipt.settle(true);
    assert!(runtime.advance_commands(PORT, at).unwrap().is_none());
    station_fixture::reload(
        &mut runtime,
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "new"}}),
            )],
        ),
        &Media,
        adapter,
        device,
        event_clock(121),
    )
    .unwrap();
    assert_eq!(runtime.node(PORT).unwrap().status(0).retiring, None);
    assert_eq!(
        station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
        "new"
    );
    assert!(runtime.stop(0));
}

#[test]
fn retired_exposed_dispatcher_requires_detachment_even_when_current_was_never_exposed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert!(runtime.adapter_control_mut(PORT, 1).is_some());
    let settings = runtime.settings(PORT).unwrap().clone();
    assert_eq!(
        runtime.replace_adapter(PORT, adapter(PORT, &settings).unwrap(), 1),
        Ok(2)
    );
    assert!(!runtime.stop(2));
    assert!(runtime.detached(PORT, 1));
    assert!(runtime.detached(PORT, 2));
    assert!(runtime.stop(3));
}

#[test]
fn repeated_detach_tracking_is_idempotent_and_unrelated_acknowledgments_preserve_it() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "main",
                json!({"remote_node": ["3000"]}),
            )],
        ),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert!(matches!(
        runtime.authorize_incoming("missing", "2000", true),
        Err(RuntimeError::MissingNode)
    ));
    assert!(runtime.authorize_incoming(PORT, "2000", false).is_err());
    runtime.authorize_incoming(PORT, "2000", true).unwrap();
    for _ in 0..2 {
        runtime.incoming(PORT, "2000", true).unwrap();
        assert!(runtime.authorize_incoming(PORT, "2000", true).is_err());
        runtime
            .command(
                PORT,
                dtmf::DigitOperation::new(
                    crate::command::Command {
                        action: LinkAction::Disconnect,
                        node: "2000".into(),
                    },
                    None,
                ),
                clock(),
                true,
            )
            .unwrap();
    }
    assert!(runtime.next_link().is_none());
    runtime.peer_detached("other", "2000", 0);
    assert!(runtime.next_link().is_none());
    runtime.peer_detached(PORT, "2000", 0);
    assert!(runtime.next_link().is_some());
    assert!(runtime.stop(0));
}

#[test]
fn peer_set_publication_preserves_live_controller_and_does_not_force_past_a_hazard() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "radio"}}),
                ),
                (
                    PORT,
                    ResourceKind::Radio,
                    "",
                    json!({"duplex_mode": "full"}),
                ),
            ],
        ),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut old = receive.acquire_pair(&mut transmit).unwrap();
    old.transmit()
        .controller
        .process_audio(true, false, &[], &mut [1.0; 960]);
    let activity = old.transmit().controller.activity();
    let prior = activity.last_sample();
    let settings = runtime.settings(PORT).unwrap().clone();
    assert_eq!(
        runtime.replace_adapter(PORT, adapter("1000", &settings).unwrap(), 1001),
        Err(RuntimeError::Busy)
    );
    assert_eq!(old.id(), 1);
    assert_eq!(activity.last_sample(), prior);
    drop(old);
    assert_eq!(
        runtime.replace_adapter(PORT, adapter("1000", &settings).unwrap(), 1002),
        Ok(2)
    );
    assert_eq!(runtime.revision(), 1);
    let mut next = receive.acquire_pair(&mut transmit).unwrap();
    assert_eq!(next.id(), 2);
    assert_eq!(next.transmit().controller.activity().last_sample(), prior);
    next.transmit()
        .controller
        .process_audio(true, false, &[], &mut [1.0; 960]);
    assert!(activity.last_sample() > prior);
    drop(next);
    assert_eq!(runtime.node(PORT).unwrap().status(1003).retiring, Some(1));
    assert_eq!(
        station_fixture::reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "radio"}})
                )]
            ),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock()
        ),
        Err(RuntimeError::Busy)
    );
    runtime.reclaim();
    assert_eq!(runtime.node(PORT).unwrap().status(1003).retiring, Some(1));
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    assert_eq!(runtime.node(PORT).unwrap().status(1003).retiring, None);
    station_fixture::reload(
        &mut runtime,
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "radio"}}),
            )],
        ),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert_eq!(receive.acquire_pair(&mut transmit).unwrap().id(), 3);
    runtime.detached(PORT, 2);
    assert!(runtime.stop(1004));
}

struct PendingStream;
impl crate::audio::PcmStreamReader for PendingStream {
    fn render(&mut self, _: &mut [f32]) -> crate::audio::PcmRead {
        crate::audio::PcmRead::Pending
    }
}

struct StreamingStation(Arc<AtomicUsize>);
impl StationMediaSession for StreamingStation {
    fn register_program(
        &mut self,
        _: crate::media::MediaProgram,
        _: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        Ok(Box::new(PendingStream))
    }

    fn start(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}
impl Drop for StreamingStation {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct StreamingMedia(Arc<AtomicUsize>);
impl NativeFilePreparer for StreamingMedia {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeSpeechPreparer for StreamingMedia {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeMediaPreparer for StreamingMedia {
    fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        Ok(Box::new(StreamingStation(Arc::clone(&self.0))))
    }
}

#[test]
fn adapter_replacement_keeps_stream_producer_with_live_controller() {
    let drops = Arc::new(AtomicUsize::new(0));
    let media = StreamingMedia(Arc::clone(&drops));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Courtesy,
                "link",
                json!({"input": "link", "speech": {"text": "Connected"}, "morse": {"text": "L"}}),
            )],
        ),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    let settings = runtime.settings(PORT).unwrap().clone();
    assert_eq!(
        runtime.replace_adapter(PORT, adapter("1000", &settings).unwrap(), 1001),
        Ok(2)
    );

    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(runtime.stop(1002));
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn multi_node_failed_candidate_and_device_activation_restore_complete_old_runtime() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut devices = station_fixture::DevicePlans::default();
    let mut runtime = devices
        .start(
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000")],
                &[
                    (
                        PORT,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "old1"}}),
                    ),
                    (
                        SECOND,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "old2"}}),
                    ),
                ],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert_eq!(runtime.node_names(), [PORT, SECOND]);
    let original = runtime.revision();
    assert_eq!(
        devices.reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000")],
                &[
                    (
                        PORT,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "new1"}})
                    ),
                    (
                        SECOND,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "fail"}})
                    )
                ]
            ),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Device)
    );
    assert_eq!(runtime.revision(), original);
    assert_eq!(
        station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
        "old1"
    );
    assert!(
        log.lock()
            .unwrap()
            .ends_with(&["close".into(), "old1".into()])
    );
    assert!(runtime.stop(clock().now_ms));
}

fn event_clock(wall_seconds: i64) -> RuntimeClock {
    RuntimeClock {
        wall_seconds,
        now_ms: 1000 + (wall_seconds - 100).max(0) as u64 * 1000,
        ..clock()
    }
}
fn scheduled_configuration(
    actions: serde_json::Value,
    polite: u64,
) -> Arc<crate::config::resources::generation::RuntimeConfiguration> {
    station_fixture::configuration(
        &[(PORT, "1000")],
        &[
            (
                PORT,
                ResourceKind::Message,
                "report",
                json!({"locales":{"en-US":{"text":"Report { $time }","tts":"Report { $time }","morse":"TIME"}},"dtmf_id":"4"}),
            ),
            (
                PORT,
                ResourceKind::Macro,
                "dispatch",
                json!({"actions":actions}),
            ),
            (
                PORT,
                ResourceKind::Event,
                "announcement",
                json!({"start":{"at":"every day at 0:02 in UTC","macro":"dispatch","politeness_minutes":polite}}),
            ),
        ],
    )
}
fn scheduled_runtime(
    actions: serde_json::Value,
    polite: u64,
    media: &dyn NativeMediaPreparer,
) -> Runtime {
    let mut runtime = station_fixture::start(
        scheduled_configuration(actions, polite),
        media,
        adapter,
        |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    runtime
}
#[test]
fn scheduled_event_clock_bounds_do_not_consume_another_event() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = scheduled_runtime(
        json!([{"action":"message","message":"Ready","destination":"local"}]),
        0,
        &CapturingMedia(log.clone()),
    );
    let invalid = RuntimeClock {
        wall_subsecond_ms: 1000,
        ..event_clock(120)
    };
    assert!(runtime.advance_commands(PORT, invalid).unwrap().is_none());
    assert!(log.lock().unwrap().is_empty());
    let (_, receipt) = runtime
        .advance_commands(PORT, event_clock(120))
        .unwrap()
        .unwrap();
    receipt.settle(true);
    assert!(
        runtime
            .advance_commands(PORT, event_clock(121))
            .unwrap()
            .is_none()
    );
    assert_eq!(*log.lock().unwrap(), ["Ready"]);
    assert!(runtime.stop(event_clock(121).now_ms));
}

#[test]
fn scheduled_event_and_manual_selection_use_one_registered_message() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = scheduled_runtime(
        json!([{"action":"message","message":"{ report }","destination":"local"}]),
        0,
        &CapturingMedia(log.clone()),
    );
    let (effect, receipt) = runtime
        .advance_commands(PORT, event_clock(120))
        .unwrap()
        .unwrap();
    assert!(matches!(effect, links::LinkEffect::None));
    receipt.settle(true);
    assert_eq!(log.lock().unwrap().len(), 1);
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut owner = tx.acquire().unwrap();
    owner
        .state()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 48_000]);
    runtime.queue_message(PORT, "4", None).unwrap();
    owner
        .state()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 48_000]);
    runtime.prepare_messages(event_clock(121));
    let log = log.lock().unwrap();
    assert_eq!(log.as_slice(), ["Report 9:07 AM", "Report 9:07 AM"]);
    drop(log);
    drop(owner);
    assert!(runtime.stop(event_clock(121).now_ms));
}

#[test]
fn scheduled_event_waits_for_receive_and_preserves_the_original_force_deadline() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = scheduled_runtime(
        json!([{"action":"message","message":"Ready","destination":"local"}]),
        1,
        &CapturingMedia(log.clone()),
    );
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut owner = tx.acquire().unwrap();
    owner
        .state()
        .controller
        .process_audio(true, false, &[], &mut [0.0; 960]);
    assert!(
        runtime
            .advance_commands(PORT, event_clock(120))
            .unwrap()
            .is_none()
    );
    let late = RuntimeClock {
        wall_subsecond_ms: 999,
        now_ms: event_clock(179).now_ms + 999,
        ..event_clock(179)
    };
    assert!(runtime.advance_commands(PORT, late).unwrap().is_none());
    assert!(log.lock().unwrap().is_empty());
    let (effect, receipt) = runtime
        .advance_commands(PORT, event_clock(180))
        .unwrap()
        .unwrap();
    assert!(matches!(effect, links::LinkEffect::None));
    receipt.settle(true);
    assert!(
        runtime
            .advance_commands(PORT, event_clock(181))
            .unwrap()
            .is_none()
    );
    assert_eq!(*log.lock().unwrap(), ["Ready"]);
    drop(owner);
    assert!(runtime.stop(event_clock(181).now_ms));
}

#[test]
fn scheduler_failure_consumes_only_its_occurrence_and_preserves_the_prior_status_queue() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = scheduled_runtime(
        json!([{"action":"message","message":"SCHEDULED","destination":"local"}]),
        0,
        &CapturingMedia(log.clone()),
    );
    for _ in 0..4 {
        runtime
            .queue_status(PORT, LinkAction::Time, None, clock(), None)
            .unwrap();
    }
    let before = log.lock().unwrap().clone();
    assert!(
        runtime
            .advance_commands(PORT, event_clock(120))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        crate::command_language::MacroOutcome::Rejected
    );
    assert_eq!(*log.lock().unwrap(), before);
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut owner = tx.acquire().unwrap();
    let mut out = [0.0; 48_000];
    owner
        .state()
        .controller
        .process_audio(false, false, &[], &mut out);
    assert!(out.into_iter().any(|x| x == 0.25));
    assert!(
        runtime
            .advance_commands(PORT, event_clock(121))
            .unwrap()
            .is_none()
    );
    assert_eq!(*log.lock().unwrap(), before);
    drop(owner);
    assert!(runtime.stop(event_clock(121).now_ms));
}

#[test]
fn successful_reload_retains_fixed_handles_hub_and_work_until_detached() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000"), (SECOND, "2000")],
            &[(
                SECOND,
                ResourceKind::Metadata,
                "",
                json!({"enabled": false}),
            )],
        ),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let node = runtime.node(PORT).unwrap();
    node.links().accept("3000", true).unwrap();
    let work = node.work().unwrap();
    let (mut rx, mut tx) = node.register_audio().unwrap();
    assert_eq!(rx.acquire().unwrap().id(), 1);
    station_fixture::reload(
        &mut runtime,
        station_fixture::configuration(
            &[(PORT, "1000"), (FOURTH, "4000")],
            &[(
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt": {"timing": {"hang_ms": 10}}}),
            )],
        ),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    assert!(!work.is_current());
    assert_eq!(runtime.node_names(), [PORT, FOURTH]);
    assert!(
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .manager()
            .reaches("3000", true)
    );
    assert_eq!(runtime.settings(PORT).unwrap().hang_ms, 10);
    assert_eq!(rx.acquire().unwrap().id(), 2);
    assert_eq!(tx.acquire().unwrap().id(), 2);
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    assert!(runtime.node(PORT).unwrap().status(0).retiring.is_some());
    drop(work);
    runtime.reclaim();
    assert!(runtime.node(PORT).unwrap().status(0).retiring.is_none());
    assert!(!runtime.stop(2000));
    assert!(runtime.take_effects().iter().any(|(name, _)| name == PORT));
    assert!(runtime.detached(PORT, 2));
    runtime.reclaim();
    assert!(runtime.stop(2000));
    assert!(rx.acquire().is_none());
    assert!(tx.acquire().is_none());
}

#[test]
fn partial_start_closes_previously_opened_devices_and_bad_media_keeps_live_revision() {
    let mut devices = station_fixture::DevicePlans::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    assert!(matches!(
        devices.start(
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000")],
                &[(
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "fail"}})
                )]
            ),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    assert_eq!(
        *log.lock().unwrap(),
        ["1000", "fail", "quiesce", "close", "quiesce", "close"]
    );
    let mut runtime = devices
        .start(
            station_fixture::single_port(true),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    let before = log.lock().unwrap().len();
    struct Unavailable;
    impl NativeFilePreparer for Unavailable {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Unavailable {
        fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeMediaPreparer for Unavailable {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    assert_eq!(
        devices.reload(
            &mut runtime,
            station_fixture::single_port(true),
            &Unavailable,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Preparation)
    );
    assert_eq!(runtime.revision(), 1);
    assert_eq!(log.lock().unwrap().len(), before);
    assert!(runtime.stop(0));
}

#[test]
fn reload_rejects_a_newer_installed_schedule_without_replacing_live_devices() {
    use crate::{access::AccessPolicy, runtime::link_schedule::LinkScheduler};
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.node(PORT).unwrap().links().reconfigure(
        AccessPolicy::new("", "").unwrap(),
        crate::command::DtmfCommandMap::default(),
        Some(LinkScheduler::new(2, Vec::new(), Vec::new(), None).unwrap()),
    );
    let before = log.lock().unwrap().clone();
    assert_eq!(
        station_fixture::reload(
            &mut runtime,
            station_fixture::single_port(true),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
            clock(),
        ),
        Err(RuntimeError::Preparation)
    );
    assert_eq!(runtime.revision(), 1);
    assert_eq!(*log.lock().unwrap(), before);
    assert!(runtime.stop(0));
}

#[test]
fn no_window_permanent_links_work_without_wall_clock() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let config = station_fixture::configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::PermanentGroup,
            "main",
            json!({"remote_node":["2000"]}),
        )],
    );
    let mut now = clock();
    now.civil = None;
    let mut runtime = station_fixture::start(config, &Media, adapter, device, now).unwrap();
    assert!(runtime.next_link().is_some());
    assert_eq!(runtime.revision(), 1);
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_message_is_not_reexecuted_when_its_generation_retires() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let actions = json!([{"action":"message","message":"Test","destination":"local"}]);
    let mut runtime = scheduled_runtime(actions.clone(), 0, &CapturingMedia(log.clone()));
    let tick = event_clock(120);
    let (_, receipt) = runtime.advance_commands(PORT, tick).unwrap().unwrap();
    receipt.settle(true);
    for _ in 0..10 {
        assert!(runtime.advance_commands(PORT, tick).unwrap().is_none());
    }
    assert_eq!(*log.lock().unwrap(), ["Test"]);
    station_fixture::reload(
        &mut runtime,
        scheduled_configuration(actions, 0),
        &CapturingMedia(log.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
        event_clock(121),
    )
    .unwrap();
    assert!(
        runtime
            .advance_commands(PORT, event_clock(121))
            .unwrap()
            .is_none()
    );
    assert_eq!(*log.lock().unwrap(), ["Test"]);
    assert!(runtime.stop(event_clock(122).now_ms));
}

#[test]
fn telemetry_is_registered_with_station_producer_not_synchronously_prepared() {
    struct Prepared {
        rate: u32,
        file_ok: bool,
        calls: Mutex<Vec<&'static str>>,
    }
    impl NativeFilePreparer for Prepared {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.calls.lock().unwrap().push("file");
            if self.file_ok {
                PreparedAudio::new(self.rate, vec![0.1; 96])
            } else {
                Err(MediaError::Unavailable)
            }
        }
    }
    impl NativeSpeechPreparer for Prepared {
        fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.calls.lock().unwrap().push("speech");
            PreparedAudio::new(self.rate, vec![0.1; 96])
        }
    }
    impl NativeMediaPreparer for Prepared {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(None)))
        }
    }
    let config = station_fixture::configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::Identifier,
            "test",
            json!({"file":"test.wav","speech":{"text":"hello"},"morse":{"text":"TEST"}}),
        )],
    );
    let media = Prepared {
        rate: 22050,
        file_ok: true,
        calls: Mutex::new(Vec::new()),
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        config,
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert!(media.calls.lock().unwrap().is_empty());
    assert!(runtime.stop(0));
}

fn attach_windows(
    runtime: &mut Runtime,
    primary: &str,
    windows: &[(&str, &str, &str, &str, u64, u64, u64)],
) {
    use crate::{
        access::AccessPolicy,
        command::DtmfCommandMap,
        runtime::link_schedule::{LinkScheduler, ReplacementSpec, RouteSpec},
        schedule::ScheduledWindow,
    };
    let route = |name: &str, label: &str, permanent| RouteSpec {
        local: PORT.into(),
        remote: if permanent { "2000" } else { "3000" }.into(),
        permanent,
        group_label: Some(label.into()),
        group_name: Some(name.into()),
        group_priority: Some(0),
    };
    let mut routes = vec![route(primary, "primary", true)];
    let mut replacements = Vec::new();
    for &(label, name, begin, end, quiet, start_warning, end_warning) in windows {
        let idx = routes.len();
        routes.push(route(name, label, false));
        replacements.push(ReplacementSpec {
            routes: vec![idx],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, begin, end).unwrap(),
            end_inactivity_ms: quiet,
            warning_before_start_ms: (start_warning > 0)
                .then_some(start_warning)
                .into_iter()
                .collect(),
            warning_before_end_ms: (end_warning > 0)
                .then_some(end_warning)
                .into_iter()
                .collect(),
            warning_message_id: Some("scheduled-link-change".into()),
        });
    }
    runtime.node(PORT).unwrap().links().reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::default(),
        Some(LinkScheduler::new(2, routes, replacements, None).unwrap()),
    );
}

#[test]
fn reconnect_reconciles_window_before_resuming_old_permanent_retry() {
    use super::{dtmf::DigitOperation, links::LinkEffect};
    use crate::command::{Command, LinkAction};
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    attach_windows(
        &mut runtime,
        "primary",
        &[("window", "window", "10:00", "11:00", 0, 0, 0)],
    );
    runtime.tick_links(clock());
    let (_, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("permanent dial")
    };
    assert!(
        !runtime
            .finish_connect(PORT, attempt, false, clock())
            .unwrap()
    );
    let operation = |action| {
        DigitOperation::new(
            Command {
                action,
                node: String::new(),
            },
            None,
        )
    };
    for civil in [None, Some((clock().civil.unwrap().0, 60))] {
        let mut invalid_clock = clock();
        invalid_clock.civil = civil;
        assert!(matches!(
            runtime.command(
                PORT,
                operation(LinkAction::ReconnectAll),
                invalid_clock,
                false
            ),
            Err(RuntimeError::Clock)
        ));
    }
    runtime
        .command(PORT, operation(LinkAction::DisconnectAll), clock(), false)
        .unwrap();
    let mut during = clock();
    during.civil = Some((
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 10, 0).unwrap(),
        0,
    ));
    runtime
        .command(PORT, operation(LinkAction::ReconnectAll), during, false)
        .unwrap();
    assert!(runtime.next_link().is_none());
    runtime.peer_detached(PORT, "2000", during.now_ms);
    assert!(
        !runtime
            .node(PORT)
            .unwrap()
            .links()
            .manager()
            .reaches("2000", true)
    );
    let (_, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("window dial")
    };
    assert_eq!(attempt.remote(), "3000");
    drop(attempt);
    assert!(runtime.stop(0));
}

#[test]
fn receive_at_runtime_origin_starts_the_link_quiet_interval() {
    use super::links::LinkEffect;
    let at = |now_ms, hour, minute| RuntimeClock {
        now_ms,
        wall_seconds: 0,
        wall_subsecond_ms: 0,
        civil: Some((
            CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, minute).unwrap(),
            0,
        )),
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        at(0, 12, 59),
    )
    .unwrap();
    attach_windows(
        &mut runtime,
        "primary",
        &[("window", "window", "12:00", "13:00", 60000, 0, 0)],
    );
    runtime.tick_links(at(0, 12, 59));
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("replacement dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        runtime.finish_connect(&local, attempt, true, at(0, 12, 59)),
        Ok(true)
    );

    let (mut receive, mut transmit) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut guard = receive.acquire_pair(&mut transmit).unwrap();
    guard
        .transmit()
        .controller
        .process_audio(true, false, &[], &mut [0.0]);
    drop(guard);

    runtime.tick_links(at(1, 13, 0));
    assert!(runtime.next_link().is_none());
    runtime.tick_links(at(59_999, 13, 0));
    assert!(runtime.next_link().is_none());
    runtime.tick_links(at(60_000, 13, 1));
    assert!(matches!(
        runtime.next_link(),
        Some((local, LinkEffect::Detach(peers))) if local == PORT && peers == ["3000"]
    ));
    runtime.peer_detached(PORT, "3000", 60_000);
    drop((receive, transmit));
    assert!(runtime.stop(60_000));
}

#[test]
fn aggregate_schedule_warning_queues_one_localized_status_message() {
    let long_name = "N1".repeat(31) + "A";
    let long_speech = format!("Connecting to {}A in 1 minute", "N,1,".repeat(31));
    for (group, lead, hour, minute, second, expected) in [
        (
            "weekday",
            (60000, 0),
            11,
            59,
            0,
            "Connecting to weekday in 1 minute",
        ),
        (
            "Net 42",
            (90000, 0),
            11,
            58,
            30,
            "Connecting to Net 42 in 1 minute, and 30 seconds",
        ),
        (
            "3000",
            (60000, 0),
            11,
            59,
            0,
            "Connecting to 3000 in 1 minute",
        ),
        (
            "Evening Net",
            (0, 60000),
            12,
            59,
            0,
            "Disconnecting from Evening Net in 1 minute",
        ),
        (
            long_name.as_str(),
            (60000, 0),
            11,
            59,
            0,
            long_speech.as_str(),
        ),
    ] {
        let clock = RuntimeClock {
            now_ms: 1_000,
            wall_seconds: 0,
            wall_subsecond_ms: 0,
            civil: Some((
                CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, minute).unwrap(),
                second,
            )),
        };
        let messages = Arc::new(Mutex::new(Vec::new()));
        let devices = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = station_fixture::start(
            station_fixture::single_port(true),
            &CapturingMedia(messages.clone()),
            adapter,
            |_, _| Ok(Box::new(Device(devices.clone())) as Box<dyn DeviceHandoff>),
            clock,
        )
        .unwrap();
        attach_windows(
            &mut runtime,
            "Primary",
            &[("weekday", group, "12:00", "13:00", 0, lead.0, lead.1)],
        );
        runtime.tick_links(clock);
        assert_eq!(*messages.lock().unwrap(), [expected], "{group} {lead:?}");
        assert!(runtime.stop(clock.now_ms));
    }
}

#[test]
fn render_preserves_c_scheduled_identity_and_ordinary_speech_rules() {
    assert_eq!(
        telemetry_speech("COUNT 42 AB1CD 1000", &["1000"]),
        "COUNT 42 A,B,1,C,D node,1,0,0,0"
    );
    assert_eq!(
        telemetry_speech("prefix 1000 AB1CD X1000", &["1000"]),
        "prefix node,1,0,0,0 A,B,1,C,D X,1,0,0,0"
    );
    assert_eq!(super::render::morse("[]{}"), None);
    assert_eq!(
        super::render::morse("hello [1000]"),
        Some("hello  1000 ".into())
    );
}

#[test]
fn removing_a_node_keeps_stalled_owned_callbacks_observable_until_detached() {
    use station_fixture::{PORT, single_port};
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::idle();
    let adopt = |runtime: &mut Runtime<()>, enabled| {
        let configuration = single_port(enabled);
        let prepared = runtime.prepare_configuration(
            Arc::clone(&configuration),
            &Media,
            |_, settings| Ok((adapter(PORT, settings)?, false)),
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
            clock(),
        )?;
        runtime.adopt_configuration(prepared, clock())?;
        runtime.confirm_station_replacements(&configuration)
    };
    adopt(&mut runtime, true).unwrap();
    let (mut rx, _tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let guard = rx.acquire().unwrap();
    adopt(&mut runtime, false).unwrap();
    assert!(runtime.node_names().is_empty());
    assert_eq!(adopt(&mut runtime, true), Err(RuntimeError::Busy));
    assert!(!runtime.stop(0));
    drop(guard);
    runtime.reclaim();
    assert!(runtime.stop(0));
}

#[test]
fn repeated_stop_cannot_forget_unacknowledged_peer_cleanup() {
    struct SlowDevice(usize);
    impl DeviceHandoff for SlowDevice {
        fn quiesce(&mut self) -> bool {
            self.0 += 1;
            self.0 > 1
        }
        fn close(&mut self) {}
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
    }
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(SlowDevice(0)) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.incoming(PORT, "2000", true).unwrap();
    assert!(!runtime.stop(0));
    assert!(!runtime.stop(0));
    assert_eq!(runtime.status(0)[0].1.state, LifecycleState::Stopping);
    runtime.peer_detached(PORT, "2000", 0);
    assert!(runtime.detached(PORT, 1));
    assert!(runtime.stop(0));
}

#[test]
fn failed_restore_keeps_current_document_but_gates_the_unavailable_node() {
    let mut devices = station_fixture::DevicePlans::default();
    struct FailingRestore(bool);
    impl DeviceHandoff for FailingRestore {
        fn quiesce(&mut self) -> bool {
            true
        }
        fn close(&mut self) {}
        fn open(&mut self, _: &GenerationSettings) -> bool {
            let success = !self.0;
            self.0 = true;
            success
        }
    }
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(FailingRestore(false)) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = devices
        .start(
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "old"}}),
                )],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    let (mut rx, tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    assert_eq!(
        devices.reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "new"}})
                )]
            ),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Restore)
    );
    assert_eq!(runtime.revision(), 1);
    assert_eq!(
        station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
        "old"
    );
    assert!(rx.acquire().is_none());
    assert!(runtime.next_link().is_none());
    drop((rx, tx));
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    devices
        .reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000")],
                &[(
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "old"}}),
                )],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert_eq!(runtime.revision(), 2);
    assert!(runtime.node(PORT).unwrap().work().is_some());
    assert!(runtime.stop(0));
}

#[test]
fn adapter_dispatcher_is_owned_off_audio_and_requires_explicit_drain_acknowledgment() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Dispatcher(Arc<AtomicUsize>);
    impl Drop for Dispatcher {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let prepare = |_: &str, _: &ResolvedNodeSettings| {
        Ok(PreparedAdapter {
            remote_commands: None,
            state: (),
            control: Dispatcher(drops.clone()),
            receive_maximum: 960,
            transmit_maximum: 960,
        })
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "old"}}),
            )],
        ),
        &Media,
        prepare,
        device,
        clock(),
    )
    .unwrap();
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    assert!(runtime.adapter_control_mut(PORT, 1).is_some());
    station_fixture::reload(
        &mut runtime,
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio": {"device_identifier": "new"}}),
            )],
        ),
        &Media,
        prepare,
        device,
        clock(),
    )
    .unwrap();
    drop(rx.acquire());
    drop(tx.acquire());
    runtime.reclaim();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(runtime.adapter_control_mut(PORT, 1).is_some());
    assert!(runtime.detached(PORT, 1));
    runtime.reclaim();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(runtime.adapter_control_mut(PORT, 2).is_some());
    assert!(!runtime.stop(0));
    assert!(runtime.adapter_control_mut(PORT, 2).is_some());
    assert!(runtime.detached(PORT, 2));
    assert!(runtime.stop(0));
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn runtime_can_be_transferred_to_its_serialized_executor_owner() {
    fn require_send<T: Send>() {}
    require_send::<Runtime>();
}

#[test]
fn control_views_and_real_radio_digit_drain_share_one_current_node() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let document = station_fixture::configuration(
        &[(PORT, "1000")],
        &[(PORT, ResourceKind::Radio, "", json!({"unknown":true}))],
    );
    let mut runtime = station_fixture::start(
        document.clone(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(
        runtime.modular_configuration().unwrap(),
        &document
    ));
    assert_eq!(document.radio.warnings().len(), 1);
    assert_eq!(runtime.node(PORT).unwrap().settings().channel, PORT);
    assert_eq!(runtime.node(PORT).unwrap().work().unwrap().id(), 1);
    assert_eq!(runtime.topology(PORT).as_deref(), Some(""));
    assert_eq!(runtime.topology("missing"), None);
    assert!(
        runtime
            .digit("missing", super::dtmf::DigitEvent::Lost)
            .is_none()
    );
    assert!(runtime.digit(PORT, super::dtmf::DigitEvent::Lost).is_none());
    assert!(!runtime.detached("missing", 1));
    assert!(runtime.adapter_control_mut(PORT, 99).is_none());
    let (mut rx, _tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    for (row, column) in [
        (941.0, 1209.0),
        (852.0, 1209.0),
        (941.0, 1336.0),
        (941.0, 1477.0),
    ] {
        for block in 0..5 {
            let mut pcm = (0..612)
                .map(|offset| {
                    if block >= 2 {
                        return 0.0;
                    }
                    let time = (block * 612 + offset) as f32 / 48000.0;
                    0.031
                        * ((std::f32::consts::TAU * row * time).sin()
                            + (std::f32::consts::TAU * column * time).sin())
                })
                .collect::<Vec<_>>();
            rx.acquire().unwrap().state().process(true, &mut pcm, 1000);
        }
    }
    let events = runtime.node(PORT).unwrap().drain_digits();
    let commands: Vec<_> = events
        .into_iter()
        .filter_map(|event| runtime.digit(PORT, event))
        .collect();
    assert_eq!(commands.len(), 1);
    let Some(super::dtmf::DigitOutcome::Mapped(input)) = commands.into_iter().next() else {
        panic!("registered RF digit sequence")
    };
    assert!(matches!(
        runtime.dispatch_mapping(PORT, input, clock()),
        Ok(links::LinkEffect::Telemetry(LinkAction::Status, None))
    ));
    runtime.node(PORT).unwrap().discard_digits();
    assert!(runtime.node(PORT).unwrap().drain_digits().is_empty());
    assert!(runtime.stop(1001));
    assert_eq!(
        runtime.replace_adapter(
            "missing",
            adapter(
                "1000",
                &document.station_settings(PORT.parse().unwrap()).unwrap()
            )
            .unwrap(),
            1002
        ),
        Err(RuntimeError::Busy)
    );
    assert_eq!(
        station_fixture::reload(
            &mut runtime,
            document,
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock()
        ),
        Err(RuntimeError::Busy)
    );
}

#[test]
fn removing_configured_route_emits_one_detach_before_any_replacement_dial() {
    use super::links::LinkEffect;
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let config = station_fixture::configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::PermanentGroup,
            "main",
            json!({"remote_node":["2000"]}),
        )],
    );
    let mut runtime = station_fixture::start(config, &Media, adapter, device, clock()).unwrap();
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("permanent dial")
    };
    assert_eq!(
        runtime.finish_connect(&local, attempt, true, clock()),
        Ok(true)
    );
    station_fixture::reload(
        &mut runtime,
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "replacement",
                json!({"remote_node": ["3000"]}),
            )],
        ),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    assert!(runtime.next_link().is_none());
    let effects = runtime.take_effects();
    assert!(
        matches!(effects.as_slice(), [(local, LinkEffect::Detach(names))] if local == PORT && names == &["2000"])
    );
    assert!(runtime.take_effects().is_empty());
    assert!(runtime.next_link().is_none());
    runtime.peer_detached(PORT, "2000", 1001);
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("replacement dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        runtime.finish_connect(&local, attempt, false, clock()),
        Ok(false)
    );
    runtime.detached(PORT, 1);
    runtime.reclaim();
    runtime.stop(1002);
    for (local, effect) in runtime.take_effects() {
        if let LinkEffect::Detach(names) = effect {
            for remote in names {
                runtime.peer_detached(&local, &remote, 1002);
            }
        }
    }
    runtime.detached(PORT, 2);
    assert!(runtime.stop(1003));
}

#[test]
fn adapter_replacement_rejects_changed_callback_bounds_and_pending_retirement() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    let settings = runtime.settings(PORT).unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("missing", adapter(PORT, &settings).unwrap(), 1000),
        Err(RuntimeError::MissingNode)
    );
    for receive_changed in [false, true] {
        let mut prepared = adapter(PORT, &settings).unwrap();
        if receive_changed {
            prepared.receive_maximum = 961;
        } else {
            prepared.transmit_maximum = 961;
        }
        assert_eq!(
            runtime.replace_adapter(PORT, prepared, 1000),
            Err(RuntimeError::Busy)
        );
    }
    assert_eq!(
        runtime.replace_adapter(PORT, adapter(PORT, &settings).unwrap(), 1000),
        Ok(2)
    );
    assert_eq!(
        runtime.replace_adapter(PORT, adapter(PORT, &settings).unwrap(), 1001),
        Err(RuntimeError::Busy)
    );
    assert!(runtime.stop(1002));
}

#[test]
fn abnormal_runtime_drop_retains_protected_adapter_instead_of_freeing_live_audio() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Adapter(Arc<AtomicUsize>);
    impl Drop for Adapter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        |_, _| {
            Ok(PreparedAdapter {
                remote_commands: None,
                state: Adapter(dropped.clone()),
                control: (),
                receive_maximum: 960,
                transmit_maximum: 960,
            })
        },
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut guard = tx.acquire().unwrap();
    drop(runtime);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(guard.state().settings.channel, PORT);
    drop(guard);
    assert!(tx.acquire().is_none());
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
}

#[test]
fn event_macros_wait_for_message_admission_and_execute_each_existing_action() {
    for (action, target) in [
        ("connect", Some("3000")),
        ("disconnect", Some("2000")),
        ("disconnect_nonpermanent_all", None),
        ("resume_permanent_links", None),
    ] {
        let mut command = json!({"action":action});
        if let Some(peer) = target {
            command["parameter"] = json!(peer);
        }
        let mut runtime = scheduled_runtime(
            json!([{"action":"message","message":"Report","destination":"local"},command]),
            0,
            &Media,
        );
        runtime.incoming(PORT, "2000", true).unwrap();
        let at = event_clock(120);
        let (effect, notice) = runtime.advance_commands(PORT, at).unwrap().unwrap();
        assert!(matches!(effect, links::LinkEffect::None));
        assert!(runtime.advance_commands(PORT, at).unwrap().is_none());
        notice.settle(true);
        let (effect, receipt) = runtime.advance_commands(PORT, at).unwrap().unwrap();
        match effect {
            links::LinkEffect::Connect(attempt) => {
                assert_eq!(attempt.remote(), "3000");
                assert!(runtime.finish_connect(PORT, attempt, true, at).unwrap());
            }
            links::LinkEffect::Detach(peers) => assert_eq!(peers, ["2000"]),
            links::LinkEffect::None => assert_eq!(action, "resume_permanent_links"),
            _ => panic!("unexpected macro effect"),
        }
        receipt.settle(true);
        assert!(runtime.advance_commands(PORT, at).unwrap().is_none());
        assert_eq!(
            runtime
                .node(PORT)
                .unwrap()
                .take_command_completion()
                .unwrap()
                .outcome,
            crate::command_language::MacroOutcome::Completed
        );
        runtime.stop(at.now_ms);
        for peer in ["2000", "3000"] {
            runtime.peer_detached(PORT, peer, at.now_ms);
        }
        runtime.detached(PORT, 1);
        assert!(runtime.stop(at.now_ms));
    }
}

#[test]
fn status_actions_register_speech_with_station_and_queue_ring_playback() {
    struct Speech(Arc<Mutex<Vec<String>>>);
    impl NativeFilePreparer for Speech {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Speech {
        fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.0.lock().unwrap().push(request.text.into());
            PreparedAudio::new(48000, vec![0.25; 960])
        }
    }
    impl NativeMediaPreparer for Speech {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(Some(Arc::clone(&self.0)))))
        }
    }
    for (action, last, expected) in [
        (LinkAction::Time, None, "Good Morning. The time is 9:07 AM."),
        (LinkAction::LastKeyed, None, "No last keyed station"),
        (
            LinkAction::LastKeyed,
            Some("2000"),
            "Last keyed node node,2,0,0,0",
        ),
        (LinkAction::Status, None, "No links"),
        (LinkAction::FullStatus, None, "No links"),
    ] {
        let media = Speech(Arc::new(Mutex::new(Vec::new())));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = station_fixture::start(
            station_fixture::single_port(true),
            &media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock(),
        )
        .unwrap();
        runtime
            .queue_status(PORT, action, last, clock(), None)
            .unwrap();
        assert_eq!(*media.0.lock().unwrap(), [expected]);
        for _ in 0..3 {
            runtime
                .queue_status(PORT, action, last, clock(), None)
                .unwrap();
        }
        assert_eq!(
            runtime.queue_status(PORT, action, last, clock(), None),
            Err(RuntimeError::Rejected)
        );
        let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let mut generation = rx.acquire_pair(&mut tx).unwrap();
        let mut audio = [0.0; 960];
        for _ in 0..13 {
            audio.fill(0.0);
            generation
                .transmit()
                .controller
                .process_audio(false, false, &[], &mut audio);
        }
        assert!(audio.iter().any(|sample| *sample != 0.0));
        drop(generation);
        let mut invalid_clock = clock();
        invalid_clock.civil = None;
        assert_eq!(
            runtime.queue_status(PORT, LinkAction::Time, None, invalid_clock, None),
            Err(RuntimeError::Clock)
        );
        assert_eq!(
            runtime.queue_status("missing", action, last, clock(), None),
            Err(RuntimeError::MissingNode)
        );
        assert!(runtime.stop(0));
    }
}

#[test]
fn loop_rejection_queues_localized_rf_speech_to_the_local_transmitter() {
    struct Speech(Arc<Mutex<Vec<String>>>);
    impl NativeFilePreparer for Speech {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Speech {
        fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.0.lock().unwrap().push(request.text.into());
            PreparedAudio::new(48000, vec![0.25; 960])
        }
    }
    impl NativeMediaPreparer for Speech {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(Some(Arc::clone(&self.0)))))
        }
    }

    let media = Speech(Arc::new(Mutex::new(Vec::new())));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_loop_rejected(PORT, "2000", None).unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        ["Connection to node,2,0,0,0 rejected because it would create a link loop"]
    );
    runtime
        .queue_remote_command_unsupported(PORT, "3123456", None)
        .unwrap();
    assert_eq!(
        media.0.lock().unwrap().last().unwrap(),
        "Remote commands are not supported for node,3,1,2,3,4,5,6"
    );
    assert!(runtime.stop(0));
}

struct LinkNoticeMedia(Arc<AtomicBool>);
struct LinkNoticeStation(Arc<AtomicBool>);
struct LinkNoticeStream {
    value: f32,
    remaining: usize,
}

impl crate::audio::PcmStreamReader for LinkNoticeStream {
    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        let count = output.len().min(self.remaining);
        output[..count].fill(self.value);
        self.remaining -= count;
        if self.remaining == 0 {
            crate::audio::PcmRead::FinalSamples(count)
        } else {
            crate::audio::PcmRead::Samples(count)
        }
    }
}

impl StationMediaSession for LinkNoticeStation {
    fn register_program(
        &mut self,
        program: crate::media::MediaProgram,
        _: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        let disconnected = program.primary.iter().any(|source| {
            source
                .speech
                .as_ref()
                .is_some_and(|speech| speech.text.contains("disconnected"))
        });
        if !disconnected && self.0.load(Ordering::Acquire) {
            return Err(MediaError::Unavailable);
        }
        Ok(Box::new(LinkNoticeStream {
            value: if disconnected { 0.5 } else { 0.25 },
            remaining: 4,
        }))
    }

    fn start(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}

impl NativeFilePreparer for LinkNoticeMedia {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}

impl NativeSpeechPreparer for LinkNoticeMedia {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}

impl NativeMediaPreparer for LinkNoticeMedia {
    fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        Ok(Box::new(LinkNoticeStation(Arc::clone(&self.0))))
    }
}

fn link_event_configuration(
    ports: &[(&str, &str, u32)],
    extra: &[(&str, ResourceKind, &str, serde_json::Value)],
) -> Arc<crate::config::resources::generation::RuntimeConfiguration> {
    let stations = ports
        .iter()
        .map(|&(id, key, _)| (id, key))
        .collect::<Vec<_>>();
    let mut resources = extra.to_vec();
    for &(id, _, number) in ports {
        resources.push((
            id,
            ResourceKind::Allstarlink,
            "",
            json!({"node_number":number}),
        ));
    }
    station_fixture::configuration(&stations, &resources)
}

fn link_notice_runtime(
    configuration: Arc<crate::config::resources::generation::RuntimeConfiguration>,
    reject_connect: Arc<AtomicBool>,
) -> Runtime {
    station_fixture::start(
        configuration,
        &LinkNoticeMedia(reject_connect),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap()
}

#[test]
fn disabled_echo_announcements_do_not_queue_audio_or_suppress_ordinary_links() {
    let mut runtime = link_notice_runtime(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Echolink,
                "",
                json!({"announcement":"off"}),
            )],
        ),
        Arc::new(AtomicBool::new(false)),
    );
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    runtime
        .queue_echo_link_event(PORT, "3002000", "K0TEST", true)
        .unwrap();
    let mut output = [0.0; 12016];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut output);
    assert!(output.iter().all(|sample| *sample == 0.0));
    runtime.queue_link_event(PORT, "2000", true).unwrap();
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut output);
    assert_eq!(output.iter().filter(|sample| **sample == 0.25).count(), 4);
    assert!(runtime.stop(0));
}

#[test]
fn link_reconnect_skips_queued_disconnect_for_every_listener_and_reverse_pair() {
    let mut runtime = link_notice_runtime(
        link_event_configuration(
            &[
                (PORT, "north", 1000),
                (SECOND, "east", 2000),
                (THIRD, "west", 3000),
            ],
            &[],
        ),
        Arc::new(AtomicBool::new(false)),
    );
    runtime.queue_link_event(PORT, "2000", false).unwrap();
    runtime.queue_link_event(SECOND, "1000", true).unwrap();

    for local in [PORT, SECOND, THIRD] {
        let (_, mut tx) = runtime.node(local).unwrap().register_audio().unwrap();
        let mut audio = vec![0.0; 12_016];
        tx.acquire()
            .unwrap()
            .state()
            .controller
            .process_audio(false, false, &[], &mut audio);
        let heard: Vec<_> = audio.into_iter().filter(|sample| *sample != 0.0).collect();
        assert_eq!(
            heard, [0.25; 4],
            "listener {local} heard a stale disconnect"
        );
    }
    assert!(runtime.stop(0));
}

#[test]
fn link_reconnect_truncates_a_disconnect_that_already_started() {
    let mut runtime = link_notice_runtime(
        station_fixture::single_port(true),
        Arc::new(AtomicBool::new(false)),
    );
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    runtime.queue_link_event(PORT, "2000", false).unwrap();
    let mut prefix = vec![0.0; 12_002];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut prefix);
    assert_eq!(prefix.iter().filter(|sample| **sample == 0.5).count(), 2);

    runtime.queue_link_event(PORT, "2000", true).unwrap();
    let mut audio = [0.0; 8];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut audio);
    assert_eq!(audio, [0.25, 0.25, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
    assert!(runtime.stop(0));
}

#[test]
fn link_reconnect_cancels_only_the_same_actual_link_pair() {
    for (first, second) in [("3000", "2000"), ("1000", "3000")] {
        let mut runtime = link_notice_runtime(
            station_fixture::configuration(&[(FOURTH, "4000")], &[]),
            Arc::new(AtomicBool::new(false)),
        );
        runtime.queue_link_event("1000", "2000", false).unwrap();
        runtime.queue_link_event(first, second, false).unwrap();
        runtime.queue_link_event("2000", "1000", true).unwrap();
        let (_, mut tx) = runtime.node(FOURTH).unwrap().register_audio().unwrap();
        let mut audio = vec![0.0; 12_016];
        tx.acquire()
            .unwrap()
            .state()
            .controller
            .process_audio(false, false, &[], &mut audio);
        let heard: Vec<_> = audio.into_iter().filter(|sample| *sample != 0.0).collect();
        assert_eq!(heard, [0.5, 0.5, 0.5, 0.5, 0.25, 0.25, 0.25, 0.25]);
        assert!(runtime.stop(0));
    }
}

#[test]
fn link_reconnect_cancels_disconnects_before_connect_preparation_fails() {
    let reject_connect = Arc::new(AtomicBool::new(false));
    let mut runtime = link_notice_runtime(
        station_fixture::single_port(true),
        Arc::clone(&reject_connect),
    );
    runtime.queue_link_event(PORT, "2000", false).unwrap();
    reject_connect.store(true, Ordering::Release);
    assert_eq!(
        runtime.queue_link_event(PORT, "2000", true),
        Err(RuntimeError::Preparation)
    );
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut audio = vec![0.0; 12_016];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut audio);
    assert!(audio.into_iter().all(|sample| sample == 0.0));
    assert!(runtime.stop(0));
}

#[test]
fn link_reconnect_keeps_bounded_admission_and_reclaims_cancelled_credit() {
    let mut runtime = link_notice_runtime(
        station_fixture::single_port(true),
        Arc::new(AtomicBool::new(false)),
    );
    for connected in [false, true, false, true] {
        runtime.queue_link_event(PORT, "2000", connected).unwrap();
    }
    for connected in [false, true] {
        assert_eq!(
            runtime.queue_link_event(PORT, "2000", connected),
            Err(RuntimeError::Rejected),
            "cancellation does not bypass the four-item ownership limit"
        );
    }
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut audio = vec![0.0; 12_032];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut audio);
    let heard: Vec<_> = audio.into_iter().filter(|sample| *sample != 0.0).collect();
    assert_eq!(heard, [0.25; 8], "both disconnect attempts were superseded");
    // Public event admission reclaims completed/cancelled media on control first.
    runtime.queue_link_event(PORT, "2000", true).unwrap();
    assert!(runtime.stop(0));
}

#[test]
fn link_lifecycle_events_register_speech_for_every_node() {
    struct Speech(Arc<Mutex<Vec<String>>>);
    impl NativeFilePreparer for Speech {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Speech {
        fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.0.lock().unwrap().push(request.text.into());
            PreparedAudio::new(48000, vec![0.25; 960])
        }
    }
    impl NativeMediaPreparer for Speech {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(Some(Arc::clone(&self.0)))))
        }
    }

    let media = Speech(Arc::new(Mutex::new(Vec::new())));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        link_event_configuration(
            &[
                (PORT, "north", 1000),
                (SECOND, "east", 2000),
                (THIRD, "west", 3000),
            ],
            &[],
        ),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_link_event(PORT, "2000", true).unwrap();
    runtime.queue_link_event(PORT, "2000", false).unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        [
            "node,2,0,0,0 connected",
            "node,1,0,0,0 connected",
            "node,1,0,0,0 connected to node,2,0,0,0",
            "node,2,0,0,0 disconnected",
            "node,1,0,0,0 disconnected",
            "node,1,0,0,0 disconnected from node,2,0,0,0",
        ]
    );
    assert!(runtime.stop(0));
}

#[test]
fn group_members_skip_direct_events_but_unrelated_nodes_hear_pair_events() {
    struct Speech(Arc<Mutex<Vec<String>>>);
    impl NativeFilePreparer for Speech {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Speech {
        fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.0.lock().unwrap().push(request.text.into());
            PreparedAudio::new(48000, vec![0.25; 960])
        }
    }
    impl NativeMediaPreparer for Speech {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(Some(Arc::clone(&self.0)))))
        }
    }
    let media = Speech(Arc::new(Mutex::new(Vec::new())));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        link_event_configuration(
            &[(PORT, "north", 1000), (FOURTH, "monitor", 4000)],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio": {"device_identifier": "first"}}),
                ),
                (
                    PORT,
                    ResourceKind::PermanentGroup,
                    "network",
                    json!({"remote_node": ["2000", "3000"]}),
                ),
            ],
        ),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_link_event(PORT, "2000", true).unwrap();
    runtime.queue_link_event(PORT, "2000", false).unwrap();
    runtime.queue_link_event("2000", "1000", true).unwrap();
    runtime.queue_link_event("2000", "1000", false).unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        [
            "node,1,0,0,0 connected to node,2,0,0,0",
            "node,1,0,0,0 disconnected from node,2,0,0,0",
            "node,2,0,0,0 connected to node,1,0,0,0",
            "node,2,0,0,0 disconnected from node,1,0,0,0",
        ]
    );
    assert!(runtime.stop(0));
}

#[test]
fn operator_pause_announces_a_connected_priority_group_member() {
    let spoken = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "network",
                json!({"group_name":"Blind Hams","remote_node":["506315"]}),
            )],
        ),
        &CapturingMedia(spoken.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (_, links::LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("expected permanent dial");
    };
    assert_eq!(attempt.remote(), "506315");
    assert!(
        runtime
            .finish_connect(PORT, attempt, true, clock())
            .unwrap()
    );
    runtime.queue_link_event(PORT, "506315", true).unwrap();
    assert!(spoken.lock().unwrap().is_empty(), "automatic member event");
    // The default registered pause command is fixed and takes no node argument.
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*806506315#", clock().now_ms),
        Err(RuntimeError::Rejected)
    ));
    let links::LinkEffect::Detach(removed) =
        enter_mapping(&mut runtime, PORT, None, "*806#", clock().now_ms).unwrap()
    else {
        panic!("expected manual disconnect");
    };
    assert_eq!(removed, ["506315"]);
    for remote in removed {
        runtime.peer_detached(PORT, &remote, clock().now_ms);
        runtime.queue_link_event(PORT, &remote, false).unwrap();
    }
    runtime.queue_priority_group_events().unwrap();
    let actual = spoken.lock().unwrap().clone();
    assert!(runtime.stop(clock().now_ms));
    assert_eq!(actual, ["node,5,0,6,3,1,5 disconnected"]);
}

fn single_priority_group() -> Arc<crate::config::resources::generation::RuntimeConfiguration> {
    station_fixture::configuration(
        &[(PORT, "1000")],
        &[(
            PORT,
            ResourceKind::PermanentGroup,
            "network",
            json!({"group_name":"Blind Hams Network","remote_node":["506315"]}),
        )],
    )
}

#[test]
fn resuming_a_priority_group_waits_for_the_dial_outcome_before_announcing() {
    let spoken = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        single_priority_group(),
        &CapturingMedia(spoken.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (_, links::LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("expected initial group dial");
    };
    assert_eq!(
        runtime.finish_connect(PORT, attempt, true, clock()),
        Ok(true)
    );
    let group = runtime
        .node(PORT)
        .unwrap()
        .links()
        .group_member("506315")
        .unwrap();
    assert_eq!(group.selection().callback_slot(true), 1);
    runtime.queue_priority_group_events().unwrap();
    let links::LinkEffect::Detach(removed) =
        enter_mapping(&mut runtime, PORT, None, "*806#", clock().now_ms).unwrap()
    else {
        panic!("expected manual disconnect");
    };
    assert_eq!(removed, ["506315"]);
    runtime.peer_detached(PORT, "506315", clock().now_ms);
    runtime.queue_priority_group_events().unwrap();
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*816#", clock().now_ms),
        Ok(links::LinkEffect::None)
    ));
    runtime.queue_priority_group_events().unwrap();
    let node = runtime.node(PORT).unwrap();
    let work = node.work().unwrap();
    let retry = node.links().take_retry(clock().now_ms, work).unwrap();
    assert_eq!(retry.remote(), "506315");
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .finish_retry(retry, true, clock().now_ms),
        Ok(true)
    );
    assert_eq!(group.selection().callback_slot(true), 1);
    runtime.queue_priority_group_events().unwrap();
    let actual = spoken.lock().unwrap().clone();
    runtime.node(PORT).unwrap().links().ended("506315");
    runtime.peer_detached(PORT, "506315", clock().now_ms);
    assert!(runtime.stop(clock().now_ms));
    assert_eq!(
        actual,
        ["Blind Hams Network. node,5,0,6,3,1,5 connected"; 2]
    );
}

#[test]
fn recovered_group_cancels_queued_outage_speech_before_rf_playback() {
    let mut audible = Vec::new();
    for recover in [false, true] {
        let mut runtime =
            link_notice_runtime(single_priority_group(), Arc::new(AtomicBool::new(false)));
        let (_, links::LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
            panic!("expected initial group dial");
        };
        assert_eq!(
            runtime.finish_connect(PORT, attempt, false, clock()),
            Ok(false)
        );
        runtime.queue_priority_group_events().unwrap();
        runtime.queue_priority_group_events().unwrap();
        if recover {
            let node = runtime.node(PORT).unwrap();
            let work = node.work().unwrap();
            let retry = node.links().take_retry(clock().now_ms, work).unwrap();
            assert_eq!(retry.remote(), "506315");
            assert_eq!(
                node.links().finish_retry(retry, true, clock().now_ms),
                Ok(true)
            );
            // The recovered transport is already live, before its first callback selects a slot.
            runtime.queue_priority_group_events().unwrap();
        }
        let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let mut output = vec![0.0; 12_016];
        tx.acquire()
            .unwrap()
            .state()
            .controller
            .process_audio(false, false, &[], &mut output);
        audible.push(
            output
                .into_iter()
                .filter(|sample| *sample != 0.0)
                .collect::<Vec<_>>(),
        );
        if recover {
            runtime.node(PORT).unwrap().links().ended("506315");
            runtime.peer_detached(PORT, "506315", clock().now_ms);
        }
        assert!(runtime.stop(clock().now_ms));
    }
    assert_eq!(audible, [vec![0.25; 4], vec![]]);
}

#[test]
fn priority_group_announces_initial_selection_and_total_outage_only() {
    use super::links::LinkEffect;
    struct Speech(Arc<Mutex<Vec<String>>>);
    impl NativeFilePreparer for Speech {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for Speech {
        fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            self.0.lock().unwrap().push(request.text.into());
            PreparedAudio::new(48000, vec![0.25; 960])
        }
    }
    impl NativeMediaPreparer for Speech {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(TestStation(Some(Arc::clone(&self.0)))))
        }
    }
    let media = Speech(Arc::new(Mutex::new(Vec::new())));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "network",
                json!({"remote_node": ["2000", "3000"], "group_name": "Blind Hams Network"}),
            )],
        ),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.queue_priority_group_events().unwrap();
    let group = runtime
        .node(PORT)
        .unwrap()
        .links()
        .group_member("2000")
        .unwrap();
    for remote in ["2000", "3000"] {
        let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
            panic!("group dial")
        };
        assert_eq!(attempt.remote(), remote);
        assert_eq!(
            runtime.finish_connect(&local, attempt, true, clock()),
            Ok(true)
        );
    }

    assert_eq!(group.selection().callback_slot(true), 1);
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        ["Blind Hams Network. node,2,0,0,0 connected"]
    );
    group
        .selection()
        .publish_desired(std::num::NonZeroUsize::new(2));
    assert_eq!(group.selection().callback_slot(true), 2);
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(media.0.lock().unwrap().len(), 1);

    for remote in ["2000", "3000"] {
        let node = runtime.node(PORT).unwrap();
        node.links().ended(remote);
        assert_ne!(
            node.links().priority_groups()[0].selected.as_deref(),
            Some(remote)
        );
        node.links().reclaimed(remote, clock().now_ms);
    }
    // The last peer's removed audio path cannot clear the old callback-active slot.
    assert_eq!(group.selection().active().unwrap().get(), 2);
    for now_ms in [1000, 2000, 4000] {
        let node = runtime.node(PORT).unwrap();
        let work = node.work().unwrap();
        let retry = node.links().take_retry(now_ms, work).unwrap();
        assert_eq!(node.links().finish_retry(retry, false, now_ms), Ok(false));
        runtime.queue_priority_group_events().unwrap();
    }
    assert_eq!(
        *media.0.lock().unwrap(),
        [
            "Blind Hams Network. node,2,0,0,0 connected",
            "Blind Hams Network is unavailable",
        ]
    );
    let node = runtime.node(PORT).unwrap();
    let work = node.work().unwrap();
    let retry = node.links().take_retry(100_000, work).unwrap();
    let recovered = retry.remote().to_owned();
    assert_eq!(node.links().finish_retry(retry, true, 100_000), Ok(true));
    assert_ne!(group.selection().callback_slot(true), 0);
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(media.0.lock().unwrap().len(), 2);

    let node = runtime.node(PORT).unwrap();
    node.links().ended(&recovered);
    node.links().reclaimed(&recovered, 100_001);
    runtime.queue_priority_group_events().unwrap();
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        [
            "Blind Hams Network. node,2,0,0,0 connected",
            "Blind Hams Network is unavailable",
            "Blind Hams Network is unavailable",
        ]
    );
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_group_telemetry_uses_only_the_active_same_peer_schedule_once() {
    use super::links::LinkEffect;

    for (hour, answered, remote, expected) in [
        (8, false, "2000", "Primary is unavailable"),
        (10, false, "3000", "Morning Net is unavailable"),
        (11, false, "3000", "Morning Net is unavailable"),
        (20, false, "3000", "Evening Net is unavailable"),
        (20, true, "3000", "Evening Net. node,3,0,0,0 connected"),
    ] {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let mut now = clock();
        now.civil = Some((
            CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap(),
            0,
        ));
        let mut runtime = station_fixture::start(
            station_fixture::single_port(true),
            &CapturingMedia(Arc::clone(&messages)),
            adapter,
            |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
            now,
        )
        .unwrap();
        attach_windows(
            &mut runtime,
            "Primary",
            &[
                ("morning", "Morning Net", "09:30", "11:00", 60000, 0, 0),
                ("evening", "Evening Net", "20:00", "21:30", 0, 0, 0),
            ],
        );
        runtime.tick_links(now);
        let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
            panic!("active group dial")
        };
        assert_eq!(attempt.remote(), remote);
        assert_eq!(
            runtime.finish_connect(&local, attempt, answered, now),
            Ok(answered)
        );
        if answered {
            let group = runtime
                .node(PORT)
                .unwrap()
                .links()
                .group_member(remote)
                .unwrap();
            assert_eq!(group.selection().callback_slot(true), 1);
        }
        runtime.queue_priority_group_events().unwrap();
        runtime.queue_priority_group_events().unwrap();
        assert_eq!(
            *messages.lock().unwrap(),
            [expected],
            "hour={hour}, answered={answered}"
        );
        let node = runtime.node(PORT).unwrap();
        node.links().ended(remote);
        node.links().reclaimed(remote, now.now_ms);
        assert!(runtime.stop(now.now_ms));
    }
}

#[test]
fn priority_group_media_failure_is_reported_after_recording_the_transition() {
    struct RejectingMedia;
    struct RejectingStation;
    impl StationMediaSession for RejectingStation {
        fn register_program(
            &mut self,
            _: crate::media::MediaProgram,
            one_shot: bool,
        ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
            if one_shot {
                Err(MediaError::Io)
            } else {
                Ok(Box::new(TestPcm(0)))
            }
        }
        fn start(&mut self) -> Result<(), MediaError> {
            Ok(())
        }
    }
    impl NativeFilePreparer for RejectingMedia {
        fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeSpeechPreparer for RejectingMedia {
        fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
            Err(MediaError::Unavailable)
        }
    }
    impl NativeMediaPreparer for RejectingMedia {
        fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
            Ok(Box::new(RejectingStation))
        }
    }

    use super::links::LinkEffect;
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "network",
                json!({"remote_node": ["2000", "3000"], "group_name": "Network"}),
            )],
        ),
        &RejectingMedia,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let group = runtime
        .node(PORT)
        .unwrap()
        .links()
        .group_member("2000")
        .unwrap();
    for _ in 0..2 {
        let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
            panic!("group dial")
        };
        assert_eq!(
            runtime.finish_connect(&local, attempt, true, clock()),
            Ok(true)
        );
    }
    assert_eq!(group.selection().callback_slot(true), 1);
    assert_eq!(
        runtime.node(PORT).unwrap().links().priority_groups()[0].selected,
        Some("2000".to_owned())
    );

    assert_eq!(
        runtime.queue_priority_group_events(),
        Err(RuntimeError::Preparation)
    );
    runtime.stop(0);
}

#[test]
fn priority_group_event_reports_a_full_status_queue() {
    use super::links::LinkEffect;

    let media = CapturingMedia(Arc::new(Mutex::new(Vec::new())));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::PermanentGroup,
                "network",
                json!({"remote_node": ["2000", "3000"], "group_name": "Network"}),
            )],
        ),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for _ in ["2000", "3000"] {
        let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
            panic!("configured group dial")
        };
        assert_eq!(
            runtime.finish_connect(&local, attempt, true, clock()),
            Ok(true)
        );
    }
    runtime
        .node(PORT)
        .unwrap()
        .links()
        .group_member("2000")
        .unwrap()
        .selection()
        .callback_slot(true);
    for _ in 0..4 {
        runtime.queue_link_event("7000", "8000", true).unwrap();
    }
    assert_eq!(
        runtime.queue_link_event("7000", "8000", true),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.node(PORT).unwrap().links().priority_groups()[0].selected,
        Some("2000".to_owned())
    );

    assert_eq!(
        runtime.queue_priority_group_events(),
        Err(RuntimeError::Rejected)
    );
    for remote in ["2000", "3000"] {
        runtime.node(PORT).unwrap().links().ended(remote);
        runtime.peer_detached(PORT, remote, 0);
    }
    assert!(runtime.stop(0));
}

#[test]
fn completed_parrot_capture_is_measured_localized_and_queued_for_playback() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Radio,
                "",
                json!({"parrot": {"enabled": true}}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    let (mut receive, mut transmit) = runtime.node(PORT).unwrap().register_audio().unwrap();
    {
        let mut callback = receive.acquire_pair(&mut transmit).unwrap();
        callback
            .transmit()
            .controller
            .observe_parrot(true, &[0.5, 0.25]);
        callback.transmit().controller.observe_parrot(false, &[]);
    }
    runtime.tick_links(clock());

    assert_eq!(
        *messages.lock().unwrap(),
        ["Peak level -6 dBFS. RMS level -8 dBFS."]
    );
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn parrot_capture_waits_for_receive_to_end_and_reuses_the_deferred_buffer() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Radio,
                "",
                json!({"parrot": {"enabled": true}}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node(PORT).unwrap().register_audio().unwrap();
    {
        let mut callback = receive.acquire_pair(&mut transmit).unwrap();
        callback
            .transmit()
            .controller
            .observe_parrot(true, &[0.5, 0.25]);
        callback.transmit().controller.observe_parrot(false, &[]);
        callback.transmit().controller.process_event(true, false);
    }
    runtime.tick_links(clock());
    assert!(messages.lock().unwrap().is_empty());

    {
        let mut callback = receive.acquire_pair(&mut transmit).unwrap();
        callback.transmit().controller.process_event(false, false);
        callback
            .transmit()
            .controller
            .observe_parrot(true, &[0.5, 0.25]);
        callback.transmit().controller.observe_parrot(false, &[]);
    }
    runtime.tick_links(clock());
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn parrot_capture_drops_a_second_prepared_burst_while_playback_is_pending() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[(
                PORT,
                ResourceKind::Radio,
                "",
                json!({"parrot": {"enabled": true}}),
            )],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node(PORT).unwrap().register_audio().unwrap();
    {
        let mut callback = receive.acquire_pair(&mut transmit).unwrap();
        for _ in 0..2 {
            callback
                .transmit()
                .controller
                .observe_parrot(true, &[0.5, 0.25]);
            callback.transmit().controller.observe_parrot(false, &[]);
        }
    }
    runtime.tick_links(clock());
    assert_eq!(messages.lock().unwrap().len(), 2);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn parrot_command_rejects_a_second_toggle_until_callback_acknowledges_enable() {
    use super::links::LinkEffect;
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let hash = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.node(PORT).unwrap().links().configure_admin(
        &hash(b"1234", "c29tZXNhbHQ"),
        &hash(b"5678", "bG9ja3NhbHQ"),
        60_000,
    );
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*8001234#", 1000),
        Ok(LinkEffect::None)
    ));
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*804#", 1001),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, None, "*805#", 1002),
        Err(RuntimeError::Rejected)
    ));
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn accepted_peer_parrot_command_confirms_only_its_source() {
    use super::links::LinkEffect;
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let mut runtime = station_fixture::start(
        station_fixture::single_port(true),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.incoming(PORT, "2000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    let links = runtime.node(PORT).unwrap().links();
    links.configure_admin(&digest, &digest, 60_000);
    links
        .attach_ingress(
            crate::link::PeerIdentity::from_operator("2000").unwrap(),
            reply.clone(),
        )
        .unwrap();
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, Some(&reply), "*8001234#", 1001),
        Ok(LinkEffect::None)
    ));
    assert!(matches!(
        enter_mapping(&mut runtime, PORT, Some(&reply), "*804#", 1002),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    let now = RuntimeClock {
        now_ms: 1003,
        ..clock()
    };
    runtime.node(PORT).unwrap().links().ended("2000");
    runtime.peer_detached(PORT, "2000", now.now_ms);
    assert!(runtime.stop(now.now_ms));
}

#[test]
fn runtime_echo_telemetry_selection_respects_dynamic_policy_and_exact_command_source() {
    use super::links::LinkEffect;
    use crate::{config::EchoLinkTelemetry as Mode, link::PeerSignals};
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    for (dynamic, peer_source) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut runtime=station_fixture::start(station_fixture::configuration(&[(PORT,"radio")],&[
            (PORT,ResourceKind::Echolink,"",json!({"telemetry":"demand","telemetry_dynamic":dynamic})),
            (PORT,ResourceKind::Command,"",json!({"mappings":{"echo_telemetry":{"command":"*83","protected":true,"action":"set_echo_telemetry"}}}))
        ]),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
            clock(),
        )
        .unwrap();
        let reply = PeerSignals::new().reply_target();
        let other = PeerSignals::new().reply_target();
        assert!(matches!(
            runtime.set_echo_telemetry("missing", Mode::On, 0),
            Err(RuntimeError::MissingNode)
        ));
        runtime.echo_response_requested("missing", &reply, 0);
        assert!(!runtime.echo_telemetry_allows("missing", &reply, 0));
        assert!(!runtime.echo_telemetry_allows(PORT, &reply, 200000));
        runtime.echo_response_requested(PORT, &reply, 200001);
        // Fixed demand still answers commands; dynamic controls mode changes,
        // not the response window of the configured policy.
        assert!(runtime.echo_telemetry_allows(PORT, &reply, 200001));
        assert!(!runtime.echo_telemetry_allows(PORT, &reply, 320001));
        assert!(!runtime.echo_telemetry_allows(PORT, &other, 200001));
        assert_eq!(
            runtime.set_echo_telemetry(PORT, Mode::Off, 200002).is_ok(),
            dynamic
        );
        runtime.incoming(PORT, "2000", true).unwrap();
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .configure_admin(&digest, &digest, 60000);
        let source = peer_source.then_some(&reply);
        if peer_source {
            runtime
                .node(PORT)
                .unwrap()
                .links()
                .attach_ingress(
                    crate::link::PeerIdentity::from_operator("2000").unwrap(),
                    reply.clone(),
                )
                .unwrap();
        }
        assert!(matches!(
            enter_mapping(&mut runtime, PORT, source, "*8001234#", 200002),
            Ok(LinkEffect::None)
        ));
        let now = RuntimeClock {
            now_ms: 200004,
            ..clock()
        };
        let result = enter_mapping(&mut runtime, PORT, source, "*831#", 200003);
        if dynamic {
            assert!(matches!(result, Ok(LinkEffect::None)));
            assert!(runtime.echo_telemetry_allows(PORT, &reply, now.now_ms));
        } else {
            assert!(matches!(result, Err(RuntimeError::Rejected)));
        }
        for source in [None, Some(reply.clone())] {
            let mut status = super::dtmf::DigitOperation::new(
                crate::command::Command {
                    action: LinkAction::Status,
                    node: String::new(),
                },
                None,
            );
            status.reply = source.clone();
            let Ok(LinkEffect::Telemetry(LinkAction::Status, actual)) =
                runtime.command(PORT, status, now, false)
            else {
                panic!("status response missing")
            };
            assert_eq!(actual, source);
        }
        runtime.node(PORT).unwrap().links().ended("2000");
        runtime.peer_detached(PORT, "2000", now.now_ms);
        assert!(runtime.stop(now.now_ms));
    }
}

#[test]
fn device_reload_rejects_outstanding_work_and_drains_in_flight_callbacks() {
    struct PausingDevice {
        can_stop: Arc<AtomicBool>,
        closes: Arc<AtomicUsize>,
    }
    impl DeviceHandoff for PausingDevice {
        fn quiesce(&mut self) -> bool {
            self.can_stop.load(Ordering::Acquire)
        }
        fn close(&mut self) {
            self.closes.fetch_add(1, Ordering::Relaxed);
        }
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
    }
    for reason in 0..3 {
        let can_stop = Arc::new(AtomicBool::new(reason != 1));
        let closes = Arc::new(AtomicUsize::new(0));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(PausingDevice {
                can_stop: can_stop.clone(),
                closes: closes.clone(),
            }) as Box<dyn DeviceHandoff>)
        };
        let mut runtime = station_fixture::start(
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000")],
                &[
                    (
                        PORT,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "old"}}),
                    ),
                    (
                        SECOND,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "same"}}),
                    ),
                ],
            ),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
        let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
        let work = (reason == 0).then(|| runtime.node(PORT).unwrap().work().unwrap());
        let protected = (reason == 2).then(|| rx.acquire().unwrap());
        let result = station_fixture::reload(
            &mut runtime,
            station_fixture::configuration(
                &[(PORT, "1000"), (SECOND, "2000")],
                &[
                    (
                        PORT,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "new"}}),
                    ),
                    (
                        SECOND,
                        ResourceKind::Interface,
                        "",
                        json!({"audio": {"device_identifier": "same"}}),
                    ),
                ],
            ),
            &Media,
            adapter,
            device,
            clock(),
        );
        if reason == 0 {
            assert_eq!(result, Err(RuntimeError::Busy));
        } else {
            assert_eq!(result, Ok(()));
            assert!(runtime.reload_pending());
            assert_eq!(runtime.poll_reload(clock()), Ok(false));
        }
        assert_eq!(
            station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
            "old"
        );
        assert_eq!(runtime.revision(), 1);
        assert_eq!(closes.load(Ordering::Relaxed), 0);
        assert!(runtime.node(SECOND).unwrap().work().unwrap().is_current());
        if reason == 1 {
            assert!(runtime.node(PORT).unwrap().work().is_none());
        }
        if reason == 2 {
            // The running RX block retains its generation. New TX blocks cannot enter.
            assert_eq!(protected.as_ref().unwrap().id(), 1);
            assert!(tx.acquire().is_none());
            assert!(
                runtime.node(PORT).unwrap().work().is_none(),
                "physical handoff admitted incumbent work while RX was draining"
            );
        }
        drop(protected);
        drop(work);
        can_stop.store(true, Ordering::Release);
        if reason == 0 {
            assert_eq!(rx.acquire().unwrap().id(), 1);
        } else {
            assert_eq!(runtime.poll_reload(clock()), Ok(true));
            assert_eq!(closes.load(Ordering::Relaxed), 1);
            assert_eq!(runtime.node(PORT).unwrap().work().unwrap().id(), 2);
            assert_eq!(
                station_fixture::selected_device(runtime.modular_configuration().unwrap(), PORT),
                "new"
            );
            assert_eq!(rx.acquire().unwrap().id(), 2);
            assert_eq!(tx.acquire().unwrap().id(), 2);
        }
        assert!(runtime.stop(0));
    }
}

fn command_runtime(
    settings: serde_json::Value,
    definitions: &[(&str, &str, serde_json::Value)],
) -> Runtime<()> {
    command_runtime_media(PORT, settings, definitions, &Media)
}

fn command_runtime_media(
    local: &str,
    settings: serde_json::Value,
    definitions: &[(&str, &str, serde_json::Value)],
    media: &dyn NativeMediaPreparer,
) -> Runtime<()> {
    command_runtime_media_settings(local, false, None, settings, definitions, media)
}

fn command_runtime_media_settings(
    local: &str,
    immediate_full_duplex: bool,
    radio: Option<crate::remote_base_command::RemoteCommandConfiguration>,
    settings: serde_json::Value,
    definitions: &[(&str, &str, serde_json::Value)],
    media: &dyn NativeMediaPreparer,
) -> Runtime<()> {
    let mut resources = vec![
        (local, ResourceKind::Command, "", settings),
        (
            local,
            ResourceKind::Allstarlink,
            "",
            json!({"node_number":1000}),
        ),
    ];
    for (kind, key, settings) in definitions {
        let kind = ResourceKind::catalog()
            .find(|k| k.as_str() == *kind)
            .expect("registered fixture kind");
        resources.push((local, kind, *key, settings.clone()));
    }
    if immediate_full_duplex {
        resources.extend([
            (
                local,
                ResourceKind::Radio,
                "",
                json!({"duplex_mode":"full"}),
            ),
            (
                local,
                ResourceKind::RxSignaling,
                "",
                json!({"timing":{"kerchunk_max_ms":0}}),
            ),
            (
                local,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"hang_ms":0}}}),
            ),
        ]);
    }
    station_fixture::start(
        station_fixture::configuration(&[(local, "North")], &resources),
        media,
        |name, settings| {
            let mut prepared = adapter(name, settings)?;
            prepared.remote_commands = radio.clone();
            Ok(prepared)
        },
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap()
}

#[test]
fn registered_dtmf_completion_reaches_the_actual_link_owner() {
    use crate::runtime::dtmf::{DigitEvent, DigitOutcome};
    let mut runtime = command_runtime(
        serde_json::json!({"terminator":"D","inactivity_timeout_ms":17,
                "mappings":{"dial":{"command":"A*","protected":false,"action":"connect"}}}),
        &[],
    );
    for boundary in [
        DigitEvent::Digit {
            digit: 'D',
            now_ms: 9,
        },
        DigitEvent::Unkey { now_ms: 9 },
        DigitEvent::Digit {
            digit: '\0',
            now_ms: 17,
        },
    ] {
        for digit in "A*00123".chars() {
            assert!(
                runtime
                    .digit(PORT, DigitEvent::Digit { digit, now_ms: 0 })
                    .is_none()
            );
        }
        assert!(
            runtime
                .digit(
                    PORT,
                    DigitEvent::Digit {
                        digit: '\0',
                        now_ms: 16
                    }
                )
                .is_none()
        );
        let Some(DigitOutcome::Mapped(input)) = runtime.digit(PORT, boundary) else {
            panic!("one completed mapping")
        };
        let effect = runtime.dispatch_mapping(PORT, input, clock()).unwrap();
        let crate::runtime::links::LinkEffect::Connect(attempt) = effect else {
            panic!("domain dial")
        };
        assert_eq!(attempt.remote(), "00123");
        runtime.node(PORT).unwrap().links().cancel_connect(attempt);
        assert!(runtime.digit(PORT, boundary).is_none());
    }
    for digit in "789D".chars() {
        assert!(
            runtime
                .digit(PORT, DigitEvent::Digit { digit, now_ms: 19 })
                .is_none()
        );
    }
    assert!(runtime.stop(20));
}

#[test]
fn registered_startup_waits_for_nested_injected_input_and_domain_admission() {
    use crate::command_language::MacroOutcome;
    use crate::runtime::links::LinkEffect;
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({"inactivity_timeout_ms":17,"mappings":{
        "dial":{"command":"A","action":"connect","protected":false}}}),
        &[
            (
                "macro",
                "root",
                json!({"actions":[{"action":"call_macro","macro":"child"},{"action":"disconnect","parameter":"00123"}]}),
            ),
            (
                "macro",
                "child",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":8}},{"action":"execute_dtmf","digits":"A00123"}]}),
            ),
            (
                "event",
                "boot",
                json!({"start":{"at":"controller_start","macro":"root"}}),
            ),
        ],
    );
    let tick = |delta| RuntimeClock {
        now_ms: clock().now_ms + delta,
        ..clock()
    };
    for delta in [0, 7, 8, 24] {
        assert!(
            runtime
                .advance_commands(PORT, tick(delta))
                .unwrap()
                .is_none()
        );
    }
    let Some((LinkEffect::Connect(dial), receipt)) =
        runtime.advance_commands(PORT, tick(25)).unwrap()
    else {
        panic!("nested dial")
    };
    assert_eq!(dial.remote(), "00123");
    assert!(runtime.advance_commands(PORT, tick(26)).unwrap().is_none());
    assert!(runtime.finish_connect(PORT, dial, true, tick(27)).unwrap());
    receipt.settle(true);
    let Some((LinkEffect::Detach(names), receipt)) =
        runtime.advance_commands(PORT, tick(28)).unwrap()
    else {
        panic!("outer disconnect")
    };
    assert_eq!(names, ["00123"]);
    receipt.settle(true);
    assert!(runtime.advance_commands(PORT, tick(29)).unwrap().is_none());
    let node = runtime.node(PORT).unwrap();
    assert_eq!(
        node.take_command_completion().unwrap().outcome,
        MacroOutcome::Completed
    );
    assert!(runtime.advance_commands(PORT, tick(30)).unwrap().is_none());
    runtime.peer_detached(PORT, "00123", 1031);
    assert!(runtime.stop(1031));
}

#[test]
fn authenticated_reverse_phone_connects_and_pauses_permanent_links_while_muted() {
    use crate::{
        link::{
            Mode, PeerIdentity, PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource, ProtocolId,
        },
        runtime::{dtmf::DigitOutcome, links::LinkEffect},
    };

    const TIMEOUT_MS: u64 = 5000;
    let mut runtime = command_runtime(
        json!({"inactivity_timeout_ms":TIMEOUT_MS}),
        &[(
            "permanent-group",
            "network",
            json!({"group_name":"Fixture Network","remote_node":["506315"]}),
        )],
    );
    let mut now = clock();
    let (_, LinkEffect::Connect(group)) = runtime.next_link().unwrap() else {
        panic!("expected permanent-group dial");
    };
    assert_eq!(group.remote(), "506315");
    assert_eq!(runtime.finish_connect(PORT, group, true, now), Ok(true));

    let source = PhoneSource::new(
        PhoneAccountId::new("account").unwrap(),
        1,
        PhoneCallId::new([3; 16]).unwrap(),
        1,
        1,
    )
    .unwrap();
    let call = PeerSignals::for_reverse_phone(source.clone());
    assert!(call.authenticate_phone());
    let reply = call.reply_target();
    let identity = PeerIdentity::parse(ProtocolId::Phone, &source.call_id().to_string()).unwrap();
    runtime
        .node(PORT)
        .unwrap()
        .links()
        .attach_ingress(identity.clone(), reply.clone())
        .unwrap();

    let enter = |runtime: &mut Runtime, now: &mut RuntimeClock, digits: &str| {
        let links = runtime.node(PORT).unwrap().links();
        let mut completed = None;
        for digit in digits.chars() {
            // A symbol just before the configured deadline rearms collection.
            now.now_ms += TIMEOUT_MS - 1;
            assert!(
                links
                    .expire_connection_digits(&reply, now.now_ms)
                    .is_empty()
            );
            completed = links.phone_digit(&source, digit, now.now_ms, reply.clone());
            assert_eq!(completed.is_some(), digit == '#');
        }
        let Some(DigitOutcome::Mapped(input)) = completed else {
            panic!("expected the completed caller command");
        };
        runtime.dispatch_mapping(PORT, input, *now).unwrap()
    };
    let LinkEffect::Connect(newsline) = enter(&mut runtime, &mut now, "*3506316#") else {
        panic!("expected transceive dial");
    };
    assert_eq!(newsline.remote(), "506316");
    assert_eq!(newsline.mode(), Mode::TRANSCEIVE);
    assert_eq!(newsline.phone_reply(), Some(&reply));
    assert!(!newsline.permanent());
    assert_eq!(runtime.finish_connect(PORT, newsline, true, now), Ok(true));

    let LinkEffect::PrivateDetach {
        remotes: removed,
        reply: target,
    } = enter(&mut runtime, &mut now, "*806#")
    else {
        panic!("expected the permanent group to pause with a caller-only response");
    };
    assert_eq!(target, reply);
    assert_eq!(removed, ["506315"]);
    runtime.peer_detached(PORT, &removed[0], now.now_ms);
    let links = runtime.node(PORT).unwrap().links();
    let peers = links.manager().snapshot();
    assert!(
        peers
            .iter()
            .any(|peer| peer.name == "506316" && !peer.ended && !peer.retrying)
    );
    assert!(
        peers
            .iter()
            .any(|peer| peer.name == "506315" && peer.paused)
    );
    assert!(
        links
            .ingress_receive_status()
            .any(|(peer, state)| peer == &identity && state.muted)
    );
    links.ended("506316");
    runtime.peer_detached(PORT, "506316", now.now_ms);
    assert!(runtime.stop(now.now_ms));
}

#[test]
fn registered_mute_tracks_exact_sources_and_keeps_phone_commands_live() {
    use crate::{
        command_language::prepare_runtime_action,
        link::{PeerIdentity, PeerSignals, PhoneAccountId, PhoneCallId, PhoneSource, ProtocolId},
        runtime::dtmf::DigitOutcome,
    };
    use serde_json::json;
    let mut runtime = command_runtime(json!({}), &[]);
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let toggle = |rt: &mut Runtime<()>, action| {
        let p = prepare_runtime_action(&action).unwrap();
        rt.management_action(port, "management", &p, clock())
            .map(|_| ())
    };
    let asl = PeerSignals::new();
    let echo = PeerSignals::new();
    let phone = PhoneSource::new(
        PhoneAccountId::new("acct").unwrap(),
        1,
        PhoneCallId::new([3; 16]).unwrap(),
        1,
        1,
    )
    .unwrap();
    let call = PeerSignals::for_reverse_phone(phone.clone());
    assert!(call.authenticate_phone());
    for (alias, sig) in [("2000", &asl), ("3000123", &echo)] {
        runtime.incoming(PORT, alias, true).unwrap();
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .attach_ingress(
                PeerIdentity::from_operator(alias).unwrap(),
                sig.reply_target(),
            )
            .unwrap();
    }
    runtime
        .node(PORT)
        .unwrap()
        .links()
        .attach_ingress(
            PeerIdentity::parse(ProtocolId::Phone, &phone.call_id().to_string()).unwrap(),
            call.reply_target(),
        )
        .unwrap();
    let caller_toggle = |runtime: &mut Runtime<()>| {
        let cmds = runtime.node(PORT).unwrap().links();
        let mut out = None;
        for digit in "*99#".chars() {
            out = cmds.phone_digit(&phone, digit, clock().now_ms, call.reply_target());
        }
        let Some(DigitOutcome::Mapped(input)) = out else {
            panic!("actual authenticated caller")
        };
        let retry = input.clone();
        let effect = runtime.dispatch_mapping(PORT, input, clock()).unwrap();
        assert!(runtime.dispatch_mapping(PORT, retry, clock()).is_err());
        let crate::runtime::links::LinkEffect::PrivatePhoneResult(target, result) = effect else {
            panic!("expected private mute confirmation");
        };
        use crate::messages::OperationResult;
        assert_eq!(target, call.reply_target());
        assert_eq!(
            result,
            if call.rx_muted() {
                OperationResult::CallerMuted
            } else {
                OperationResult::CallerUnmuted
            }
        );
    };
    assert!(call.rx_muted());
    let redirected = PeerSignals::redirected(&call.reply_target());
    toggle(
        &mut runtime,
        json!({"action":"rx_mute_toggle","target":"phone_patch","parameter":phone.call_id().to_string()}),
    )
    .unwrap();
    for _ in 0..2 {
        toggle(
            &mut runtime,
            json!({"action":"rx_mute_toggle","target":"peers"}),
        )
        .unwrap();
    }
    assert!(call.rx_muted() && redirected.rx_muted());
    assert!(!asl.rx_muted() && !echo.rx_muted());
    caller_toggle(&mut runtime);
    assert!(!call.rx_muted() && !redirected.rx_muted());
    toggle(
        &mut runtime,
        json!({"action":"rx_mute_toggle","target":"peers","link_type":"echolink"}),
    )
    .unwrap();
    assert!(echo.rx_muted() && !asl.rx_muted() && !call.rx_muted());
    for expected in [true, false] {
        toggle(
            &mut runtime,
            json!({"action":"rx_mute_toggle","target":"peers"}),
        )
        .unwrap();
        assert_eq!(
            (asl.rx_muted(), echo.rx_muted(), call.rx_muted()),
            (expected, expected, expected)
        );
    }
    toggle(
        &mut runtime,
        json!({"action":"rx_mute_toggle","target":"peer","parameter":"2000"}),
    )
    .unwrap();
    assert!(asl.rx_muted() && !echo.rx_muted());
    for expected in [true, false] {
        caller_toggle(&mut runtime);
        assert_eq!(call.rx_muted(), expected);
        assert!(asl.rx_muted());
    }
    call.end();
    assert!(
        toggle(
            &mut runtime,
            json!({"action":"rx_mute_toggle","target":"phone_patch"})
        )
        .is_err()
    );
    toggle(
        &mut runtime,
        json!({"action":"rx_mute_toggle","target":"port"}),
    )
    .unwrap();
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut txg = tx.acquire().unwrap();
    assert!(!txg.state().controller.admits_local_receive(true));
    assert!(!txg.state().controller.admits_local_receive(false));
    toggle(
        &mut runtime,
        json!({"action":"rx_mute_toggle","target":"port"}),
    )
    .unwrap();
    assert!(txg.state().controller.admits_local_receive(true));
    assert!(!txg.state().controller.admits_local_receive(false));
    drop(txg);
    for (name, sig) in [("2000", asl), ("3000123", echo)] {
        sig.end();
        runtime.node(PORT).unwrap().links().ended(name);
        runtime.peer_detached(PORT, name, clock().now_ms);
    }
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn aggregate_receive_mute_gates_attached_phone_and_network_audio_and_keying() {
    use crate::{
        audio::LinkAudioQueue,
        command_language::prepare_runtime_action,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{
            AudioPeer, LinkAudio, Mode, PeerIdentity, PeerInput, PeerSignals, PhoneAccountId,
            PhoneCallId, PhoneSource, ProtocolId, SourcePosition,
        },
    };
    use serde_json::json;

    struct Input(Arc<PeerSignals>, f32);
    impl PeerInput for Input {
        fn source_rate(&self) -> u32 {
            48_000
        }
        fn signals(&self) -> &PeerSignals {
            &self.0
        }
        fn available(&self) -> u64 {
            4
        }
        fn finite_playout(&self) -> Option<bool> {
            Some(true)
        }
        fn render(&mut self, output: &mut [f32]) -> Option<usize> {
            output.fill(self.1);
            Some(output.len())
        }
    }

    let mut runtime = command_runtime(json!({}), &[]);
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    runtime.incoming(PORT, "2000", true).unwrap();
    let peer = Arc::new(PeerSignals::new());
    let make_call = |id: u8| {
        PhoneSource::new(
            PhoneAccountId::new("account").unwrap(),
            1,
            PhoneCallId::new([id; 16]).unwrap(),
            u64::from(id),
            u64::from(id),
        )
        .unwrap()
    };
    let incoming_id = make_call(1);
    let incoming = Arc::new(PeerSignals::for_phone(incoming_id.clone()));
    assert!(incoming.authenticate_phone());
    let outgoing_id = make_call(2);
    let outgoing = Arc::new(PeerSignals::for_outgoing_phone(outgoing_id.clone()));
    let detached = PeerSignals::for_phone(make_call(3));
    assert!(detached.authenticate_phone());
    let private_id = make_call(4);
    let private = PeerSignals::for_phone(private_id.clone());

    let phone_identity = |source: &PhoneSource| {
        PeerIdentity::parse(ProtocolId::Phone, &source.call_id().to_string()).unwrap()
    };
    let mut attach = |identity, signal: &PeerSignals| {
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .attach_ingress(identity, signal.reply_target())
    };
    attach(PeerIdentity::from_operator("2000").unwrap(), &peer).unwrap();
    attach(phone_identity(&incoming_id), &incoming).unwrap();
    attach(phone_identity(&outgoing_id), &outgoing).unwrap();
    assert_eq!(
        attach(phone_identity(&private_id), &private),
        Err(crate::link::AdmissionError::Denied)
    );

    // The media-only recipient still cannot collect a call-scoped command.
    assert!(!outgoing.reply_target().phone_authenticated());
    for digit in "*99#".chars() {
        assert!(
            runtime
                .node(PORT)
                .unwrap()
                .links()
                .phone_digit(&outgoing_id, digit, clock().now_ms, outgoing.reply_target())
                .is_none()
        );
    }

    let mut peers = Vec::new();
    let mut outputs = Vec::new();
    for (signal, phone, level) in [
        (&peer, None, 0.0625),
        (&incoming, Some(&incoming_id), 0.125),
        (&outgoing, Some(&outgoing_id), 0.25),
    ] {
        let (writer, output) = LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
        let input = Input(Arc::clone(signal), level);
        peers.push(
            match phone {
                Some(source) => AudioPeer::for_phone(
                    source,
                    &signal.reply_target(),
                    Mode::phone(true),
                    input,
                    writer,
                    4,
                ),
                None => AudioPeer::new("2000", Mode::TRANSCEIVE, input, writer, 4, 0),
            }
            .unwrap(),
        );
        outputs.push(output);
    }
    let (mut mixer, mut dispatch) = LinkAudio::new(peers, 4).unwrap();
    let (mut controller, _) = NodeController::new(
        ControllerSettings {
            hang_ms: 0,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let toggle =
        prepare_runtime_action(&json!({"action":"rx_mute_toggle","target":"peers"})).unwrap();
    for (index, muted) in [false, true, false].into_iter().enumerate() {
        if index != 0 {
            runtime
                .management_action(port, "management", &toggle, clock())
                .unwrap();
        }
        for signal in [&peer, &incoming, &outgoing] {
            assert_eq!(signal.rx_muted(), muted);
            signal.publish_pcm();
            signal.set_radio_keyed(true);
            assert!(signal.radio_keyed());
            assert_eq!(signal.output_radio_keyed(signal.receive_epoch()), !muted);
        }
        assert!(!detached.rx_muted() && !private.rx_muted());

        let mut rf = [0.0; 4];
        assert_eq!(
            mixer
                .process(&mut controller, false, &mut rf, SourcePosition::default())
                .unwrap(),
            !muted
        );
        assert_eq!(rf, [if muted { 0.0 } else { 0.4375 }; 4]);
        dispatch.dispatch(1);
        for (output, mixed) in outputs.iter_mut().zip([0.375, 0.3125, 0.1875]) {
            let mut programme = [0.0; 4];
            assert_eq!(output.read_span(&mut programme).is_some(), !muted);
            assert_eq!(programme, [if muted { 0.0 } else { mixed }; 4]);
        }
    }
    for signal in [&peer, &incoming, &outgoing] {
        signal.end();
    }
    runtime.node(PORT).unwrap().links().ended("2000");
    runtime.peer_detached(PORT, "2000", clock().now_ms);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_protected_macro_expiry_cancels_before_its_nested_dial() {
    use crate::{
        command_language::MacroOutcome,
        runtime::dtmf::{DigitEvent, DigitOutcome},
    };
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({"mappings":{
            "secured":{"command":"B","action":"call_macro","macro":"outer","protected":true}
        }}),
        &[
            (
                "macro",
                "outer",
                json!({"actions":[{"action":"call_macro","macro":"inner"}]}),
            ),
            (
                "macro",
                "inner",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":25}},{"action":"connect","parameter":"00200"}]}),
            ),
        ],
    );
    let tick = |ms| RuntimeClock {
        now_ms: clock().now_ms + ms,
        ..clock()
    };
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    runtime
        .node(PORT)
        .unwrap()
        .links()
        .configure_admin(&digest, &digest, 20);
    let enter = |rt: &mut Runtime<()>, digits: &str, ms| {
        let mut output = None;
        for digit in digits.chars() {
            output = rt.digit(
                PORT,
                DigitEvent::Digit {
                    digit,
                    now_ms: tick(ms).now_ms,
                },
            );
        }
        output
    };
    assert!(enter(&mut runtime, "B#", 0).is_none());
    for (digits, t) in [("*8001234#", 0), ("B#", 1)] {
        let Some(DigitOutcome::Mapped(input)) = enter(&mut runtime, digits, t) else {
            panic!("mapped")
        };
        runtime.dispatch_mapping(PORT, input, tick(t)).unwrap();
    }
    assert!(runtime.advance_commands(PORT, tick(1)).unwrap().is_none());
    assert!(runtime.advance_commands(PORT, tick(26)).unwrap().is_none());
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        MacroOutcome::Cancelled
    );
    assert!(runtime.stop(tick(27).now_ms));
}

#[test]
fn registered_direct_injection_uses_a_private_collector_and_monotonic_deadline() {
    use crate::{
        command_language::{MacroOutcome, prepare_runtime_action},
        runtime::links::LinkEffect,
    };
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({"inactivity_timeout_ms":10,"mappings":{
        "dial":{"command":"A","action":"connect","protected":false}}}),
        &[],
    );
    let action =
        prepare_runtime_action(&json!({"action":"execute_dtmf","digits":"A0002"})).unwrap();
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let tick = |ms| RuntimeClock {
        now_ms: clock().now_ms + ms,
        ..clock()
    };
    let (_, effect) = runtime
        .management_action(port, "operator", &action, clock())
        .unwrap();
    assert!(matches!(effect, LinkEffect::CommandQueued(_)));
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    assert!(
        runtime
            .advance_commands(
                PORT,
                RuntimeClock {
                    wall_seconds: -1,
                    civil: None,
                    ..tick(9)
                }
            )
            .unwrap()
            .is_none()
    );
    let Some((LinkEffect::Connect(dial), receipt)) = runtime
        .advance_commands(
            PORT,
            RuntimeClock {
                wall_seconds: -1,
                civil: None,
                ..tick(10)
            },
        )
        .unwrap()
    else {
        panic!("injected dial")
    };
    assert_eq!(dial.remote(), "0002");
    runtime.node(PORT).unwrap().links().cancel_connect(dial);
    receipt.settle(true);
    assert!(runtime.advance_commands(PORT, tick(11)).unwrap().is_none());
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        MacroOutcome::Completed
    );
    assert!(runtime.stop(tick(12).now_ms));
}

#[test]
fn registered_account_rejects_numeric_self_aliases_under_a_private_owner() {
    use crate::command_language::prepare_runtime_action;
    use crate::link::AdmissionError;
    use serde_json::json;
    let local = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut runtime = command_runtime_media(local, json!({}), &[], &Media);
    for target in ["1000", "01000", "0001000"] {
        let request =
            prepare_runtime_action(&json!({"action":"connect","parameter":target})).unwrap();
        assert!(matches!(
            runtime.management_action(local.parse().unwrap(), "operator", &request, clock()),
            Err(RuntimeError::Link(AdmissionError::Loop))
        ));
        assert!(matches!(
            runtime.incoming(local, target, true),
            Err(RuntimeError::Link(AdmissionError::Loop))
        ));
    }
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_command_message_uses_the_port_recipe_and_queue_before_the_next_dial() {
    use crate::{command_language::prepare_runtime_action, runtime::links::LinkEffect};
    use serde_json::json;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = command_runtime_media(
        PORT,
        json!({}),
        &[
            (
                "message",
                "report",
                json!({"locales":{"en-US":{"text":"Ready","tts":"Ready","morse":"OK"}}}),
            ),
            (
                "macro",
                "order",
                json!({"actions":[{"action":"message","message":"{ report }","destination":"local"},{"action":"connect","parameter":"0099"}]}),
            ),
        ],
        &CapturingMedia(log.clone()),
    );
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let submit = |rt: &mut Runtime<()>, payload| {
        let action = prepare_runtime_action(&payload).unwrap();
        rt.management_action(port, "operator", &action, clock())
            .unwrap();
    };
    submit(
        &mut runtime,
        json!({"action":"message","message":"{ report }","destination":"peers"}),
    );
    assert!(log.lock().unwrap().is_empty());
    submit(&mut runtime, json!({"action":"call_macro","macro":"order"}));
    let Some((LinkEffect::None, admitted)) = runtime.advance_commands(PORT, clock()).unwrap()
    else {
        panic!("queued report")
    };
    assert_eq!(*log.lock().unwrap(), ["Ready"]);
    let (_, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut owner = tx.acquire().unwrap();
    let mut output = [0.0; 16_000];
    owner
        .state()
        .controller
        .process_audio(false, false, &[], &mut output);
    assert!(output.iter().any(|sample| *sample != 0.0));
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    admitted.settle(true);
    let Some((LinkEffect::Connect(attempt), admitted)) =
        runtime.advance_commands(PORT, clock()).unwrap()
    else {
        panic!("next domain action")
    };
    assert_eq!(attempt.remote(), "0099");
    runtime.node(PORT).unwrap().links().cancel_connect(attempt);
    admitted.settle(true);
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    drop(owner);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_retiring_generation_cancels_a_wait_without_dialing_its_replacement() {
    use crate::command_language::{MacroOutcome, prepare_runtime_action};
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({}),
        &[(
            "macro",
            "wait",
            json!({"actions":[
        {"action":"pause","timing":{"duration_ms":20}},{"action":"connect","parameter":"00204"}]}),
        )],
    );
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let call = prepare_runtime_action(&json!({"action":"call_macro","macro":"wait"})).unwrap();
    let (_, links::LinkEffect::CommandQueued(reply)) = runtime
        .management_action(port, "operator", &call, clock())
        .unwrap()
    else {
        panic!("macro admission")
    };
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    assert!(runtime.stop(clock().now_ms + 1));
    assert_eq!(reply.outcome(), Some(MacroOutcome::Cancelled));
}

#[test]
fn registered_permanent_connect_is_idempotent_only_for_the_owned_active_mode() {
    use crate::{command_language::prepare_runtime_action, runtime::links::LinkEffect};
    use serde_json::json;
    let mut runtime = command_runtime(json!({}), &[]);
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let dial =
        prepare_runtime_action(&json!({"action":"connect_permanent","parameter":"00220"})).unwrap();
    let (_, LinkEffect::Connect(attempt)) = runtime
        .management_action(port, "operator", &dial, clock())
        .unwrap()
    else {
        panic!("first real dial")
    };
    assert!(
        runtime
            .finish_connect(PORT, attempt, true, clock())
            .unwrap()
    );
    assert!(matches!(
        runtime
            .management_action(port, "operator", &dial, clock())
            .unwrap()
            .1,
        LinkEffect::None
    ));
    let other = prepare_runtime_action(
        &json!({"action":"connect_permanent","parameter":"00220","mode":"monitor"}),
    )
    .unwrap();
    assert!(
        runtime
            .management_action(port, "operator", &other, clock())
            .is_err()
    );
    let pause = prepare_runtime_action(&json!({"action":"pause_permanent_links"})).unwrap();
    assert!(
        matches!(runtime.management_action(port,"operator",&pause,clock()).unwrap().1,LinkEffect::Detach(peers) if peers==["00220"])
    );
    runtime.peer_detached(PORT, "00220", clock().now_ms);
    let resume = prepare_runtime_action(&json!({"action":"resume_permanent_links"})).unwrap();
    runtime
        .management_action(port, "operator", &resume, clock())
        .unwrap();
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_window_ending_behind_a_real_pending_action_never_runs_either_boundary() {
    use crate::{command_language::prepare_runtime_action, runtime::links::LinkEffect};
    use serde_json::json;
    let dial = |target| json!({"action":"connect","parameter":target});
    let mut runtime = command_runtime(
        json!({}),
        &[
            ("macro", "block", json!({"actions":[dial("0019")]})),
            ("macro", "start", json!({"actions":[dial("0011")]})),
            ("macro", "end", json!({"actions":[dial("0012")]})),
            (
                "event",
                "window",
                json!({"start":{"at":"every day at 0:02 in UTC","macro":"start"},"end":{"at":"every day at 0:03 in UTC","macro":"end"}}),
            ),
        ],
    );
    let tick = |second| RuntimeClock {
        wall_seconds: second,
        now_ms: clock().now_ms + ((second - 100) as u64) * 1000,
        ..clock()
    };
    let action = prepare_runtime_action(&json!({"action":"call_macro","macro":"block"})).unwrap();
    runtime
        .management_action(
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            "operator",
            &action,
            tick(101),
        )
        .unwrap();
    let Some((LinkEffect::Connect(attempt), receipt)) =
        runtime.advance_commands(PORT, tick(101)).unwrap()
    else {
        panic!("blocking real dial")
    };
    assert!(runtime.advance_commands(PORT, tick(122)).unwrap().is_none());
    assert!(runtime.advance_commands(PORT, tick(181)).unwrap().is_none());
    runtime.node(PORT).unwrap().links().cancel_connect(attempt);
    receipt.settle(true);
    for second in [182, 183, 184] {
        assert!(
            runtime
                .advance_commands(PORT, tick(second))
                .unwrap()
                .is_none()
        )
    }
    assert!(
        runtime
            .node(PORT)
            .unwrap()
            .links()
            .manager()
            .snapshot()
            .is_empty()
    );
    assert!(runtime.stop(tick(185).now_ms));
}

#[test]
fn registered_management_radio_action_reaches_the_provider_without_rf_authority() {
    use crate::{command_language::prepare_runtime_action, runtime::links::LinkEffect};
    let mut runtime = command_runtime(serde_json::json!({}), &[]);
    let request = prepare_runtime_action(
        &serde_json::json!({"action":"radio","operation":"status","detail":"short"}),
    )
    .unwrap();
    let (_, LinkEffect::RemoteBase(operation)) = runtime
        .management_action(
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            "operator",
            &request,
            clock(),
        )
        .unwrap()
    else {
        panic!("native radio command")
    };
    assert!(!operation.receiver_source() && !operation.audible_source());
    assert!(operation.remote.is_some());
    drop(operation);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_scheduler_warning_captures_boundary_arguments_across_a_pause() {
    use crate::runtime::links::LinkEffect;
    use serde_json::json;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = command_runtime_media(
        PORT,
        json!({}),
        &[
            (
                "macro",
                "connect",
                json!({"actions":[{"action":"connect","parameter":"00200"}]}),
            ),
            (
                "macro",
                "warn",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":5000}},{"action":"message","message":"{ scheduled-link-change }","destination":"local"}]}),
            ),
            (
                "event",
                "dawn",
                json!({"start":{"at":"every day at 0:04 in UTC","macro":"connect","warnings":[{"at":"every day at 0:02 in UTC","macro":"warn"}]}}),
            ),
        ],
        &CapturingMedia(log.clone()),
    );
    let at = |s: i64| RuntimeClock {
        wall_seconds: s,
        now_ms: 1000 + ((s - 100) as u64) * 1000,
        ..clock()
    };
    assert!(runtime.advance_commands(PORT, at(120)).unwrap().is_none());
    let Some((LinkEffect::None, receipt)) = runtime.advance_commands(PORT, at(130)).unwrap() else {
        panic!("captured warning")
    };
    receipt.settle(true);
    let spoken = log.lock().unwrap().join(" ");
    assert!(
        spoken.contains("2 minutes")
            && spoken.contains("Connecting")
            && spoken.contains(&render::telemetry_speech("00200", &["00200"])),
        "{spoken}"
    );
    drop(spoken);
    assert!(runtime.advance_commands(PORT, at(131)).unwrap().is_none());
    assert!(runtime.stop(at(132).now_ms));
}

#[test]
fn registered_mixed_route_warning_preserves_kind_and_deadline_without_inventing_a_single_link() {
    use crate::runtime::links::LinkEffect;
    use serde_json::json;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = command_runtime_media(
        PORT,
        json!({}),
        &[
            (
                "message",
                "when",
                json!({"locales":{"en-US":{"text":"{ $event-kind } in { $time-remaining }","tts":"{ $event-kind } in { $time-remaining }","morse":"WARN"}}}),
            ),
            (
                "macro",
                "changes",
                json!({"actions":[{"action":"connect","parameter":"2000"},{"action":"disconnect_all"}]}),
            ),
            (
                "macro",
                "announce",
                json!({"actions":[{"action":"message","message":"{ when }","destination":"local"},{"action":"message","message":"{ scheduled-link-change }","destination":"local"}]}),
            ),
            (
                "event",
                "test",
                json!({"start":{"at":"every day at 0:04 in UTC","macro":"changes","warnings":[{"at":"every day at 0:02 in UTC","macro":"announce"}]}}),
            ),
        ],
        &CapturingMedia(log.clone()),
    );
    let at = RuntimeClock {
        wall_seconds: 120,
        ..clock()
    };
    let Some((LinkEffect::None, receipt)) = runtime.advance_commands(PORT, at).unwrap() else {
        panic!("captured event bindings")
    };
    receipt.settle(true);
    assert!(runtime.advance_commands(PORT, at).unwrap().is_none());
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        crate::command_language::MacroOutcome::Rejected
    );
    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["start_warning in 2 minutes"]
    );
    assert!(runtime.stop(at.now_ms));
}

#[test]
fn registered_queued_management_responses_keep_their_individual_terminal_outcomes() {
    use crate::command_language::{MacroOutcome, prepare_runtime_action};
    use crate::runtime::links::LinkEffect;
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({}),
        &[(
            "macro",
            "dial",
            json!({"actions":[{"action":"connect","parameter":"2000"}]}),
        )],
    );
    let action = prepare_runtime_action(&json!({"action":"call_macro","macro":"dial"})).unwrap();
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let mut enqueue = || match runtime
        .management_action(port, "operator", &action, clock())
        .unwrap()
        .1
    {
        LinkEffect::CommandQueued(response) => response,
        _ => panic!("queued result"),
    };
    let first = enqueue();
    let second = enqueue();
    let third = enqueue();
    for (response, accept, outcome) in [
        (&first, false, MacroOutcome::Rejected),
        (&second, true, MacroOutcome::Completed),
    ] {
        assert!(response.outcome().is_none());
        let Some((LinkEffect::Connect(a), receipt)) =
            runtime.advance_commands(PORT, clock()).unwrap()
        else {
            panic!("ordered dial")
        };
        runtime.node(PORT).unwrap().links().cancel_connect(a);
        receipt.settle(accept);
        assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
        assert_eq!(response.outcome(), Some(outcome));
    }
    assert_eq!(first.outcome(), Some(MacroOutcome::Rejected));
    assert!(third.outcome().is_none());
    assert!(runtime.stop(clock().now_ms));
    assert_eq!(third.outcome(), Some(MacroOutcome::Cancelled));
}

#[test]
fn registered_injected_management_radio_reads_available_owner_without_grant_or_reply() {
    use crate::{
        command_language::{MacroOutcome, prepare_runtime_action},
        remote_base_command::RemoteDtmfAccess,
        runtime::links::LinkEffect,
    };
    use serde_json::json;
    for (available, protected) in [(true, false), (true, true), (false, false)] {
        let prepared = available.then(|| crate::remote_base_command::RemoteCommandConfiguration {
            prefix: String::new(),
            access: RemoteDtmfAccess::Disabled,
        });
        let mut runtime = command_runtime_media_settings(
            PORT,
            false,
            prepared,
            json!({"mappings":{"check":{"command":"A","action":"radio","operation":"status","detail":"short","protected":protected}}}),
            &[(
                "macro",
                "read",
                json!({"actions":[{"action":"execute_dtmf","digits":"A"}]}),
            )],
            &Media,
        );
        let call = prepare_runtime_action(&json!({"action":"call_macro","macro":"read"})).unwrap();
        let (_, LinkEffect::CommandQueued(response)) = runtime
            .management_action(
                "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
                "manager",
                &call,
                clock(),
            )
            .unwrap()
        else {
            panic!("call")
        };
        let mut reached = false;
        for delta in [0, 3000, 3001, 3002] {
            let at = RuntimeClock {
                now_ms: clock().now_ms + delta,
                ..clock()
            };
            if let Some((effect, receipt)) = runtime.advance_commands(PORT, at).unwrap() {
                let LinkEffect::RemoteBase(operation) = effect else {
                    panic!("radio status")
                };
                assert!(
                    !operation.receiver_source()
                        && !operation.audible_source()
                        && operation.reply.is_none()
                );
                reached = true;
                receipt.settle(true);
            }
        }
        assert_eq!(reached, available && !protected);
        assert_eq!(response.outcome() == Some(MacroOutcome::Completed), reached);
        assert!(runtime.stop(clock().now_ms + 3003));
    }
}

#[test]
fn registered_peer_macro_formats_the_actual_ingress_across_a_pause() {
    use crate::{
        link::{PeerIdentity, PeerSignals},
        runtime::{dtmf::DigitOutcome, links::LinkEffect},
    };
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = command_runtime_media_settings(
        PORT,
        true,
        None,
        json!({"mappings":{"call":{"command":"B","protected":false,"action":"call_macro","macro":"report"}}}),
        &[(
            "macro",
            "report",
            json!({"actions":[
                {"action":"pause","timing":{"duration_ms":12}},
                {"action":"message","message":"Peer { $peer-node-number } { $peer-protocol } { $peer-callsign } { $event-kind }","destination":"local"}
            ]}),
        )],
        &CapturingMedia(captured.clone()),
    );
    let original = PeerSignals::new();
    original.bind_peer_callsign("K0TEST");
    let reply = PeerSignals::redirected(&original.reply_target()).reply_target();
    runtime.incoming(PORT, "2000", true).unwrap();
    let links = runtime.node(PORT).unwrap().links();
    links
        .attach_ingress(PeerIdentity::from_operator("2000").unwrap(), reply.clone())
        .unwrap();
    assert!(matches!(
        links
            .peer_text("2000", b"D 1000 8888 1 B", reply.clone(), false, 0, 10)
            .unwrap(),
        LinkEffect::None
    ));
    let LinkEffect::Digit(DigitOutcome::Mapped(input)) = links
        .peer_text("2000", b"D 1000 8888 2 #", reply.clone(), false, 0, 10)
        .unwrap()
    else {
        panic!("relayed macro mapping");
    };
    let at = |now_ms| RuntimeClock { now_ms, ..clock() };
    assert!(matches!(
        runtime.dispatch_mapping(PORT, input, at(10)).unwrap(),
        LinkEffect::CommandQueued(_)
    ));
    assert!(runtime.advance_commands(PORT, at(10)).unwrap().is_none());
    assert!(captured.lock().unwrap().is_empty());
    if let Some((effect, receipt)) = runtime.advance_commands(PORT, at(22)).unwrap() {
        assert!(matches!(effect, LinkEffect::None));
        receipt.settle(true);
    }
    assert_eq!(
        *captured.lock().unwrap(),
        ["Peer 2,0,0,0 allstarlink K,0,T,E,S,T command"]
    );
    let links = runtime.node(PORT).unwrap().links();
    links.retire_source(&reply);
    links.ended("2000");
    runtime.peer_detached(PORT, "2000", 23);
    assert!(runtime.stop(23));
}

#[test]
fn registered_peer_macro_keeps_exact_private_radio_collector_through_pause() {
    use crate::{
        command_language::MacroOutcome,
        link::{PeerIdentity, PeerSignals},
        remote_base_command::RemoteDtmfAccess,
        runtime::{dtmf::DigitOutcome, links::LinkEffect},
    };
    use serde_json::json;
    for retire in [false, true] {
        let mut runtime = command_runtime(
            json!({"mappings":{"call":{"command":"B","protected":false,"action":"call_macro","macro":"read"}}}),
            &[(
                "macro",
                "read",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":12}},{"action":"radio","operation":"status","detail":"short"}]}),
            )],
        );
        let source = PeerSignals::new().reply_target();
        runtime.incoming(PORT, "2000", true).unwrap();
        let links = runtime.node(PORT).unwrap().links();
        links.configure_remote("6", RemoteDtmfAccess::Open).unwrap();
        links
            .attach_ingress(PeerIdentity::from_operator("2000").unwrap(), source.clone())
            .unwrap();
        assert!(links.peer_digit("2000", 'B', 10, source.clone()).is_none());
        let Some(DigitOutcome::Mapped(input)) = links.peer_digit("2000", '#', 10, source.clone())
        else {
            panic!("peer mapping")
        };
        let at = |n| RuntimeClock {
            now_ms: n,
            ..clock()
        };
        let LinkEffect::CommandQueued(response) =
            runtime.dispatch_mapping(PORT, input, at(10)).unwrap()
        else {
            panic!("macro receipt")
        };
        assert!(runtime.advance_commands(PORT, at(10)).unwrap().is_none());
        let links = runtime.node(PORT).unwrap().links();
        assert!(links.expire_peer_digits(11).is_empty());
        if retire {
            links.retire_source(&source);
        }
        let effect = runtime.advance_commands(PORT, at(22)).unwrap();
        if retire {
            assert!(effect.is_none());
            assert_eq!(response.outcome(), Some(MacroOutcome::Cancelled));
        } else {
            let Some((LinkEffect::RemoteBase(op), receipt)) = effect else {
                panic!("private read from retained collector")
            };
            assert_eq!(op.reply.as_ref(), Some(&source));
            assert!(
                runtime
                    .node(PORT)
                    .unwrap()
                    .links()
                    .remote_authorized(&op, 22)
            );
            receipt.settle(true);
            assert!(runtime.advance_commands(PORT, at(23)).unwrap().is_none());
            assert_eq!(response.outcome(), Some(MacroOutcome::Completed));
        }
        runtime.node(PORT).unwrap().links().ended("2000");
        runtime.peer_detached(PORT, "2000", 24);
        assert!(runtime.stop(24));
    }
}

#[test]
fn registered_non_ingress_selection_preserves_the_receivers_own_target() {
    use crate::{
        command_language::{MacroOutcome, prepare_runtime_action},
        runtime::{
            dtmf::{DigitEvent, DigitOutcome},
            links::LinkEffect,
        },
    };
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({"mappings":{"select":{"command":"A","protected":false,"action":"select_command_target"}}}),
        &[
            (
                "macro",
                "steal",
                json!({"actions":[{"action":"select_command_target","parameter":"3000"}]}),
            ),
            (
                "event",
                "boot",
                json!({"start":{"at":"controller_start","macro":"steal"}}),
            ),
        ],
    );
    runtime.incoming(PORT, "2000", true).unwrap();
    let mut outcome = None;
    for digit in "A2000#".chars() {
        outcome = runtime.digit(
            PORT,
            DigitEvent::Digit {
                digit,
                now_ms: clock().now_ms,
            },
        );
    }
    let Some(DigitOutcome::Mapped(input)) = outcome else {
        panic!("local selection")
    };
    let LinkEffect::VerifyCommandTarget(op) =
        runtime.dispatch_mapping(PORT, input, clock()).unwrap()
    else {
        panic!("directory check")
    };
    runtime.command(PORT, op, clock(), true).unwrap();
    let select =
        prepare_runtime_action(&json!({"action":"select_command_target","parameter":"4000"}))
            .unwrap();
    assert!(matches!(
        runtime.management_action(
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            "mgr",
            &select,
            clock()
        ),
        Err(RuntimeError::Unavailable)
    ));
    assert!(runtime.advance_commands(PORT, clock()).unwrap().is_none());
    assert_eq!(
        runtime
            .node(PORT)
            .unwrap()
            .take_command_completion()
            .unwrap()
            .outcome,
        MacroOutcome::Rejected
    );
    let Some(DigitOutcome::Operation(op)) = runtime.digit(
        PORT,
        DigitEvent::Digit {
            digit: '5',
            now_ms: clock().now_ms,
        },
    ) else {
        panic!("RF forwarded digit")
    };
    assert_eq!(op.command.node, "2000");
    runtime.node(PORT).unwrap().links().ended("2000");
    runtime.peer_detached(PORT, "2000", clock().now_ms);
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn registered_numeric_alias_waits_for_requester_then_fans_out_only_to_configured_audience() {
    use crate::{
        audio::LinkAudioQueue,
        link::{AudioPeer, LinkAudio, Mode, PeerIdentity, PeerInput, PeerSignals, SourcePosition},
        runtime::{dtmf::DigitOutcome, links::LinkEffect},
    };
    use serde_json::json;
    struct Input(Arc<PeerSignals>);
    impl PeerInput for Input {
        fn source_rate(&self) -> u32 {
            48000
        }
        fn signals(&self) -> &PeerSignals {
            &self.0
        }
        fn available(&self) -> u64 {
            0
        }
        fn render(&mut self, output: &mut [f32]) -> Option<usize> {
            output.fill(0.);
            Some(0)
        }
    }
    for route in ["local", "peers", "local_and_links"] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut rt = command_runtime_media_settings(
            PORT,
            true,
            None,
            json!({"mappings":{"alias":{"command":"A","protected":false,"action":"message"}}}),
            &[(
                "message",
                "report",
                json!({"dtmf_id":"7","locales":{"en-US":{"text":"Ready","tts":"Ready","morse":"OK"}},"playback":{"destination":route}}),
            )],
            &CapturingMedia(log.clone()),
        );
        let mut add = |name: &str| {
            let sig = Arc::new(PeerSignals::new());
            rt.incoming(PORT, name, true).unwrap();
            rt.node(PORT)
                .unwrap()
                .links()
                .attach_ingress(
                    PeerIdentity::from_operator(name).unwrap(),
                    sig.reply_target(),
                )
                .unwrap();
            let (out, read) = LinkAudioQueue::new(4096, 64).unwrap().into_endpoints();
            (
                AudioPeer::new(name, Mode::TRANSCEIVE, Input(sig.clone()), out, 256, 0).unwrap(),
                sig,
                read,
            )
        };
        let (first, caller, mut first_out) = add("2000");
        let (second, _, mut other_out) = add("3000");
        // A live token from an unrelated transport cannot impersonate this link's D envelope.
        assert!(
            rt.node(PORT)
                .unwrap()
                .links()
                .peer_text(
                    "2000",
                    b"D 1000 2000 1 A",
                    PeerSignals::new().reply_target(),
                    false,
                    0,
                    9
                )
                .is_err()
        );
        let (mut mixer, mut distributor) = LinkAudio::new(vec![first, second], 256).unwrap();
        let (_, mut tx) = rt.node(PORT).unwrap().register_audio().unwrap();
        let mut owner = tx.acquire().unwrap();
        let mut outcome = None;
        for digit in "A7#".chars() {
            outcome =
                rt.node(PORT)
                    .unwrap()
                    .links()
                    .peer_digit("2000", digit, 10, caller.reply_target());
        }
        let Some(DigitOutcome::Mapped(input)) = outcome else {
            panic!("numeric alias")
        };
        assert!(matches!(
            rt.dispatch_mapping(PORT, input, clock()),
            Ok(LinkEffect::None)
        ));
        let mut render = || {
            let mut rf = [0.; 256];
            let mut a = [0.; 256];
            let mut b = [0.; 256];
            let key = mixer
                .process(
                    &mut owner.state().controller,
                    false,
                    &mut rf,
                    SourcePosition::default(),
                )
                .unwrap();
            distributor.dispatch(1);
            first_out.read(&mut a);
            other_out.read(&mut b);
            let heard = |x: &[f32]| x.iter().any(|s| (*s - 0.25).abs() < 0.001);
            (heard(&rf), heard(&a), heard(&b), key)
        };
        caller.set_radio_keyed(true);
        let (rf, a, b, _) = render();
        assert_eq!((rf, a, b), (false, false, false));
        caller.set_radio_keyed(false);
        let mut heard = (false, false, false, false);
        for _ in 0..128 {
            let (rf, a, b, key) = render();
            heard.0 |= rf;
            heard.1 |= a;
            heard.2 |= b;
            heard.3 |= key && (rf || a || b);
        }
        assert_eq!(
            heard,
            (
                route != "peers",
                route != "local",
                route != "local",
                route != "peers"
            ),
            "{route}"
        );
        drop(owner);
        rt.node(PORT).unwrap().links().ended("2000");
        rt.node(PORT).unwrap().links().ended("3000");
        rt.peer_detached(PORT, "2000", 50);
        rt.peer_detached(PORT, "3000", 50);
        assert!(rt.stop(50));
    }
}

#[test]
fn registered_reload_releases_same_instant_order_after_retiring_queued_work() {
    use crate::{
        command_language::prepare_runtime_action,
        runtime::{DeviceUpdate, links::LinkEffect},
    };

    struct Staged(Arc<AtomicBool>);
    impl DeviceHandoff for Staged {
        fn quiesce(&mut self) -> bool {
            true
        }
        fn close(&mut self) {}
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
        fn poll_update(&mut self, _: &GenerationSettings) -> Result<DeviceUpdate, RuntimeError> {
            Ok(DeviceUpdate::Staged)
        }
        fn poll_finish_update(&mut self, _: bool) -> Result<bool, RuntimeError> {
            Ok(self.0.load(Ordering::Acquire))
        }
    }
    let dial = |target| json!({"actions":[{"action":"connect","parameter":target}]});
    let configuration = |gain| {
        station_fixture::configuration(
            &[(PORT, "North")],
            &[
                (
                    PORT,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"input_gain_db":gain}}),
                ),
                (
                    PORT,
                    ResourceKind::Allstarlink,
                    "",
                    json!({"node_number":1000}),
                ),
                (
                    PORT,
                    ResourceKind::Macro,
                    "block",
                    json!({"actions":[{"action":"pause","timing":{"duration_ms":21000}}]}),
                ),
                (PORT, ResourceKind::Macro, "first", dial("00201")),
                (PORT, ResourceKind::Macro, "second", dial("00202")),
                (
                    PORT,
                    ResourceKind::Event,
                    "first",
                    json!({"start":{"at":"every day at 0:02 in UTC","macro":"first"},"sequence_number":1}),
                ),
                (
                    PORT,
                    ResourceKind::Event,
                    "second",
                    json!({"start":{"at":"every day at 0:02 in UTC","macro":"second"},"sequence_number":2}),
                ),
            ],
        )
    };
    let tick = |second: i64| RuntimeClock {
        wall_seconds: second,
        now_ms: clock().now_ms + (second - 100) as u64 * 1000,
        ..clock()
    };
    for finish_during_handoff in [false, true] {
        let ready = Arc::new(AtomicBool::new(false));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(Staged(ready.clone())) as Box<dyn DeviceHandoff>)
        };
        let mut runtime =
            station_fixture::start(configuration(0), &Media, adapter, device, tick(100)).unwrap();
        let block =
            prepare_runtime_action(&json!({"action":"call_macro","macro":"block"})).unwrap();
        runtime
            .management_action(PORT.parse().unwrap(), "operator", &block, tick(100))
            .unwrap();
        assert!(runtime.advance_commands(PORT, tick(100)).unwrap().is_none());
        assert!(runtime.advance_commands(PORT, tick(120)).unwrap().is_none());
        station_fixture::reload(
            &mut runtime,
            configuration(-6),
            &Media,
            adapter,
            device,
            tick(120),
        )
        .unwrap();
        assert!(runtime.reload_pending());
        if finish_during_handoff {
            assert!(runtime.advance_commands(PORT, tick(121)).unwrap().is_none());
            let Some((LinkEffect::Connect(first), receipt)) =
                runtime.advance_commands(PORT, tick(121)).unwrap()
            else {
                panic!("the incumbent must still own the first scheduled macro");
            };
            assert_eq!(first.remote(), "00201");
            runtime.node(PORT).unwrap().links().cancel_connect(first);
            receipt.settle(true);
            assert!(runtime.advance_commands(PORT, tick(121)).unwrap().is_none());
        }

        ready.store(true, Ordering::Release);
        assert_eq!(runtime.poll_reload(tick(122)), Ok(true));
        let Some((LinkEffect::Connect(second), receipt)) =
            runtime.advance_commands(PORT, tick(122)).unwrap()
        else {
            panic!(
                "the successor was stranded after reload, incumbent completed={finish_during_handoff}"
            );
        };
        assert_eq!(second.remote(), "00202");
        runtime.node(PORT).unwrap().links().cancel_connect(second);
        receipt.settle(true);
        assert!(runtime.advance_commands(PORT, tick(122)).unwrap().is_none());
        assert!(runtime.stop(tick(122).now_ms));
    }
}

#[test]
fn pending_device_staging_commit_and_uncertain_reverse_retain_the_transaction() {
    let mut devices = station_fixture::DevicePlans::default();
    use crate::runtime::DeviceUpdate;
    #[derive(Default)]
    struct Progress {
        staged: bool,
        finish: bool,
        reverse_failed: bool,
        drained: bool,
        attempts: Vec<&'static str>,
    }
    struct Pending(Arc<Mutex<Progress>>);
    impl DeviceHandoff for Pending {
        fn quiesce(&mut self) -> bool {
            let mut state = self.0.lock().unwrap();
            state.attempts.push("quiesce");
            state.drained
        }
        fn close(&mut self) {
            self.0.lock().unwrap().attempts.push("close");
        }
        fn open(&mut self, settings: &GenerationSettings) -> bool {
            self.0.lock().unwrap().attempts.push("open");
            settings.device != "fail"
        }
        fn poll_update(&mut self, _: &GenerationSettings) -> Result<DeviceUpdate, RuntimeError> {
            let mut state = self.0.lock().unwrap();
            state.attempts.push("stage");
            Ok(if state.staged {
                DeviceUpdate::Staged
            } else {
                DeviceUpdate::Pending
            })
        }
        fn poll_finish_update(&mut self, commit: bool) -> Result<bool, RuntimeError> {
            let mut state = self.0.lock().unwrap();
            state
                .attempts
                .push(if commit { "commit" } else { "reverse" });
            if !commit && state.reverse_failed {
                Err(RuntimeError::Restore)
            } else {
                Ok(state.finish)
            }
        }
    }
    for shutdown in [true, false] {
        let state = Arc::new(Mutex::new(Progress::default()));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(Pending(state.clone())) as Box<dyn DeviceHandoff>)
        };
        let configuration = |gain: i32, fail: bool| {
            let mut ports = vec![(PORT, "1000")];
            let mut resources = vec![(
                PORT,
                ResourceKind::Interface,
                "",
                json!({"audio":{"device_identifier":"first","input_gain_db":gain}}),
            )];
            if fail {
                ports.push((SECOND, "2000"));
                resources.push((
                    SECOND,
                    ResourceKind::Interface,
                    "",
                    json!({"audio":{"device_identifier":"fail"}}),
                ));
            }
            station_fixture::configuration(&ports, &resources)
        };
        let mut runtime = devices
            .start(configuration(0, false), &Media, adapter, device, clock())
            .unwrap();
        let previous = runtime.revision();
        devices
            .reload(
                &mut runtime,
                configuration(-6, false),
                &Media,
                adapter,
                device,
                clock(),
            )
            .unwrap();
        assert!(runtime.reload_pending());
        assert_eq!(runtime.revision(), previous);
        state.lock().unwrap().staged = true;
        assert_eq!(runtime.poll_reload(clock()), Ok(false));
        assert_eq!(runtime.revision(), previous);
        assert!(state.lock().unwrap().attempts.contains(&"commit"));
        state.lock().unwrap().finish = true;
        assert_eq!(runtime.poll_reload(clock()), Ok(true));
        assert!(runtime.revision() > previous);
        assert!(runtime.detached(PORT, previous));
        let incumbent = runtime.revision();
        {
            let mut steps = state.lock().unwrap();
            steps.finish = false;
            steps.drained = false;
            steps.attempts.clear();
        }
        devices
            .reload(
                &mut runtime,
                configuration(0, true),
                &Media,
                adapter,
                device,
                clock(),
            )
            .unwrap();
        assert_eq!(runtime.revision(), incumbent);
        assert!(runtime.reload_pending());
        assert!(state.lock().unwrap().attempts.contains(&"reverse"));
        state.lock().unwrap().reverse_failed = true;
        assert_eq!(runtime.poll_reload(clock()), Ok(false));
        assert!(runtime.reload_pending());
        if shutdown {
            assert!(!runtime.stop(0));
            state.lock().unwrap().drained = true;
            assert!(runtime.stop(0));
            continue;
        }
        state.lock().unwrap().drained = true;
        assert_eq!(runtime.poll_reload(clock()), Err(RuntimeError::Restore));
        assert!(runtime.detached(PORT, incumbent));
        runtime.reclaim();
        state.lock().unwrap().attempts.clear();
        devices
            .reload(
                &mut runtime,
                configuration(-6, false),
                &Media,
                adapter,
                device,
                clock(),
            )
            .unwrap();
        assert!(!runtime.reload_pending());
        assert!(state.lock().unwrap().attempts.contains(&"open"));
        assert!(runtime.node(PORT).unwrap().work().is_some());
        assert!(runtime.stop(0));
    }
}

#[test]
fn equipment_macro_retains_callers_deadline_after_a_wait() {
    use crate::command_language::{MacroOutcome, prepare_runtime_action};
    use serde_json::json;
    let mut runtime = command_runtime(
        json!({"macro_limits":{"timing":{"max_duration_ms":10}}}),
        &[
            (
                "macro",
                "parent",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":5}},
            {"action":"set_transmit_inhibit","enabled":true}]}),
            ),
            (
                "macro",
                "child",
                json!({"actions":[{"action":"pause","timing":{"duration_ms":5}},
            {"action":"connect","parameter":"00204"}]}),
            ),
        ],
    );
    let port = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap();
    let now = |delta| RuntimeClock {
        now_ms: clock().now_ms + delta,
        ..clock()
    };
    let parent = prepare_runtime_action(&json!({"action":"call_macro","macro":"parent"})).unwrap();
    let (_, links::LinkEffect::CommandQueued(_)) = runtime
        .management_action(port, "operator", &parent, now(0))
        .unwrap()
    else {
        panic!("parent admission")
    };
    assert!(runtime.advance_commands(PORT, now(0)).unwrap().is_none());
    let Some((links::LinkEffect::SiteCommand(_, permit), receipt)) =
        runtime.advance_commands(PORT, now(5)).unwrap()
    else {
        panic!("site action from actual parent")
    };
    let links::LinkEffect::CommandQueued(child) = runtime
        .equipment_macro(PORT, "child", Some(&permit), now(5))
        .unwrap()
    else {
        panic!("child admission")
    };
    receipt.settle(true);
    assert!(runtime.advance_commands(PORT, now(5)).unwrap().is_none());
    assert!(runtime.advance_commands(PORT, now(5)).unwrap().is_none());
    let effect = runtime.advance_commands(PORT, now(11)).unwrap();
    assert!(
        effect.is_none(),
        "expired equipment child emitted a domain effect"
    );
    assert_eq!(child.outcome(), Some(MacroOutcome::Limit));
    assert!(runtime.stop(now(12).now_ms));
}

#[test]
fn busy_dial_speech_reaches_only_the_originating_radio_while_peer_routes_are_live() {
    use crate::{
        audio::LinkAudioQueue,
        link::{
            AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals, PhoneAccountId, PhoneCallId,
            PhoneSource, SourcePosition,
        },
    };

    struct Quiet(PeerSignals);
    impl PeerInput for Quiet {
        fn source_rate(&self) -> u32 {
            48_000
        }
        fn signals(&self) -> &PeerSignals {
            &self.0
        }
        fn available(&self) -> u64 {
            0
        }
        fn render(&mut self, out: &mut [f32]) -> Option<usize> {
            out.fill(0.0);
            None
        }
    }

    let messages = Arc::new(Mutex::new(Vec::new()));
    let devices = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = station_fixture::start(
        station_fixture::configuration(
            &[(PORT, "1000")],
            &[
                (
                    PORT,
                    ResourceKind::Radio,
                    "",
                    json!({"duplex_mode": "full"}),
                ),
                (
                    PORT,
                    ResourceKind::TxSignaling,
                    "",
                    json!({"ptt": {"timing": {"hang_ms": 0}}}),
                ),
                (
                    PORT,
                    ResourceKind::RxSignaling,
                    "",
                    json!({"timing": {"kerchunk_max_ms": 0}}),
                ),
            ],
        ),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(devices.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let call = PhoneSource::new(
        PhoneAccountId::new("private").unwrap(),
        1,
        PhoneCallId::new([3; 16]).unwrap(),
        1,
        3,
    )
    .unwrap();
    let private = PeerSignals::for_phone(call.clone());
    assert!(private.authenticate_phone());
    let (wire_peer, mut from_peer) = LinkAudioQueue::new(960, 8).unwrap().into_endpoints();
    let (wire_call, mut from_call) = LinkAudioQueue::new(960, 8).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        "2000",
        Mode::TRANSCEIVE,
        Quiet(PeerSignals::new()),
        wire_peer,
        960,
        0,
    )
    .unwrap();
    let handset = AudioPeer::for_phone(
        &call,
        &private.reply_target(),
        Mode::phone(true),
        Quiet(private),
        wire_call,
        960,
    )
    .unwrap();
    let (mut mixer, mut dispatch) = LinkAudio::new(vec![peer, handset], 960).unwrap();
    let (mut rx, mut tx) = runtime.node(PORT).unwrap().register_audio().unwrap();
    let mut station = rx.acquire_pair(&mut tx).unwrap();
    let controller = &mut station.transmit().controller;
    let mut origin = [0.125; 960];
    mixer
        .process(controller, true, &mut origin, SourcePosition::default())
        .unwrap();
    dispatch.dispatch(1);
    for output in [&mut from_peer, &mut from_call] {
        let mut pcm = [0.0; 960];
        assert!(output.read_span(&mut pcm).is_some());
        assert!(
            pcm.contains(&0.125),
            "local carrier reaches each connected recipient"
        );
    }

    let before = messages.lock().unwrap().len();
    runtime
        .queue_phone_dial_failure(PORT, 486, || true)
        .unwrap();
    let mut audible = false;
    let mut leaked = false;
    for _ in 0..40 {
        origin.fill(0.0);
        let keyed = mixer
            .process(controller, false, &mut origin, SourcePosition::default())
            .unwrap();
        audible |= keyed && origin.iter().any(|&x| x > 0.1);
        dispatch.dispatch(1);
        for output in [&mut from_peer, &mut from_call] {
            let mut pcm = [0.0; 960];
            if output.read_span(&mut pcm).is_some() {
                leaked |= pcm.iter().any(|&x| x != 0.0);
            }
        }
    }
    drop(station);
    assert!(runtime.stop(0));
    assert_eq!(&messages.lock().unwrap()[before..], &["The line is busy."]);
    assert!(
        audible,
        "the originating station actually rendered the 486 speech stream"
    );
    assert!(
        !leaked,
        "neither the network destination nor the separate caller may hear the reply"
    );
}

#[test]
fn permanent_group_config_is_expanded_into_ordered_routes() {
    let configuration = station_fixture::configuration(
        &[(PORT, "524950")],
        &[(
            PORT,
            ResourceKind::PermanentGroup,
            "blind-hams",
            json!({
                "remote_node":["506315","506312","506310","506311","506313","506314"],
                "group_name":"The Blind Hams Network"
            }),
        )],
    );
    let mut runtime = station_fixture::start(
        configuration,
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let routes = runtime
        .node(PORT)
        .unwrap()
        .links()
        .schedule()
        .unwrap()
        .route_specs();
    assert_eq!(
        routes
            .iter()
            .map(|route| route.remote.as_str())
            .collect::<Vec<_>>(),
        ["506315", "506312", "506310", "506311", "506313", "506314"]
    );
    assert!(routes.iter().all(|route| {
        route.group_label.as_deref() == Some("blind-hams")
            && route.group_name.as_deref() == Some("The Blind Hams Network")
    }));
    assert_eq!(
        routes
            .iter()
            .map(|route| route.group_priority)
            .collect::<Vec<_>>(),
        (0..6).map(Some).collect::<Vec<_>>()
    );
    assert!(runtime.stop(0));
}

#[path = "coffee_transition_tests.rs"]
mod coffee_transition;
