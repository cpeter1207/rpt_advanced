use rpt_advanced_core::{
    access::AccessPolicy,
    link::{AdmissionError, LinkManager, Mode, Peer, Protocol, TopologyManager},
};

#[test]
fn audio_peer_preparation_checks_identity_rate_and_callback_capacity() {
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        link::{AudioPeer, LinkAudio, PeerInput, PeerSignals},
    };
    struct Empty {
        rate: u32,
        signals: PeerSignals,
    }
    impl PeerInput for Empty {
        fn source_rate(&self) -> u32 {
            self.rate
        }
        fn signals(&self) -> &PeerSignals {
            &self.signals
        }
        fn available(&self) -> u64 {
            0
        }
        fn render(&mut self, _: &mut [f32]) -> bool {
            false
        }
    }
    for (name, rate, maximum) in [
        ("bad", 48000, 1),
        ("2000", 48000, 0),
        ("2000", 0, 1),
        ("2000", 48001, 1),
    ] {
        let (outbound, _) = LinkAudioQueue::new(1).unwrap().into_endpoints();
        assert!(
            AudioPeer::new(
                name,
                Mode::TRANSCEIVE,
                Empty {
                    rate,
                    signals: PeerSignals::new()
                },
                outbound,
                maximum,
                0
            )
            .is_err()
        );
    }
    assert!(LinkAudio::<Empty>::new(vec![], 0).is_err());
    let (outbound, _) = LinkAudioQueue::new(1).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        "2000",
        Mode::TRANSCEIVE,
        Empty {
            rate: 48000,
            signals: PeerSignals::new(),
        },
        outbound,
        1,
        0,
    )
    .unwrap();
    assert!(LinkAudio::new(vec![peer], 2).is_err());
    let (mut empty, mut dispatcher) = LinkAudio::<Empty>::new(vec![], 1).unwrap();
    let (mut controller, _) = rpt_advanced_core::controller::NodeController::new(
        rpt_advanced_core::controller::ControllerSettings::default(),
        vec![],
        vec![],
        rpt_advanced_core::controller::CourtesySettings::default(),
    )
    .unwrap();
    assert!(!empty.process(&mut controller, false, &mut []).unwrap());
    assert!(!empty.process(&mut controller, false, &mut [0.0]).unwrap());
    assert_eq!(dispatcher.dispatch(1), 0);
}

#[test]
fn unexpected_permanent_loss_retries_only_after_reader_reclamation() {
    for local in ["", "bad node", "node\0"] {
        assert!(LinkManager::new(local).is_err());
    }
    let mut hub = LinkManager::new("1000").unwrap();
    hub.attach("2000", Mode::MONITOR, true).unwrap();
    hub.reclaim("2000", 0);
    assert!(hub.take_retry(0).is_none());
    hub.end("2000");
    assert!(hub.take_retry(1).is_none());
    hub.reclaim("2000", 2);
    let retry = hub.take_retry(2).unwrap();
    assert_eq!(retry.name(), "2000");
    assert_eq!(retry.mode(), Mode::MONITOR);
    hub.publish_retry(&retry).unwrap();
    hub.finish_retry(retry, true, 3);
    assert!(hub.take_retry(3).is_none());
    assert_eq!(hub.snapshot().len(), 1);
    assert!(hub.snapshot()[0].permanent);
    hub.reject_loop("2000");
    hub.reclaim("2000", 4);
    assert!(hub.take_retry(4).is_none());
    assert!(hub.snapshot().is_empty());
}

