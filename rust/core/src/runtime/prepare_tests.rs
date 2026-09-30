use super::*;
use crate::audio::PcmStreamReader;
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Media {
    file: Result<PreparedAudio, MediaError>,
    speech: Result<PreparedAudio, MediaError>,
    calls: RefCell<Vec<&'static str>>,
}
impl NativeFilePreparer for Media {
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        assert_eq!(request.path, Path::new("test.wav"));
        assert!(!request.cancellation.is_cancelled());
        self.calls.borrow_mut().push("file");
        self.file.clone()
    }
}
impl NativeSpeechPreparer for Media {
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        assert_eq!(request.text, "TEST");
        assert!(!request.cancellation.is_cancelled());
        self.calls.borrow_mut().push("speech");
        self.speech.clone()
    }
}
fn media(result: Result<PreparedAudio, MediaError>) -> Media {
    Media {
        file: result.clone(),
        speech: result,
        calls: RefCell::new(vec![]),
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

struct StreamReader {
    samples: Vec<f32>,
    offset: usize,
    started: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl crate::audio::PcmStreamReader for StreamReader {
    fn start(&mut self) {
        self.started.store(true, Ordering::Release);
    }

    fn render(&mut self, output: &mut [f32]) -> crate::audio::PcmRead {
        let count = output.len().min(self.samples.len() - self.offset);
        if count == 0 {
            return crate::audio::PcmRead::Finished;
        }
        output[..count].copy_from_slice(&self.samples[self.offset..self.offset + count]);
        self.offset += count;
        crate::audio::PcmRead::Samples(count)
    }

    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Station {
    sources: Arc<Mutex<Vec<MediaSource>>>,
    started: Arc<AtomicBool>,
    fail_register: bool,
    fail_start: bool,
}

impl StationMediaSession for Station {
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
            cancelled: Arc::new(AtomicBool::new(false)),
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

struct StationFixture {
    session: Box<dyn StationMediaSession>,
    sources: Arc<Mutex<Vec<MediaSource>>>,
    started: Arc<AtomicBool>,
}

fn station(fail_register: bool, fail_start: bool) -> StationFixture {
    let sources = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(AtomicBool::new(false));
    StationFixture {
        session: Box::new(Station {
            sources: Arc::clone(&sources),
            started: Arc::clone(&started),
            fail_register,
            fail_start,
        }),
        sources,
        started,
    }
}

struct StreamingMedia {
    media: Media,
    sources: Arc<Mutex<Vec<MediaSource>>>,
    started: Arc<AtomicBool>,
    fail_register: bool,
    fail_start: bool,
}

impl NativeFilePreparer for StreamingMedia {
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        self.media.file(request)
    }

    fn station(
        &self,
        node: &str,
        generation: u64,
    ) -> Result<Option<Box<dyn StationMediaSession>>, MediaError> {
        assert_eq!(node, "1000");
        assert_eq!(generation, 7);
        Ok(Some(Box::new(Station {
            sources: Arc::clone(&self.sources),
            started: Arc::clone(&self.started),
            fail_register: self.fail_register,
            fail_start: self.fail_start,
        })))
    }
}

impl NativeSpeechPreparer for StreamingMedia {
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        self.media.speech(request)
    }
}

fn streaming_media(fail_register: bool, fail_start: bool) -> StreamingMedia {
    StreamingMedia {
        media: media(Err(MediaError::Unavailable)),
        sources: Arc::new(Mutex::new(Vec::new())),
        started: Arc::new(AtomicBool::new(false)),
        fail_register,
        fail_start,
    }
}

#[test]
fn native_source_contract_rejects_rate_or_peak_and_distinguishes_unavailable_from_incompatible() {
    let error = ConfigError::syntax(3, "invalid input");
    assert_eq!(
        RuntimeError::from(error.clone()),
        RuntimeError::Config(error)
    );
    for (result, expected) in [
        (PreparedAudio::new(48000, vec![0.25]), Ok(Some(vec![0.25]))),
        (
            PreparedAudio::new(8000, vec![0.25]),
            Err(RuntimeError::Preparation),
        ),
        (
            PreparedAudio::new(48000, vec![1.01]),
            Err(RuntimeError::Preparation),
        ),
        (Err(MediaError::Unavailable), Ok(None)),
        (
            Err(MediaError::IncompatibleAdapter),
            Err(RuntimeError::Preparation),
        ),
    ] {
        let media = media(result);
        let mut settings = settings();
        assert_eq!(speech(&media, &settings, "TEST"), expected);
        settings.sound_file = "test.wav".into();
        settings.speech_text = "TEST".into();
        assert_eq!(source(&media, &settings), expected);
    }
}

#[test]
fn courtesy_preparation_validates_tones_even_when_file_succeeds_and_can_fall_back_to_tones() {
    for file_available in [false, true] {
        let media = media(if file_available {
            PreparedAudio::new(48000, vec![0.5; 48])
        } else {
            Err(MediaError::Unavailable)
        });
        for (tone, morse, available, succeeds) in [
            ("", "", file_available, true),
            ("1000/1", "", true, true),
            ("", "E", true, true),
            ("invalid", "E", false, false),
            ("", "~", false, false),
        ] {
            let document = ConfigDocument::parse(&format!("[1000]\n[courtesy 1000 test]\ninput=receiver\nsound_file=test.wav\nlevel_db=-6\ntone_sequence={tone}\nmorse_text={morse}\n")).unwrap();
            let resolved =
                ResolvedCourtesySettings::resolve(&document, &NodeId::new("1000").unwrap(), "test")
                    .unwrap()
                    .value;
            let result = courtesy_media(&media, &mut None, &resolved, &settings());
            assert_eq!(
                result.is_ok(),
                succeeds,
                "file={file_available}, tone={tone}, morse={morse}"
            );
            if succeeds {
                assert_eq!(result.unwrap().is_some(), available);
            }
        }
    }
}

#[test]
fn controller_composition_prepares_all_courtesy_assignments_and_skips_empty_declarations() {
    let media = media(Err(MediaError::Unavailable));
    let document = ConfigDocument::parse("[1000]\n[identifier 1000 empty]\n[announcement 1000 empty]\n[announcement 1000 spoken]\nmorse_text=TEST\n[courtesy 1000 receiver]\ninput=receiver\ntone_sequence=1000/1\n[courtesy 1000 generic]\ninput=link\nmorse_text=E\n[courtesy 1000 peer]\ninput=link\nremote_node=2000\nmorse_text=E\n[courtesy 1000 empty]\ninput=link\nremote_node=3000\n").unwrap();
    let node = NodeId::new("1000").unwrap();
    let resolved = ResolvedNodeSettings::resolve(&document, &node)
        .unwrap()
        .value;
    assert!(controller(&document, &node, &resolved, &media, 1).is_ok());
    let mut invalid = resolved;
    invalid.hang_ms = u64::MAX;
    assert!(matches!(
        controller(&document, &node, &invalid, &media, 1),
        Err(RuntimeError::Preparation)
    ));
    let invalid_text =
        ConfigDocument::parse("[1000]\n[identifier 1000 invalid]\nmorse_text=~\n").unwrap();
    assert!(matches!(
        controller(&invalid_text, &node, &invalid, &media, 1),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn station_registers_file_and_speech_as_a_stream_without_preparing_either() {
    let media = media(Err(MediaError::Unavailable));
    let fixture = station(false, false);
    let mut session = Some(fixture.session);
    let mut settings = settings();
    settings.sound_file = "notice.wav".into();
    settings.speech_text = "Hello".into();
    settings.speech_model = "/models/voice.onnx".into();
    settings.speech_speed_percent = 125;
    settings.speech_level_db = -8;

    let Some(SourceAudio::Stream(mut reader)) =
        source_with_station(&media, &mut session, &settings).unwrap()
    else {
        panic!("station media must use its PCM stream");
    };
    let registered = fixture.sources.lock().unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].file.as_deref(), Some(Path::new("notice.wav")));
    let speech = registered[0].speech.as_ref().unwrap();
    assert_eq!(speech.text, "Hello");
    assert_eq!(speech.model, Path::new("/models/voice.onnx"));
    assert_eq!(speech.speed_percent, 125);
    assert_eq!(speech.level_db, -8);
    assert!(media.calls.borrow().is_empty());

    let mut output = [0.0; 2];
    reader.start();
    assert!(fixture.started.load(Ordering::Acquire));
    assert_eq!(
        reader.render(&mut output),
        crate::audio::PcmRead::Samples(2)
    );
    assert_eq!(output, [0.5, 0.25]);
}

#[test]
fn station_without_media_skips_registration_and_register_failure_is_preparation_error() {
    let media = media(Err(MediaError::Unavailable));
    let fixture = station(false, false);
    let mut session = Some(fixture.session);
    assert!(
        source_with_station(&media, &mut session, &settings())
            .unwrap()
            .is_none()
    );
    assert!(fixture.sources.lock().unwrap().is_empty());

    let fixture = station(true, false);
    let mut session = Some(fixture.session);
    let mut configured = settings();
    configured.speech_text = "Hello".into();
    assert!(matches!(
        source_with_station(&media, &mut session, &configured),
        Err(RuntimeError::Preparation)
    ));
}

#[test]
fn controller_registers_streams_and_starts_station_or_reports_start_failure() {
    let document = ConfigDocument::parse(
        "[1000]\n[identifier 1000 primary]\nsound_file=notice.wav\nmorse_text=E\n",
    )
    .unwrap();
    let node = NodeId::new("1000").unwrap();
    let settings = ResolvedNodeSettings::resolve(&document, &node)
        .unwrap()
        .value;
    for (fail_start, expected) in [(false, Ok(())), (true, Err(RuntimeError::Preparation))] {
        let media = streaming_media(false, fail_start);
        let result = controller(&document, &node, &settings, &media, 7);
        assert_eq!(result.map(|_| ()), expected);
        assert_eq!(media.started.load(Ordering::Acquire), !fail_start);
        assert_eq!(media.sources.lock().unwrap().len(), 1);
    }
}

#[test]
fn streamed_courtesy_applies_level_after_ring_read() {
    let document = ConfigDocument::parse(
        "[1000]\n[courtesy 1000 receiver]\ninput=receiver\nspeech_text=Hello\nmorse_text=E\nlevel_db=-6\n",
    )
    .unwrap();
    let courtesy =
        ResolvedCourtesySettings::resolve(&document, &NodeId::new("1000").unwrap(), "receiver")
            .unwrap()
            .value;
    let fixture = station(false, false);
    let mut session = Some(fixture.session);
    assert!(
        courtesy_media(
            &media(Err(MediaError::Unavailable)),
            &mut session,
            &courtesy,
            &settings()
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn courtesy_gain_stream_scales_only_produced_samples_and_forwards_lifecycle() {
    let started = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut stream = GainStream {
        source: Box::new(StreamReader {
            samples: vec![0.5, 0.25],
            offset: 0,
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        }),
        gain: 0.5,
    };
    stream.start();
    let mut output = [9.0; 3];
    assert_eq!(
        stream.render(&mut output),
        crate::audio::PcmRead::Samples(2)
    );
    assert_eq!(output, [0.25, 0.125, 9.0]);
    stream.cancel();
    assert!(started.load(Ordering::Acquire));
    assert!(cancelled.load(Ordering::Acquire));
}
