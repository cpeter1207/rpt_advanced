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
         [courtesy 1000 link]\ninput=link\ntone_sequence=500/10\nmorse_text=L\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&config, &node).unwrap().value;
    let media = media(false, false);
    controller(&config, &node, &resolved, &media, 7).unwrap();
    assert!(media.started.load(Ordering::Acquire));
    let sources = media.sources.lock().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources[0].file.as_deref(),
        Some(std::path::Path::new("id.wav"))
    );
    assert_eq!(sources[0].speech.as_ref().unwrap().text, "ID");
    assert_eq!(sources[0].morse.as_ref().unwrap().text, "ID");
    assert_eq!(sources[1].tone.as_ref().unwrap().sequence, "500/10");
    assert_eq!(sources[1].morse.as_ref().unwrap().text, "L");
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