#[test]
fn hub_checks_exact_local_transitive_paused_and_ended_admission_states() {
    use rpt_advanced_core::link::AdmissionError;
    let mut hub = LinkManager::new("1000").unwrap();
    assert_eq!(
        hub.attach("1000", Mode::TRANSCEIVE, false),
        Err(AdmissionError::Loop)
    );
    hub.attach("2000", Mode::TRANSCEIVE, false).unwrap();
    hub.update_topology("2000", b"L T3000").unwrap();
    assert_eq!(
        hub.attach("3000", Mode::TRANSCEIVE, false),
        Err(AdmissionError::Loop)
    );
    let mut publisher = TopologyManager::default();
    assert_eq!(publisher.publish(&hub, 0).len(), 1);
    hub.update_topology("2000", b"L T3000").unwrap();
    assert!(publisher.publish(&hub, 1).is_empty());
    assert!(hub.update_topology("missing", b"L").is_err());
    hub.end("missing");
    hub.reject_loop("missing");
    hub.end("2000");
    assert!(!hub.remote_digit("2000", '1', &AccessPolicy::new("", "").unwrap()));
    assert!(!hub.reaches("3000", true));
    hub.attach("3000", Mode::TRANSCEIVE, false).unwrap();
    assert_eq!(hub.update_topology("3000", b"L T2000"), Ok(false));
    hub.reclaim("missing", 0);
    hub.retain_retry("4000", Mode::TRANSCEIVE, 100).unwrap();
    assert_eq!(
        hub.attach("4000", Mode::TRANSCEIVE, false),
        Err(AdmissionError::Loop)
    );
    assert!(!hub.owns_permanent("5000"));
    assert!(!hub.disconnect_permanent("5000"));
    assert!(!hub.reaches("5000", true));
    assert!(hub.take_retry(99).is_none());
    let attempt = hub.take_retry(100).unwrap();
    assert!(hub.take_retry(100).is_none());
    hub.pause_retries();
    assert!(!hub.reaches("4000", false));
    assert!(hub.reaches("4000", true));
    assert_eq!(hub.publish_retry(&attempt), Err(AdmissionError::Stale));
    hub.finish_retry(attempt, false, 100);
    assert!(hub.take_retry(1000).is_none());
    hub.resume_retries(1000);
    let attempt = hub.take_retry(1000).unwrap();
    hub.publish_retry(&attempt).unwrap();
    hub.finish_retry(attempt, false, 1000);
    assert!(hub.take_retry(u64::MAX).is_none());
    assert!(hub.owns_permanent("4000"));
    assert!(!hub.owns_permanent("3000"));
    assert!(!hub.disconnect_permanent("missing"));
}

#[test]
fn key_advice_falls_back_to_eligible_flood_without_echoing_ingress_source_or_local() {
    let mut hub = LinkManager::new("1000").unwrap();
    for remote in ["2000", "3000", "4000", "5000"] {
        hub.attach(remote, Mode::TRANSCEIVE, false).unwrap();
    }
    hub.end("5000");
    let key = Protocol::parse(b"K 6000 3000 1 7").unwrap();
    assert_eq!(
        hub.relay_key("2000", &key, false, 0, 0),
        [("4000".into(), "K 6000 3000 1 7".into())]
    );
    assert!(hub.relay_key("missing", &key, false, 0, 0).is_empty());
    assert!(hub.relay_key("5000", &key, false, 0, 0).is_empty());
    for text in [b"K 1000 3000 1 7".as_slice(), b"K? * 1000 0 0", b"!IAXKEY!"] {
        assert!(
            hub.relay_key("2000", &Protocol::parse(text).unwrap(), false, 0, 0)
                .is_empty()
        );
    }
}

#[test]
fn media_signals_publish_pcm_and_cancel_stale_key_source() {
    use rpt_advanced_core::link::PeerSignals;
    let signals = PeerSignals::default();
    assert!(!signals.select_source(0, "200"));
    assert_eq!(signals.pcm_epoch(), 0);
    signals.publish_pcm();
    assert_eq!(signals.pcm_epoch(), 1);
    let key = signals.set_active(true);
    assert!(!signals.select_source(key, "bad/source"));
    assert!(!signals.select_source(key, &"a".repeat(64)));
    assert_eq!(signals.set_active(true), key);
    assert!(signals.select_source(key, "303"));
    let mut storage = [0; 64];
    assert_eq!(signals.selected_source(key, &mut storage), Some("303"));
    signals.set_active(false);
    assert!(!signals.select_source(key, "404"));
    let next = signals.set_active(true);
    assert_ne!(next, key);
    assert_eq!(signals.selected_source(next, &mut storage), None);
    signals.end();
    assert!(signals.ended());
}

