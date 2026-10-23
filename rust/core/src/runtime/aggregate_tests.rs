use super::*;
use crate::{
    command::LinkAction,
    config::{ConfigDocument, ResolvedNodeSettings},
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
        _: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        for source in program.primary {
            if let (Some(messages), Some(speech)) = (&self.0, source.speech) {
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
fn manual_identifier_and_tail_success_satisfy_their_automatic_opportunity() {
    for (family, timing) in [
        ("identifier", "regardless_of_activity=yes\n"),
        ("tail", "delay_ms=0\n"),
    ] {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = Runtime::start(
            ConfigDocument::parse(&format!(
                "[1000]\nduplex=full\ntransmit_hang_ms=0\nkerchunk_max_ms=0\n[{family} 1000 report]\ninterval_ms=1000\n{timing}speech_text=Report\nmorse_text=E\ndtmf_id=1\n"
            )).unwrap(),
            &CapturingMedia(messages.clone()), adapter,
            |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
            clock(),
        ).unwrap();
        let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
        let mut generation = tx.acquire().unwrap();
        let controller = &mut generation.state().controller;
        controller.process_audio(false, false, &[], &mut [0.0; 12_000]);
        runtime.queue_message("1000", "1", None).unwrap();
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
        drop(generation);
        assert!(runtime.stop(0));
    }
}

#[test]
fn site_manual_command_publishes_inhibit_without_waiting_for_poll() {
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nduplex=full\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut actual = Vec::new();
    for inhibit in [true, false] {
        runtime
            .node("1000")
            .unwrap()
            .site_command(crate::site::SiteCommand::Inhibit(inhibit), 0)
            .unwrap();
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
    let document = ConfigDocument::parse("[1000]\nduplex=full\n").unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        document.clone(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let node = runtime.node("1000").unwrap();
    node.site()
        .command(crate::site::SiteCommand::Inhibit(true), 0)
        .unwrap();
    runtime
        .reload(
            document,
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
            clock(),
        )
        .unwrap();
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let source = "[1000]\n[sensor 1000 supply]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\nchannel=in1_input\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=/dev/gpiochip0\nline=7\n";
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = Runtime::start(
        ConfigDocument::parse(source).unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
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
    let site = runtime.node("1000").unwrap().site();
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
    runtime
        .reload(
            ConfigDocument::parse(&format!("{source}pulse_ms=900\n")).unwrap(),
            &Media,
            adapter,
            device,
            now,
        )
        .unwrap();
    let snapshot = runtime.node("1000").unwrap().site().snapshot(1200);
    assert_eq!(snapshot.outputs["relay"].pulse_deadline_ms, Some(1500));
    assert_eq!(snapshot.sensors["supply"].minimum.unwrap().value, 13.8);
    assert!(runtime.detached("1000", 1));
    runtime.reclaim();
    runtime
        .node("1000")
        .unwrap()
        .site()
        .advance(1500, &observations, &Default::default());
    assert!(!runtime.node("1000").unwrap().site().snapshot(1500).outputs["relay"].requested_on);
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
        let mut runtime = Runtime::start(ConfigDocument::parse("[1000]\n[sensor 1000 outdoor]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\nchannel=temp1_input\n[message temperature]\nmessage_id=site-temperature\narg_temperature=sensor:outdoor\ndtmf_id=1\n").unwrap(), &CapturingMedia(messages.clone()), adapter, |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock()).unwrap();
        runtime.node("1000").unwrap().site().advance(
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
        runtime.queue_message("1000", "1", None).unwrap();
        let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
        ("1", 1000, "battery is 13.3 volts."),
        ("1", 6000, "battery is unavailable."),
        ("2", 1000, "door is inactive."),
        ("99", 1000, "The requested site reading is unavailable."),
    ] {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = Runtime::start(
            ConfigDocument::parse("[1000]\n[sensor 1000 battery]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\nchannel=in1_input\nunit=volts\nprecision=1\ndtmf_id=1\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=/dev/gpiochip0\nline=3\ndtmf_id=2\n").unwrap(),
            &CapturingMedia(messages.clone()), adapter,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock(),
        ).unwrap();
        let peer = crate::link::PeerSignals::new();
        peer.set_radio_keyed(true);
        runtime
            .queue_site_report("1000", selector, Some(peer.reply_target()))
            .unwrap();
        let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
        let mut generation = rx.acquire_pair(&mut tx).unwrap();
        let mut audio = [0.0; 960];
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut audio);
        runtime.prepare_messages(clock());
        assert!(messages.lock().unwrap().is_empty());
        runtime.node("1000").unwrap().site().advance(
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
            },
        );
        assert!(audio.iter().all(|sample| *sample == 0.0));
        assert!(
            replies
                .iter()
                .any(|sample| sample.sample != 0.0 && sample.target == peer.reply_identity())
        );
        assert_eq!(
            runtime.node("1000").unwrap().site().snapshot(1000).sensors["battery"]
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for selector in ["", "0", "01", "unknown"] {
        assert_eq!(
            runtime.queue_site_report("1000", selector, None),
            Err(RuntimeError::Rejected)
        );
    }
    assert_eq!(
        runtime.queue_site_report("missing", "1", None),
        Err(RuntimeError::MissingNode)
    );
    for _ in 0..4 {
        runtime.queue_site_report("1000", "99", None).unwrap();
    }
    assert_eq!(
        runtime.queue_site_report("1000", "99", None),
        Err(RuntimeError::Rejected),
        "invalid selectors must not consume reply capacity"
    );
    assert!(runtime.stop(0));
}

#[test]
fn site_named_notification_retries_after_queue_pressure_and_plays_locally() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n[message ready]\ncontent=Ready\ndtmf_id=1\n").unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert_eq!(
        runtime.queue_named_message("missing", "ready"),
        Err(RuntimeError::MissingNode)
    );
    assert_eq!(
        runtime.queue_named_message("1000", "missing"),
        Err(RuntimeError::Preparation)
    );
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    for _ in 0..4 {
        runtime
            .queue_message("1000", "1", Some(peer.reply_target()))
            .unwrap();
    }
    assert_eq!(runtime.queue_named_message("1000", "ready"), Ok(false));
    runtime.prepare_messages(clock());
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    peer.end();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12001]);
    runtime.prepare_messages(clock());
    assert_eq!(runtime.queue_named_message("1000", "ready"), Ok(true));
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
        },
    );
    assert!(audio.iter().any(|sample| *sample != 0.0));
    assert!(replies.iter().all(|sample| sample.sample == 0.0));
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn composition_runtime_ids_and_tails_keep_identification_with_unavailable_meter() {
    use crate::site::{Observation, ReadingQuality, SiteObservations};
    for family in ["identifier", "tail"] {
        for valid in [true, false] {
            let messages = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::new(Mutex::new(Vec::new()));
            let timing = if family == "identifier" {
                "interval_ms=1\nregardless_of_activity=yes\n"
            } else {
                ""
            };
            let document = ConfigDocument::parse(&format!(
                "[1000]\ntransmit_hang_ms=0\nkerchunk_max_ms=0\n[sensor 1000 outdoor]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\nchannel=temp1_input\n[{family} 1000 regular]\n{timing}content=Repeater. {{message_macro_temperature}} {{message_macro_time}}\n[message temperature]\nmessage_id=site-temperature\narg_temperature=sensor:outdoor\n[message time]\nmessage_id=site-time\narg_time=time\n"
            )).unwrap();
            let mut runtime = Runtime::start(
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
                .node("1000")
                .unwrap()
                .site()
                .advance(1000, &readings, &Default::default());
            let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\ntransmit_hang_ms=0\nkerchunk_max_ms=0\n[tail 1000 time]\ncontent={message_macro_time}\n[message time]\nmessage_id=site-time\narg_time=time\n").unwrap(),
        &CapturingMedia(messages.clone()), adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock(),
    ).unwrap();
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n[identifier 1000 regular]\ninterval_ms=1\nregardless_of_activity=yes\ncontent={message_macro_time}\n[message time]\nmessage_id=site-time\narg_time=time\n").unwrap(),
        &CapturingMedia(messages.clone()), adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>), clock(),
    ).unwrap();
    assert!(messages.lock().unwrap().is_empty());
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
fn composition_runtime_command_waits_for_unkey_and_keeps_exact_peer() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[message time]\nmessage_id=site-time\narg_time=time\ndtmf_id=1\n",
        )
        .unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let peer = crate::link::PeerSignals::new();
    peer.set_radio_keyed(true);
    runtime
        .queue_message("1000", "1", Some(peer.reply_target()))
        .unwrap();
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
        },
    );
    assert!(audio.iter().all(|sample| *sample == 0.0));
    assert!(
        replies
            .iter()
            .any(|sample| sample.sample != 0.0 && sample.target == peer.reply_identity())
    );
    let other = crate::link::PeerSignals::new();
    peer.end();
    assert_eq!(
        runtime.queue_message("1000", "1", Some(peer.reply_target())),
        Err(RuntimeError::Rejected)
    );
    assert!(
        runtime
            .queue_message("1000", "1", Some(other.reply_target()))
            .is_ok()
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn composition_runtime_full_queue_retries_after_adapter_replacement() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[message time]\nmessage_id=site-time\narg_time=time\ndtmf_id=1\n",
        )
        .unwrap(),
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
            .queue_message("1000", "1", Some(peer.reply_target()))
            .unwrap();
    }
    assert_eq!(
        runtime.queue_message("1000", "1", None),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_message("1000", "2", None),
        Err(RuntimeError::Rejected)
    );
    runtime.prepare_messages(clock());
    assert!(messages.lock().unwrap().is_empty());
    let settings = runtime.settings("1000").unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1001),
        Ok(2)
    );
    assert!(runtime.detached("1000", 1));
    runtime.reclaim();
    peer.end();
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12001]);
    runtime.prepare_messages(clock());
    runtime.queue_message("1000", "1", None).unwrap();
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

