use super::*;
use crate::{
    command::LinkAction,
    config::{ConfigDocument, ResolvedNodeSettings},
    media::{FileRequest, MediaError, PreparedAudio, SpeechRequest},
    schedule::{CivilTime, Weekday},
};
use std::sync::{Arc, Mutex};

struct Media;
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
        assert_eq!(
            runtime
                .digit(
                    local,
                    DigitEvent::Digit {
                        digit: '0',
                        now_ms: 4
                    }
                )
                .unwrap()
                .command
                .action,
            LinkAction::Status
        );
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
    runtime.queue_event(&event, &Media).unwrap();
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
                dtmf::DigitOperation {
                    command: crate::command::Command {
                        action: LinkAction::Disconnect,
                        node: "2000".into(),
                    },
                    digit: None,
                },
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
    assert!(runtime.queue_event(&dispatch, &Media).is_ok());
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
        assert_eq!(runtime.queue_event(&dispatch, &Media), Ok(()));
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
    assert_eq!(
        runtime.queue_event(&dispatch, &Media),
        Err(RuntimeError::Rejected)
    );
    assert!(runtime.next_event(clock()).unwrap().is_none());
    assert!(runtime.stop(0));
}

#[test]
fn native_media_contract_and_source_fallback_are_enforced_before_device_open() {
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
    let config =
        "[1000]\n[identifier 1000 test]\nsound_file=test.wav\nspeech_text=hello\nmorse_text=TEST\n";
    for (rate, file_ok, expected) in [
        (48000, true, vec!["file"]),
        (48000, false, vec!["file", "speech"]),
        (22050, true, vec!["file"]),
    ] {
        let media = Prepared {
            rate,
            file_ok,
            calls: Mutex::new(Vec::new()),
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let result = Runtime::start(
            ConfigDocument::parse(config).unwrap(),
            &media,
            adapter,
            |_, _| Ok(Box::new(Device(log.clone())) as Box<dyn DeviceHandoff>),
            clock(),
        );
        assert_eq!(*media.calls.lock().unwrap(), expected);
        if rate == 48000 {
            assert!(result.unwrap().stop(0));
        } else {
            assert!(matches!(result, Err(RuntimeError::Preparation)));
            assert!(log.lock().unwrap().is_empty());
        }
    }
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
    let operation = |action| DigitOperation {
        command: Command {
            action,
            node: String::new(),
        },
        digit: None,
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
    let commands = runtime.node("1000").unwrap().drain_digits();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command.action, LinkAction::Status);
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
        runtime.queue_event(&event, &Media).unwrap();
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
        assert_eq!(
            runtime.queue_event(&event, &Media),
            Err(RuntimeError::Rejected)
        );
    }
}

#[test]
fn status_actions_prepare_matching_speech_and_queue_real_radio_playback() {
    struct Speech(Mutex<Vec<String>>);
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
    for (action, last, expected) in [
        (LinkAction::Time, None, "Good Morning. The time is 9:07 AM."),
        (LinkAction::LastKeyed, None, "NO LAST KEYED"),
        (
            LinkAction::LastKeyed,
            Some("2000"),
            "LAST KEYED node,2,0,0,0",
        ),
        (LinkAction::Status, None, "NO LINKS"),
        (LinkAction::FullStatus, None, "NO LINKS"),
    ] {
        let media = Speech(Mutex::new(Vec::new()));
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
            .queue_status("1000", action, last, clock(), &media)
            .unwrap();
        assert_eq!(*media.0.lock().unwrap(), [expected]);
        for _ in 0..3 {
            runtime
                .queue_status("1000", action, last, clock(), &media)
                .unwrap();
        }
        assert_eq!(
            runtime.queue_status("1000", action, last, clock(), &media),
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
            runtime.queue_status("1000", LinkAction::Time, None, invalid_clock, &media),
            Err(RuntimeError::Clock)
        );
        assert_eq!(
            runtime.queue_status("missing", action, last, clock(), &media),
            Err(RuntimeError::MissingNode)
        );
        assert!(runtime.stop(0));
    }
}

#[test]
fn link_lifecycle_events_prepare_perspective_aware_speech_for_every_node() {
    struct Speech(Mutex<Vec<String>>);
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

    let media = Speech(Mutex::new(Vec::new()));
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

    runtime
        .queue_link_event("1000", "2000", true, &media)
        .unwrap();
    runtime
        .queue_link_event("1000", "2000", false, &media)
        .unwrap();
    assert_eq!(
        *media.0.lock().unwrap(),
        [
            "node,2,0,0,0 CONNECTED",
            "node,1,0,0,0 CONNECTED",
            "node,1,0,0,0 CONNECTED TO node,2,0,0,0",
            "node,2,0,0,0 DISCONNECTED",
            "node,1,0,0,0 DISCONNECTED",
            "node,1,0,0,0 DISCONNECTED FROM node,2,0,0,0",
        ]
    );
    assert!(runtime.stop(0));
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