#[test]
fn concurrent_advisory_publication_never_exposes_a_torn_identity() {
    use rpt_advanced_core::link::PeerSignals;
    use std::sync::{Arc, Barrier};
    let signals = Arc::new(PeerSignals::new());
    let edge = signals.set_active(true);
    let first = "A".repeat(63);
    let second = "B".repeat(63);
    assert!(signals.select_source(edge, &first));
    let start = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let publisher = Arc::clone(&signals);
        let ready = Arc::clone(&start);
        let a = &first;
        let b = &second;
        let writer = scope.spawn(move || {
            ready.wait();
            for index in 0..100_000 {
                assert!(publisher.select_source(edge, if index % 2 == 0 { a } else { b }));
            }
        });
        start.wait();
        let mut storage = [0; 64];
        for _ in 0..100_000 {
            if let Some(source) = signals.selected_source(edge, &mut storage) {
                assert!(source == first || source == second, "torn source: {source}");
            }
        }
        writer.join().unwrap();
    });
    assert_eq!(
        signals.selected_source(edge, &mut [0; 64]),
        Some(second.as_str())
    );
}

#[test]
fn link_admission_and_mix_validation_reject_bad_inputs_without_mutating_output() {
    use rpt_advanced_core::link::{MixSource, NativeMixer};
    for identity in ["", "node", "20/0"] {
        assert!(Peer::new(identity).is_err());
    }
    let mut peer = Peer::new("200").unwrap();
    assert!(!peer.digit('E', 0));
    let epoch = peer.activity(true, 0).unwrap();
    assert!(peer.query(epoch + 1, "100").is_none());
    assert!(peer.query(epoch, "not valid").is_none());
    assert!(peer.query(epoch, "100").is_some());
    assert!(!peer.accept_key(epoch, "100", "300", false));
    assert!(!peer.accept_key(epoch, "999", "300", true));
    assert!(!peer.accept_key(epoch, "100", "bad/source", true));
    assert!(peer.accept_key(epoch, "100", "300", true));
    assert!(NativeMixer::new(0).is_err());
    let mixer = NativeMixer::new(1).unwrap();
    let mut output = [0.75];
    assert!(
        mixer
            .destination(0, Mode::TRANSCEIVE, &[], false, &[], &mut output)
            .is_err()
    );
    assert_eq!(output, [0.75]);
    assert_eq!(
        mixer.destination(0, Mode::TRANSCEIVE, &[0.5], false, &[], &mut output),
        Ok(false)
    );
    assert_eq!(output, [0.0]);
    output[0] = 0.75;
    let inactive = [MixSource {
        identity: 1,
        mode: Mode::TRANSCEIVE,
        active: false,
        audio: &[0.2],
    }];
    assert_eq!(
        mixer.destination(0, Mode::TRANSCEIVE, &[0.0], false, &inactive, &mut output),
        Ok(false)
    );
    output[0] = 0.75;
    let sources = [MixSource {
        identity: 0,
        mode: Mode::TRANSCEIVE,
        active: false,
        audio: &[],
    }];
    assert!(mixer.local(&sources, &mut output).is_err());
    assert!(
        mixer
            .destination(0, Mode::TRANSCEIVE, &[0.0], false, &sources, &mut output)
            .is_err()
    );
    assert_eq!(output, [0.75]);
}

#[test]
fn topology_deduplicates_shared_routes_and_bounds_combined_publication() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::TRANSCEIVE, false).unwrap();
    hub.attach("300", Mode::TRANSCEIVE, false).unwrap();
    hub.update_topology("200", b"L T100,T400").unwrap();
    hub.update_topology("300", b"L R400").unwrap();
    assert_eq!(hub.full_topology(), "T200,T400,T300");
    assert_eq!(
        TopologyManager::default()
            .publish(&hub, 0)
            .iter()
            .find(|(peer, _)| peer == "300")
            .unwrap()
            .1,
        "L T200,T400"
    );
    // Each ingress fits the protocol limit while their combined fanout exceeds it.
    for (peer, prefix) in [("200", "a"), ("300", "b")] {
        let routes = (0..100)
            .map(|index| format!("T{prefix}{index:03}{}", "x".repeat(55)))
            .collect::<Vec<_>>()
            .join(",");
        hub.update_topology(peer, format!("L {routes}").as_bytes())
            .unwrap();
    }
    let topology = hub.full_topology();
    assert!(topology.len() < 10000);
    assert!(topology.ends_with(",R000000"));
}