#[test]
fn pad_test_queues_one_local_stream_after_unkey() {
    use crate::runtime::dtmf::{DigitEvent, DigitOutcome, PadTestResult};
    let messages = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for digit in "*82*#ABCD".chars() {
        assert!(
            runtime
                .digit("1000", DigitEvent::Digit { digit, now_ms: 1 })
                .is_none()
        );
    }
    assert!(messages.lock().unwrap().is_empty());
    let Some(DigitOutcome::PadTest(report)) =
        runtime.digit("1000", DigitEvent::Unkey { now_ms: 2 })
    else {
        panic!("pad result")
    };
    runtime.queue_pad_test("1000", &report).unwrap();
    assert_eq!(*messages.lock().unwrap(), ["star pound A B C D"]);
    assert!(
        runtime
            .digit("1000", DigitEvent::Unkey { now_ms: 3 })
            .is_none()
    );
    for _ in 0..3 {
        runtime.queue_pad_test("1000", &report).unwrap();
    }
    assert_eq!(
        runtime.queue_pad_test("1000", &report),
        Err(RuntimeError::Rejected)
    );
    assert_eq!(
        runtime.queue_pad_test("missing", &report),
        Err(RuntimeError::MissingNode)
    );
    assert_eq!(
        runtime.queue_pad_test(
            "1000",
            &PadTestResult {
                digits: "invalid".into(),
                overflowed: false
            }
        ),
        Err(RuntimeError::Preparation)
    );
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
        state: (),
        control: (),
        receive_maximum: 960,
        transmit_maximum: 960,
    })
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[general]\nstatus_snapshot_interval_ms=80\n[1000]\n[2000]\nstatus_snapshot_interval_ms=25\n",
        )
        .unwrap(),
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
        vec![("1000".to_owned(), 80), ("2000".to_owned(), 25)]
    );
    assert!(runtime.stop(0));
}

#[test]
fn changed_radio_settings_reopen_the_device_during_configuration_reload() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    log.lock().unwrap().clear();

    runtime
        .reload(
            ConfigDocument::parse("[1000]\n[radio 1000]\ncm119_profile=nhrc\n").unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();

    let events = log.lock().unwrap().clone();
    assert_eq!(events, ["quiesce", "close", "1000"]);
    assert_eq!(
        runtime.settings("1000").unwrap().radio.cm119_profile,
        crate::config::Cm119Profile::Nhrc
    );
    assert!(runtime.stop(0));
}

