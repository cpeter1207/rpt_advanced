use super::*;
use crate::{
    media::{FileRequest, SpeechRequest},
    schedule::Weekday,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct StreamReader {
    samples: Vec<f32>,
    offset: usize,
    started: Arc<AtomicBool>,
}
impl crate::audio::PcmStreamReader for StreamReader {
    fn start(&mut self) {
        self.started.store(true, Ordering::Release);
    }
    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        let count = output
            .len()
            .min(self.samples.len().saturating_sub(self.offset));
        if count == 0 {
            return crate::audio::PcmRead::Finished;
        }
        output[..count].copy_from_slice(&self.samples[self.offset..self.offset + count]);
        self.offset += count;
        if self.offset == self.samples.len() {
            crate::audio::PcmRead::FinalSamples(count)
        } else {
            crate::audio::PcmRead::Samples(count)
        }
    }
}

struct Session {
    sources: Arc<Mutex<Vec<MediaSource>>>,
    one_shot: Arc<Mutex<Vec<bool>>>,
    started: Arc<AtomicBool>,
    fail_register: bool,
    fail_start: bool,
}
impl StationMediaSession for Session {
    fn register_program(
        &mut self,
        program: crate::media::MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        if self.fail_register {
            return Err(MediaError::Io);
        }
        if !one_shot {
            assert!(!self.started.load(Ordering::Acquire));
        }
        self.one_shot.lock().unwrap().push(one_shot);
        self.sources.lock().unwrap().extend(program.primary);
        Ok(Box::new(StreamReader {
            samples: vec![0.5, 0.25],
            offset: 0,
            started: Arc::clone(&self.started),
        }))
    }
    fn register_prepared(
        &mut self,
        audio: PreparedAudio,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        Ok(Box::new(StreamReader {
            samples: audio.samples().to_vec(),
            offset: 0,
            started: Arc::clone(&self.started),
        }))
    }
    fn start(&mut self) -> Result<(), MediaError> {
        if self.fail_start {
            return Err(MediaError::Io);
        }
        self.started.store(true, Ordering::Release);
        Ok(())
    }
}

struct Media {
    sources: Arc<Mutex<Vec<MediaSource>>>,
    one_shot: Arc<Mutex<Vec<bool>>>,
    started: Arc<AtomicBool>,
    fail_register: bool,
    fail_start: bool,
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
    fn station(
        &self,
        node: &str,
        generation: u64,
    ) -> Result<Box<dyn StationMediaSession>, MediaError> {
        assert_eq!(node, "1000");
        assert_eq!(generation, 7);
        Ok(Box::new(Session {
            sources: Arc::clone(&self.sources),
            one_shot: Arc::clone(&self.one_shot),
            started: Arc::clone(&self.started),
            fail_register: self.fail_register,
            fail_start: self.fail_start,
        }))
    }
}
fn media(fail_register: bool, fail_start: bool) -> Media {
    Media {
        sources: Arc::new(Mutex::new(Vec::new())),
        one_shot: Arc::new(Mutex::new(Vec::new())),
        started: Arc::new(AtomicBool::new(false)),
        fail_register,
        fail_start,
    }
}
fn settings() -> ResolvedIdentifierSettings {
    ResolvedIdentifierSettings::resolve(
        &ConfigDocument::parse("[1000]\n").unwrap(),
        &NodeId::new("1000").unwrap(),
        None,
    )
    .unwrap()
    .value
}

#[test]
fn timeout_notices_are_reusable_sources_prepared_before_station_start() {
    let config = ConfigDocument::parse(
        "[1000]\ntransmit_timeout_ms=180001\n\
         [speech 1000]\nvoice=timeout.onnx\nspeed_percent=125\nlevel_db=-9\n\
         [morse 1000]\nspeed_wpm=17\nfrequency_hz=725\nlevel_db=-12\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();

    assert!(media.started.load(Ordering::Acquire));
    assert_eq!(*media.one_shot.lock().unwrap(), [false, false]);
    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 2);
    for (source, speech_text, morse_text) in [
        (
            &sources[0],
            "Repeater timed out after 3 minutes, and 1 second.",
            "REPEATER TIMED OUT AFTER 3M 1S",
        ),
        (
            &sources[1],
            "Repeater timeout cleared.",
            "REPEATER TIMEOUT CLEARED",
        ),
    ] {
        let speech = source.speech.as_ref().unwrap();
        assert_eq!(speech.text, speech_text);
        assert_eq!(speech.model, std::path::Path::new("timeout.onnx"));
        assert_eq!(speech.speed_percent, 125);
        assert_eq!(speech.level_db, -9);
        let morse = source.morse.as_ref().unwrap();
        assert_eq!(morse.text, morse_text);
        assert_eq!(morse.speed_wpm, 17);
        assert_eq!(morse.frequency_hz, 725.0);
        assert_eq!(morse.level_db, -12);
        assert!(source.file.is_none());
        assert!(source.tone.is_none());
        assert_eq!(source.provider_gain_db, 0);
    }
}