#[test]
fn protocol_rejects_wrong_route_markers_and_each_invalid_key_field() {
    for text in [
        "L X200",
        "K 100 bad/source 1 0",
        "K bad/destination 100 1 0",
        "K? * 100 0 1",
        "K 100 200 1 -1",
        "K 100 200 1 18446744073709551616",
        "L T200,",
    ] {
        assert!(Protocol::parse(text.as_bytes()).is_none(), "{text}");
    }
    let mut hub = LinkManager::new("1000").unwrap();
    hub.attach("2000", Mode::TRANSCEIVE, false).unwrap();
    hub.attach("3000", Mode::TRANSCEIVE, false).unwrap();
    assert_eq!(hub.update_topology("2000", b"L T3000"), Ok(true));
    let publication = TopologyManager::default().publish(&hub, 0);
    assert_eq!(
        publication
            .iter()
            .find(|(peer, _)| peer == "3000")
            .unwrap()
            .1,
        "L T2000"
    );
}

#[test]
fn local_nonnumeric_name_does_not_relax_decimal_remote_admission() {
    let mut hub = LinkManager::new("usb_test").unwrap();
    assert!(hub.attach("other", Mode::TRANSCEIVE, false).is_err());
    hub.attach("200", Mode::TRANSCEIVE, false).unwrap();
    let message = Protocol::parse(b"K? * 300 0 0").unwrap();
    assert_eq!(
        hub.relay_key("200", &message, true, 0, 0)[0].1,
        "K 300 usb_test 1 0"
    );
    let mut local_only = LinkManager::new("usb/test").unwrap();
    local_only.attach("200", Mode::TRANSCEIVE, false).unwrap();
    assert!(local_only.relay_key("200", &message, true, 0, 0).is_empty());
}

#[test]
fn prepared_audio_drives_receive_edges_and_bounded_mix_minus() {
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, PeerInput, PeerSignals},
    };
    struct Input {
        signals: std::sync::Arc<PeerSignals>,
        value: f32,
    }
    impl PeerInput for Input {
        fn source_rate(&self) -> u32 {
            48000
        }
        fn signals(&self) -> &PeerSignals {
            &self.signals
        }
        fn available(&self) -> u64 {
            14400
        }
        fn render(&mut self, output: &mut [f32]) -> bool {
            output.fill(self.value);
            true
        }
    }
    let (producer, mut consumer) = LinkAudioQueue::new(1920).unwrap().into_endpoints();
    let first = Input {
        signals: std::sync::Arc::new(PeerSignals::new()),
        value: 0.25,
    };
    first.signals.publish_pcm();
    let first_signal = first.signals.clone();
    let peer = AudioPeer::new("200", Mode::TRANSCEIVE, first, producer, 960, 100).unwrap();
    let (local_producer, mut local_consumer) = LinkAudioQueue::new(960).unwrap().into_endpoints();
    let local = Input {
        signals: std::sync::Arc::new(PeerSignals::new()),
        value: 0.75,
    };
    local.signals.publish_pcm();
    let local_signal = local.signals.clone();
    let local_peer =
        AudioPeer::new("400", Mode::LOCAL_MONITOR, local, local_producer, 960, 0).unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer, local_peer], 960).unwrap();
    let status = links.status();
    assert_eq!(status.last_keyed(), None);
    let (mut controller, _control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut output = [0.5; 960];
    assert!(links.process(&mut controller, true, &mut output).unwrap());
    let mut outgoing = [0.0; 960];
    assert_eq!(consumer.read(&mut outgoing), 960);
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(consumer.read(&mut outgoing), 0);
    assert!(outgoing.iter().all(|value| *value == 0.5));
    assert_eq!(links.active_count(), 2);
    assert_eq!(status.last_keyed(), Some("400"));
    assert_eq!(local_consumer.read(&mut outgoing), 960);
    for _ in 0..6 {
        links
            .process(&mut controller, false, &mut [0.0; 960])
            .unwrap();
        dispatcher.dispatch(1);
        consumer.read(&mut outgoing);
    }
    first_signal.end();
    local_signal.end();
    links
        .process(&mut controller, false, &mut [0.0; 960])
        .unwrap();
    assert_eq!(links.active_count(), 0);
    assert_eq!(status.last_keyed(), Some("400"));
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(consumer.read(&mut outgoing), 960);
    assert!(outgoing.iter().all(|value| *value == 0.0));
    assert!(
        links
            .process(&mut controller, false, &mut [0.0; 961])
            .is_err()
    );
}