#[test]
fn admission_loss_discards_partial_commands_on_every_active_node() {
    use crate::runtime::dtmf::DigitEvent;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=first\n[2000]\nradio_channel=second\n")
            .unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    for local in ["1000", "2000"] {
        for digit in ['*', '7'] {
            assert!(
                runtime
                    .digit(local, DigitEvent::Digit { digit, now_ms: 1 })
                    .is_none()
            );
        }
    }
    runtime.discard_digits();
    for local in ["1000", "2000"] {
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
        for digit in ['*', '7'] {
            assert!(
                runtime
                    .digit(local, DigitEvent::Digit { digit, now_ms: 3 })
                    .is_none()
            );
        }
        assert!(matches!(
            runtime
                .digit(
                    local,
                    DigitEvent::Digit {
                        digit: '0',
                        now_ms: 4
                    }
                )
                .unwrap(),
            super::dtmf::DigitOutcome::Operation(operation) if operation.command.action == LinkAction::Status
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\nradio_channel=old\n[event 1000 morning]\nat=daily 09:07\nmessage=hello\n",
        )
        .unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    assert!(runtime.next_link().is_none());
    let event = runtime.next_event(clock()).unwrap().unwrap();
    runtime.queue_event(&event).unwrap();
    assert!(matches!(
        runtime.event_command(&event, clock()),
        Ok(links::LinkEffect::None)
    ));
    assert!(runtime.complete_event(&event));
    runtime
        .reload(
            ConfigDocument::parse("[1000]\nradio_channel=new\n").unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert_eq!(runtime.node("1000").unwrap().status(0).retiring, None);
    assert_eq!(runtime.settings("1000").unwrap().channel, "new");
    assert!(runtime.stop(0));
}

#[test]
fn retired_exposed_dispatcher_requires_detachment_even_when_current_was_never_exposed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert!(runtime.adapter_control_mut("1000", 1).is_some());
    let settings = runtime.settings("1000").unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1),
        Ok(2)
    );
    assert!(!runtime.stop(2));
    assert!(runtime.detached("1000", 1));
    assert!(runtime.detached("1000", 2));
    assert!(runtime.stop(3));
}

#[test]
fn repeated_detach_tracking_is_idempotent_and_unrelated_acknowledgments_preserve_it() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n[permanent 1000 main]\nremote_node=3000\n").unwrap(),
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
    assert!(runtime.authorize_incoming("1000", "2000", false).is_err());
    runtime.authorize_incoming("1000", "2000", true).unwrap();
    for _ in 0..2 {
        runtime.incoming("1000", "2000", true).unwrap();
        assert!(runtime.authorize_incoming("1000", "2000", true).is_err());
        runtime
            .command(
                "1000",
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
    runtime.peer_detached("1000", "2000", 0);
    assert!(runtime.next_link().is_some());
    assert!(runtime.stop(0));
}

#[test]
fn peer_set_publication_preserves_live_controller_and_does_not_force_past_a_hazard() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=radio\nduplex=full\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut old = receive.acquire_pair(&mut transmit).unwrap();
    old.transmit()
        .controller
        .process_audio(true, false, &[], &mut [1.0; 960]);
    let activity = old.transmit().controller.activity();
    let prior = activity.last_sample();
    let settings = runtime.settings("1000").unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1001),
        Err(RuntimeError::Busy)
    );
    assert_eq!(old.id(), 1);
    assert_eq!(activity.last_sample(), prior);
    drop(old);
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1002),
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
    assert_eq!(runtime.node("1000").unwrap().status(1003).retiring, Some(1));
    assert_eq!(
        runtime.reload(
            ConfigDocument::parse("[1000]\nradio_channel=radio\n").unwrap(),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock()
        ),
        Err(RuntimeError::Busy)
    );
    runtime.reclaim();
    assert_eq!(runtime.node("1000").unwrap().status(1003).retiring, Some(1));
    assert!(runtime.detached("1000", 1));
    runtime.reclaim();
    assert_eq!(runtime.node("1000").unwrap().status(1003).retiring, None);
    runtime
        .reload(
            ConfigDocument::parse("[1000]\nradio_channel=radio\n").unwrap(),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock(),
        )
        .unwrap();
    assert_eq!(receive.acquire_pair(&mut transmit).unwrap().id(), 3);
    runtime.detached("1000", 2);
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[courtesy 1000 link]\ninput=link\nspeech_text=Connected\nmorse_text=L\n",
        )
        .unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    let settings = runtime.settings("1000").unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1001),
        Ok(2)
    );

    assert!(runtime.detached("1000", 1));
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=old1\n[2000]\nradio_channel=old2\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    assert_eq!(runtime.node_names(), ["1000", "2000"]);
    let original = runtime.revision();
    assert_eq!(
        runtime.reload(
            ConfigDocument::parse("[1000]\nradio_channel=new1\n[2000]\nradio_channel=fail\n")
                .unwrap(),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Device)
    );
    assert_eq!(runtime.revision(), original);
    assert_eq!(runtime.settings("1000").unwrap().channel, "old1");
    assert!(
        log.lock()
            .unwrap()
            .ends_with(&["close".into(), "old1".into()])
    );
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn scheduled_event_composition_uses_the_shared_recipe_and_manual_selector() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[event 1000 clock]\nat=daily 09:07\ncontent={message}\nmessage_id=site-time\narg_time=time\ndtmf_id=4\n",
        )
        .unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .expect("a scheduled composition is a valid message source");
    let dispatch = runtime.next_event(clock()).unwrap().unwrap();
    runtime.queue_event(&dispatch).unwrap();
    assert!(
        messages.lock().unwrap().is_empty(),
        "values resolve at playback, not admission"
    );
    assert!(runtime.complete_event(&dispatch));
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12_001]);
    runtime.prepare_messages(clock());
    assert_eq!(*messages.lock().unwrap(), ["The time is 9:07 AM."]);
    drop(generation);
    assert!(runtime.queue_message("1000", "4", None).is_ok());
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_event_expired_while_receiving_does_not_start_after_unkey() {
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\nduplex=full\n[event 1000 notice]\nat=daily 09:07\nmessage=TEST\npolite_maximum_wait_ms=10\n",
        )
        .unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let dispatch = runtime.next_event(clock()).unwrap().unwrap();
    runtime.queue_event(&dispatch).unwrap();
    assert!(runtime.complete_event(&dispatch));
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(true, false, &[], &mut [0.0; 960]);
    let mut output = [0.0; 24_000];
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut output);
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "a skipped occurrence must never become audible later"
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_event_subsecond_lateness_does_not_renew_the_deadline() {
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n[event 1000 notice]\nat=daily 09:07\nmessage=TEST\npolite_maximum_wait_ms=100\n").unwrap(),
        &Media, adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    ).unwrap();
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut [0.0; 12_000]);
    let late = RuntimeClock {
        now_ms: 1250,
        wall_subsecond_ms: 250,
        ..clock()
    };
    let dispatch = runtime.next_event(late).unwrap().unwrap();
    runtime.queue_event(&dispatch).unwrap();
    assert!(runtime.complete_event(&dispatch));
    let mut output = [0.0; 960];
    generation
        .transmit()
        .controller
        .process_audio(false, false, &[], &mut output);
    assert_eq!(
        output, [0.0; 960],
        "the 100ms budget expired before the delayed control tick"
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_event_replaces_an_older_unstarted_occurrence() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nduplex=full\n[event 1000 clock]\nat=every 1 minute\ncontent={message}\nmessage_id=site-time\narg_time=time\npolite_maximum_wait_ms=300000\n").unwrap(),
        &CapturingMedia(messages.clone()), adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    ).unwrap();
    let first = runtime.next_event(clock()).unwrap().unwrap();
    runtime.queue_event(&first).unwrap();
    assert!(runtime.complete_event(&first));
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut generation = rx.acquire_pair(&mut tx).unwrap();
    for _ in 0..60 {
        generation
            .transmit()
            .controller
            .process_audio(true, false, &[], &mut [0.0; 48_000]);
    }
    let mut next = RuntimeClock {
        now_ms: 61_000,
        wall_seconds: 160,
        ..clock()
    };
    next.civil = Some((
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 8).unwrap(),
        0,
    ));
    let latest = runtime.next_event(next).unwrap().unwrap();
    runtime.queue_event(&latest).unwrap();
    assert!(runtime.complete_event(&latest));
    for _ in 0..80 {
        generation
            .transmit()
            .controller
            .process_audio(false, false, &[], &mut [0.0; 960]);
        runtime.prepare_messages(next);
    }
    assert_eq!(
        *messages.lock().unwrap(),
        ["The time is 9:08 AM."],
        "stale unstarted occurrences must be discarded, not replayed"
    );
    drop(generation);
    assert!(runtime.stop(0));
}

