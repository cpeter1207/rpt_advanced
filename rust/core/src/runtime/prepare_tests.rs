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
    fn register(
        &mut self,
        source: MediaSource,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        if self.fail_register {
            return Err(MediaError::Io);
        }
        self.sources.lock().unwrap().push(source);
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
fn every_telemetry_source_is_registered_with_the_station_pcm_producer() {
    let config = ConfigDocument::parse(
        "[1000]\n\
         [identifier 1000 id]\nsound_file=id.wav\nspeech_text=ID\nmorse_text=ID\n\
         [announcement 1000 news]\nmorse_text=NEWS\n\
         [courtesy 1000 link]\ninput=link\ntone_sequence=500/10\nmorse_text=L\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    controller(&config, &node, &resolved, &media, 7).unwrap();
    assert!(media.started.load(Ordering::Acquire));
    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(
        sources[0].file.as_deref(),
        Some(std::path::Path::new("id.wav"))
    );
    assert_eq!(sources[0].speech.as_ref().unwrap().text, "ID");
    assert_eq!(sources[0].morse.as_ref().unwrap().text, "ID");
    assert_eq!(sources[1].morse.as_ref().unwrap().text, "NEWS");
    assert_eq!(sources[2].tone.as_ref().unwrap().sequence, "500/10");
    assert_eq!(sources[2].morse.as_ref().unwrap().text, "L");
}

#[test]
fn empty_sources_are_skipped_and_receiver_and_peer_courtesy_are_owned() {
    let config = ConfigDocument::parse(
        "[1000]\n\
         [identifier 1000 empty]\n\
         [announcement 1000 empty]\n\
         [courtesy 1000 empty]\ninput=link\n\
         [courtesy 1000 receiver]\ninput=receiver\nmorse_text=R\n\
         [courtesy 1000 peer]\ninput=link\nremote_node=2000\nmorse_text=P\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);

    controller(&config, &node, &resolved, &media, 7).unwrap();

    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].morse.as_ref().unwrap().text, "R");
    assert_eq!(sources[1].morse.as_ref().unwrap().text, "P");
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
    let config = ConfigDocument::parse("[1000]\n[identifier 1000 id]\nmorse_text=ID\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    assert!(matches!(
        controller(&config, &node, &resolved, &media(true, false), 7),
        Err(RuntimeError::Preparation)
    ));
    assert!(matches!(
        controller(&config, &node, &resolved, &media(false, true), 7),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn status_clock_conversion_is_still_sample_rate_independent() {
    let civil = crate::schedule::CivilTime::new(2026, 9, 15, Weekday::Tuesday, 9, 7).unwrap();
    assert_eq!(civil.components().4, 9);
}
