use super::Playback;

#[test]
fn absent_and_empty_pcm_share_morse_restart_and_trait_rendering() {
    use crate::audio::AudioSource;
    for prepared in [None, Some(vec![])] {
        let mut playback = Playback::new(prepared, "E", 20, 1000.0, -6, false).unwrap();
        let mut first = [0.0; 48];
        assert_eq!(AudioSource::render(&mut playback, &mut first), 48);
        assert!(first.iter().any(|sample| *sample != 0.0));
        playback.restart(false);
        let mut next = [0.0; 48];
        assert_eq!(AudioSource::render(&mut playback, &mut next), 48);
        assert_eq!(first, next);
    }
}

#[test]
fn playback_abandons_prepared_audio_for_unadvanced_morse_on_reception() {
    let mut playback =
        Playback::new(Some(vec![0.1, 0.2, 0.3, 0.4]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 2];
    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(output, [0.1, 0.2]);

    let mut morse = vec![0.0; 160];
    assert_eq!(playback.render(true, &mut morse), 160);
    assert_eq!(morse[0], 0.0);
    assert!(morse.iter().any(|sample| *sample != 0.0));
    assert_eq!(playback.render(false, &mut output), 2);
    assert_ne!(output, [0.3, 0.4]);
}

#[test]
fn reception_switches_to_morse_even_for_zero_capacity() {
    let mut playback = Playback::new(Some(vec![0.1]), "", 20, 1_000.0, -6, false).unwrap();
    assert_eq!(playback.render(true, &mut []), 0);
    let mut output = [99.0];
    assert_eq!(playback.render(false, &mut output), 0);
}

#[test]
fn playback_marks_prepared_media_complete_on_its_final_sample() {
    let mut playback = Playback::new(Some(vec![0.1, 0.2]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 2];
    assert_eq!(playback.render(false, &mut output), 2);
    assert_eq!(playback.render(false, &mut output), 0);
}

#[test]
fn playback_restart_reuses_prepared_pcm_after_an_interrupted_run() {
    let mut playback = Playback::new(Some(vec![0.1, 0.2]), "E", 20, 1_000.0, -6, false).unwrap();
    let mut output = [0.0; 1];
    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(playback.render(true, &mut output), 1);
    playback.restart(false);
    assert!(!playback.is_finished());
    assert_eq!(playback.render(false, &mut output), 1);
    assert_eq!(output, [0.1]);
}

#[test]
fn playback_restart_resets_morse_without_reconstructing_it() {
    let mut playback = Playback::new(None, "E", 20, 1_000.0, -6, false).unwrap();
    let mut first = [0.0; 48];
    let mut restarted = [0.0; 48];
    assert_eq!(playback.render(false, &mut first), 48);
    playback.restart(true);
    assert_eq!(playback.render(false, &mut restarted), 48);
    assert_eq!(restarted, first);
}