#[test]
fn aggregate_scheduled_template_and_macro_use_trigger_clock_and_current_node() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let config = ConfigDocument::parse("[1000]\ncallsign=AB1CD\n[template hello]\ntext=${node}. ${time}\n[macro connect]\naction=connect\ntarget_node=2000\n[event 1000 morning]\nat=daily 09:07\ntemplate=hello\nmacro=connect\n").unwrap();
    let mut runtime = Runtime::start(
        config,
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let dispatch = runtime.next_event(clock()).unwrap().unwrap();
    assert_eq!(dispatch.morse(), Some("1000. 9:07 AM"));
    assert!(dispatch.speech().unwrap().starts_with("node,1,0,0,0."));
    assert!(runtime.queue_event(&dispatch).is_ok());
    assert!(runtime.complete_event(&dispatch));
    assert!(runtime.next_event(clock()).unwrap().is_none());
    assert!(runtime.stop(clock().now_ms));
}

#[test]
fn successful_reload_retains_fixed_handles_hub_and_work_until_detached() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n[2000]\nnode_enabled=no\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let node = runtime.node("1000").unwrap();
    node.links().accept("3000", true).unwrap();
    let work = node.work().unwrap();
    let (mut rx, mut tx) = node.register_audio().unwrap();
    assert_eq!(rx.acquire().unwrap().id(), 1);
    runtime
        .reload(
            ConfigDocument::parse("[1000]\ntransmit_hang_ms=10\n[4000]\n").unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert!(!work.is_current());
    assert_eq!(runtime.node_names(), ["1000", "4000"]);
    assert!(
        runtime
            .node("1000")
            .unwrap()
            .links()
            .manager()
            .reaches("3000", true)
    );
    assert_eq!(runtime.settings("1000").unwrap().hang_ms, 10);
    assert_eq!(rx.acquire().unwrap().id(), 2);
    assert_eq!(tx.acquire().unwrap().id(), 2);
    assert!(runtime.detached("1000", 1));
    runtime.reclaim();
    assert!(runtime.node("1000").unwrap().status(0).retiring.is_some());
    drop(work);
    runtime.reclaim();
    assert!(runtime.node("1000").unwrap().status(0).retiring.is_none());
    assert!(!runtime.stop(2000));
    assert!(
        runtime
            .take_effects()
            .iter()
            .any(|(name, _)| name == "1000")
    );
    assert!(runtime.detached("1000", 2));
    runtime.reclaim();
    assert!(runtime.stop(2000));
    assert!(rx.acquire().is_none());
    assert!(tx.acquire().is_none());
}

#[test]
fn partial_start_closes_previously_opened_devices_and_bad_media_keeps_live_revision() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    assert!(matches!(
        Runtime::start(
            ConfigDocument::parse("[1000]\n[2000]\nradio_channel=fail\n").unwrap(),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Device)
    ));
    assert_eq!(*log.lock().unwrap(), ["1000", "fail", "close"]);
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let before = log.lock().unwrap().len();
    let bad = ConfigDocument::parse("[1000]\ntransmit_hang_ms=18446744073709551615\n").unwrap();
    assert_eq!(
        runtime.reload(bad, &Media, adapter, device, clock()),
        Err(RuntimeError::Preparation)
    );
    assert_eq!(runtime.revision(), 1);
    assert_eq!(log.lock().unwrap().len(), before);
    assert!(runtime.stop(0));
}

#[test]
fn no_window_permanent_links_work_without_wall_clock_and_window_reload_requires_it() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let text = "[1000]\n[permanent 1000 main]\nremote_node=2000\n";
    let mut now = clock();
    now.civil = None;
    let mut runtime = Runtime::start(
        ConfigDocument::parse(text).unwrap(),
        &Media,
        adapter,
        device,
        now,
    )
    .unwrap();
    assert!(runtime.next_link().is_some());
    let replacement = format!(
        "{text}[schedule 1000 daytime]\nremote_node=3000\nreplace_permanent=main\nstart_time=09:00\nend_time=10:00\n"
    );
    assert_eq!(
        runtime.reload(
            ConfigDocument::parse(&replacement).unwrap(),
            &Media,
            adapter,
            device,
            now
        ),
        Err(RuntimeError::Clock)
    );
    assert_eq!(runtime.revision(), 1);
    assert!(runtime.stop(0));
}