#[test]
fn generated_status_stays_local_and_never_reaches_a_link() {
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, PeerInput, PeerSignals},
    };
    struct Quiet(PeerSignals);
    impl PeerInput for Quiet {
        fn source_rate(&self) -> u32 {
            48000
        }
        fn signals(&self) -> &PeerSignals {
            &self.0
        }
        fn available(&self) -> u64 {
            0
        }
        fn render(&mut self, output: &mut [f32]) -> bool {
            output.fill(0.0);
            true
        }
    }
    let (producer, mut consumer) = LinkAudioQueue::new(960).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        "200",
        Mode::TRANSCEIVE,
        Quiet(PeerSignals::new()),
        producer,
        960,
        100,
    )
    .unwrap();
    let (mut links, mut dispatcher) = LinkAudio::new(vec![peer], 960).unwrap();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    control.queue_status("E", Some(vec![0.5; 240])).unwrap();
    let mut found_local = false;
    for _ in 0..14 {
        let mut rf = [0.0; 960];
        links.process(&mut controller, false, &mut rf).unwrap();
        assert_eq!(dispatcher.dispatch(1), 1);
        let mut output = [0.0; 960];
        let shortfall = consumer.read(&mut output);
        assert_eq!(shortfall, 960);
        assert!(output.iter().all(|sample| *sample == 0.0));
        if rf.iter().any(|sample| *sample > 0.0) {
            found_local = true;
        }
    }
    assert!(found_local);
}

#[test]
fn strict_protocol_rejects_partial_or_ambiguous_messages() {
    for text in [
        "L",
        "L ",
        "L T123,Rabc-1,C_a,L9",
        "K? * 123 0 0",
        "K 123 remote_2 1 18446744073709551615",
    ] {
        assert!(Protocol::parse(text.as_bytes()).is_some(), "{text}");
    }
    for text in [
        "L T",
        "L T123,",
        "L T123,,R4",
        "L T123 garbage",
        "L T12\0R4",
        "K? 1 2 0 0",
        "K? * 1 1 0",
        "K 1 2 2 0",
        "K 1 2 1 -1",
        "K 1 2 1 18446744073709551616",
        "K 1 2 1 0 extra",
        "K 1 2 1 0\n",
    ] {
        assert!(Protocol::parse(text.as_bytes()).is_none(), "{text:?}");
    }
    assert!(Protocol::parse(b"L T123\0").is_some());
    assert!(Protocol::parse(format!("L T{}", "1".repeat(10000)).as_bytes()).is_none());
}

#[test]
fn loop_rejection_cancels_permanent_recovery_and_full_topology_uses_wire_modes() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::MONITOR, true).unwrap();
    hub.update_topology("200", b"L T300").unwrap();
    hub.attach("400", Mode::LOCAL_MONITOR, false).unwrap();
    assert_eq!(hub.full_topology(), "R200,R300");
    hub.reject_loop("200");
    assert!(hub.reaches("200", true));
    hub.reclaim("200", 0);
    assert!(!hub.reaches("200", true));
    assert!(hub.take_retry(0).is_none());
}

#[test]
fn topology_keeps_valid_cache_and_ignores_direct_caller_reflection() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::TRANSCEIVE, false).unwrap();
    assert_eq!(hub.update_topology("200", b"L T100,T300"), Ok(false));
    assert!(hub.attach("300", Mode::TRANSCEIVE, false).is_err());
    assert!(hub.update_topology("200", b"L T").is_err());
    assert!(hub.reaches("300", true));
    hub.end("200");
    assert!(!hub.reaches("300", true));
    assert!(hub.reaches("200", true));
    hub.reclaim("200", 0);
    hub.attach("300", Mode::TRANSCEIVE, false).unwrap();
    hub.attach("200", Mode::TRANSCEIVE, false).unwrap();
    assert_eq!(hub.update_topology("200", b"L T300"), Ok(true));
}