#[test]
fn disabled_timeout_registers_no_protection_sources() {
    let config = ConfigDocument::parse("[1000]\ntransmit_timeout_ms=0\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();
    assert!(media.started.load(Ordering::Acquire));
    assert!(media.sources.lock().unwrap().is_empty());
    assert!(media.one_shot.lock().unwrap().is_empty());
}

#[test]
fn prepared_timeout_sources_reach_the_controller_priority_playback() {
    let config = ConfigDocument::parse("[1000]\nfull_duplex=yes\ntransmit_timeout_ms=1\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    let (mut controller, _, _, _station, _) = controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();
    controller.process_audio(true, false, &[], &mut [0.8; 48]);
    let mut notice = [0.8; 2];
    controller.process_audio(true, false, &[], &mut notice);
    assert_eq!(notice, [0.5, 0.25]);
    let mut cleared = [0.8; 2];
    controller.process_audio(false, false, &[], &mut cleared);
    assert_eq!(cleared, [0.5, 0.25]);
}

#[test]
fn timeout_media_registration_and_start_failure_reject_preparation() {
    let config = ConfigDocument::parse("[1000]\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    for (fail_register, fail_start) in [(true, false), (false, true)] {
        let media = media(fail_register, fail_start);
        assert!(matches!(
            controller(
                &config,
                &node,
                &resolved,
                &media,
                7,
                &crate::messages::MessageCatalog::load("en-US").unwrap(),
                None,
            ),
            Err(RuntimeError::Preparation)
        ));
        assert!(!media.started.load(Ordering::Acquire));
    }
}

#[test]
fn timeout_milliseconds_round_up_to_the_reported_second() {
    for (milliseconds, speech, morse) in [
        (1, "1 second", "1S"),
        (1000, "1 second", "1S"),
        (1001, "2 seconds", "2S"),
        (60_000, "1 minute", "1M"),
        (60_001, "1 minute, and 1 second", "1M 1S"),
    ] {
        let config =
            ConfigDocument::parse(&format!("[1000]\ntransmit_timeout_ms={milliseconds}\n"))
                .unwrap();
        let node = NodeId::new("1000").unwrap();
        let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
        let media = media(false, false);
        controller(
            &config,
            &node,
            &resolved,
            &media,
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        )
        .unwrap();
        let sources = media.sources.lock().unwrap();
        assert_eq!(
            sources[0].speech.as_ref().unwrap().text,
            format!("Repeater timed out after {speech}.")
        );
        assert_eq!(
            sources[0].morse.as_ref().unwrap().text,
            format!("REPEATER TIMED OUT AFTER {morse}")
        );
    }
}

#[test]
fn timeout_duration_uses_the_captured_calendar_with_a_no_clock_fallback() {
    let config = ConfigDocument::parse("[1000]\ntransmit_timeout_ms=2419200000\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    for (civil, speech, morse) in [
        (None, "4 weeks", "4W"),
        (
            Some(CivilTime::new(2026, 2, 1, Weekday::Sunday, 0, 0).unwrap()),
            "1 month",
            "1M",
        ),
    ] {
        let media = media(false, false);
        controller(
            &config,
            &node,
            &resolved,
            &media,
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            civil,
        )
        .unwrap();
        let sources = media.sources.lock().unwrap();
        assert_eq!(
            sources[0].speech.as_ref().unwrap().text,
            format!("Repeater timed out after {speech}.")
        );
        assert_eq!(
            sources[0].morse.as_ref().unwrap().text,
            format!("REPEATER TIMED OUT AFTER {morse}")
        );
    }
}

#[test]
fn static_telemetry_is_registered_and_identifiers_request_fresh_attempts() {
    let config = ConfigDocument::parse(
        "[1000]\ntransmit_timeout_ms=0\n\
         [identifier 1000 id]\ninterval_ms=1\nregardless_of_activity=yes\nsound_file=id.wav\nspeech_text=ID\nmorse_text=ID\n\
         [announcement 1000 news]\nmorse_text=NEWS\n\
         [courtesy 1000 link]\ninput=link\ntone_sequence=500/10\nmorse_text=L\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    let (mut controller, mut control, _, _station, recipes) = controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();
    assert!(media.started.load(Ordering::Acquire));
    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].morse.as_ref().unwrap().text, "NEWS");
    assert_eq!(sources[1].tone.as_ref().unwrap().sequence, "500/10");
    assert_eq!(sources[1].morse.as_ref().unwrap().text, "L");
    controller.process_audio(false, false, &[], &mut [0.0; 960]);
    let request = control.take_message_request().unwrap();
    let super::super::composition::MessageRecipe::Composition(recipe) =
        &recipes.items[request.content_index]
    else {
        panic!("identifier composition");
    };
    let source = &recipe.settings_for(None).media;
    assert_eq!(source.file.as_deref(), Some(std::path::Path::new("id.wav")));
    assert_eq!(source.speech.as_ref().unwrap().text, "ID");
    assert_eq!(source.morse.as_ref().unwrap().text, "ID");
}

#[test]
fn empty_sources_are_skipped_and_receiver_and_peer_courtesy_are_owned() {
    let config = ConfigDocument::parse(
        "[1000]\ntransmit_timeout_ms=0\n\
         [identifier 1000 empty]\n\
         [announcement 1000 empty]\n\
         [announcement 1000 recorded]\nsound_file=news.wav\n\
         [courtesy 1000 empty]\ninput=link\n\
         [courtesy 1000 receiver]\ninput=receiver\nmorse_text=R\n\
         [courtesy 1000 peer]\ninput=link\nremote_node=2000\nmorse_text=P\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);

    controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();

    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(
        sources[0].file.as_deref(),
        Some(std::path::Path::new("news.wav"))
    );
    assert_eq!(sources[1].morse.as_ref().unwrap().text, "R");
    assert_eq!(sources[2].morse.as_ref().unwrap().text, "P");
}

#[test]
fn invalid_recurring_content_is_disabled_without_blocking_a_valid_tail() {
    let config = ConfigDocument::parse(
        "[1000]\ntransmit_hang_ms=0\ntransmit_timeout_ms=0\nkerchunk_max_ms=0\n\
         [identifier 1000 broken]\ninterval_ms=1\nregardless_of_activity=yes\ncontent={message_macro_missing}\n\
         [tail 1000 empty]\n\
         [tail 1000 broken]\ncontent={message_macro_missing}\n\
         [tail 1000 recorded]\nsound_file=tail.wav\nmorse_text=TAIL\n\
         [event 1000 broken]\nat=daily 12:00\ncontent={message_macro_missing}\ndtmf_id=7\n\
         [event 1000 action]\nat=daily 12:00\nmacro=resume\ndtmf_id=8\n\
         [macro resume]\naction=reconnect_all\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    let (mut controller, mut control, _, _station, recipes) = controller(
        &config,
        &node,
        &resolved,
        &media,
        7,
        &crate::messages::MessageCatalog::load("en-US").unwrap(),
        None,
    )
    .unwrap();
    let disabled = recipes
        .warnings
        .iter()
        .filter(|warning| warning.fallback == "definition disabled")
        .map(|warning| warning.section.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        disabled,
        [
            "identifier 1000 broken",
            "tail 1000 broken",
            "event 1000 broken"
        ]
    );
    assert!(recipes.events.is_empty());
    assert!(recipes.selectors.is_empty());
    let mut requests = Vec::new();
    for receiving in [true, false, false] {
        controller.process_audio(receiving, false, &[], &mut [0.0; 48]);
        if let Some(request) = control.take_message_request() {
            requests.push(request);
        }
    }
    assert_eq!(requests.len(), 1);
    let super::super::composition::MessageRecipe::Composition(recipe) =
        &recipes.items[requests[0].content_index]
    else {
        panic!("recorded tail composition");
    };
    assert_eq!(
        recipe.settings_for(None).media.file.as_deref(),
        Some(std::path::Path::new("tail.wav"))
    );
    assert!(media.sources.lock().unwrap().is_empty());
}

#[test]
fn tails_allow_file_or_speech_without_relaxing_identifier_morse_requirements() {
    for source in ["sound_file=tail.wav", "speech_text=Tail message"] {
        let config = ConfigDocument::parse(&format!(
            "[1000]\ntransmit_hang_ms=0\nkerchunk_max_ms=0\n\
             [identifier 1000 recorded]\n{source}\n\
             [tail 1000 recorded]\n{source}\n"
        ))
        .unwrap();
        let node = NodeId::new("1000").unwrap();
        let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
        let media = media(false, false);
        let (mut controller, mut control, _, _station, recipes) = controller(
            &config,
            &node,
            &resolved,
            &media,
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        )
        .unwrap();
        let disabled: Vec<_> = recipes
            .warnings
            .iter()
            .filter(|warning| warning.fallback == "definition disabled")
            .map(|warning| warning.section.as_str())
            .collect();
        assert_eq!(disabled, ["identifier 1000 recorded"], "{source}");
        controller.process_audio(true, false, &[], &mut [0.0; 48]);
        controller.process_audio(false, false, &[], &mut [0.0; 48]);
        controller.process_audio(false, false, &[], &mut [0.0; 48]);
        assert!(control.take_message_request().is_some(), "{source}");
    }
}

#[test]
fn manual_message_selectors_share_the_identifier_and_tail_namespace() {
    for (id_selector, tail_selector, valid) in
        [(2, 3, true), (1, 3, false), (2, 1, false), (2, 2, false)]
    {
        let config = ConfigDocument::parse(&format!(
            "[1000]\n[message report]\ncontent=Ready\ndtmf_id=1\n\
             [identifier 1000 id]\nmorse_text=ID\ndtmf_id={id_selector}\n\
             [tail 1000 tail]\nmorse_text=TAIL\ndtmf_id={tail_selector}\n"
        ))
        .unwrap();
        let node = NodeId::new("1000").unwrap();
        let settings = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
        let media = media(false, false);
        let result = controller(
            &config,
            &node,
            &settings,
            &media,
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        );
        assert_eq!(
            result.is_ok(),
            valid,
            "ID={id_selector}, tail={tail_selector}"
        );
        if let Ok((_, _, _, _, recipes)) = result {
            assert_eq!(
                recipes
                    .selectors
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["1", "2", "3"]
            );
        }
    }
}

#[test]
fn event_selectors_share_the_existing_message_namespace() {
    for selector in [1, 2] {
        let config = ConfigDocument::parse(&format!("[1000]\n[message notice]\ncontent=Ready\ndtmf_id=1\n[event 1000 clock]\nat=every 1 hour\ncontent=Clock\ndtmf_id={selector}\n")).unwrap();
        let node = NodeId::new("1000").unwrap();
        let settings = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
        let result = controller(
            &config,
            &node,
            &settings,
            &media(false, false),
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        );
        assert_eq!(result.is_ok(), selector == 2);
        if let Ok((_, _, _, _, recipes)) = result {
            assert_eq!(recipes.selectors["2"], recipes.events["clock"]);
        }
    }
}

#[test]
fn status_speech_and_morse_are_registered_as_one_producer_owned_chain() {
    let media = media(false, false);
    streamed_status(
        &mut *media.station("1000", 7).unwrap(),
        &settings(),
        "L",
        "Connected",
    )
    .unwrap();
    let source = media.sources.lock().unwrap().pop().unwrap();
    assert_eq!(source.speech.unwrap().text, "Connected");
    assert_eq!(source.morse.unwrap().text, "L");
}

#[test]
fn parrot_recording_is_prepared_without_a_spoken_report() {
    let media = media(false, false);
    let mut station = media.station("1000", 7).unwrap();
    let playback = streamed_parrot(
        &mut *station,
        &settings(),
        "",
        PreparedAudio::new(48_000, vec![0.5, 0.25]).unwrap(),
    );
    assert!(playback.is_ok());
    assert!(media.sources.lock().unwrap().is_empty());
}

#[test]
fn parrot_report_is_omitted_when_no_speech_model_is_configured() {
    let config = ConfigDocument::parse("[1000]\n[speech]\nvoice=\n").unwrap();
    let settings =
        ResolvedIdentifierSettings::resolve(&config, &NodeId::new("1000").unwrap(), None)
            .unwrap()
            .value;
    let media = media(false, false);
    let mut station = media.station("1000", 7).unwrap();

    let playback = streamed_parrot(
        &mut *station,
        &settings,
        "Peak level -3 dBFS",
        PreparedAudio::new(48_000, vec![0.5, 0.25]).unwrap(),
    );

    assert!(playback.is_ok());
    assert!(media.sources.lock().unwrap().is_empty());
}

#[test]
fn config_failures_convert_to_the_runtime_error_boundary() {
    let error = ConfigDocument::parse("[unterminated").unwrap_err();
    assert_eq!(
        RuntimeError::from(error.clone()),
        RuntimeError::Config(error)
    );
}

#[test]
fn station_registration_and_start_fail_before_publishing_controller_state() {
    let config = ConfigDocument::parse(
        "[1000]\ntransmit_timeout_ms=0\n[announcement 1000 news]\nmorse_text=NEWS\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    assert!(matches!(
        controller(
            &config,
            &node,
            &resolved,
            &media(true, false),
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        ),
        Err(RuntimeError::Preparation)
    ));
    assert!(matches!(
        controller(
            &config,
            &node,
            &resolved,
            &media(false, true),
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap(),
            None,
        ),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn status_clock_conversion_is_still_sample_rate_independent() {
    let civil = crate::schedule::CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 7).unwrap();
    assert_eq!(civil.components().4, 9);
}