#[test]
fn scheduled_message_queue_retry_is_once_and_stale_dispatch_does_not_prepare_speech() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let text = "[1000]\n[event 1000 test]\nat=daily 09:07\nmessage=TEST\n";
    let mut runtime = Runtime::start(
        ConfigDocument::parse(text).unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let dispatch = runtime.next_event(clock()).unwrap().unwrap();
    for _ in 0..10 {
        assert_eq!(runtime.queue_event(&dispatch), Ok(()));
    }
    runtime
        .reload(
            ConfigDocument::parse(text).unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert_eq!(runtime.queue_event(&dispatch), Err(RuntimeError::Rejected));
    assert!(runtime.next_event(clock()).unwrap().is_none());
    assert!(runtime.stop(0));
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
    let config =
        "[1000]\n[identifier 1000 test]\nsound_file=test.wav\nspeech_text=hello\nmorse_text=TEST\n";
    let media = Prepared {
        rate: 22050,
        file_ok: true,
        calls: Mutex::new(Vec::new()),
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse(config).unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    assert!(media.calls.lock().unwrap().is_empty());
    assert!(runtime.stop(0));
}

#[test]
fn reconnect_reconciles_window_before_resuming_old_permanent_retry() {
    use super::{dtmf::DigitOperation, links::LinkEffect};
    use crate::command::{Command, LinkAction};
    let text = "[1000]\n[permanent 1000 main]\nremote_node=2000\n[schedule 1000 window]\nremote_node=3000\nreplace_permanent=main\nstart_time=10:00\nend_time=11:00\n";
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse(text).unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (_, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("permanent dial")
    };
    assert!(
        !runtime
            .finish_connect("1000", attempt, false, clock())
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
                "1000",
                operation(LinkAction::ReconnectAll),
                invalid_clock,
                false
            ),
            Err(RuntimeError::Clock)
        ));
    }
    runtime
        .command("1000", operation(LinkAction::DisconnectAll), clock(), false)
        .unwrap();
    let mut during = clock();
    during.civil = Some((
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 10, 0).unwrap(),
        0,
    ));
    runtime
        .command("1000", operation(LinkAction::ReconnectAll), during, false)
        .unwrap();
    assert!(runtime.next_link().is_none());
    runtime.peer_detached("1000", "2000", during.now_ms);
    assert!(
        !runtime
            .node("1000")
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
    let text = "[1000]\n[permanent 1000 main]\nremote_node=2000\n[schedule 1000 window]\nremote_node=3000\nreplace_permanent=main\nstart_time=12:00\nend_time=13:00\nend_inactivity_ms=60000\n";
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(text).unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        at(0, 12, 59),
    )
    .unwrap();
    runtime.tick_links(at(0, 12, 59));
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("replacement dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        runtime.finish_connect(&local, attempt, true, at(0, 12, 59)),
        Ok(true)
    );

    let (mut receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
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
        Some((local, LinkEffect::Detach(peers))) if local == "1000" && peers == ["3000"]
    ));
    runtime.peer_detached("1000", "3000", 60_000);
    drop((receive, transmit));
    assert!(runtime.stop(60_000));
}

#[test]
fn aggregate_schedule_warning_queues_one_localized_status_message() {
    let long_name = "N1".repeat(31) + "A";
    let long_setting = format!("group_name={long_name}\n");
    let long_speech = format!("Connecting to {}A in 1 minute", "N,1,".repeat(31));
    for (group, lead, hour, minute, second, expected) in [
        (
            "",
            "warning_before_start_ms=60000",
            11,
            59,
            0,
            "Connecting to weekday in 1 minute",
        ),
        (
            "group_name=Net 42\n",
            "warning_before_start_ms=90000",
            11,
            58,
            30,
            "Connecting to Net 42 in 1 minute, and 30 seconds",
        ),
        (
            "group_name=3000\n",
            "warning_before_start_ms=60000",
            11,
            59,
            0,
            "Connecting to 3000 in 1 minute",
        ),
        (
            "group_name=Evening Net\n",
            "warning_before_end_ms=60000",
            12,
            59,
            0,
            "Disconnecting from Evening Net in 1 minute",
        ),
        (
            long_setting.as_str(),
            "warning_before_start_ms=60000",
            11,
            59,
            0,
            long_speech.as_str(),
        ),
    ] {
        let config = format!(
            "[general]\nlanguage=fr-CA\n[1000]\n[permanent 1000 main]\nremote_node=2000\n[schedule 1000 weekday]\nremote_node=3000\n{group}replace_permanent=main\ndays=Monday-Friday\nstart_time=12:00\nend_time=13:00\nend_inactivity_ms=0\n{lead}\nwarning_message_id=scheduled-link-change\n"
        );
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
        let mut runtime = Runtime::start(
            ConfigDocument::parse(&config).unwrap(),
            &CapturingMedia(messages.clone()),
            adapter,
            |_, _| Ok(Box::new(Device(devices.clone())) as Box<dyn DeviceHandoff>),
            clock,
        )
        .unwrap();
        assert!(
            runtime
                .warnings()
                .iter()
                .any(|warning| warning.key == "language")
        );
        runtime.tick_links(clock);
        assert_eq!(*messages.lock().unwrap(), [expected], "{group}{lead}");
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
        super::render::scheduled_speech("prefix 1000. AB1CD. X1000.", "1000", "AB1CD", ""),
        "prefix node,1,0,0,0. A,B,1,C,D. X,1,0,0,0,."
    );
    assert_eq!(super::render::morse("[]{}"), None);
    assert_eq!(
        super::render::morse("hello [1000]"),
        Some("hello  1000 ".into())
    );
}