#[test]
fn admission_and_remote_digits_recheck_verified_access() {
    let mut hub = LinkManager::new("100").unwrap();
    let policy = AccessPolicy::new("200", "").unwrap();
    assert!(hub.admit_incoming("200", false, &policy).is_err());
    assert!(hub.admit_incoming("100", true, &policy).is_err());
    assert!(hub.admit_incoming("201", true, &policy).is_err());
    hub.admit_incoming("200", true, &policy).unwrap();
    assert!(hub.admit_incoming("200", true, &policy).is_err());
    assert!(hub.remote_digit("200", '*', &policy));
    assert!(!hub.remote_digit("200", 'x', &policy));
    assert!(!hub.remote_digit("200", '*', &AccessPolicy::new("", "200").unwrap()));
}

#[test]
fn permanent_retry_preserves_mode_and_cancellation_beats_inflight_result() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.retain_retry("200", Mode::LOCAL_MONITOR, 10).unwrap();
    let attempt = hub.take_retry(10).unwrap();
    assert_eq!(attempt.mode(), Mode::LOCAL_MONITOR);
    hub.finish_retry(attempt, false, 10);
    assert!(hub.take_retry(1009).is_none());
    let attempt = hub.take_retry(1010).unwrap();
    hub.pause_retries();
    hub.cancel_retry("200");
    hub.finish_retry(attempt, false, 1010);
    hub.resume_retries(2000);
    assert!(hub.take_retry(u64::MAX).is_none());
    assert!(!hub.reaches("200", true));
}

#[test]
fn topology_publication_preserves_mode_and_excludes_nonforwarding_sources() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::TRANSCEIVE, false).unwrap();
    hub.attach("300", Mode::MONITOR, false).unwrap();
    hub.attach("400", Mode::LOCAL_MONITOR, false).unwrap();
    hub.update_topology("300", b"L T500,C600").unwrap();
    let mut topology = TopologyManager::default();
    let messages = topology.publish(&hub, 0);
    assert_eq!(
        messages.iter().find(|(node, _)| node == "200").unwrap().1,
        "L R300,R500,C600"
    );
    assert!(topology.publish(&hub, 29999).is_empty());
    assert_eq!(topology.publish(&hub, 30000).len(), 3);
}

#[test]
fn keyed_source_first_downstream_reply_wins_only_during_active_epoch() {
    let mut peer = Peer::new("200").unwrap();
    let epoch = peer.activity(true, 0).unwrap();
    assert_eq!(peer.query(epoch, "100").unwrap(), "K? * 100 0 0");
    assert!(!peer.accept_key(epoch, "100", "200", true));
    assert!(peer.accept_key(epoch, "100", "300", true));
    assert!(!peer.accept_key(epoch, "100", "400", true));
    assert_eq!(peer.selected_source(), "300");
    assert!(peer.activity(true, 999).is_none());
    let next = peer.activity(true, 1000).unwrap();
    peer.query(next, "100").unwrap();
    assert!(!peer.accept_key(epoch, "100", "400", true));
    peer.activity(false, 1001);
    assert!(!peer.accept_key(next, "100", "400", true));
    assert_eq!(peer.selected_source(), "300");
    assert!(peer.query(next, "100").is_none());
}

#[test]
fn digits_expire_outside_audio_and_hash_cancels_timeout() {
    let mut peer = Peer::new("200").unwrap();
    assert!(peer.digit('*', 10));
    assert!(!peer.expire_digit(3009));
    assert!(peer.expire_digit(3010));
    assert!(!peer.expire_digit(3011));
    peer.digit('1', 4000);
    peer.digit('#', 4001);
    assert!(!peer.expire_digit(8000));
}

