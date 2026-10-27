use super::*;

struct Text(bool);
impl crate::link::LocalTextInput for Text {
    fn render(&mut self, pcm: &mut [f32]) -> bool {
        pcm.fill(if self.0 { 0.125 } else { 0.0 });
        self.0
    }
}
fn active_peer(name: &str, value: f32) -> (AudioPeer<FiniteInput>, LinkAudioConsumer) {
    let signals = PeerSignals::new();
    signals.publish_pcm();
    let (output, input) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let peer = AudioPeer::new(
        name,
        Mode::TRANSCEIVE,
        FiniteInput {
            signals,
            samples: std::iter::repeat_n(value, 12_480).collect(),
        },
        output,
        4,
        0,
    )
    .unwrap();
    (peer, input)
}

#[test]
fn text_local_mix_isolation_preserves_both_peer_planes_and_parrot_capture() {
    let (first, mut first_out) = active_peer("2000", 0.125);
    let (second, mut second_out) = active_peer("3000", 0.25);
    let (audio, mut dispatcher) = LinkAudio::new(vec![first, second], 4).unwrap();
    let mut audio = audio.with_text(Box::new(Text(true)));
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            full_duplex: true,
            parrot_enabled: true,
            ..Default::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    let mut rf = [0.125; 4];
    assert!(
        audio
            .process(&mut node, true, &mut rf, SourcePosition::default())
            .unwrap()
    );
    assert_eq!(rf, [0.625; 4]);
    dispatcher.dispatch(1);
    let mut output = [0.0; 4];
    assert_eq!(first_out.read(&mut output), 0);
    assert_eq!(output, [0.375; 4]);
    assert_eq!(second_out.read(&mut output), 0);
    assert_eq!(output, [0.25; 4]);
    for peer in &audio.peers {
        peer.input.signals.end();
    }
    audio
        .process(&mut node, false, &mut [0.0; 4], SourcePosition::default())
        .unwrap();
    assert_eq!(control.take_parrot_capture().unwrap().samples(), &[0.5; 4]);
}

#[test]
fn text_local_mix_obeys_duplex_inhibit_timeout_and_playable_output() {
    for (full_duplex, receive, inhibit, playable, expected) in [
        (true, false, false, true, true),
        (false, true, false, true, false),
        (true, false, true, true, false),
        (true, false, false, false, false),
    ] {
        let (audio, _) = LinkAudio::<FiniteInput>::new(vec![], 96).unwrap();
        let mut audio = audio.with_text(Box::new(Text(playable)));
        let (mut node, control) = NodeController::new(
            ControllerSettings {
                full_duplex,
                transmit_timeout_ms: 1,
                timeout_lockout_ms: 1000,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        control.set_site_inhibit(inhibit);
        let mut rf = [0.0; 96];
        let keyed = audio
            .process(&mut node, receive, &mut rf, SourcePosition::default())
            .unwrap();
        assert_eq!(rf[..48].iter().any(|value| *value != 0.0), expected);
        assert_eq!(&rf[48..], &[0.0; 48]);
        assert!(!keyed);
    }
}
