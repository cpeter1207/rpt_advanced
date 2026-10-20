//! A station safety inhibit gates RF without disabling reception or peer delivery.
use super::*;

#[test]
fn site_inhibit_mutes_radio_and_ptt_without_blocking_receive_to_peers() {
    let (peer, mut sent) = test_audio_peer("2000", crate::link::Mode::TRANSCEIVE, None, 1);
    let (mut links, mut dispatcher) = crate::link::LinkAudio::new(vec![peer], 1).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    for _ in 0..4 {
        control.queue_status("E", Some(vec![0.5; 48])).unwrap();
    }
    assert!(control.queue_status("E", None).is_err());
    for inhibited in [false, true, false] {
        control.set_site_inhibit(inhibited);
        let mut audio = [0.25];
        links
            .process(
                &mut node,
                true,
                &mut audio,
                crate::link::SourcePosition::default(),
            )
            .unwrap();
        dispatcher.dispatch(1);
        let mut received = [0.0];
        assert_eq!(sent.read(&mut received), 0);
        assert_eq!(received, [0.25]);
        assert_eq!(node.keyed, !inhibited);
        assert_eq!(audio, [if inhibited { 0.0 } else { 0.25 }]);
    }
}

#[test]
fn site_inhibit_does_not_block_an_addressed_peer_reply() {
    let peer = crate::link::PeerSignals::new();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    control.set_site_inhibit(true);
    assert!(control.queue_prepared_status(media(0.5, 48).reply_to(Some(peer.reply_target()))));
    let mut audio = [0.0; 960];
    let mut parrot = [0.0; 960];
    let mut active = false;
    let mut replies = [PeerReplySample::default(); 960];
    let mut delivered = false;
    for _ in 0..2 {
        node.process_audio_with_parrot(
            false,
            false,
            false,
            &[],
            &mut audio,
            ParrotOutput {
                samples: &mut parrot,
                active: &mut active,
                replies: Some(&mut replies),
                regeneration: None,
            },
        );
        delivered |= replies
            .iter()
            .any(|sample| sample.sample == 0.5 && sample.target == peer.reply_identity());
    }
    assert!(delivered);
    assert!(!node.keyed);
    assert!(audio.iter().all(|sample| *sample == 0.0));
}