#[test]
fn retry_final_gate_rechecks_pause_cancellation_generation_and_routes() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.retain_retry("200", Mode::TRANSCEIVE, 0).unwrap();
    let attempt = hub.take_retry(0).unwrap();
    hub.pause_retries();
    assert!(hub.publish_retry(&attempt).is_err());
    hub.resume_retries(0);
    hub.attach("300", Mode::TRANSCEIVE, false).unwrap();
    hub.update_topology("300", b"L T200").unwrap();
    assert!(hub.publish_retry(&attempt).is_err());
    hub.update_topology("300", b"L").unwrap();
    hub.invalidate_generation();
    assert!(hub.publish_retry(&attempt).is_err());
    hub.finish_retry(attempt, false, 0);
    assert!(hub.take_retry(0).is_some());
}

#[test]
fn disconnect_all_retains_temporary_modes_but_disconnect_temporary_preserves_permanent() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::MONITOR, false).unwrap();
    hub.attach("300", Mode::LOCAL_MONITOR, true).unwrap();
    assert_eq!(hub.disconnect_temporary(), vec!["200"]);
    assert!(hub.reaches("300", true));
    assert_eq!(hub.disconnect_all(), vec!["300"]);
    assert!(hub.take_retry(0).is_none());
    assert!(!hub.reaches("300", false));
    hub.resume_retries(42);
    let attempt = hub.take_retry(42).unwrap();
    assert_eq!(attempt.mode(), Mode::LOCAL_MONITOR);
    hub.publish_retry(&attempt).unwrap();
    hub.finish_retry(attempt, true, 42);
    assert!(hub.reaches("300", false));
    assert_eq!(hub.close(), vec!["300"]);
    assert!(!hub.reaches("300", true));
}

#[test]
fn keyed_queries_reply_locally_and_relay_without_echoing_ingress_or_source() {
    let mut hub = LinkManager::new("100").unwrap();
    for node in ["200", "300", "400"] {
        hub.attach(node, Mode::TRANSCEIVE, false).unwrap();
    }
    let query = Protocol::parse(b"K? * 300 0 0").unwrap();
    assert_eq!(
        hub.relay_key("200", &query, true, 1500, 3600),
        vec![
            ("200".into(), "K 300 100 1 2".into()),
            ("400".into(), "K? * 300 0 0".into())
        ]
    );
    let reply = Protocol::parse(b"K 400 300 1 0").unwrap();
    assert_eq!(
        hub.relay_key("200", &reply, false, 0, 0),
        vec![("400".into(), "K 400 300 1 0".into())]
    );
    let reflected = Protocol::parse(b"K? * 100 0 0").unwrap();
    assert!(hub.relay_key("200", &reflected, true, 0, 0).is_empty());
}

#[test]
fn mix_minus_preserves_local_monitor_local_receive_and_float_headroom() {
    use rpt_advanced_core::link::{MixSource, NativeMixer};
    let mixer = NativeMixer::new(3).unwrap();
    let sources = [
        MixSource {
            identity: 1,
            mode: Mode::TRANSCEIVE,
            active: true,
            audio: &[0.75, 0.5, -0.5],
        },
        MixSource {
            identity: 2,
            mode: Mode::LOCAL_MONITOR,
            active: true,
            audio: &[0.75, 0.5, -0.5],
        },
    ];
    let mut output = [9.0; 3];
    assert!(mixer.local(&sources, &mut output).unwrap());
    assert_eq!(output, [1.5, 1.0, -1.0]);
    assert!(
        mixer
            .destination(2, Mode::TRANSCEIVE, &[0.25; 3], true, &sources, &mut output)
            .unwrap()
    );
    assert_eq!(output, [1.0, 0.75, -0.25]);
    assert!(
        !mixer
            .destination(
                1,
                Mode::TRANSCEIVE,
                &[0.25; 3],
                false,
                &sources,
                &mut output
            )
            .unwrap()
    );
    assert_eq!(output, [0.0; 3]);
    assert!(
        !mixer
            .destination(1, Mode::MONITOR, &[0.25; 3], true, &sources, &mut output)
            .unwrap()
    );
    assert_eq!(output, [0.0; 3]);
    assert!(mixer.local(&sources, &mut [0.0; 4]).is_err());
}

