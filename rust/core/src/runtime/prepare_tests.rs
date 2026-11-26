use super::*;
use crate::config::resources::candidate::ResourceKind;
use crate::station_fixture::{PORT, configuration, projected_telemetry};
use crate::{
    media::{FileRequest, SpeechRequest},
    schedule::Weekday,
};
use serde_json::json;
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
        assert_eq!(node, PORT);
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
    let config = configuration(&[(PORT, "radio")], &[]);
    let id = PORT.parse().unwrap();
    modular_controller(
        &config,
        id,
        &config.station_settings(id).unwrap(),
        &media(false, false),
        7,
        None,
    )
    .unwrap()
    .2
}
#[test]
fn timeout_notices_are_reusable_sources_prepared_before_station_start() {
    let config = configuration(
        &[(PORT, "radio")],
        &[
            (
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"timeout_ms":180001}}}),
            ),
            (
                PORT,
                ResourceKind::TelemetrySettings,
                "",
                json!({"speech":{"voice":"timeout.onnx","timing":{"speed_percent":125},"level_db":-9},"morse":{"timing":{"speed_wpm":17},"frequency_hz":725,"level_db":-12}}),
            ),
        ],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    let media = media(false, false);
    modular_controller(&config, node, &resolved, &media, 7, None).unwrap();

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
    let config = configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::TxSignaling,
            "",
            json!({"ptt":{"timing":{"timeout_ms":0}}}),
        )],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    let media = media(false, false);
    modular_controller(&config, node, &resolved, &media, 7, None).unwrap();
    assert!(media.started.load(Ordering::Acquire));
    assert!(media.sources.lock().unwrap().is_empty());
    assert!(media.one_shot.lock().unwrap().is_empty());
}

#[test]
fn prepared_timeout_sources_reach_the_controller_priority_playback() {
    let config = configuration(
        &[(PORT, "radio")],
        &[
            (
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"timeout_ms":1}}}),
            ),
            (PORT, ResourceKind::Radio, "", json!({"duplex_mode":"full"})),
        ],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    let media = media(false, false);
    let (mut controller, _, _, _station, _) =
        modular_controller(&config, node, &resolved, &media, 7, None).unwrap();
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
    let config = configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::TxSignaling,
            "",
            json!({"ptt":{"timing":{"timeout_ms":60000}}}),
        )],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    for (fail_register, fail_start) in [(true, false), (false, true)] {
        let media = media(fail_register, fail_start);
        assert!(matches!(
            modular_controller(&config, node, &resolved, &media, 7, None,),
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
        let config = configuration(
            &[(PORT, "radio")],
            &[(
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"timeout_ms":milliseconds}}}),
            )],
        );
        let node = PORT.parse().unwrap();
        let resolved = config.station_settings(node).unwrap();
        let media = media(false, false);
        modular_controller(&config, node, &resolved, &media, 7, None).unwrap();
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
    let config = configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::TxSignaling,
            "",
            json!({"ptt":{"timing":{"timeout_ms":2419200000u32}}}),
        )],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    for (civil, speech, morse) in [
        (None, "4 weeks", "4W"),
        (
            Some(CivilTime::new(2026, 2, 1, Weekday::Sunday, 0, 0).unwrap()),
            "1 month",
            "1M",
        ),
    ] {
        let media = media(false, false);
        modular_controller(&config, node, &resolved, &media, 7, civil).unwrap();
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
    let config = configuration(
        &[(PORT, "radio")],
        &[
            (
                PORT,
                ResourceKind::TxSignaling,
                "",
                json!({"ptt":{"timing":{"timeout_ms":0}}}),
            ),
            (
                PORT,
                ResourceKind::Identifier,
                "id",
                json!({"timing":{"interval_ms":1},"regardless_of_activity":true,"file":"id.wav","speech":{"text":"ID"},"morse":{"text":"ID"}}),
            ),
            (
                PORT,
                ResourceKind::Announcement,
                "news",
                json!({"morse":{"text":"NEWS"}}),
            ),
            (
                PORT,
                ResourceKind::Courtesy,
                "link",
                json!({"input":"link","tone":{"sequence":"500/10"},"morse":{"text":"L"}}),
            ),
        ],
    );
    let id = PORT.parse().unwrap();
    let media = media(false, false);
    let (mut controller, mut control, _, _station, recipes) = modular_controller(
        &config,
        id,
        &config.station_settings(id).unwrap(),
        &media,
        7,
        None,
    )
    .unwrap();
    assert!(media.started.load(Ordering::Acquire));
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
    let prepared =
        &config.scopes[&crate::config::resources::candidate::ResourceScope::Port(id)].media;
    let news = &prepared.recipes[&("announcements", "news".into())]
        .settings_for(None)
        .media;
    assert_eq!(news.morse.as_ref().unwrap().text, "NEWS");
    let link = &prepared.recipes[&("courtesies", "link".into())]
        .settings_for(None)
        .media;
    assert_eq!(link.tone.as_ref().unwrap().sequence, "500/10");
    assert_eq!(link.morse.as_ref().unwrap().text, "L");
}