#[test]
fn removing_a_node_keeps_stalled_owned_callbacks_observable_until_detached() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let device = |_: &str, _: &ResolvedNodeSettings| {
        Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>)
    };
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let (mut rx, _tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let guard = rx.acquire().unwrap();
    runtime
        .reload(
            ConfigDocument::parse("[1000]\nnode_enabled=no\n").unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert!(runtime.node_names().is_empty());
    assert_eq!(
        runtime.reload(
            ConfigDocument::parse("[1000]\n").unwrap(),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Busy)
    );
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(SlowDevice(0)) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.incoming("1000", "2000", true).unwrap();
    assert!(!runtime.stop(0));
    assert!(!runtime.stop(0));
    assert_eq!(runtime.status(0)[0].1.state, LifecycleState::Stopping);
    runtime.peer_detached("1000", "2000", 0);
    assert!(runtime.detached("1000", 1));
    assert!(runtime.stop(0));
}

#[test]
fn failed_restore_keeps_current_document_but_gates_the_unavailable_node() {
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=old\n").unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let (mut rx, _tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    assert_eq!(
        runtime.reload(
            ConfigDocument::parse("[1000]\nradio_channel=new\n").unwrap(),
            &Media,
            adapter,
            device,
            clock()
        ),
        Err(RuntimeError::Restore)
    );
    assert_eq!(runtime.revision(), 1);
    assert_eq!(runtime.settings("1000").unwrap().channel, "old");
    assert!(rx.acquire().is_none());
    assert!(runtime.next_link().is_none());
    assert!(runtime.detached("1000", 1));
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=old\n").unwrap(),
        &Media,
        prepare,
        device,
        clock(),
    )
    .unwrap();
    let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    assert!(runtime.adapter_control_mut("1000", 1).is_some());
    runtime
        .reload(
            ConfigDocument::parse("[1000]\nradio_channel=new\n").unwrap(),
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
    assert!(runtime.adapter_control_mut("1000", 1).is_some());
    assert!(runtime.detached("1000", 1));
    runtime.reclaim();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(runtime.adapter_control_mut("1000", 2).is_some());
    assert!(!runtime.stop(0));
    assert!(runtime.adapter_control_mut("1000", 2).is_some());
    assert!(runtime.detached("1000", 2));
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
    let document = ConfigDocument::parse("[1000]\nunknown=yes\n").unwrap();
    let mut runtime = Runtime::start(
        document.clone(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    assert_eq!(runtime.document(), &document);
    assert_eq!(runtime.warnings().len(), 1);
    assert_eq!(runtime.node("1000").unwrap().settings().channel, "1000");
    assert_eq!(runtime.node("1000").unwrap().work().unwrap().id(), 1);
    assert_eq!(runtime.topology("1000").as_deref(), Some(""));
    assert_eq!(runtime.topology("missing"), None);
    assert!(
        runtime
            .digit("missing", super::dtmf::DigitEvent::Lost)
            .is_none()
    );
    assert!(
        runtime
            .digit("1000", super::dtmf::DigitEvent::Lost)
            .is_none()
    );
    assert!(!runtime.detached("missing", 1));
    assert!(runtime.adapter_control_mut("1000", 99).is_none());
    let (mut rx, _tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let events = runtime.node("1000").unwrap().drain_digits();
    let commands: Vec<_> = events
        .into_iter()
        .filter_map(|event| runtime.digit("1000", event))
        .collect();
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], super::dtmf::DigitOutcome::Operation(operation) if operation.command.action == LinkAction::Status)
    );
    runtime.node("1000").unwrap().discard_digits();
    assert!(runtime.node("1000").unwrap().drain_digits().is_empty());
    assert!(runtime.stop(1001));
    assert_eq!(
        runtime.replace_adapter(
            "missing",
            adapter(
                "1000",
                &ResolvedNodeSettings::resolve(
                    &document,
                    &crate::config::NodeId::new("1000").unwrap()
                )
                .unwrap()
                .value
            )
            .unwrap(),
            1002
        ),
        Err(RuntimeError::Busy)
    );
    assert_eq!(
        runtime.reload(
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
    let text = "[1000]\n[permanent 1000 main]\nremote_node=2000\n";
    let mut runtime = Runtime::start(
        ConfigDocument::parse(text).unwrap(),
        &Media,
        adapter,
        device,
        clock(),
    )
    .unwrap();
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("permanent dial")
    };
    assert_eq!(
        runtime.finish_connect(&local, attempt, true, clock()),
        Ok(true)
    );
    runtime
        .reload(
            ConfigDocument::parse("[1000]\n[permanent 1000 replacement]\nremote_node=3000\n")
                .unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
    assert!(runtime.next_link().is_none());
    let effects = runtime.take_effects();
    assert!(
        matches!(effects.as_slice(), [(local, LinkEffect::Detach(names))] if local == "1000" && names == &["2000"])
    );
    assert!(runtime.take_effects().is_empty());
    assert!(runtime.next_link().is_none());
    runtime.peer_detached("1000", "2000", 1001);
    let (local, LinkEffect::Connect(attempt)) = runtime.next_link().unwrap() else {
        panic!("replacement dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        runtime.finish_connect(&local, attempt, false, clock()),
        Ok(false)
    );
    runtime.detached("1000", 1);
    runtime.reclaim();
    runtime.stop(1002);
    for (local, effect) in runtime.take_effects() {
        if let LinkEffect::Detach(names) = effect {
            for remote in names {
                runtime.peer_detached(&local, &remote, 1002);
            }
        }
    }
    runtime.detached("1000", 2);
    assert!(runtime.stop(1003));
}

#[test]
fn adapter_replacement_rejects_changed_callback_bounds_and_pending_retirement() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone()))),
        clock(),
    )
    .unwrap();
    let settings = runtime.settings("1000").unwrap().clone();
    assert_eq!(
        runtime.replace_adapter("missing", adapter("1000", &settings).unwrap(), 1000),
        Err(RuntimeError::MissingNode)
    );
    for receive_changed in [false, true] {
        let mut prepared = adapter("1000", &settings).unwrap();
        if receive_changed {
            prepared.receive_maximum = 961;
        } else {
            prepared.transmit_maximum = 961;
        }
        assert_eq!(
            runtime.replace_adapter("1000", prepared, 1000),
            Err(RuntimeError::Busy)
        );
    }
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1000),
        Ok(2)
    );
    assert_eq!(
        runtime.replace_adapter("1000", adapter("1000", &settings).unwrap(), 1001),
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        |_, _| {
            Ok(PreparedAdapter {
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
    let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut guard = tx.acquire().unwrap();
    drop(runtime);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(guard.state().settings.channel, "1000");
    drop(guard);
    assert!(tx.acquire().is_none());
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
}

#[test]
fn event_macros_wait_for_message_admission_and_execute_each_existing_action() {
    for (action, target, message) in [
        ("connect", "3000", "message=TEST\n"),
        ("disconnect", "2000", ""),
        ("disconnect_all", "", ""),
        ("reconnect_all", "", ""),
    ] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let text = format!(
            "[1000]\n[macro 1000 operation]\naction={action}\ntarget_node={target}\n[event 1000 event]\nat=daily 09:07\nmacro=operation\n{message}"
        );
        let mut runtime = Runtime::start(
            ConfigDocument::parse(&text).unwrap(),
            &Media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock(),
        )
        .unwrap();
        runtime.incoming("1000", "2000", true).unwrap();
        let event = runtime.next_event(clock()).unwrap().unwrap();
        assert_eq!(event.operation().unwrap().node, target);
        if !message.is_empty() {
            assert!(matches!(
                runtime.event_command(&event, clock()),
                Err(RuntimeError::Rejected)
            ));
        }
        runtime.queue_event(&event).unwrap();
        match runtime.event_command(&event, clock()).unwrap() {
            links::LinkEffect::Connect(attempt) => {
                assert_eq!(attempt.remote(), "3000");
                assert!(
                    runtime
                        .finish_connect("1000", attempt, true, clock())
                        .unwrap()
                );
            }
            links::LinkEffect::Detach(peers) => assert_eq!(peers, ["2000"]),
            links::LinkEffect::None => assert_eq!(action, "reconnect_all"),
            _ => panic!("unexpected macro effect"),
        }
        assert!(runtime.complete_event(&event));
        assert!(!runtime.complete_event(&event));
        assert!(runtime.next_event(clock()).unwrap().is_none());
        assert!(matches!(
            runtime.event_command(&event, clock()),
            Err(RuntimeError::Rejected)
        ));
        runtime.stop(0);
        for peer in ["2000", "3000"] {
            runtime.peer_detached("1000", peer, 0);
        }
        runtime.detached("1000", 1);
        assert!(runtime.stop(0));
        assert!(runtime.next_event(clock()).unwrap().is_none());
        assert_eq!(runtime.queue_event(&event), Err(RuntimeError::Rejected));
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
        let mut runtime = Runtime::start(
            ConfigDocument::parse("[1000]\n").unwrap(),
            &media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone()))),
            clock(),
        )
        .unwrap();
        runtime
            .queue_status("1000", action, last, clock(), None)
            .unwrap();
        assert_eq!(*media.0.lock().unwrap(), [expected]);
        for _ in 0..3 {
            runtime
                .queue_status("1000", action, last, clock(), None)
                .unwrap();
        }
        assert_eq!(
            runtime.queue_status("1000", action, last, clock(), None),
            Err(RuntimeError::Rejected)
        );
        let (mut rx, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
            runtime.queue_status("1000", LinkAction::Time, None, invalid_clock, None),
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_loop_rejected("1000", "2000", None).unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        ["Connection to node,2,0,0,0 rejected because it would create a link loop"]
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

fn link_notice_runtime(document: &str, reject_connect: Arc<AtomicBool>) -> Runtime {
    Runtime::start(
        ConfigDocument::parse(document).unwrap(),
        &LinkNoticeMedia(reject_connect),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap()
}

#[test]
fn link_reconnect_skips_queued_disconnect_for_every_listener_and_reverse_pair() {
    let mut runtime = link_notice_runtime(
        "[1000]\nradio_channel=first\n[2000]\nradio_channel=second\n[3000]\nradio_channel=third\n",
        Arc::new(AtomicBool::new(false)),
    );
    runtime.queue_link_event("1000", "2000", false).unwrap();
    runtime.queue_link_event("2000", "1000", true).unwrap();

    for local in ["1000", "2000", "3000"] {
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
    let mut runtime = link_notice_runtime("[1000]\n", Arc::new(AtomicBool::new(false)));
    let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    runtime.queue_link_event("1000", "2000", false).unwrap();
    let mut prefix = vec![0.0; 12_002];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut prefix);
    assert_eq!(prefix.iter().filter(|sample| **sample == 0.5).count(), 2);

    runtime.queue_link_event("1000", "2000", true).unwrap();
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
    let mut runtime = link_notice_runtime("[4000]\n", Arc::new(AtomicBool::new(false)));
    runtime.queue_link_event("1000", "2000", false).unwrap();
    runtime.queue_link_event("3000", "2000", false).unwrap();
    runtime.queue_link_event("2000", "1000", true).unwrap();
    let (_, mut tx) = runtime.node("4000").unwrap().register_audio().unwrap();
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

#[test]
fn link_reconnect_cancels_disconnects_before_connect_preparation_fails() {
    let reject_connect = Arc::new(AtomicBool::new(false));
    let mut runtime = link_notice_runtime("[1000]\n", Arc::clone(&reject_connect));
    runtime.queue_link_event("1000", "2000", false).unwrap();
    reject_connect.store(true, Ordering::Release);
    assert_eq!(
        runtime.queue_link_event("1000", "2000", true),
        Err(RuntimeError::Preparation)
    );
    let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let mut runtime = link_notice_runtime("[1000]\n", Arc::new(AtomicBool::new(false)));
    for connected in [false, true, false, true] {
        runtime.queue_link_event("1000", "2000", connected).unwrap();
    }
    for connected in [false, true] {
        assert_eq!(
            runtime.queue_link_event("1000", "2000", connected),
            Err(RuntimeError::Rejected),
            "cancellation does not bypass the four-item ownership limit"
        );
    }
    let (_, mut tx) = runtime.node("1000").unwrap().register_audio().unwrap();
    let mut audio = vec![0.0; 12_032];
    tx.acquire()
        .unwrap()
        .state()
        .controller
        .process_audio(false, false, &[], &mut audio);
    let heard: Vec<_> = audio.into_iter().filter(|sample| *sample != 0.0).collect();
    assert_eq!(heard, [0.25; 8], "both disconnect attempts were superseded");
    // Public event admission reclaims completed/cancelled media on control first.
    runtime.queue_link_event("1000", "2000", true).unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\nradio_channel=first\n\
             [2000]\nradio_channel=second\n\
             [3000]\nradio_channel=third\n",
        )
        .unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_link_event("1000", "2000", true).unwrap();
    runtime.queue_link_event("1000", "2000", false).unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\nradio_channel=first\n[permanent 1000 network]\nremote_node=2000,3000\n[4000]\n",
        )
        .unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    runtime.queue_link_event("1000", "2000", true).unwrap();
    runtime.queue_link_event("1000", "2000", false).unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[permanent 1000 network]\nremote_node=2000,3000\ngroup_name=Blind Hams Network\n",
        )
        .unwrap(),
        &media,
        adapter,
        |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.queue_priority_group_events().unwrap();
    let group = runtime
        .node("1000")
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
        let node = runtime.node("1000").unwrap();
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
        let node = runtime.node("1000").unwrap();
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
    let node = runtime.node("1000").unwrap();
    let work = node.work().unwrap();
    let retry = node.links().take_retry(100_000, work).unwrap();
    let recovered = retry.remote().to_owned();
    assert_eq!(node.links().finish_retry(retry, true, 100_000), Ok(true));
    assert_ne!(group.selection().callback_slot(true), 0);
    runtime.queue_priority_group_events().unwrap();
    assert_eq!(media.0.lock().unwrap().len(), 2);

    let node = runtime.node("1000").unwrap();
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
        let mut runtime = Runtime::start(
            ConfigDocument::parse(
                "[1000]\n[permanent 1000 primary]\nremote_node=2000\ngroup_name=Primary\n\
                 [schedule 1000 morning]\nremote_node=3000\ngroup_name=Morning Net\n\
                 replace_permanent=primary\nstart_time=09:30\nend_time=11:00\nend_inactivity_ms=60000\n\
                 [schedule 1000 evening]\nremote_node=3000\ngroup_name=Evening Net\n\
                 replace_permanent=primary\nstart_time=20:00\nend_time=21:30\n",
            )
            .unwrap(),
            &CapturingMedia(Arc::clone(&messages)),
            adapter,
            |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
            now,
        )
        .unwrap();
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
                .node("1000")
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
        let node = runtime.node("1000").unwrap();
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
            _: bool,
        ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
            Err(MediaError::Io)
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[permanent 1000 network]\nremote_node=2000,3000\ngroup_name=Network\n",
        )
        .unwrap(),
        &RejectingMedia,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let group = runtime
        .node("1000")
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
        runtime.node("1000").unwrap().links().priority_groups()[0].selected,
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse(
            "[1000]\n[permanent 1000 network]\nremote_node=2000,3000\ngroup_name=Network\n",
        )
        .unwrap(),
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
        .node("1000")
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
        runtime.node("1000").unwrap().links().priority_groups()[0].selected,
        Some("2000".to_owned())
    );

    assert_eq!(
        runtime.queue_priority_group_events(),
        Err(RuntimeError::Rejected)
    );
    for remote in ["2000", "3000"] {
        runtime.node("1000").unwrap().links().ended(remote);
        runtime.peer_detached("1000", remote, 0);
    }
    assert!(runtime.stop(0));
}