#[test]
fn receive_activity_primes_conceals_brief_gaps_drains_and_honors_eof() {
    use rpt_advanced_core::link::ReceiveState;
    let mut receive = ReceiveState::new(8000).unwrap();
    assert!(!receive.should_render(0, 0, false, 960));
    assert!(!receive.should_render(1, 1600, false, 960));
    assert!(receive.should_render(2, 2080, false, 960));
    assert!(receive.should_render(2, 0, false, 960));
    assert!(receive.should_render(2, 0, false, 960));
    assert!(!receive.should_render(2, 0, false, 960));
    assert!(receive.should_render(2, 10, false, 960));
    assert!(!receive.should_render(3, 2080, true, 960));
    assert!(ReceiveState::new(0).is_err());
}

#[test]
fn exact_permanent_ownership_status_and_explicit_disconnect_preserve_other_modes() {
    let mut links = LinkManager::new("100").unwrap();
    links.attach("200", Mode::MONITOR, false).unwrap();
    links.attach("300", Mode::LOCAL_MONITOR, true).unwrap();
    assert!(!links.disconnect("300"));
    assert!(links.owns_permanent("300"));
    links.retain_retry("400", Mode::TRANSCEIVE, 12).unwrap();
    links.pause_retries();
    let status = links.snapshot();
    let retry = status.iter().find(|peer| peer.name == "400").unwrap();
    assert!(retry.retrying && retry.paused && retry.permanent);
    assert_eq!(retry.due_ms, Some(12));
    assert!(links.disconnect_permanent("400"));
    assert!(links.disconnect_permanent("300"));
    assert!(!links.disconnect_permanent("200"));
    assert!(links.disconnect("200"));
    assert!(links.snapshot().is_empty());
}

#[test]
fn retry_tokens_reject_foreign_serials_and_temporary_failure_drops_intent() {
    let mut hub = LinkManager::new("100").unwrap();
    hub.attach("200", Mode::MONITOR, false).unwrap();
    hub.update_topology("200", b"L T300").unwrap();
    assert_eq!(
        hub.attach("300", Mode::TRANSCEIVE, false),
        Err(AdmissionError::Loop)
    );
    assert_eq!(
        hub.update_topology("200", b"!NEWKEY!"),
        Err(AdmissionError::Invalid)
    );
    hub.disconnect_all();
    assert!(!hub.owns_permanent("200"));
    assert!(!hub.disconnect_permanent("200"));
    hub.resume_retries(0);
    let temporary = hub.take_retry(0).unwrap();
    hub.finish_retry(temporary, false, 0);
    assert!(!hub.reaches("200", true));

    let mut foreign = LinkManager::new("101").unwrap();
    foreign.retain_retry("300", Mode::TRANSCEIVE, 0).unwrap();
    let foreign_name = foreign.take_retry(0).unwrap();
    hub.retain_retry("200", Mode::TRANSCEIVE, 0).unwrap();
    let first = hub.take_retry(0).unwrap();
    assert_eq!(hub.publish_retry(&foreign_name), Err(AdmissionError::Stale));
    hub.finish_retry(foreign_name, false, 0);
    hub.finish_retry(first, false, 0);
    let second = hub.take_retry(1000).unwrap();
    let mut same_name = LinkManager::new("102").unwrap();
    same_name.retain_retry("200", Mode::TRANSCEIVE, 0).unwrap();
    let old_serial = same_name.take_retry(0).unwrap();
    assert_eq!(hub.publish_retry(&old_serial), Err(AdmissionError::Stale));
    hub.finish_retry(old_serial, false, 0);
    assert!(hub.publish_retry(&second).is_ok());
    hub.finish_retry(second, true, 1000);

    let mut current = LinkManager::new("103").unwrap();
    current.invalidate_generation();
    current.retain_retry("200", Mode::TRANSCEIVE, 0).unwrap();
    let _current = current.take_retry(0).unwrap();
    let mut old_generation = LinkManager::new("104").unwrap();
    old_generation
        .retain_retry("200", Mode::TRANSCEIVE, 0)
        .unwrap();
    current.finish_retry(old_generation.take_retry(0).unwrap(), false, 0);
    assert!(!current.reaches("200", true));
}
