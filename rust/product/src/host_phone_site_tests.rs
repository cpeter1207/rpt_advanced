//! Bound-node site recipes use the existing exact-call reply owner, never radio telemetry.
use super::*;
use crate::media::text::tests::SpeechFixture;
use rpt_advanced_core::{
    audio::PlaybackOutcome,
    link::PeerSignals,
    site::{Observation, ReadingQuality, SiteObservations},
};

fn configuration() -> ConfigDocument {
    ConfigDocument::parse(&format!(
        "[1000]\nradio_channel=usb\nduplex=full\n[2000]\nradio_channel=second\nduplex=full\n\
         [sensor 1000 battery]\nenabled=yes\nbackend=hwmon\ndevice=/sys/selected\n\
         channel=in1_input\nunit=volts\nprecision=1\ndtmf_id=1\n\
         [input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=/dev/gpiochip0\n\
         line=3\ndtmf_id=2\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n{}",
        mailbox_configuration("1000")
    ))
    .unwrap()
}

fn site_host() -> (Host, Arc<SpeechFixture>) {
    let speech = Arc::new(SpeechFixture::default());
    let host = Host::start(
        configuration(),
        crate::media::text::tests::preparer(Arc::clone(&speech)),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    (host, speech)
}

fn finish(host: &mut Host, source: &PhoneSource, reply: &PeerReply) {
    let mut prompt = host.take_phone_reply(source, reply).unwrap().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while prompt.outcome() == PlaybackOutcome::Active {
        assert!(Instant::now() < deadline);
        prompt.render(false, &mut [0.0; 960]);
        std::thread::yield_now();
    }
    assert_eq!(prompt.outcome(), PlaybackOutcome::Succeeded);
}

fn call(host: &mut Host, authenticated: bool) -> (PhoneSource, PeerReply) {
    let (phone, _) = crate::link::session::tests::phone_peer();
    let id = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(id).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    finish(host, &source, &reply);
    if authenticated {
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        finish(host, &source, &reply);
    }
    (source, reply)
}

fn report(
    host: &mut Host,
    local: &str,
    selector: &str,
    reply: &PeerReply,
    now_ms: u64,
) -> Result<(), RuntimeError> {
    host.immediate(
        local,
        LinkEffect::SiteStatus {
            selector: selector.into(),
            reply: Some(reply.clone()),
        },
        RuntimeClock { now_ms, ..clock() },
    )
}

#[test]
fn ordinary_site_report_stays_on_requesting_peer() {
    use crate::link::ring::{InboundPolicy, InboundRing};
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        link::{AudioPeer, LinkAudio, PeerIdentity, SourcePosition},
    };

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, speech) = site_host();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut peers = Vec::new();
    for remote in ["2000", "3000"] {
        let (input, reader) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
        let (writer, output) = LinkAudioQueue::new(960, 2).unwrap().into_endpoints();
        peers.push(
            AudioPeer::new(
                PeerIdentity::from_operator(remote).unwrap(),
                Mode::TRANSCEIVE,
                reader,
                writer,
                960,
                0,
            )
            .unwrap(),
        );
        inputs.push(input);
        outputs.push(output);
    }
    let domain = host.media_workers[0].1.retirement_domain();
    let (mut adapter, mut dispatcher) = LinkAudio::new_in_domain(peers, 960, domain).unwrap();
    let reply = inputs[0].signals().reply_target();
    assert!(reply.phone_source().is_none());
    report(&mut host, "1000", "1", &reply, 10).unwrap();

    let lease = Arc::clone(&host.leases[0].1);
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let source_clock = Arc::clone(&lease.lock().unwrap().status.source_clock);
    let mut audio = [0.0; 960];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut heard = false;
    while !heard {
        assert!(
            Instant::now() < deadline,
            "ordinary site reply did not reach its peer"
        );
        audio.fill(0.0);
        let stamp = source_clock.advance(audio.len());
        let mut generation = owners.1.acquire().unwrap();
        let source = SourcePosition {
            generation: generation.id(),
            sample: stamp.sample,
            stream: stamp.stream,
            born_ns: stamp.born_ns,
        };
        assert!(
            !adapter
                .process(
                    &mut generation.state().controller,
                    false,
                    &mut audio,
                    source
                )
                .unwrap()
        );
        assert!(audio.iter().all(|sample| *sample == 0.0));
        drop(generation);
        assert_eq!(dispatcher.dispatch(1), 1);
        for (index, output) in outputs.iter_mut().enumerate() {
            let mut samples = [0.0; 960];
            output.read(&mut samples);
            if index == 0 {
                heard |= samples.iter().any(|sample| *sample != 0.0);
            } else {
                assert!(samples.iter().all(|sample| *sample == 0.0));
            }
        }
        host.runtime.prepare_messages(clock());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*speech.opened.lock().unwrap(), ["battery is unavailable."]);
    assert!(host.phone_calls.is_empty());
    drop((adapter, dispatcher, inputs, outputs));
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.stop(20));
}