#[test]
fn completed_parrot_capture_is_measured_localized_and_queued_for_playback() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nparrot_enabled=yes\n").unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();

    let (mut receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nparrot_enabled=yes\n").unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nparrot_enabled=yes\n").unwrap(),
        &CapturingMedia(messages.clone()),
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let (mut receive, mut transmit) = runtime.node("1000").unwrap().register_audio().unwrap();
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
    use super::dtmf::DigitEvent;
    use super::links::LinkEffect;
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let hash = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.node("1000").unwrap().links().configure_admin(
        &hash(b"1234", "c29tZXNhbHQ"),
        &hash(b"5678", "bG9ja3NhbHQ"),
        60_000,
    );
    let feed = |runtime: &mut Runtime, sequence: &str, now_ms: u64| {
        let mut operation = None;
        for digit in sequence.chars() {
            operation = runtime
                .digit("1000", DigitEvent::Digit { digit, now_ms })
                .or(operation);
        }
        operation
    };
    assert!(feed(&mut runtime, "*8001234#", 1000).is_none());
    let super::dtmf::DigitOutcome::Operation(enabled) = feed(&mut runtime, "*804", 1001).unwrap()
    else {
        panic!("operation")
    };
    assert!(matches!(
        runtime.command("1000", enabled, clock(), false),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    let super::dtmf::DigitOutcome::Operation(disabled) = feed(&mut runtime, "*805", 1002).unwrap()
    else {
        panic!("operation")
    };
    assert!(matches!(
        runtime.command("1000", disabled, clock(), false),
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
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\n").unwrap(),
        &Media,
        adapter,
        |_, _| Ok(Box::new(Device(Arc::new(Mutex::new(Vec::new())))) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    runtime.incoming("1000", "2000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    let operation = {
        let links = runtime.node("1000").unwrap().links();
        links.configure_admin(&digest, &digest, 60_000);
        for digit in "*8001234#".chars() {
            assert!(
                links
                    .peer_digit("2000", digit, 1001, reply.clone())
                    .is_none()
            );
        }
        let mut operation = None;
        for digit in "*804".chars() {
            operation = links
                .peer_digit("2000", digit, 1002, reply.clone())
                .or(operation);
        }
        match operation.unwrap() {
            super::dtmf::DigitOutcome::Operation(operation) => operation,
            _ => panic!("operation"),
        }
    };
    let mut now = clock();
    now.now_ms = 1003;
    assert!(matches!(
        runtime.command("1000", operation, now, false),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    runtime.node("1000").unwrap().links().ended("2000");
    runtime.peer_detached("1000", "2000", now.now_ms);
    assert!(runtime.stop(now.now_ms));
}

#[test]
fn device_reload_rejects_outstanding_work_failed_quiescence_and_protected_audio() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct PausingDevice(Arc<AtomicBool>);
    impl DeviceHandoff for PausingDevice {
        fn quiesce(&mut self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn close(&mut self) {}
        fn open(&mut self, _: &GenerationSettings) -> bool {
            true
        }
    }
    for reason in 0..3 {
        let can_stop = Arc::new(AtomicBool::new(reason != 1));
        let device = |_: &str, _: &ResolvedNodeSettings| {
            Ok(Box::new(PausingDevice(can_stop.clone())) as Box<dyn DeviceHandoff>)
        };
        let mut runtime = Runtime::start(
            ConfigDocument::parse("[1000]\nradio_channel=old\n").unwrap(),
            &Media,
            adapter,
            device,
            clock(),
        )
        .unwrap();
        let (mut rx, _tx) = runtime.node("1000").unwrap().register_audio().unwrap();
        let work = (reason == 0).then(|| runtime.node("1000").unwrap().work().unwrap());
        let protected = (reason == 2).then(|| rx.acquire().unwrap());
        assert_eq!(
            runtime.reload(
                ConfigDocument::parse("[1000]\nradio_channel=new\n").unwrap(),
                &Media,
                adapter,
                device,
                clock()
            ),
            Err(RuntimeError::Busy)
        );
        assert_eq!(runtime.settings("1000").unwrap().channel, "old");
        assert_eq!(runtime.revision(), 1);
        drop(protected);
        drop(work);
        assert_eq!(rx.acquire().unwrap().id(), 1);
        can_stop.store(true, Ordering::Release);
        assert!(runtime.stop(0));
    }
}
