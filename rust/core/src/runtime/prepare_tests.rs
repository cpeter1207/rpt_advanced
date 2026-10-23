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
    started: Arc<AtomicBool>,
    fail_register: bool,
    fail_start: bool,
}
impl StationMediaSession for Session {
    fn register_program(
        &mut self,
        program: crate::media::MediaProgram,
        _: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        if self.fail_register {
            return Err(MediaError::Io);
        }
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
            started: Arc::clone(&self.started),
            fail_register: self.fail_register,
            fail_start: self.fail_start,
        }))
    }
}
fn media(fail_register: bool, fail_start: bool) -> Media {
    Media {
        sources: Arc::new(Mutex::new(Vec::new())),
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
fn static_telemetry_is_registered_and_identifiers_request_fresh_attempts() {
    let config = ConfigDocument::parse(
        "[1000]\n\
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
        "[1000]\n\
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
        "[1000]\ntransmit_hang_ms=0\nkerchunk_max_ms=0\n\
         [identifier 1000 broken]\ninterval_ms=1\nregardless_of_activity=yes\ncontent={message_macro_missing}\n\
         [tail 1000 empty]\n\
         [tail 1000 broken]\ncontent={message_macro_missing}\n\
         [tail 1000 recorded]\nsound_file=tail.wav\nmorse_text=TAIL\n",
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
    )
    .unwrap();
    let disabled = recipes
        .warnings
        .iter()
        .filter(|warning| warning.fallback == "definition disabled")
        .map(|warning| warning.section.as_str())
        .collect::<Vec<_>>();
    assert_eq!(disabled, ["identifier 1000 broken", "tail 1000 broken"]);
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
    let config =
        ConfigDocument::parse("[1000]\n[announcement 1000 news]\nmorse_text=NEWS\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    assert!(matches!(
        controller(
            &config,
            &node,
            &resolved,
            &media(true, false),
            7,
            &crate::messages::MessageCatalog::load("en-US").unwrap()
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
            &crate::messages::MessageCatalog::load("en-US").unwrap()
        ),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn status_clock_conversion_is_still_sample_rate_independent() {
    let civil = crate::schedule::CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 7).unwrap();
    assert_eq!(civil.components().4, 9);
}