#[test]
fn empty_sources_are_skipped_and_receiver_and_peer_courtesy_are_owned() {
    let (_, _, p) = projected_telemetry(&[
        (ResourceKind::Identifier, "empty", json!({})),
        (ResourceKind::Announcement, "empty", json!({})),
        (
            ResourceKind::Announcement,
            "recorded",
            json!({"file":"news.wav"}),
        ),
        (ResourceKind::Courtesy, "empty", json!({"input":"link"})),
        (
            ResourceKind::Courtesy,
            "receiver",
            json!({"input":"receiver","morse":{"text":"R"}}),
        ),
        (
            ResourceKind::Courtesy,
            "peer",
            json!({"input":"link","remote_node":"2000","morse":{"text":"P"}}),
        ),
    ])
    .unwrap();
    assert_eq!(p.recipes.len(), 3);
    assert_eq!(p.announcements["recorded"].sound_file, "news.wav");
    assert_eq!(p.courtesies["receiver"].morse_text, "R");
    assert_eq!(p.courtesies["peer"].remote_node, "2000");
    assert_eq!(p.courtesies["peer"].morse_text, "P");
}

#[test]
fn unresolved_recurring_content_cannot_publish_and_a_valid_tail_prepares() {
    let good = (
        ResourceKind::Tail,
        "recorded",
        json!({"file":"tail.wav","morse":{"text":"TAIL"}}),
    );
    assert!(
        projected_telemetry(&[
            good.clone(),
            (
                ResourceKind::Identifier,
                "broken",
                json!({"content":"{ missing }"})
            )
        ])
        .is_err()
    );
    let (_, _, prepared) = projected_telemetry(&[good]).unwrap();
    assert_eq!(
        prepared.tails["recorded"].message.media.file.as_deref(),
        Some(std::path::Path::new("tail.wav"))
    );
}

#[test]
fn tails_allow_file_or_speech_without_relaxing_identifier_morse_requirements() {
    for body in [
        json!({"file":"tail.wav"}),
        json!({"speech":{"text":"Tail message"}}),
    ] {
        assert!(
            projected_telemetry(&[(ResourceKind::Identifier, "recorded", body.clone())]).is_err()
        );
        let (_, _, p) = projected_telemetry(&[(ResourceKind::Tail, "recorded", body)]).unwrap();
        assert!(p.recipes.contains_key(&("tails", "recorded".into())));
    }
}

#[test]
fn manual_message_selectors_share_the_identifier_and_tail_namespace() {
    for (id, tail, valid) in [
        ("2", "3", true),
        ("1", "3", false),
        ("2", "1", false),
        ("2", "2", false),
    ] {
        let result = projected_telemetry(&[
            (
                ResourceKind::Message,
                "report",
                json!({"content":"Ready","dtmf_id":"1"}),
            ),
            (
                ResourceKind::Identifier,
                "id",
                json!({"morse":{"text":"ID"},"dtmf_id":id}),
            ),
            (
                ResourceKind::Tail,
                "tail",
                json!({"morse":{"text":"TAIL"},"dtmf_id":tail}),
            ),
        ]);
        assert_eq!(result.is_ok(), valid, "ID={id}, tail={tail}");
        if let Ok((_, messages, prepared)) = result {
            assert_eq!(
                messages.recipes["report"]
                    .settings_for(None)
                    .dtmf_id
                    .as_deref(),
                Some("1")
            );
            assert_eq!(prepared.identifiers["id"].dtmf_id.as_deref(), Some("2"));
            assert_eq!(prepared.tails["tail"].message.dtmf_id.as_deref(), Some("3"));
        }
    }
}

#[test]
fn status_speech_and_morse_are_registered_as_one_producer_owned_chain() {
    let media = media(false, false);
    streamed_status(
        &mut *media.station(PORT, 7).unwrap(),
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
    let mut station = media.station(PORT, 7).unwrap();
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
    let mut settings = settings();
    settings.speech_model.clear();
    let media = media(false, false);
    let mut station = media.station(PORT, 7).unwrap();

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
    let error = crate::config::NodeId::new("").unwrap_err();
    assert_eq!(
        RuntimeError::from(error.clone()),
        RuntimeError::Config(error)
    );
}

#[test]
fn station_registration_and_start_fail_before_publishing_controller_state() {
    let config = configuration(
        &[(PORT, "radio")],
        &[(
            PORT,
            ResourceKind::TxSignaling,
            "",
            json!({"ptt":{"timing":{"timeout_ms":1000}}}),
        )],
    );
    let node = PORT.parse().unwrap();
    let resolved = config.station_settings(node).unwrap();
    assert!(matches!(
        modular_controller(&config, node, &resolved, &media(true, false), 7, None,),
        Err(RuntimeError::Preparation)
    ));
    assert!(matches!(
        modular_controller(&config, node, &resolved, &media(false, true), 7, None,),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn status_clock_conversion_is_still_sample_rate_independent() {
    let civil = crate::schedule::CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 7).unwrap();
    assert_eq!(civil.components().4, 9);
}