#[test]
fn private_site_report_uses_existing_reading_quality_and_exact_private_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (selector, quality, now_ms, expected) in [
        ("1", ReadingQuality::Valid, 1000, "battery is 13.3 volts."),
        ("1", ReadingQuality::Valid, 6000, "battery is unavailable."),
        ("1", ReadingQuality::Fault, 1000, "battery is unavailable."),
        (
            "1",
            ReadingQuality::Unavailable,
            1000,
            "battery is unavailable.",
        ),
        ("2", ReadingQuality::Valid, 1000, "door is inactive."),
        (
            "99",
            ReadingQuality::Valid,
            1000,
            "The requested site reading is unavailable.",
        ),
    ] {
        let (mut host, speech) = site_host();
        let (source, reply) = call(&mut host, true);
        host.runtime.node("1000").unwrap().site().advance(
            1000,
            &SiteObservations {
                sensors: [(
                    "battery".into(),
                    Observation {
                        value: Some(13.26),
                        observed_at_ms: Some(1000),
                        quality,
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
        let result = report(&mut host, "1000", selector, &reply, now_ms);
        if result.is_ok() {
            finish(&mut host, &source, &reply);
        }
        let observed = speech.opened.lock().unwrap().last().cloned();
        assert!(host.peers.is_empty());
        assert!(!super::super::probe_public_phone_media(&mut host, "1000"));
        assert!(host.stop(now_ms + 10));
        assert_eq!(result, Ok(()));
        assert_eq!(observed.as_deref(), Some(expected));
    }
}

#[test]
fn private_site_report_rechecks_current_account_mailbox_and_exact_owner_before_preparation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for case in [
        "unauthenticated",
        "foreign",
        "ended",
        "wrong node",
        "removed",
        "disabled",
        "rebound",
    ] {
        let (mut host, speech) = site_host();
        let (source, reply) = call(&mut host, case != "unauthenticated");
        let foreign = PeerSignals::for_phone(source.clone());
        assert!(foreign.authenticate_phone());
        let attempted = if case == "foreign" {
            foreign.reply_target()
        } else {
            reply.clone()
        };
        if case == "ended" {
            reply.retire();
        }
        if matches!(case, "removed" | "disabled" | "rebound") {
            let account = match case {
                "removed" => String::new(),
                "disabled" => account_configuration("1000").replace("enabled=yes", "enabled=no"),
                _ => account_configuration("2000"),
            };
            host.reload(document("usb", &account), clock()).unwrap();
            pump_until(&mut host, |host| {
                host.phone_source(source.call_id()).is_none()
            });
        }
        let before = speech.opened.lock().unwrap().len();
        let result = report(
            &mut host,
            if case == "wrong node" { "2000" } else { "1000" },
            "1",
            &attempted,
            10,
        );
        let pending = host.take_phone_reply(&source, &reply);
        assert!(host.stop(20));
        assert_eq!(result, Err(RuntimeError::Rejected), "{case}");
        assert!(pending.is_err() || pending.unwrap().is_none(), "{case}");
        assert_eq!(speech.opened.lock().unwrap().len(), before, "{case}");
    }
}

#[test]
fn private_site_report_queue_pressure_preserves_existing_order_and_reclaims_rejected_producer() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Tagged(usize, Arc<Mutex<Vec<usize>>>);
    impl PcmStreamReader for Tagged {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    impl Drop for Tagged {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _) = site_host();
    let (source, reply) = call(&mut host, true);
    let dropped = Arc::new(Mutex::new(Vec::new()));
    for id in 0..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Tagged(id, Arc::clone(&dropped))))
            .unwrap();
    }
    assert_eq!(
        report(&mut host, "1000", "1", &reply, 10),
        Err(RuntimeError::Rejected)
    );
    assert!(dropped.lock().unwrap().is_empty());
    for id in 0..16 {
        drop(host.take_phone_reply(&source, &reply).unwrap().unwrap());
        assert_eq!(*dropped.lock().unwrap(), (0..=id).collect::<Vec<_>>());
    }
    // The refused producer did not replace a queued owner or occupy a reply slot.
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    report(&mut host, "1000", "1", &reply, 10).unwrap();
    finish(&mut host, &source, &reply);
    assert!(host.stop(20));
}

#[test]
fn private_site_report_retirement_revokes_queued_and_borrowed_streams_before_same_id_replacement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for reload in [false, true] {
        let (mut host, _) = site_host();
        let (source, reply) = call(&mut host, true);
        report(&mut host, "1000", "1", &reply, 10).unwrap();
        let mut held = host.take_phone_reply(&source, &reply).unwrap().unwrap();
        report(&mut host, "1000", "2", &reply, 10).unwrap();
        if reload {
            host.reload(configuration(), clock()).unwrap();
            pump_until(&mut host, |host| {
                host.phone_source(source.call_id()).is_none()
            });
        } else {
            assert!(host.retire_phone(&source));
        }
        assert!(!held.current());
        assert_eq!(held.render(false, &mut [0.0; 960]), 0);
        assert_eq!(held.outcome(), PlaybackOutcome::Cancelled);
        drop(held);
        let (fresh, fresh_reply) = call(&mut host, true);
        assert_eq!(source.call_id(), fresh.call_id());
        assert_ne!(source.ingress(), fresh.ingress());
        assert_eq!(
            report(&mut host, "1000", "1", &reply, 10),
            Err(RuntimeError::Rejected)
        );
        assert!(
            host.take_phone_reply(&fresh, &fresh_reply)
                .unwrap()
                .is_none()
        );
        report(&mut host, "1000", "2", &fresh_reply, 10).unwrap();
        finish(&mut host, &fresh, &fresh_reply);
        assert!(host.stop(20));
    }
}

#[test]
fn private_site_report_preparation_failure_never_queues_a_fallback() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, speech) = site_host();
    let (source, reply) = call(&mut host, true);
    let before = speech.opened.lock().unwrap().len();
    for selector in ["", "01", "not-a-selector"] {
        assert_eq!(
            report(&mut host, "1000", selector, &reply, 10),
            Err(RuntimeError::Rejected)
        );
    }
    assert_eq!(
        host.immediate(
            "1000",
            LinkEffect::SiteStatus {
                selector: "1".into(),
                reply: Some(reply.clone()),
            },
            RuntimeClock {
                civil: None,
                ..clock()
            }
        ),
        Err(RuntimeError::Clock)
    );
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    assert!(host.stop(20));
    assert_eq!(speech.opened.lock().unwrap().len(), before);
}
