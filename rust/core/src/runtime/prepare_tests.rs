use super::*;
use std::cell::RefCell;

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
            let result = courtesy_media(&media, &resolved, &settings());
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
    assert!(controller(&document, &node, &resolved, &media).is_ok());
    let mut invalid = resolved;
    invalid.hang_ms = u64::MAX;
    assert!(matches!(
        controller(&document, &node, &invalid, &media),
        Err(RuntimeError::Preparation)
    ));
    let invalid_text =
        ConfigDocument::parse("[1000]\n[identifier 1000 invalid]\nmorse_text=~\n").unwrap();
    assert!(matches!(
        controller(&invalid_text, &node, &invalid, &media),
        Err(RuntimeError::Preparation)
    ));
}
